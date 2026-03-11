use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use crossterm::QueueableCommand;
use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{Clear, ClearType};
#[cfg(not(test))]
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use serde_yaml::Number;
use time::OffsetDateTime;

mod fleet;
mod fleet_runtime;
mod fleet_tui;
mod logging;
mod prompt_editor;
mod run_loop;
mod source_inputs;
mod template;
mod tui;

use fleet::{
    FleetControlCommand, FleetControlEnvelope, FleetListedRun, FleetRunEvent, FleetRunRecord,
    FleetVisibleArgs, dispatch_fleet_command, fleet_command_label, fleet_manager_counts,
    fleet_manager_visible_runs, load_fleet_runs, run_matches_profile_filter, send_fleet_command,
};
#[cfg(test)]
use fleet::{fleet_health, is_version_mismatch};
#[cfg(test)]
use fleet_runtime::{
    FleetControlKey, arm_bulk_action, fleet_control_key, fleet_mark_resize,
    fleet_pending_action_cleared_message, fleet_single_stop_confirmation,
    fleet_step_selection_left, fleet_step_selection_right,
};
use fleet_runtime::{run_fleet_manager_tui, run_fleet_manager_tui_embedded};
use fleet_tui::{
    FleetDetailRenderArgs, FleetPaneRenderer, LegacyFleetPaneRenderer, fleet_bulk_confirmation,
    fleet_header_line, fleet_status_line,
};
#[cfg(test)]
use fleet_tui::{fleet_detail_lines, fleet_run_list_line, fleet_run_list_lines};
use logging::{Logger, redacted_sent_detail};
use prompt_editor::{PromptEditorConfirm, PromptEditorState};
#[cfg(test)]
use prompt_editor::{
    PromptHistoryItem, load_prompt_history_items_from_paths, save_prompt_history_items_to_path,
};
#[cfg(test)]
use run_loop::*;
use run_loop::{
    RawModeGuard, compact_timestamp, fleet_heartbeat_drift_seconds,
    fleet_heartbeat_interval_seconds, format_fleet_heartbeat_metric, latest_stop_reason,
    render_with_retry, run_loop, select_history_entry, should_emit_fleet_heartbeat,
    store_run_history,
};
use source_inputs::{collect_source_inputs, dedupe_preserve_order};
use template::{collect_template_placeholders, default_template, find_missing_vars};
use tui::{
    build_grouped_log_lines, detect_icon_mode, detect_style, layout_mode, render_footer,
    render_footer_summary, resolve_ui_mode, sanitize_tui_log_line, supports_unicode,
    tui_frame_signature,
};

const LOOPMUX_VERSION: &str = env!("CARGO_PKG_VERSION");
const FLEET_HEARTBEAT_MIN_INTERVAL_SECONDS: u64 = 30;
const FLEET_HEARTBEAT_MAX_INTERVAL_SECONDS: u64 = 300;
const FLEET_HEARTBEAT_POLL_MULTIPLIER: u64 = 12;

#[derive(Debug, Parser)]
#[command(name = "loopmux")]
#[command(about = "Loop prompts into tmux panes with triggers and delays.")]
#[command(
    help_template = "{before-help}{name} {version}\n{about-with-newline}\n{usage-heading} {usage}\n\nCommands:\n{subcommands}\nOptions:\n{options}\n{after-help}"
)]
#[command(
    after_help = "Quick orientation:\n  - Runs against tmux pane scope (`all`, `session`, `session:window`, or `session:window.pane`)\n  - Default safety: trigger-edge ON (sends on false->true trigger transitions)\n  - Running `loopmux` with no subcommand auto-starts matching profiles from ~/.config/loopmux/config.yaml\n\nCommon commands:\n  - run: start looping prompts into target panes\n  - validate: check config/scope without sending\n  - init: print starter YAML template\n  - runs: inspect/stop local loopmux processes\n\nTry next:\n  loopmux run --help\n"
)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a loop against a tmux target scope.
    Run(Box<RunArgs>),
    /// Validate configuration without sending anything.
    Validate(ValidateArgs),
    /// Print a starter YAML config to stdout.
    Init(InitArgs),
    /// Simulate pane output for trigger testing.
    Simulate(SimulateArgs),
    /// Manage active local loopmux runs.
    Runs(RunsArgs),
    /// Inspect and validate workspace startup profiles.
    Config(ConfigArgs),
}

#[derive(Debug, Parser)]
#[command(
    after_help = concat!(
        "Examples:\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --once\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --exclude \"PROD\"\n  loopmux run --config loop.yaml --duration 2h\n  loopmux run --tui\n  loopmux run --exec \"gw-watch-comp\" --poll 10 --iterations 3\n\nDefaults:\n  tail=1 (last non-blank line)\n  poll=5s\n  initial-poll=5s\n  trigger-confirm-seconds=5\n  history-limit=50\n  log-preview-lines=3\n  trigger-edge=on\n  recheck-before-send=on\n\nDuration units: s, m, h, d, w, mon (30d), y (365d)\n\n",
        "Version: ",
        env!("CARGO_PKG_VERSION"),
        "\n"
    )
)]
struct RunArgs {
    /// Path to the YAML config file.
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    /// Inline prompt (mutually exclusive with --config).
    #[arg(long, conflicts_with = "config")]
    prompt: Option<String>,
    /// Inline trigger regex (requires --prompt).
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    trigger: Option<String>,
    /// Inline trigger expression (requires --prompt).
    #[arg(long, requires = "prompt", conflicts_with_all = ["config", "trigger"])]
    trigger_expr: Option<String>,
    /// Treat --trigger as an exact line match (trimmed comparison).
    #[arg(long, requires = "trigger", conflicts_with_all = ["config", "trigger_expr"])]
    trigger_exact_line: bool,
    /// Inline exclude regex.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    exclude: Option<String>,
    /// Optional pre block for inline prompt.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    pre: Option<String>,
    /// Optional post block for inline prompt.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    post: Option<String>,
    /// Inline executable command to run on each poll tick (mutually exclusive with trigger mode).
    #[arg(long, conflicts_with = "config")]
    exec: Option<String>,
    /// tmux target scope (session, session:window, or session:window.pane), overrides config.
    #[arg(long, short = 't')]
    target: Vec<String>,
    /// File containing tmux targets (one per line, '#' comments ignored).
    #[arg(long)]
    targets_file: Vec<PathBuf>,
    /// File source to scan for triggers.
    #[arg(long)]
    file: Vec<PathBuf>,
    /// File containing file sources (one path per line, '#' comments ignored).
    #[arg(long)]
    files_file: Vec<PathBuf>,
    /// Iterations to run, overrides config.
    #[arg(long, short = 'n')]
    iterations: Option<u32>,
    /// Tail lines from source capture (default 1).
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    tail: Option<usize>,
    /// Head lines from source capture.
    #[arg(long, requires = "prompt", conflicts_with_all = ["config", "tail"])]
    head: Option<usize>,
    /// Run a single send and exit.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    once: bool,
    /// Validate config and tmux target without sending.
    #[arg(long)]
    dry_run: bool,
    /// Update status output on a single line.
    #[arg(long)]
    single_line: bool,
    /// Enable TUI mode (status bar + log + shortcuts).
    #[arg(long)]
    tui: bool,
    /// Poll interval in seconds when waiting for changes.
    #[arg(long)]
    poll: Option<u64>,
    /// Initial wait in seconds before the second scan (default 5).
    #[arg(long)]
    initial_poll: Option<u64>,
    /// Seconds a trigger must remain matched before send (default 5).
    #[arg(long)]
    trigger_confirm_seconds: Option<u64>,
    /// Number of captured lines to show in folded trigger preview logs.
    #[arg(long)]
    log_preview_lines: Option<usize>,
    /// Disable trigger edge-guard and allow repeated sends while trigger stays true.
    #[arg(long)]
    no_trigger_edge: bool,
    /// Disable trigger recheck immediately before sending.
    #[arg(long)]
    no_recheck_before_send: bool,
    /// Emit per-scan trigger decision diagnostics.
    #[arg(long)]
    debug_trigger: bool,
    /// Fanout mode for matched panes.
    #[arg(long, default_value = "matched")]
    fanout: FanoutMode,
    /// Stop after a duration (e.g. 5m, 2h, 1d, 1w, 1mon, 1y).
    #[arg(long)]
    duration: Option<String>,
    /// Max history entries to keep/show for TUI picker.
    #[arg(long)]
    history_limit: Option<usize>,
    /// Max characters allowed in TUI prompt editor.
    #[arg(long)]
    prompt_edit_max_chars: Option<usize>,
    /// Optional run codename (auto-generated when omitted).
    #[arg(long)]
    name: Option<String>,
}

const DEFAULT_HISTORY_LIMIT: usize = 50;
const DEFAULT_PROMPT_EDIT_MAX_CHARS: usize = 100;
const DEFAULT_INITIAL_POLL_SECONDS: u64 = 5;
const DEFAULT_TRIGGER_CONFIRM_SECONDS: u64 = 5;

#[derive(Debug, Serialize, Deserialize, Default)]
struct RunHistory {
    entries: Vec<HistoryEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct HistoryEntry {
    last_run: String,
    target: String,
    prompt: String,
    trigger: String,
    trigger_expr: Option<String>,
    trigger_exact_line: Option<bool>,
    exclude: Option<String>,
    pre: Option<String>,
    post: Option<String>,
    iterations: Option<u32>,
    tail: Option<usize>,
    head: Option<usize>,
    once: bool,
    poll: Option<u64>,
    initial_poll: Option<u64>,
    trigger_confirm_seconds: Option<u64>,
    log_preview_lines: Option<usize>,
    trigger_edge: Option<bool>,
    recheck_before_send: Option<bool>,
    fanout: Option<FanoutMode>,
    duration: Option<String>,
}

#[derive(Debug, Parser)]
struct ValidateArgs {
    /// Path to the YAML config file.
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    /// tmux target scope (session, session:window, or session:window.pane), overrides config.
    #[arg(long, short = 't')]
    target: Vec<String>,
    /// File containing tmux targets (one per line, '#' comments ignored).
    #[arg(long)]
    targets_file: Vec<PathBuf>,
    /// File source to validate.
    #[arg(long)]
    file: Vec<PathBuf>,
    /// File containing file sources (one path per line, '#' comments ignored).
    #[arg(long)]
    files_file: Vec<PathBuf>,
    /// Iterations to run, overrides config.
    #[arg(long, short = 'n')]
    iterations: Option<u32>,
    /// Validate config without checking tmux target.
    #[arg(long)]
    skip_tmux: bool,
}

#[derive(Debug, Parser)]
struct InitArgs {
    /// Path to write the YAML config file. If omitted, prints to stdout.
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct SimulateArgs {
    /// Line to print after delay.
    #[arg(long)]
    line: String,
    /// Seconds to sleep before printing (default 5).
    #[arg(long, default_value_t = 5)]
    sleep: u64,
    /// Number of times to print the line (omit to repeat forever).
    #[arg(long)]
    repeat: Option<u32>,
}

#[derive(Debug, Parser)]
#[command(
    after_help = concat!(
        "Quick fleet flow:\n",
        "  loopmux runs ls\n",
        "  loopmux runs hold <id-or-name>\n",
        "  loopmux runs resume <id-or-name>\n",
        "  loopmux runs next <id-or-name>\n",
        "  loopmux runs renew <id-or-name>\n",
        "  loopmux runs stop <id-or-name>\n",
        "  loopmux runs --profile docs ls\n",
        "  loopmux runs tui\n\n",
        "Tip: use run names (`--name`) for easier targeting in fleet commands.\n\n",
        "Version: ",
        env!("CARGO_PKG_VERSION"),
        "\n"
    )
)]
struct RunsArgs {
    /// Filter runs by profile id/name.
    #[arg(long)]
    profile: Option<String>,
    #[command(subcommand)]
    action: Option<RunsAction>,
}

#[derive(Debug, Parser)]
#[command(after_help = concat!("Version: ", env!("CARGO_PKG_VERSION"), "\n"))]
struct ConfigArgs {
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// List discovered profiles and startup selection status.
    List {
        /// Show all profiles (including disabled and non-matching cwd).
        #[arg(long)]
        all: bool,
    },
    /// Validate profiles and print actionable per-profile errors.
    Validate {
        /// Validate all profiles (including disabled and non-matching cwd).
        #[arg(long)]
        all: bool,
    },
    /// Diagnose workspace profile setup and suggest fixes.
    Doctor {
        /// Diagnose all profiles (including disabled and non-matching cwd).
        #[arg(long)]
        all: bool,
    },
    /// Dry-run one profile by id without launching a process.
    Test {
        /// Profile id to dry-run.
        #[arg(long)]
        profile: String,
    },
}

#[derive(Debug, Subcommand)]
enum RunsAction {
    /// List active local loopmux runs.
    Ls,
    /// Open fleet manager TUI.
    Tui,
    /// Stop a run by id or name.
    Stop { target: String },
    /// Put a run on hold by id or name.
    Hold { target: String },
    /// Resume a held run by id or name.
    Resume { target: String },
    /// Force next cycle by id or name.
    Next { target: String },
    /// Renew counters and hashes by id or name.
    Renew { target: String },
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
struct Config {
    target: Option<String>,
    targets: Option<Vec<String>>,
    files: Option<Vec<String>>,
    exec: Option<ExecConfig>,
    iterations: Option<u32>,
    infinite: Option<bool>,
    poll: Option<u64>,
    initial_poll: Option<u64>,
    trigger_confirm_seconds: Option<u64>,
    log_preview_lines: Option<usize>,
    trigger_edge: Option<bool>,
    recheck_before_send: Option<bool>,
    fanout: Option<FanoutMode>,
    duration: Option<String>,
    prompt_edit_max_chars: Option<usize>,
    rule_eval: Option<RuleEval>,
    default_action: Option<Action>,
    delay: Option<DelayConfig>,
    rules: Option<Vec<Rule>>,
    logging: Option<LoggingConfig>,
    template_vars: Option<TemplateVars>,
    tail: Option<usize>,
    once: Option<bool>,
    single_line: Option<bool>,
    tui: Option<bool>,
    name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ExecConfig {
    command: String,
}

#[derive(Debug, Deserialize)]
struct WorkspaceConfig {
    imports: Option<Vec<String>>,
    runs: Option<Vec<RunProfile>>,
    events: Option<Vec<RunProfile>>,
    id: Option<String>,
    enabled: Option<bool>,
    when: Option<RunProfileWhen>,
    #[serde(flatten)]
    config: Config,
}

#[derive(Debug, Deserialize, Clone)]
struct RunProfile {
    id: Option<String>,
    enabled: Option<bool>,
    when: Option<RunProfileWhen>,
    #[serde(flatten)]
    config: Config,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct RunProfileWhen {
    cwd_matches: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct ResolvedRunProfile {
    id: String,
    source_path: PathBuf,
    config: Config,
    enabled: bool,
    when: RunProfileWhen,
}

#[derive(Debug, Default, Clone)]
struct SourceInputs {
    tmux_targets: Vec<String>,
    file_paths: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
enum FanoutMode {
    Matched,
    Broadcast,
}

#[derive(Debug, Clone)]
enum TargetScope {
    All,
    Session(String),
    Window { session: String, window: String },
    Pane(String),
}

#[derive(Debug, Clone)]
struct TmuxPane {
    target: String,
    session: String,
    window: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Action {
    pre: Option<PromptBlock>,
    prompt: Option<PromptBlock>,
    post: Option<PromptBlock>,
}

type TemplateVars = BTreeMap<String, TemplateValue>;

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
#[allow(dead_code)]
enum TemplateValue {
    String(String),
    Number(Number),
    Bool(bool),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
enum RuleEval {
    FirstMatch,
    MultiMatch,
    Priority,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[allow(dead_code)]
struct Rule {
    id: Option<String>,
    #[serde(rename = "match")]
    match_: Option<MatchCriteria>,
    exclude: Option<MatchCriteria>,
    action: Option<Action>,
    delay: Option<DelayConfig>,
    confirm_seconds: Option<u64>,
    next: Option<String>,
    priority: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct MatchCriteria {
    regex: Option<String>,
    trigger_expr: Option<String>,
    exact_line: Option<String>,
    contains: Option<String>,
    starts_with: Option<String>,
}

#[derive(Debug)]
struct TriggerExpr {
    ast: TriggerExprNode,
    terms: Vec<Regex>,
}

#[derive(Debug)]
enum TriggerExprNode {
    Term(usize),
    And(Box<TriggerExprNode>, Box<TriggerExprNode>),
    Or(Box<TriggerExprNode>, Box<TriggerExprNode>),
}

#[derive(Debug)]
enum TriggerExprRawNode {
    Term { pattern: String, pos: usize },
    And(Box<TriggerExprRawNode>, Box<TriggerExprRawNode>),
    Or(Box<TriggerExprRawNode>, Box<TriggerExprRawNode>),
}

#[derive(Debug)]
enum TriggerExprToken {
    Term { pattern: String, pos: usize },
    And { pos: usize },
    Or { pos: usize },
    LParen { pos: usize },
    RParen { pos: usize },
}

impl TriggerExprToken {
    fn pos(&self) -> usize {
        match self {
            Self::Term { pos, .. }
            | Self::And { pos }
            | Self::Or { pos }
            | Self::LParen { pos }
            | Self::RParen { pos } => *pos,
        }
    }
}

struct TriggerExprParser<'a> {
    tokens: &'a [TriggerExprToken],
    index: usize,
    source_len: usize,
}

impl<'a> TriggerExprParser<'a> {
    fn parse(mut self) -> Result<TriggerExprRawNode> {
        let expr = self.parse_expr(0)?;
        if let Some(token) = self.peek() {
            bail!(
                "invalid trigger expression at pos {}: unexpected token",
                token.pos()
            );
        }
        Ok(expr)
    }

    fn parse_expr(&mut self, min_prec: u8) -> Result<TriggerExprRawNode> {
        let mut left = self.parse_primary()?;
        while let Some((op, pos, precedence)) = self.peek_operator() {
            if precedence < min_prec {
                break;
            }
            self.index += 1;
            if let Some(next) = self.peek() {
                if matches!(
                    next,
                    TriggerExprToken::And { .. }
                        | TriggerExprToken::Or { .. }
                        | TriggerExprToken::RParen { .. }
                ) {
                    bail!("invalid trigger expression at pos {pos}: expected term after '{op}'");
                }
            } else {
                bail!("invalid trigger expression at pos {pos}: trailing operator '{op}'");
            }
            let right = self.parse_expr(precedence + 1)?;
            left = match op {
                "&&" => TriggerExprRawNode::And(Box::new(left), Box::new(right)),
                "||" => TriggerExprRawNode::Or(Box::new(left), Box::new(right)),
                _ => unreachable!(),
            };
        }
        Ok(left)
    }

    fn parse_primary(&mut self) -> Result<TriggerExprRawNode> {
        let Some(token) = self.next() else {
            bail!(
                "invalid trigger expression at pos {}: expected term",
                self.source_len
            );
        };
        match token {
            TriggerExprToken::Term { pattern, pos } => Ok(TriggerExprRawNode::Term {
                pattern: pattern.clone(),
                pos: *pos,
            }),
            TriggerExprToken::LParen { .. } => {
                let expr = self.parse_expr(0)?;
                match self.next() {
                    Some(TriggerExprToken::RParen { .. }) => Ok(expr),
                    Some(next) => bail!(
                        "invalid trigger expression at pos {}: missing right parenthesis",
                        next.pos()
                    ),
                    None => bail!(
                        "invalid trigger expression at pos {}: missing right parenthesis",
                        self.source_len
                    ),
                }
            }
            TriggerExprToken::And { pos } => {
                bail!("invalid trigger expression at pos {pos}: expected term after '&&'")
            }
            TriggerExprToken::Or { pos } => {
                bail!("invalid trigger expression at pos {pos}: expected term after '||'")
            }
            TriggerExprToken::RParen { pos } => {
                bail!("invalid trigger expression at pos {pos}: unexpected token")
            }
        }
    }

    fn peek_operator(&self) -> Option<(&'static str, usize, u8)> {
        match self.peek() {
            Some(TriggerExprToken::And { pos }) => Some(("&&", *pos, 2)),
            Some(TriggerExprToken::Or { pos }) => Some(("||", *pos, 1)),
            _ => None,
        }
    }

    fn peek(&self) -> Option<&'a TriggerExprToken> {
        self.tokens.get(self.index)
    }

    fn next(&mut self) -> Option<&'a TriggerExprToken> {
        let token = self.tokens.get(self.index);
        if token.is_some() {
            self.index += 1;
        }
        token
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct DelayConfig {
    mode: DelayMode,
    value: Option<u64>,
    min: Option<u64>,
    max: Option<u64>,
    jitter: Option<f64>,
    backoff: Option<BackoffConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
enum DelayMode {
    Fixed,
    Range,
    Jitter,
    Backoff,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct BackoffConfig {
    base: u64,
    factor: f64,
    max: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct LoggingConfig {
    path: Option<PathBuf>,
    format: Option<LogFormat>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
enum LogFormat {
    Text,
    Jsonl,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
enum PromptBlock {
    Single(String),
    Multi(Vec<String>),
}

#[derive(Debug)]
struct RuleMatch<'a> {
    rule: &'a Rule,
    index: usize,
}

#[derive(Debug, Clone)]
struct SendPlan {
    source_target: String,
    rule_id: Option<String>,
    rule_index: usize,
    next_rule: Option<String>,
    edge_key: String,
    prompt: String,
    trigger_preview: String,
    trigger_preview_lines: usize,
    stop_after: bool,
    delay_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
struct RunIdentity {
    id: String,
    name: String,
}

struct FleetRunRegistry {
    identity: RunIdentity,
    profile_id: String,
    state_path: PathBuf,
    control_path: PathBuf,
    last_control_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FleetSortMode {
    LastSeen,
    Sends,
    Health,
    Name,
    State,
}

impl FleetSortMode {
    fn next(self) -> Self {
        match self {
            FleetSortMode::LastSeen => FleetSortMode::Sends,
            FleetSortMode::Sends => FleetSortMode::Health,
            FleetSortMode::Health => FleetSortMode::Name,
            FleetSortMode::Name => FleetSortMode::State,
            FleetSortMode::State => FleetSortMode::LastSeen,
        }
    }

    fn label(self) -> &'static str {
        match self {
            FleetSortMode::LastSeen => "last_seen",
            FleetSortMode::Sends => "sends",
            FleetSortMode::Health => "health",
            FleetSortMode::Name => "name",
            FleetSortMode::State => "state",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FleetViewPreset {
    Default,
    NeedsAttention,
    MismatchOnly,
    Holding,
}

impl FleetViewPreset {
    fn next(self) -> Self {
        match self {
            FleetViewPreset::Default => FleetViewPreset::NeedsAttention,
            FleetViewPreset::NeedsAttention => FleetViewPreset::MismatchOnly,
            FleetViewPreset::MismatchOnly => FleetViewPreset::Holding,
            FleetViewPreset::Holding => FleetViewPreset::Default,
        }
    }

    fn label(self) -> &'static str {
        match self {
            FleetViewPreset::Default => "default",
            FleetViewPreset::NeedsAttention => "needs-attention",
            FleetViewPreset::MismatchOnly => "mismatch-only",
            FleetViewPreset::Holding => "holding-focus",
        }
    }
}

#[derive(Debug, Clone)]
enum PendingFleetAction {
    SingleStop {
        run_id: String,
        run_name: String,
    },
    Bulk {
        command: FleetControlCommand,
        run_ids: Vec<String>,
        run_names: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FleetStateFilter {
    All,
    Active,
    Holding,
    Stale,
}

impl FleetStateFilter {
    fn next(self) -> Self {
        match self {
            FleetStateFilter::All => FleetStateFilter::Active,
            FleetStateFilter::Active => FleetStateFilter::Holding,
            FleetStateFilter::Holding => FleetStateFilter::Stale,
            FleetStateFilter::Stale => FleetStateFilter::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            FleetStateFilter::All => "all",
            FleetStateFilter::Active => "active",
            FleetStateFilter::Holding => "holding",
            FleetStateFilter::Stale => "stale",
        }
    }

    fn allows(self, run: &FleetListedRun) -> bool {
        match self {
            FleetStateFilter::All => true,
            FleetStateFilter::Active => !run.stale,
            FleetStateFilter::Holding => !run.stale && run.record.state == "holding",
            FleetStateFilter::Stale => run.stale,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Run(args)) => run(*args),
        Some(Command::Validate(args)) => validate(args),
        Some(Command::Init(args)) => init(args),
        Some(Command::Simulate(args)) => simulate(args),
        Some(Command::Runs(args)) => runs(args),
        Some(Command::Config(args)) => config_command(args),
        None => run_default_workspace_profiles(),
    }
}

fn simulate(args: SimulateArgs) -> Result<()> {
    let delay = std::time::Duration::from_secs(args.sleep);
    match args.repeat {
        Some(count) => {
            let repeat = count.max(1);
            for _ in 0..repeat {
                if args.sleep > 0 {
                    std::thread::sleep(delay);
                }
                println!("[{}] {}", timestamp_local_now(), args.line);
                std::io::stdout().flush()?;
            }
        }
        None => loop {
            if args.sleep > 0 {
                std::thread::sleep(delay);
            }
            println!("[{}] {}", timestamp_local_now(), args.line);
            std::io::stdout().flush()?;
        },
    }
    Ok(())
}

fn run(args: RunArgs) -> Result<()> {
    let args = hydrate_run_args_from_history(args)?;
    let mut config = resolve_run_config(&args)?;
    let sources = collect_source_inputs(
        &args.target,
        &args.targets_file,
        &args.file,
        &args.files_file,
    )?;
    if !sources.tmux_targets.is_empty() {
        config.target = sources.tmux_targets.first().cloned();
        config.targets = Some(sources.tmux_targets.clone());
    }
    if !sources.file_paths.is_empty() {
        config.files = Some(sources.file_paths);
    }
    let run_name = args.name.clone().or_else(|| config.name.clone());
    let identity = resolve_run_identity(run_name.as_deref());
    let resolved = resolve_config(
        config,
        ResolveConfigArgs {
            target_override: None,
            iterations_override: args.iterations,
            skip_tmux: false,
            tail_override: args.tail,
            head_override: args.head,
            once: args.once,
            single_line: args.single_line,
            tui: args.tui,
            trigger_edge_override: args.no_trigger_edge.then_some(false),
            recheck_before_send_override: args.no_recheck_before_send.then_some(false),
            debug_trigger: args.debug_trigger,
            profile_id: None,
        },
    )?;

    if args.dry_run {
        print_validation(&resolved);
        println!("- run_id: {}", identity.id);
        println!("- run_name: {}", identity.name);
        return Ok(());
    }

    let run_result = run_loop(resolved, identity);
    if run_result.is_ok() {
        store_run_history(&args)?;
    }
    run_result
}

fn runs(args: RunsArgs) -> Result<()> {
    let profile_filter = args.profile.as_deref();
    let action = args.action.unwrap_or(RunsAction::Ls);
    if let Some((target, command)) = runs_action_fleet_command(&action) {
        return send_fleet_command(target, command);
    }
    match action {
        RunsAction::Ls => print_fleet_runs(profile_filter),
        RunsAction::Tui => run_fleet_manager_tui(profile_filter),
        RunsAction::Stop { .. }
        | RunsAction::Hold { .. }
        | RunsAction::Resume { .. }
        | RunsAction::Next { .. }
        | RunsAction::Renew { .. } => unreachable!("handled by runs_action_fleet_command"),
    }
}

fn runs_action_fleet_command(action: &RunsAction) -> Option<(&str, FleetControlCommand)> {
    match action {
        RunsAction::Stop { target } => Some((target.as_str(), FleetControlCommand::Stop)),
        RunsAction::Hold { target } => Some((target.as_str(), FleetControlCommand::Hold)),
        RunsAction::Resume { target } => Some((target.as_str(), FleetControlCommand::Resume)),
        RunsAction::Next { target } => Some((target.as_str(), FleetControlCommand::Next)),
        RunsAction::Renew { target } => Some((target.as_str(), FleetControlCommand::Renew)),
        RunsAction::Ls | RunsAction::Tui => None,
    }
}

fn config_command(args: ConfigArgs) -> Result<()> {
    let action = args.action.unwrap_or(ConfigAction::List { all: false });
    match action {
        ConfigAction::List { all } => config_list(args.config.as_ref(), all),
        ConfigAction::Validate { all } => config_validate(args.config.as_ref(), all),
        ConfigAction::Doctor { all } => config_doctor(args.config.as_ref(), all),
        ConfigAction::Test { profile } => config_test(args.config.as_ref(), &profile),
    }
}

fn config_test(path_override: Option<&PathBuf>, profile_id: &str) -> Result<()> {
    let (config_path, profiles, cwd) = load_workspace_profile_context(path_override)?;
    let matches = profiles
        .iter()
        .filter(|profile| profile.id == profile_id)
        .cloned()
        .collect::<Vec<_>>();
    if matches.is_empty() {
        bail!(
            "profile `{}` not found in {}; run `loopmux config list --all` to discover ids",
            profile_id,
            config_path.display()
        );
    }
    if matches.len() > 1 {
        bail!(
            "profile id `{}` is duplicated ({} entries); fix ids before testing",
            profile_id,
            matches.len()
        );
    }

    let profile = &matches[0];
    let cwd_match = profile_matches_cwd(profile, &cwd);
    let selected_for_startup = profile.enabled && cwd_match;

    let resolved = validate_workspace_profile(profile).with_context(|| {
        format!(
            "profile `{}` failed validation; run `loopmux config doctor --all` for guidance",
            profile_id
        )
    })?;

    println!("Config file: {}", config_path.display());
    println!("Profile id: {}", profile.id);
    println!("Source: {}", profile.source_path.display());
    println!("Enabled: {}", yes_no(profile.enabled));
    println!("Cwd match: {} ({})", yes_no(cwd_match), cwd.display());
    println!("Selected for startup: {}", yes_no(selected_for_startup));
    println!("Target: {}", resolved.target_label);
    println!("Rules: {}", resolved.rules.len());
    println!("Mode: {}", if resolved.tui { "tui" } else { "plain" });
    println!(
        "Capture: {}",
        match resolved.capture_window {
            CaptureWindow::Tail(lines) => format!("tail({lines})"),
            CaptureWindow::Head(lines) => format!("head({lines})"),
        }
    );
    println!("Dry-run OK: profile is valid and ready.");
    Ok(())
}

fn config_doctor(path_override: Option<&PathBuf>, all: bool) -> Result<()> {
    let (config_path, profiles, cwd) = load_workspace_profile_context(path_override)?;
    if profiles.is_empty() {
        bail!(
            "no runnable profiles found in {}; define a top-level profile or add `runs` entries with target/default_action/rules",
            config_path.display()
        );
    }

    let selected = selected_workspace_profiles(&profiles, &cwd, all);
    let mut issues = Vec::new();
    let mut warnings = Vec::new();

    let mut seen_ids = HashSet::new();
    for profile in &profiles {
        if !seen_ids.insert(profile.id.clone()) {
            issues.push(format!(
                "duplicate profile id `{}`; give each profile a unique `id`",
                profile.id
            ));
        }
    }

    let disabled_count = profiles.iter().filter(|profile| !profile.enabled).count();
    if disabled_count > 0 {
        warnings.push(format!(
            "{} profile(s) are disabled; set `enabled: true` if they should auto-start",
            disabled_count
        ));
    }

    let enabled_unmatched = profiles
        .iter()
        .filter(|profile| profile.enabled)
        .filter(|profile| !profile_matches_cwd(profile, &cwd))
        .count();
    if enabled_unmatched > 0 {
        warnings.push(format!(
            "{} enabled profile(s) do not match cwd `{}`; adjust `when.cwd_matches` or run from a matching folder",
            enabled_unmatched,
            cwd.display()
        ));
    }

    if selected.is_empty() {
        issues.push(format!(
            "no selected profiles for startup (all={}): use `loopmux config list --all` to inspect selection",
            yes_no(all)
        ));
    }

    let mut tui_profiles = Vec::new();
    for profile in &selected {
        match validate_workspace_profile(profile) {
            Ok(resolved) => {
                if resolved.tui {
                    tui_profiles.push(profile.id.clone());
                }
            }
            Err(err) => issues.push(format!("profile={} invalid: {err}", profile.id)),
        }
    }
    if tui_profiles.len() > 1 {
        issues.push(format!(
            "multiple selected profiles enable `tui` ({}); keep `tui: true` on only one profile",
            tui_profiles.join(", ")
        ));
    }

    println!("Workspace config: {}", config_path.display());
    println!("Current cwd: {}", cwd.display());
    println!("Profiles discovered: {}", profiles.len());
    println!("Profiles selected: {}", selected.len());

    if warnings.is_empty() {
        println!("Warnings: none");
    } else {
        println!("Warnings:");
        for warning in warnings {
            println!("- {warning}");
        }
    }

    if issues.is_empty() {
        println!("Doctor result: healthy");
        return Ok(());
    }

    bail!(
        "doctor found {} issue(s):\n- {}",
        issues.len(),
        issues.join("\n- ")
    )
}

fn config_list(path_override: Option<&PathBuf>, all: bool) -> Result<()> {
    let (config_path, profiles, cwd) = load_workspace_profile_context(path_override)?;
    if profiles.is_empty() {
        println!("No profiles found in {}", config_path.display());
        return Ok(());
    }
    let selected_ids = selected_workspace_profiles(&profiles, &cwd, all)
        .into_iter()
        .map(|profile| profile.id)
        .collect::<HashSet<_>>();

    println!("Workspace config: {}", config_path.display());
    println!("Current cwd: {}", cwd.display());
    println!(
        "Profiles (all={}):",
        if all { "yes" } else { "startup-selection" }
    );
    for profile in &profiles {
        let cwd_match = profile_matches_cwd(profile, &cwd);
        let selected = selected_ids.contains(&profile.id);
        println!(
            "- id={} enabled={} cwd_match={} selected={} source={}",
            profile.id,
            yes_no(profile.enabled),
            yes_no(cwd_match),
            yes_no(selected),
            profile.source_path.display()
        );
    }
    println!(
        "Selected profiles: {} of {}",
        selected_ids.len(),
        profiles.len()
    );
    Ok(())
}

fn config_validate(path_override: Option<&PathBuf>, all: bool) -> Result<()> {
    let (config_path, profiles, cwd) = load_workspace_profile_context(path_override)?;
    let selected = selected_workspace_profiles(&profiles, &cwd, all);
    if selected.is_empty() {
        println!(
            "No profiles selected for validation in {} (cwd={})",
            config_path.display(),
            cwd.display()
        );
        return Ok(());
    }

    let mut errors = Vec::new();
    let mut validated = 0usize;
    for profile in &selected {
        match validate_workspace_profile(profile) {
            Ok(resolved) => {
                validated += 1;
                println!(
                    "OK profile={} target={} rules={} mode={}",
                    profile.id,
                    resolved.target_label,
                    resolved.rules.len(),
                    if resolved.tui { "tui" } else { "plain" }
                );
            }
            Err(err) => errors.push(format!("profile={} error={err}", profile.id)),
        }
    }
    if !errors.is_empty() {
        bail!(
            "validation failed for {}/{} selected profiles in {}:\n- {}",
            errors.len(),
            selected.len(),
            config_path.display(),
            errors.join("\n- ")
        );
    }

    println!(
        "Validation OK: {} profile(s) validated from {}",
        validated,
        config_path.display()
    );
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn load_workspace_profile_context(
    path_override: Option<&PathBuf>,
) -> Result<(PathBuf, Vec<ResolvedRunProfile>, PathBuf)> {
    let config_path = resolve_workspace_config_path(path_override)?;
    if !config_path.exists() && path_override.is_none() {
        ensure_default_workspace_config_exists(&config_path)?;
    }
    if !config_path.exists() {
        bail!("workspace config not found at {}", config_path.display());
    }
    let profiles = load_workspace_profiles(&config_path)?;
    let cwd = std::env::current_dir().context("failed to read current working directory")?;
    Ok((config_path, profiles, cwd))
}

fn ensure_default_workspace_config_exists(config_path: &Path) -> Result<()> {
    if config_path.exists() {
        return Ok(());
    }
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let template = default_template();
    std::fs::write(config_path, template).with_context(|| {
        format!(
            "failed to write default config to {}",
            config_path.display()
        )
    })?;
    println!(
        "Created default workspace config at {}",
        config_path.display()
    );
    println!("Included `continue-loop` starter event (`exact_line: <CONTINUE-LOOP>`).");
    Ok(())
}

fn resolve_workspace_config_path(path_override: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = path_override {
        return Ok(path.clone());
    }
    default_workspace_config_path()
}

fn selected_workspace_profiles(
    profiles: &[ResolvedRunProfile],
    cwd: &Path,
    all: bool,
) -> Vec<ResolvedRunProfile> {
    profiles
        .iter()
        .filter(|profile| all || profile.enabled)
        .filter(|profile| all || profile_matches_cwd(profile, cwd))
        .cloned()
        .collect()
}

fn validate_workspace_profile(profile: &ResolvedRunProfile) -> Result<ResolvedConfig> {
    resolve_config(
        profile.config.clone(),
        ResolveConfigArgs {
            target_override: None,
            iterations_override: None,
            skip_tmux: false,
            tail_override: None,
            head_override: None,
            once: false,
            single_line: false,
            tui: false,
            trigger_edge_override: None,
            recheck_before_send_override: None,
            debug_trigger: false,
            profile_id: Some(profile.id.clone()),
        },
    )
}

fn run_default_workspace_profiles() -> Result<()> {
    let (config_path, profiles, cwd) = load_workspace_profile_context(None)?;
    if profiles.is_empty() {
        bail!(
            "default config loaded from {} but no runnable profiles were defined",
            config_path.display()
        );
    }

    let selected = selected_workspace_profiles(&profiles, &cwd, false);

    if selected.is_empty() {
        println!(
            "No enabled profiles matched cwd={} from {}",
            cwd.display(),
            config_path.display()
        );
        println!("Tip: add `when.cwd_matches` patterns or disable filters for a profile.");
        return Ok(());
    }

    let mut validation_errors = Vec::new();
    let mut tui_profiles = Vec::new();
    for profile in &selected {
        match validate_workspace_profile(profile) {
            Ok(resolved) => {
                if resolved.tui {
                    tui_profiles.push(profile.id.clone());
                }
            }
            Err(err) => validation_errors.push(format!("profile={} error={err}", profile.id)),
        }
    }
    if !validation_errors.is_empty() {
        bail!(
            "profile validation failed:\n- {}",
            validation_errors.join("\n- ")
        );
    }
    if tui_profiles.len() > 1 {
        bail!(
            "multiple matched profiles enable tui ({}) which cannot share one terminal; disable tui on all but one profile",
            tui_profiles.join(", ")
        );
    }

    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    for profile in selected {
        let runtime_path = write_runtime_profile_config(&profile)?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("run").arg("--config").arg(&runtime_path);
        if let Some(name) = profile
            .config
            .name
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            cmd.arg("--name").arg(name);
        } else {
            cmd.arg("--name").arg(&profile.id);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        let child = cmd.spawn().with_context(|| {
            format!(
                "failed to start profile={} from {}",
                profile.id,
                profile.source_path.display()
            )
        })?;
        println!(
            "Started profile={} pid={} source={} runtime={}",
            profile.id,
            child.id(),
            profile.source_path.display(),
            runtime_path.display()
        );
    }

    println!("Use `loopmux runs ls` or `loopmux runs tui` to monitor active runs.");
    Ok(())
}

fn default_workspace_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set for default config path")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("loopmux")
        .join("config.yaml"))
}

fn runtime_profiles_dir() -> Result<PathBuf> {
    Ok(fleet_dir()?.join("profiles"))
}

fn load_workspace_profiles(path: &PathBuf) -> Result<Vec<ResolvedRunProfile>> {
    let mut visited = HashSet::new();
    load_workspace_profiles_from_path(path, &mut visited)
}

fn load_workspace_profiles_from_path(
    path: &PathBuf,
    visited: &mut HashSet<PathBuf>,
) -> Result<Vec<ResolvedRunProfile>> {
    let absolute_path = if path.is_absolute() {
        path.clone()
    } else {
        std::env::current_dir()
            .context("failed to get cwd for profile path")?
            .join(path)
    };
    let normalized = absolute_path
        .canonicalize()
        .unwrap_or(absolute_path.clone());
    if !visited.insert(normalized.clone()) {
        return Ok(Vec::new());
    }

    let contents = std::fs::read_to_string(&normalized)
        .with_context(|| format!("failed to read {}", normalized.display()))?;
    let workspace: WorkspaceConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", normalized.display()))?;

    let mut profiles = Vec::new();
    let mut index = 0usize;
    if config_has_profile_definition(&workspace.config) {
        let id = workspace
            .id
            .clone()
            .unwrap_or_else(|| "main".to_string())
            .trim()
            .to_string();
        profiles.push(ResolvedRunProfile {
            id: if id.is_empty() {
                "main".to_string()
            } else {
                sanitize_run_name(&id)
            },
            source_path: normalized.clone(),
            config: workspace.config.clone(),
            enabled: workspace.enabled.unwrap_or(true),
            when: workspace.when.clone().unwrap_or_default(),
        });
        index += 1;
    }

    let mut declared_runs = workspace.runs.unwrap_or_default();
    declared_runs.extend(workspace.events.unwrap_or_default());
    for (run_index, run) in declared_runs.into_iter().enumerate() {
        if !config_has_profile_definition(&run.config) {
            continue;
        }
        let fallback = format!("run-{}", index + run_index + 1);
        let id = run
            .id
            .as_ref()
            .filter(|value| !value.trim().is_empty())
            .map(|value| sanitize_run_name(value))
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback);
        profiles.push(ResolvedRunProfile {
            id,
            source_path: normalized.clone(),
            config: run.config,
            enabled: run.enabled.unwrap_or(true),
            when: run.when.unwrap_or_default(),
        });
    }

    for import in workspace.imports.unwrap_or_default() {
        let import_path = resolve_workspace_import_path(&normalized, &import)?;
        profiles.extend(load_workspace_profiles_from_path(&import_path, visited)?);
    }

    Ok(profiles)
}

fn config_has_profile_definition(config: &Config) -> bool {
    config.default_action.is_some()
        || config.rules.is_some()
        || config.exec.is_some()
        || config.target.is_some()
        || config
            .targets
            .as_ref()
            .is_some_and(|targets| !targets.is_empty())
}

fn resolve_workspace_import_path(base_config_path: &Path, value: &str) -> Result<PathBuf> {
    let expanded = if let Some(stripped) = value.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME not set for import expansion")?;
        PathBuf::from(home).join(stripped)
    } else {
        PathBuf::from(value)
    };
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let parent = base_config_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "failed to resolve import '{}' because base path has no parent",
            value
        )
    })?;
    Ok(parent.join(expanded))
}

fn profile_matches_cwd(profile: &ResolvedRunProfile, cwd: &Path) -> bool {
    let Some(patterns) = profile.when.cwd_matches.as_ref() else {
        return true;
    };
    if patterns.is_empty() {
        return true;
    }
    let cwd_value = cwd.display().to_string();
    patterns
        .iter()
        .filter_map(|pattern| expand_workspace_pattern(pattern).ok())
        .any(|pattern| wildcard_match(&pattern, &cwd_value))
}

fn expand_workspace_pattern(value: &str) -> Result<String> {
    if let Some(stripped) = value.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME not set for pattern expansion")?;
        return Ok(PathBuf::from(home).join(stripped).display().to_string());
    }
    Ok(value.to_string())
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == value {
        return true;
    }
    let escaped = regex::escape(pattern).replace("\\*", ".*");
    let regex_value = format!("^{escaped}$");
    Regex::new(&regex_value)
        .map(|regex| regex.is_match(value))
        .unwrap_or(false)
}

fn write_runtime_profile_config(profile: &ResolvedRunProfile) -> Result<PathBuf> {
    let dir = runtime_profiles_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create runtime profile dir: {}", dir.display()))?;
    let timestamp = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let path = dir.join(format!("{}-{timestamp}.yaml", profile.id));
    let mut config = profile.config.clone();
    if config.name.is_none() {
        config.name = Some(profile.id.clone());
    }
    let serialized = serde_yaml::to_string(&config)
        .with_context(|| format!("failed to serialize profile config: {}", profile.id))?;
    std::fs::write(&path, serialized)
        .with_context(|| format!("failed to write runtime profile file: {}", path.display()))?;
    Ok(path)
}

fn hydrate_run_args_from_history(mut args: RunArgs) -> Result<RunArgs> {
    let needs_history = args.tui
        && args.config.is_none()
        && args.prompt.is_none()
        && args.trigger.is_none()
        && args.trigger_expr.is_none();
    if !needs_history {
        return Ok(args);
    }

    let entry = select_history_entry(args.history_limit.unwrap_or(DEFAULT_HISTORY_LIMIT))?;
    if args.target.is_empty() {
        args.target = vec![entry.target];
    }
    args.prompt = Some(entry.prompt);
    if !entry.trigger.trim().is_empty() {
        args.trigger = Some(entry.trigger);
    }
    args.trigger_expr = entry.trigger_expr;
    if !args.trigger_exact_line {
        args.trigger_exact_line = entry.trigger_exact_line.unwrap_or(false);
    }
    args.exclude = entry.exclude;
    args.pre = entry.pre;
    args.post = entry.post;
    if args.iterations.is_none() {
        args.iterations = entry.iterations;
    }
    if args.tail.is_none() {
        args.tail = entry.tail;
    }
    if args.head.is_none() {
        args.head = entry.head;
    }
    if !args.once {
        args.once = entry.once;
    }
    if args.poll.is_none() {
        args.poll = entry.poll;
    }
    if args.initial_poll.is_none() {
        args.initial_poll = entry.initial_poll;
    }
    if args.trigger_confirm_seconds.is_none() {
        args.trigger_confirm_seconds = entry.trigger_confirm_seconds;
    }
    if args.log_preview_lines.is_none() {
        args.log_preview_lines = entry.log_preview_lines;
    }
    if !args.no_trigger_edge
        && let Some(trigger_edge) = entry.trigger_edge
    {
        args.no_trigger_edge = !trigger_edge;
    }
    if !args.no_recheck_before_send
        && let Some(recheck_before_send) = entry.recheck_before_send
    {
        args.no_recheck_before_send = !recheck_before_send;
    }
    if args.fanout == FanoutMode::Matched
        && let Some(fanout) = entry.fanout
    {
        args.fanout = fanout;
    }
    if args.duration.is_none() {
        args.duration = entry.duration;
    }
    Ok(args)
}

fn history_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set for history storage")?;
    Ok(PathBuf::from(home).join(".loopmux"))
}

fn history_path() -> Result<PathBuf> {
    Ok(history_dir()?.join("history.json"))
}

fn prompt_editor_history_path() -> Result<PathBuf> {
    Ok(history_dir()?.join("prompt_editor_history.json"))
}

fn fleet_dir() -> Result<PathBuf> {
    Ok(history_dir()?.join("runs"))
}

fn fleet_state_dir() -> Result<PathBuf> {
    Ok(fleet_dir()?.join("state"))
}

fn fleet_control_dir() -> Result<PathBuf> {
    Ok(fleet_dir()?.join("control"))
}

fn fleet_state_path(run_id: &str) -> Result<PathBuf> {
    Ok(fleet_state_dir()?.join(format!("{run_id}.json")))
}

fn fleet_control_path(run_id: &str) -> Result<PathBuf> {
    Ok(fleet_control_dir()?.join(format!("{run_id}.json")))
}

fn resolve_run_identity(name_override: Option<&str>) -> RunIdentity {
    let pid = std::process::id();
    let now = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let id = format!("run-{now}-{pid}");
    let name = name_override
        .map(sanitize_run_name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(auto_run_name);
    RunIdentity { id, name }
}

fn sanitize_run_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn auto_run_name() -> String {
    const ADJECTIVES: &[&str] = &[
        "amber", "brisk", "calm", "daring", "ember", "frost", "gold", "hazel", "indigo", "jolly",
        "keen", "lunar", "mellow", "nova", "opal", "proud", "quick", "river",
    ];
    const NOUNS: &[&str] = &[
        "otter", "fox", "owl", "lynx", "falcon", "orca", "puma", "raven", "kite", "heron", "wolf",
        "bison", "yak", "ibis", "drake", "badger", "beaver", "hare",
    ];
    let seed = OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .unsigned_abs();
    let adj = ADJECTIVES[(seed as usize) % ADJECTIVES.len()];
    let noun = NOUNS[((seed / 97) as usize) % NOUNS.len()];
    let suffix = (seed % 10_000) as u16;
    format!("{adj}-{noun}-{suffix:04}")
}

impl FleetRunRegistry {
    fn new(identity: RunIdentity, profile_id: Option<String>) -> Result<Self> {
        std::fs::create_dir_all(fleet_state_dir()?)?;
        std::fs::create_dir_all(fleet_control_dir()?)?;
        let profile_id = profile_id
            .unwrap_or_else(|| identity.name.clone())
            .trim()
            .to_string();
        Ok(Self {
            state_path: fleet_state_path(&identity.id)?,
            control_path: fleet_control_path(&identity.id)?,
            identity,
            profile_id,
            last_control_token: None,
        })
    }

    fn update(&self, target: &str, state: LoopState, sends: u32, poll_seconds: u64) -> Result<()> {
        let now = timestamp_now();
        let host = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("HOST"))
            .unwrap_or_else(|_| "localhost".to_string());
        let state_label = fleet_state_label(state).to_string();
        let base_record = FleetRunRecord {
            id: self.identity.id.clone(),
            name: self.identity.name.clone(),
            profile_id: self.profile_id.clone(),
            pid: std::process::id(),
            host,
            target: target.to_string(),
            state: state_label.clone(),
            sends,
            poll_seconds,
            started_at: now.clone(),
            last_seen: now.clone(),
            version: LOOPMUX_VERSION.to_string(),
            events: Vec::new(),
            heartbeat_sends_reported: sends,
            heartbeat_reported_at: Some(now.clone()),
        };

        let mut record = if self.state_path.exists() {
            match std::fs::read_to_string(&self.state_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<FleetRunRecord>(&raw).ok())
            {
                Some(existing) => {
                    let mut events = existing.events;
                    if existing.state != state_label {
                        events.push(FleetRunEvent {
                            timestamp: now.clone(),
                            kind: "state".to_string(),
                            detail: format!("{} -> {}", existing.state, state_label),
                        });
                    }
                    if sends > existing.sends {
                        events.push(FleetRunEvent {
                            timestamp: now.clone(),
                            kind: "send".to_string(),
                            detail: format!("+{} sends (total {})", sends - existing.sends, sends),
                        });
                    }
                    if existing.target != target {
                        events.push(FleetRunEvent {
                            timestamp: now.clone(),
                            kind: "target".to_string(),
                            detail: format!("{} -> {}", existing.target, target),
                        });
                    }
                    let heartbeat_interval = fleet_heartbeat_interval_seconds(poll_seconds);
                    let should_emit_heartbeat = should_emit_fleet_heartbeat(
                        OffsetDateTime::now_utc(),
                        existing.heartbeat_reported_at.as_deref(),
                        poll_seconds,
                    );
                    let mut heartbeat_sends_reported = if sends < existing.heartbeat_sends_reported
                    {
                        sends
                    } else {
                        existing.heartbeat_sends_reported
                    };
                    let mut heartbeat_reported_at = existing.heartbeat_reported_at.clone();
                    if heartbeat_reported_at.is_none() {
                        heartbeat_sends_reported = sends;
                        heartbeat_reported_at = Some(now.clone());
                    }
                    if should_emit_heartbeat {
                        let sends_delta = sends.saturating_sub(heartbeat_sends_reported);
                        let drift_seconds = fleet_heartbeat_drift_seconds(
                            OffsetDateTime::now_utc(),
                            existing.heartbeat_reported_at.as_deref(),
                            heartbeat_interval,
                        );
                        events.push(FleetRunEvent {
                            timestamp: now.clone(),
                            kind: "heartbeat".to_string(),
                            detail: format_fleet_heartbeat_metric(
                                state,
                                sends,
                                sends_delta,
                                poll_seconds,
                                heartbeat_interval,
                                drift_seconds,
                            ),
                        });
                        heartbeat_sends_reported = sends;
                        heartbeat_reported_at = Some(now.clone());
                    }
                    if events.len() > 24 {
                        let keep_from = events.len() - 24;
                        events.drain(0..keep_from);
                    }
                    FleetRunRecord {
                        started_at: existing.started_at,
                        events,
                        heartbeat_sends_reported,
                        heartbeat_reported_at,
                        ..base_record
                    }
                }
                None => {
                    let mut record = base_record;
                    record.events.push(FleetRunEvent {
                        timestamp: now.clone(),
                        kind: "start".to_string(),
                        detail: format!("run started on {}", target),
                    });
                    record
                }
            }
        } else {
            let mut record = base_record;
            record.events.push(FleetRunEvent {
                timestamp: now.clone(),
                kind: "start".to_string(),
                detail: format!("run started on {}", target),
            });
            record
        };
        record.last_seen = now;
        let content = serde_json::to_string_pretty(&record)?;
        std::fs::write(&self.state_path, content)?;
        Ok(())
    }

    fn consume_control_command(&mut self) -> Result<Option<FleetControlCommand>> {
        if !self.control_path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&self.control_path)?;
        let envelope: FleetControlEnvelope = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => {
                let _ = std::fs::remove_file(&self.control_path);
                return Ok(None);
            }
        };
        if self
            .last_control_token
            .as_ref()
            .map(|token| token == &envelope.token)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        self.last_control_token = Some(envelope.token);
        let _ = std::fs::remove_file(&self.control_path);
        Ok(Some(envelope.command))
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.state_path);
        let _ = std::fs::remove_file(&self.control_path);
    }
}

impl Drop for FleetRunRegistry {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn fleet_state_label(state: LoopState) -> &'static str {
    match state {
        LoopState::Running => "running",
        LoopState::Holding => "holding",
        LoopState::Waiting => "waiting",
        LoopState::Delay => "delay",
        LoopState::Sending => "sending",
        LoopState::Error => "error",
        LoopState::Stopped => "stopped",
    }
}

fn resolve_fleet_target(target: &str, runs: &[FleetListedRun]) -> Result<FleetListedRun> {
    if let Some(run) = runs
        .iter()
        .find(|run| run.record.id == target && !run.stale)
    {
        return Ok(run.clone());
    }
    let matches = runs
        .iter()
        .filter(|run| run.record.name == target && !run.stale)
        .cloned()
        .collect::<Vec<_>>();
    if matches.is_empty() {
        if runs
            .iter()
            .any(|run| run.record.id == target || run.record.name == target)
        {
            bail!("run is stale/inactive: {target}");
        }
        bail!("run not found: {target}");
    }
    if matches.len() > 1 {
        bail!("multiple runs share name '{target}', use run id");
    }
    Ok(matches[0].clone())
}

fn print_fleet_runs(profile_filter: Option<&str>) -> Result<()> {
    let mut runs = load_fleet_runs()?;
    if let Some(profile_filter) = profile_filter {
        runs.retain(|run| run_matches_profile_filter(run, profile_filter));
    }
    runs.sort_by(|a, b| b.record.last_seen.cmp(&a.record.last_seen));
    if runs.is_empty() {
        if let Some(profile_filter) = profile_filter {
            println!(
                "No local loopmux runs found for profile filter '{}'.",
                profile_filter
            );
        } else {
            println!("No local loopmux runs found.");
        }
        return Ok(());
    }
    println!("Active local loopmux runs (local v{}):", LOOPMUX_VERSION);
    for run in runs {
        let stale = if run.stale { "stale" } else { "active" };
        let version = if run.record.version.is_empty() {
            "unknown"
        } else {
            run.record.version.as_str()
        };
        let mismatch = if run.version_mismatch {
            "mismatch"
        } else {
            "match"
        };
        println!(
            "- {} ({}) id={} profile={} pid={} state={} sends={} target={} version={} ({}) last_seen={}",
            run.record.name,
            stale,
            run.record.id,
            if run.record.profile_id.trim().is_empty() {
                "-"
            } else {
                run.record.profile_id.as_str()
            },
            run.record.pid,
            run.record.state,
            run.record.sends,
            run.record.target,
            version,
            mismatch,
            run.record.last_seen,
        );
    }
    Ok(())
}

fn build_prompt(action: &Action) -> String {
    let mut parts = Vec::new();
    push_block(&mut parts, action.pre.as_ref());
    push_block(&mut parts, action.prompt.as_ref());
    push_block(&mut parts, action.post.as_ref());
    parts.join("\n")
}

fn push_block(parts: &mut Vec<String>, block: Option<&PromptBlock>) {
    let Some(block) = block else {
        return;
    };
    match block {
        PromptBlock::Single(text) => parts.push(text.clone()),
        PromptBlock::Multi(items) => parts.extend(items.iter().cloned()),
    }
}

fn compute_delay_seconds(
    delay: &DelayConfig,
    rule_match: &RuleMatch<'_>,
    backoff_state: &mut std::collections::HashMap<String, BackoffState>,
) -> Result<u64> {
    match delay.mode {
        DelayMode::Fixed => Ok(delay.value.unwrap_or(0)),
        DelayMode::Range => random_between(delay.min.unwrap_or(0), delay.max.unwrap_or(0)),
        DelayMode::Jitter => {
            let base = random_between(delay.min.unwrap_or(0), delay.max.unwrap_or(0))? as f64;
            let jitter = delay.jitter.unwrap_or(0.0);
            let spread = base * jitter;
            let min = (base - spread).max(0.0);
            let max = base + spread;
            let jittered = random_between(min as u64, max as u64)? as f64;
            Ok(jittered as u64)
        }
        DelayMode::Backoff => delay
            .backoff
            .as_ref()
            .map(|backoff| {
                let key = rule_match
                    .rule
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("rule-{}", rule_match.index));
                let state = backoff_state.entry(key).or_insert(BackoffState {
                    attempts: 0,
                    last_sent: None,
                });
                state.attempts = state.attempts.saturating_add(1);
                state.last_sent = Some(OffsetDateTime::now_utc());
                let factor = backoff.factor;
                let exponent = (state.attempts.saturating_sub(1)) as i32;
                let mut delay = (backoff.base as f64) * factor.powi(exponent);
                if let Some(max) = backoff.max {
                    delay = delay.min(max as f64);
                }
                delay as u64
            })
            .ok_or_else(|| anyhow::anyhow!("delay.mode=backoff requires backoff")),
    }
}

fn random_between(min: u64, max: u64) -> Result<u64> {
    if min > max {
        bail!("invalid delay range: {min}-{max}");
    }
    if min == max {
        return Ok(min);
    }
    let span = max - min + 1;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system time error")?
        .subsec_nanos() as u64;
    Ok(min + (nanos % span))
}

fn validate(args: ValidateArgs) -> Result<()> {
    let mut config = load_config(args.config.as_ref())?;
    let sources = collect_source_inputs(
        &args.target,
        &args.targets_file,
        &args.file,
        &args.files_file,
    )?;
    if !sources.tmux_targets.is_empty() {
        config.target = sources.tmux_targets.first().cloned();
        config.targets = Some(sources.tmux_targets.clone());
    }
    if !sources.file_paths.is_empty() {
        config.files = Some(sources.file_paths);
    }
    let resolved = resolve_config(
        config,
        ResolveConfigArgs {
            target_override: None,
            iterations_override: args.iterations,
            skip_tmux: args.skip_tmux,
            tail_override: None,
            head_override: None,
            once: false,
            single_line: false,
            tui: false,
            trigger_edge_override: None,
            recheck_before_send_override: None,
            debug_trigger: false,
            profile_id: None,
        },
    )?;
    print_validation(&resolved);
    Ok(())
}

fn init(args: InitArgs) -> Result<()> {
    let template = default_template();
    if let Some(path) = args.output {
        std::fs::write(&path, template)
            .with_context(|| format!("failed to write template to {}", path.display()))?;
        println!("Wrote template to {}", path.display());
    } else {
        print!("{template}");
    }
    Ok(())
}

fn load_config(path: Option<&PathBuf>) -> Result<Config> {
    let Some(path) = path else {
        bail!("--config is required");
    };
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: Config = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(config)
}

fn resolve_run_config(args: &RunArgs) -> Result<Config> {
    if args.config.is_some() {
        return load_config(args.config.as_ref());
    }

    if let Some(exec_command) = args.exec.as_ref() {
        if args.prompt.is_some()
            || args.trigger.is_some()
            || args.trigger_expr.is_some()
            || args.trigger_exact_line
            || args.exclude.is_some()
            || args.pre.is_some()
            || args.post.is_some()
        {
            bail!(
                "--exec cannot be combined with --prompt/--trigger/--trigger-expr/--exclude/--pre/--post"
            );
        }
        if !args.target.is_empty()
            || !args.targets_file.is_empty()
            || !args.file.is_empty()
            || !args.files_file.is_empty()
        {
            bail!("--exec cannot be combined with --target/--targets-file/--file/--files-file");
        }

        let command = exec_command.trim();
        if command.is_empty() {
            bail!("--exec command cannot be empty");
        }

        return Ok(Config {
            target: None,
            targets: None,
            files: None,
            exec: Some(ExecConfig {
                command: command.to_string(),
            }),
            iterations: args.iterations,
            infinite: None,
            poll: args.poll,
            initial_poll: args.initial_poll,
            trigger_confirm_seconds: args.trigger_confirm_seconds,
            log_preview_lines: args.log_preview_lines,
            trigger_edge: Some(!args.no_trigger_edge),
            recheck_before_send: Some(!args.no_recheck_before_send),
            fanout: Some(args.fanout),
            duration: args.duration.clone(),
            prompt_edit_max_chars: args.prompt_edit_max_chars,
            rule_eval: None,
            default_action: None,
            delay: None,
            rules: None,
            logging: None,
            template_vars: None,
            tail: args.tail,
            once: Some(args.once),
            single_line: Some(args.single_line),
            tui: Some(args.tui),
            name: args.name.clone(),
        });
    }

    let Some(prompt) = args.prompt.as_ref() else {
        bail!("--config or --prompt is required");
    };
    if args.trigger.is_none() && args.trigger_expr.is_none() {
        bail!("--trigger or --trigger-expr is required when using --prompt");
    };

    let default_action = Action {
        pre: args
            .pre
            .as_ref()
            .map(|value| PromptBlock::Single(value.clone())),
        prompt: Some(PromptBlock::Single(prompt.clone())),
        post: args
            .post
            .as_ref()
            .map(|value| PromptBlock::Single(value.clone())),
    };
    let rule = Rule {
        id: Some("inline".to_string()),
        match_: Some(MatchCriteria {
            regex: if args.trigger_expr.is_some() || args.trigger_exact_line {
                None
            } else {
                args.trigger.clone()
            },
            trigger_expr: args.trigger_expr.clone(),
            exact_line: if args.trigger_expr.is_none() && args.trigger_exact_line {
                args.trigger.clone()
            } else {
                None
            },
            contains: None,
            starts_with: None,
        }),
        exclude: args.exclude.as_ref().map(|value| MatchCriteria {
            regex: Some(value.clone()),
            trigger_expr: None,
            exact_line: None,
            contains: None,
            starts_with: None,
        }),
        action: None,
        delay: None,
        confirm_seconds: None,
        next: None,
        priority: None,
    };

    Ok(Config {
        target: args.target.first().cloned(),
        targets: if args.target.is_empty() {
            None
        } else {
            Some(args.target.clone())
        },
        files: None,
        exec: None,
        iterations: args.iterations,
        infinite: None,
        poll: args.poll,
        initial_poll: args.initial_poll,
        trigger_confirm_seconds: args.trigger_confirm_seconds,
        log_preview_lines: args.log_preview_lines,
        trigger_edge: Some(!args.no_trigger_edge),
        recheck_before_send: Some(!args.no_recheck_before_send),
        fanout: Some(args.fanout),
        duration: args.duration.clone(),
        prompt_edit_max_chars: args.prompt_edit_max_chars,
        rule_eval: Some(RuleEval::FirstMatch),
        default_action: Some(default_action),
        delay: None,
        rules: Some(vec![rule]),
        logging: None,
        template_vars: None,
        tail: args.tail,
        once: Some(args.once),
        single_line: Some(args.single_line),
        tui: Some(args.tui),
        name: args.name.clone(),
    })
}

#[derive(Debug)]
struct ResolvedConfig {
    profile_id: Option<String>,
    exec_command: Option<String>,
    target_scope: TargetScope,
    target_label: String,
    explicit_targets: Option<Vec<String>>,
    file_sources: Vec<String>,
    iterations: Option<u32>,
    infinite: bool,
    has_prompt: bool,
    poll: u64,
    initial_poll: u64,
    trigger_confirm_seconds: u64,
    log_preview_lines: usize,
    trigger_edge: bool,
    recheck_before_send: bool,
    debug_trigger: bool,
    fanout: FanoutMode,
    duration: Option<Duration>,
    prompt_edit_max_chars: usize,
    rule_eval: RuleEval,
    rules: Vec<Rule>,
    delay: Option<DelayConfig>,
    prompt_placeholders: Vec<String>,
    template_vars: Vec<String>,
    default_action: Action,
    logging: LoggingConfigResolved,
    capture_window: CaptureWindow,
    once: bool,
    single_line: bool,
    tui: bool,
}

#[derive(Debug, Clone, Copy)]
enum CaptureWindow {
    Tail(usize),
    Head(usize),
}

impl CaptureWindow {
    fn from_overrides(tail: Option<usize>, head: Option<usize>) -> Self {
        if let Some(lines) = head {
            return Self::Head(lines.max(1));
        }
        Self::Tail(tail.unwrap_or(1).max(1))
    }

    fn lines(self) -> usize {
        match self {
            Self::Tail(lines) | Self::Head(lines) => lines,
        }
    }

    fn is_tail(self) -> bool {
        matches!(self, Self::Tail(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiMode {
    Plain,
    SingleLine,
    Tui,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopState {
    Running,
    Holding,
    Waiting,
    Delay,
    Sending,
    Error,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Compact,
    Standard,
    Wide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IconMode {
    Nerd,
    Ascii,
}

#[derive(Debug, Clone, Copy)]
struct StyleConfig {
    use_color: bool,
    use_bg: bool,
    use_unicode_ellipsis: bool,
    dim_logs: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuiAction {
    Pause,
    Resume,
    HoldToggle,
    Fleet,
    Stop,
    Next,
    Renew,
    ActiveListToggle,
    ActiveListUp,
    ActiveListDown,
    ActiveListLeft,
    ActiveListRight,
    ActiveListToggleSelection,
    ActiveListEnableAll,
    ActiveListDisableAll,
    ActiveListClose,
    PromptEditorToggle,
    PromptEditorDeleteSelected,
    PromptEditorClearHistory,
    PromptEditorUndo,
    PromptEditorConfirmYes,
    PromptEditorConfirmNo,
    PromptEditorBackspace,
    PromptEditorInput(char),
    ToggleLogView,
    Redraw,
    Quit,
}

fn map_run_tui_key_action(
    code: KeyCode,
    modifiers: KeyModifiers,
    prompt_editor_open: bool,
    prompt_confirm_open: bool,
) -> Option<TuiAction> {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        return Some(TuiAction::Stop);
    }
    if prompt_editor_open {
        return match code {
            KeyCode::Char('e') => Some(TuiAction::PromptEditorToggle),
            KeyCode::Up => Some(TuiAction::ActiveListUp),
            KeyCode::Down => Some(TuiAction::ActiveListDown),
            KeyCode::Enter => Some(TuiAction::ActiveListToggleSelection),
            KeyCode::Char(' ') => Some(TuiAction::ActiveListToggleSelection),
            KeyCode::Char('d') => Some(TuiAction::PromptEditorDeleteSelected),
            KeyCode::Char('c') => Some(TuiAction::PromptEditorClearHistory),
            KeyCode::Char('u') => Some(TuiAction::PromptEditorUndo),
            KeyCode::Char('y') if prompt_confirm_open => Some(TuiAction::PromptEditorConfirmYes),
            KeyCode::Char('n') | KeyCode::Char('N') if prompt_confirm_open => {
                Some(TuiAction::PromptEditorConfirmNo)
            }
            KeyCode::Backspace => Some(TuiAction::PromptEditorBackspace),
            KeyCode::Esc => Some(TuiAction::ActiveListClose),
            KeyCode::Char(ch) if !ch.is_control() => Some(TuiAction::PromptEditorInput(ch)),
            _ => None,
        };
    }
    match code {
        KeyCode::Char('p') => Some(TuiAction::Pause),
        KeyCode::Char('r') => Some(TuiAction::Resume),
        KeyCode::Char('h') => Some(TuiAction::HoldToggle),
        KeyCode::Char('f') => Some(TuiAction::Fleet),
        KeyCode::Char('e') => Some(TuiAction::PromptEditorToggle),
        KeyCode::Char('l') => Some(TuiAction::ActiveListToggle),
        KeyCode::Up => Some(TuiAction::ActiveListUp),
        KeyCode::Down => Some(TuiAction::ActiveListDown),
        KeyCode::Left => Some(TuiAction::ActiveListLeft),
        KeyCode::Right => Some(TuiAction::ActiveListRight),
        KeyCode::Enter => Some(TuiAction::ActiveListToggleSelection),
        KeyCode::Char(' ') => Some(TuiAction::ActiveListToggleSelection),
        KeyCode::Char('a') => Some(TuiAction::ActiveListEnableAll),
        KeyCode::Char('d') => Some(TuiAction::ActiveListDisableAll),
        KeyCode::Char('g') => Some(TuiAction::ToggleLogView),
        KeyCode::Esc => Some(TuiAction::ActiveListClose),
        KeyCode::Char('R') => Some(TuiAction::Renew),
        KeyCode::Char('s') => Some(TuiAction::Stop),
        KeyCode::Char('n') => Some(TuiAction::Next),
        KeyCode::Char('q') => Some(TuiAction::Quit),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogViewMode {
    Chronological,
    GroupedByPane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveListColumn {
    Session,
    Window,
    Pane,
}

#[derive(Debug, Clone)]
struct ActiveListCursor {
    column: ActiveListColumn,
    session_idx: usize,
    window_idx: usize,
    pane_idx: usize,
}

impl Default for ActiveListCursor {
    fn default() -> Self {
        Self {
            column: ActiveListColumn::Session,
            session_idx: 0,
            window_idx: 0,
            pane_idx: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct KnownPaneTarget {
    target: String,
    session: String,
    window: String,
}

#[derive(Debug, Default, Clone)]
struct InjectionFilterState {
    known_targets: BTreeMap<String, KnownPaneTarget>,
    disabled_targets: HashSet<String>,
    popup_open: bool,
    cursor: ActiveListCursor,
}

impl InjectionFilterState {
    fn observe_trigger_target(&mut self, target: &str) {
        let Ok((session, window, _pane)) = parse_target(target) else {
            return;
        };
        self.known_targets
            .entry(target.to_string())
            .or_insert_with(|| KnownPaneTarget {
                target: target.to_string(),
                session: session.to_string(),
                window: window.to_string(),
            });
    }

    fn is_allowed(&self, target: &str) -> bool {
        !self.disabled_targets.contains(target)
    }

    fn active_counts(&self) -> (usize, usize) {
        let total = self.known_targets.len();
        let disabled = self
            .known_targets
            .keys()
            .filter(|target| self.disabled_targets.contains(*target))
            .count();
        (total.saturating_sub(disabled), total)
    }

    fn open_popup(&mut self) {
        self.popup_open = true;
        self.normalize_cursor();
    }

    fn close_popup(&mut self) {
        self.popup_open = false;
    }

    fn enable_all(&mut self) {
        self.disabled_targets.clear();
    }

    fn disable_all(&mut self) {
        self.disabled_targets = self.known_targets.keys().cloned().collect();
    }

    fn normalize_cursor(&mut self) {
        let sessions = self.sessions();
        if sessions.is_empty() {
            self.cursor = ActiveListCursor::default();
            return;
        }
        self.cursor.session_idx = self
            .cursor
            .session_idx
            .min(sessions.len().saturating_sub(1));
        let Some(session) = sessions.get(self.cursor.session_idx) else {
            return;
        };
        let windows = self.windows_for(session);
        self.cursor.window_idx = self.cursor.window_idx.min(windows.len().saturating_sub(1));
        if let Some(window) = windows.get(self.cursor.window_idx) {
            let panes = self.panes_for(session, window);
            self.cursor.pane_idx = self.cursor.pane_idx.min(panes.len().saturating_sub(1));
        } else {
            self.cursor.pane_idx = 0;
        }
    }

    fn sessions(&self) -> Vec<String> {
        self.known_targets
            .values()
            .map(|item| item.session.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn windows_for(&self, session: &str) -> Vec<String> {
        self.known_targets
            .values()
            .filter(|item| item.session == session)
            .map(|item| item.window.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn panes_for(&self, session: &str, window: &str) -> Vec<String> {
        self.known_targets
            .values()
            .filter(|item| item.session == session && item.window == window)
            .map(|item| item.target.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn selected_session(&self) -> Option<String> {
        self.sessions().get(self.cursor.session_idx).cloned()
    }

    fn selected_window(&self, session: &str) -> Option<String> {
        self.windows_for(session)
            .get(self.cursor.window_idx)
            .cloned()
    }

    fn move_up(&mut self) {
        match self.cursor.column {
            ActiveListColumn::Session => {
                self.cursor.session_idx = self.cursor.session_idx.saturating_sub(1)
            }
            ActiveListColumn::Window => {
                self.cursor.window_idx = self.cursor.window_idx.saturating_sub(1)
            }
            ActiveListColumn::Pane => self.cursor.pane_idx = self.cursor.pane_idx.saturating_sub(1),
        }
        self.normalize_cursor();
    }

    fn move_down(&mut self) {
        match self.cursor.column {
            ActiveListColumn::Session => {
                self.cursor.session_idx = self.cursor.session_idx.saturating_add(1)
            }
            ActiveListColumn::Window => {
                self.cursor.window_idx = self.cursor.window_idx.saturating_add(1)
            }
            ActiveListColumn::Pane => self.cursor.pane_idx = self.cursor.pane_idx.saturating_add(1),
        }
        self.normalize_cursor();
    }

    fn move_left(&mut self) {
        self.cursor.column = match self.cursor.column {
            ActiveListColumn::Session => ActiveListColumn::Session,
            ActiveListColumn::Window => ActiveListColumn::Session,
            ActiveListColumn::Pane => ActiveListColumn::Window,
        };
        self.normalize_cursor();
    }

    fn move_right(&mut self) {
        self.cursor.column = match self.cursor.column {
            ActiveListColumn::Session => ActiveListColumn::Window,
            ActiveListColumn::Window => ActiveListColumn::Pane,
            ActiveListColumn::Pane => ActiveListColumn::Pane,
        };
        self.normalize_cursor();
    }

    fn toggle_current_selection(&mut self) {
        self.normalize_cursor();
        let Some(session) = self.selected_session() else {
            return;
        };
        match self.cursor.column {
            ActiveListColumn::Session => {
                let targets = self
                    .known_targets
                    .values()
                    .filter(|item| item.session == session)
                    .map(|item| item.target.clone())
                    .collect::<Vec<_>>();
                self.toggle_targets(&targets);
            }
            ActiveListColumn::Window => {
                let Some(window) = self.selected_window(&session) else {
                    return;
                };
                let targets = self
                    .known_targets
                    .values()
                    .filter(|item| item.session == session && item.window == window)
                    .map(|item| item.target.clone())
                    .collect::<Vec<_>>();
                self.toggle_targets(&targets);
            }
            ActiveListColumn::Pane => {
                let Some(window) = self.selected_window(&session) else {
                    return;
                };
                let panes = self.panes_for(&session, &window);
                let Some(target) = panes.get(self.cursor.pane_idx) else {
                    return;
                };
                self.toggle_targets(std::slice::from_ref(target));
            }
        }
    }

    fn toggle_targets(&mut self, targets: &[String]) {
        if targets.is_empty() {
            return;
        }
        let all_enabled = targets.iter().all(|target| self.is_allowed(target));
        if all_enabled {
            for target in targets {
                self.disabled_targets.insert(target.clone());
            }
        } else {
            for target in targets {
                self.disabled_targets.remove(target);
            }
        }
    }
}

struct TuiState {
    raw_mode_guard: RawModeGuard,
    width: u16,
    height: u16,
    icon_mode: IconMode,
    style: StyleConfig,
    logs: Vec<String>,
    max_logs: usize,
    overlay_lines: Option<Vec<String>>,
    overlay_help: Option<String>,
    footer_note: Option<String>,
    status_bar_renderer: Box<dyn StatusBarRenderer>,
    footer_renderer: Box<dyn FooterRenderer>,
    process_usage_provider: Box<dyn ProcessUsageProvider>,
    usage_sample: Option<ProcessUsageSample>,
    log_view: LogViewMode,
    last_frame_signature: Option<u64>,
    last_frame_at: Option<Instant>,
    skipped_redraws: u64,
}

struct ProcessUsageSample {
    captured_at: Instant,
    summary: String,
}

trait ProcessUsageProvider {
    fn sample(&self, pid: u32) -> Option<String>;
}

struct StatusBarRenderArgs<'a> {
    state: LoopState,
    layout: LayoutMode,
    icon_mode: IconMode,
    style: StyleConfig,
    width: u16,
    config: &'a ResolvedConfig,
    current: u32,
    total: u32,
    rule_id: Option<&'a str>,
    elapsed: &'a str,
    remaining_duration: Option<&'a str>,
    next_scan_remaining: Option<&'a str>,
    process_usage: Option<&'a str>,
}

trait StatusBarRenderer {
    fn render(&self, args: StatusBarRenderArgs<'_>) -> String;
}

struct FooterRenderArgs<'a> {
    style: StyleConfig,
    width: u16,
    summary: Option<&'a str>,
    note: Option<&'a str>,
    overlay_help: Option<&'a str>,
}

trait FooterRenderer {
    fn render(&self, args: FooterRenderArgs<'_>) -> String;
}

struct LegacyStatusBarRenderer;
struct LegacyFooterRenderer;

impl StatusBarRenderer for LegacyStatusBarRenderer {
    fn render(&self, args: StatusBarRenderArgs<'_>) -> String {
        render_status_bar(&args)
    }
}

impl FooterRenderer for LegacyFooterRenderer {
    fn render(&self, args: FooterRenderArgs<'_>) -> String {
        render_footer(
            args.style,
            args.width,
            args.summary,
            args.note,
            args.overlay_help,
        )
    }
}

struct SystemProcessUsageProvider;

impl ProcessUsageProvider for SystemProcessUsageProvider {
    fn sample(&self, pid: u32) -> Option<String> {
        let output = std::process::Command::new("ps")
            .args(["-o", "%cpu=", "-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_process_usage_summary(&stdout)
    }
}

impl TuiState {
    fn new(_config: &ResolvedConfig) -> Result<Self> {
        let raw_mode_guard = RawModeGuard::acquire("failed to enable raw mode")?;
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        let style = detect_style();
        Ok(Self {
            raw_mode_guard,
            width,
            height,
            icon_mode: detect_icon_mode(),
            style,
            logs: Vec::new(),
            max_logs: height.saturating_sub(3) as usize,
            overlay_lines: None,
            overlay_help: None,
            footer_note: None,
            status_bar_renderer: Box::new(LegacyStatusBarRenderer),
            footer_renderer: Box::new(LegacyFooterRenderer),
            process_usage_provider: Box::new(SystemProcessUsageProvider),
            usage_sample: None,
            log_view: LogViewMode::Chronological,
            last_frame_signature: None,
            last_frame_at: None,
            skipped_redraws: 0,
        })
    }

    fn toggle_log_view(&mut self) {
        self.log_view = match self.log_view {
            LogViewMode::Chronological => LogViewMode::GroupedByPane,
            LogViewMode::GroupedByPane => LogViewMode::Chronological,
        };
    }

    fn process_usage_summary(&mut self) -> Option<String> {
        self.usage_sample
            .as_ref()
            .map(|sample| sample.summary.clone())
    }

    fn refresh_process_usage_summary(&mut self) {
        if let Some(sample) = self.usage_sample.as_ref()
            && sample.captured_at.elapsed() < Duration::from_secs(1)
        {
            return;
        }

        let summary = match self.process_usage_provider.sample(std::process::id()) {
            Some(summary) => summary,
            None => {
                self.usage_sample = None;
                return;
            }
        };
        self.usage_sample = Some(ProcessUsageSample {
            captured_at: Instant::now(),
            summary,
        });
    }

    fn set_overlay_lines(&mut self, lines: Option<Vec<String>>) {
        self.overlay_lines = lines;
    }

    fn set_overlay_help(&mut self, help: Option<String>) {
        self.overlay_help = help;
    }

    fn set_footer_note(&mut self, note: Option<String>) {
        self.footer_note = note;
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        state: LoopState,
        config: &ResolvedConfig,
        current: u32,
        total: u32,
        rule_id: Option<&str>,
        active_elapsed: std::time::Duration,
        next_scan_remaining: Option<std::time::Duration>,
    ) -> Result<()> {
        let elapsed = format_std_duration(active_elapsed);
        let remaining_duration = config
            .duration
            .map(|limit| format_std_duration(limit.saturating_sub(active_elapsed)));
        let next_scan_remaining = next_scan_remaining.map(format_clock_countdown);
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        self.width = width;
        self.height = height;
        self.max_logs = height.saturating_sub(3) as usize;

        let layout = layout_mode(width);
        let process_usage = self.process_usage_summary();
        let bar = self.status_bar_renderer.render(StatusBarRenderArgs {
            state,
            layout,
            icon_mode: self.icon_mode,
            style: self.style,
            width,
            config,
            current,
            total,
            rule_id,
            elapsed: &elapsed,
            remaining_duration: remaining_duration.as_deref(),
            next_scan_remaining: next_scan_remaining.as_deref(),
            process_usage: process_usage.as_deref(),
        });

        let log_height = if width < 60 { 0 } else { self.max_logs };
        let overlay_lines = self.overlay_lines.as_ref();
        let display_lines = if let Some(lines) = overlay_lines {
            lines.clone()
        } else if matches!(self.log_view, LogViewMode::GroupedByPane) {
            build_grouped_log_lines(&self.logs, log_height, self.style.use_unicode_ellipsis)
        } else {
            self.logs
                .iter()
                .rev()
                .take(log_height)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
        };

        let footer_row = self.height.saturating_sub(1);
        let footer_summary = if state == LoopState::Stopped {
            Some(render_footer_summary(config, current, total, &elapsed))
        } else {
            None
        };
        let view_note = match self.log_view {
            LogViewMode::Chronological => "view chrono",
            LogViewMode::GroupedByPane => "view grouped",
        };
        let footer_note_owned = if let Some(note) = self.footer_note.as_ref() {
            format!("{note} {view_note}")
        } else if state == LoopState::Stopped {
            if let Some(reason) = latest_stop_reason(&self.logs) {
                format!("stop {reason} {view_note}")
            } else {
                view_note.to_string()
            }
        } else {
            view_note.to_string()
        };
        let footer_note_owned = if self.skipped_redraws > 0 {
            format!("{footer_note_owned} skip {}", self.skipped_redraws)
        } else {
            footer_note_owned
        };
        let footer = self.footer_renderer.render(FooterRenderArgs {
            style: self.style,
            width,
            summary: footer_summary.as_deref(),
            note: Some(footer_note_owned.as_str()),
            overlay_help: self.overlay_help.as_deref(),
        });
        let frame_signature = tui_frame_signature(
            width,
            height,
            &bar,
            &display_lines,
            &footer,
            overlay_lines.is_some(),
        );
        if self.last_frame_signature == Some(frame_signature)
            && let Some(last_frame_at) = self.last_frame_at
            && last_frame_at.elapsed() < Duration::from_millis(250)
        {
            self.skipped_redraws = self.skipped_redraws.saturating_add(1);
            return Ok(());
        }
        render_with_retry("run-view", || {
            let mut out = std::io::stdout();
            out.queue(MoveTo(0, 0))?;
            out.queue(Clear(ClearType::All))?;
            write!(out, "{bar}")?;

            for idx in 0..log_height {
                let raw_line = display_lines.get(idx).cloned().unwrap_or_default();
                let mut line = fit_line(&raw_line, width as usize, self.style.use_unicode_ellipsis);
                if self.style.use_color && self.style.dim_logs && !line.is_empty() {
                    let log_prefix = style_prefix(Some(log_line_color(&raw_line)), None, false);
                    line = format!("{log_prefix}{line}\x1B[0m");
                }
                out.queue(MoveTo(0, (idx + 1) as u16))?;
                out.queue(Clear(ClearType::CurrentLine))?;
                write!(out, "{line}")?;
            }

            out.queue(MoveTo(0, footer_row))?;
            out.queue(Clear(ClearType::CurrentLine))?;
            write!(out, "{footer}")?;
            out.flush()?;
            Ok(())
        })?;
        self.last_frame_signature = Some(frame_signature);
        self.last_frame_at = Some(Instant::now());
        Ok(())
    }

    fn push_log(&mut self, line: String) {
        self.logs.push(sanitize_tui_log_line(&line));
        if self.logs.len() > 500 {
            self.logs.drain(0..self.logs.len().saturating_sub(500));
        }
    }

    fn poll_input(
        &self,
        prompt_editor_open: bool,
        prompt_confirm_open: bool,
    ) -> Result<Option<TuiAction>> {
        if event::poll(Duration::from_millis(10)).context("poll input failed")? {
            let ev = event::read()?;
            return Ok(match ev {
                Event::Resize(_, _) => Some(TuiAction::Redraw),
                Event::Key(KeyEvent {
                    code, modifiers, ..
                }) => {
                    map_run_tui_key_action(code, modifiers, prompt_editor_open, prompt_confirm_open)
                }
                _ => None,
            });
        }
        Ok(None)
    }

    fn shutdown(&mut self) -> Result<()> {
        self.raw_mode_guard.release()?;
        Ok(())
    }
}

fn active_list_note(filter: &InjectionFilterState) -> Option<String> {
    let (active, total) = filter.active_counts();
    if total == 0 {
        None
    } else {
        Some(format!("active {active}/{total}"))
    }
}

fn sync_tui_overlays(
    tui_state: &mut TuiState,
    filter: &InjectionFilterState,
    prompt_editor: &PromptEditorState,
) {
    let mut note = active_list_note(filter).unwrap_or_default();
    if prompt_editor.open {
        let editor_note = format!(
            "prompt {} / {} chars",
            prompt_editor.current.chars().count(),
            prompt_editor.max_chars
        );
        if note.is_empty() {
            note = editor_note;
        } else {
            note = format!("{note} {editor_note}");
        }
        tui_state.set_overlay_lines(Some(render_prompt_editor_popup(
            prompt_editor,
            tui_state.width as usize,
            tui_state.max_logs,
            tui_state.style.use_unicode_ellipsis,
        )));
        let sep = if tui_state.style.use_unicode_ellipsis {
            " · "
        } else {
            " . "
        };
        tui_state.set_overlay_help(Some(format!(
            "prompt editor{sep}↑/↓ select{sep}enter apply{sep}type/backspace edit{sep}d delete{sep}c clear{sep}u undo{sep}y/n confirm{sep}e/esc close"
        )));
    } else if filter.popup_open {
        tui_state.set_overlay_lines(Some(render_active_list_popup(
            filter,
            tui_state.width as usize,
            tui_state.max_logs,
            tui_state.style.use_unicode_ellipsis,
        )));
        let sep = if tui_state.style.use_unicode_ellipsis {
            " · "
        } else {
            " . "
        };
        tui_state.set_overlay_help(Some(format!(
            "active list{sep}<-/-> cols{sep}↑/↓ rows{sep}space/enter toggle{sep}a all on{sep}d all off{sep}q/esc close"
        )));
    } else {
        tui_state.set_overlay_lines(None);
        tui_state.set_overlay_help(None);
    }
    if note.is_empty() {
        tui_state.set_footer_note(None);
    } else {
        tui_state.set_footer_note(Some(note));
    }
}

fn render_prompt_editor_popup(
    editor: &PromptEditorState,
    width: usize,
    max_lines: usize,
    use_unicode: bool,
) -> Vec<String> {
    let on = if use_unicode { "●" } else { "*" };
    let off = if use_unicode { "○" } else { "-" };
    let pointer = if use_unicode { "▶▶" } else { ">>" };
    let col_width = width.saturating_sub(2).max(20);
    let mut lines = Vec::new();
    lines.push("Prompt Editor - edit or choose history".to_string());
    lines.push(format!(
        "current ({} / {}): {}",
        editor.current.chars().count(),
        editor.max_chars,
        fit_line(&editor.current, col_width.saturating_sub(24), use_unicode)
    ));
    lines.push("choose source (original first):".to_string());

    let mut entries = Vec::new();
    entries.push((
        "original".to_string(),
        editor.original.clone(),
        "-".to_string(),
    ));
    for item in &editor.history {
        entries.push((
            "history".to_string(),
            item.text.clone(),
            compact_timestamp(&item.created_at),
        ));
    }

    let rows = max_lines.saturating_sub(lines.len()).max(1);
    for idx in 0..rows {
        let Some((kind, text, when)) = entries.get(idx) else {
            lines.push(String::new());
            continue;
        };
        let marker = if idx == editor.selected_idx {
            pointer
        } else {
            "  "
        };
        let enabled = if idx == editor.selected_idx { on } else { off };
        lines.push(format!(
            "{marker} {enabled} {kind:<8} [{}] {}",
            when,
            fit_line(text, col_width.saturating_sub(24), use_unicode)
        ));
    }

    if let Some(confirm) = editor.confirm {
        let msg = match confirm {
            PromptEditorConfirm::DeleteSelected => "confirm delete selected history? y yes / n no",
            PromptEditorConfirm::ClearAll => "confirm clear all history? y yes / n no",
        };
        lines.push(msg.to_string());
    }
    lines
}

fn render_active_list_popup(
    filter: &InjectionFilterState,
    width: usize,
    max_lines: usize,
    use_unicode: bool,
) -> Vec<String> {
    let mark_on = if use_unicode { "●" } else { "*" };
    let mark_off = if use_unicode { "○" } else { "-" };
    let mark_partial = if use_unicode { "◐" } else { "+" };
    let focus_selected = if use_unicode { "▶▶ " } else { ">>>" };
    let focus_column = if use_unicode { "▸  " } else { "-->" };
    let focus_none = "   ";
    let spacer = if use_unicode { " │ " } else { " | " };
    let col_width = (width.saturating_sub(8) / 3).max(18);
    let active_column = filter.cursor.column;

    let sessions = filter.sessions();
    let selected_session = sessions.get(filter.cursor.session_idx).cloned();
    let windows = selected_session
        .as_deref()
        .map(|session| filter.windows_for(session))
        .unwrap_or_default();
    let selected_window = selected_session
        .as_deref()
        .and_then(|session| {
            windows
                .get(filter.cursor.window_idx)
                .map(|window| (session, window))
        })
        .map(|(session, window)| (session.to_string(), window.to_string()));
    let panes = selected_window
        .as_ref()
        .map(|(session, window)| filter.panes_for(session, window))
        .unwrap_or_default();

    let mut lines = Vec::new();
    lines.push("Active List - injection filter".to_string());
    let session_header = if matches!(active_column, ActiveListColumn::Session) {
        "» SESSIONS «"
    } else {
        "sessions"
    };
    let window_header = if matches!(active_column, ActiveListColumn::Window) {
        "» WINDOWS «"
    } else {
        "windows"
    };
    let pane_header = if matches!(active_column, ActiveListColumn::Pane) {
        "» PANES «"
    } else {
        "panes"
    };
    lines.push(format!(
        "{}{}{}{}{}",
        pad_to_width(session_header, col_width),
        spacer,
        pad_to_width(window_header, col_width),
        spacer,
        pad_to_width(pane_header, col_width)
    ));

    let rows = max_lines.saturating_sub(lines.len()).max(1);
    for idx in 0..rows {
        let session_cell = if let Some(session) = sessions.get(idx) {
            let targets = filter
                .known_targets
                .values()
                .filter(|item| item.session == *session)
                .map(|item| item.target.clone())
                .collect::<Vec<_>>();
            let enabled = targets
                .iter()
                .filter(|target| filter.is_allowed(target))
                .count();
            let mark = if enabled == 0 {
                mark_off
            } else if enabled == targets.len() {
                mark_on
            } else {
                mark_partial
            };
            let pointer = if matches!(active_column, ActiveListColumn::Session)
                && idx == filter.cursor.session_idx
            {
                focus_selected
            } else if matches!(active_column, ActiveListColumn::Session) {
                focus_column
            } else {
                focus_none
            };
            format!("{pointer}{mark} {session} ({enabled}/{})", targets.len())
        } else {
            String::new()
        };

        let window_cell = if let Some(session) = selected_session.as_ref() {
            if let Some(window) = windows.get(idx) {
                let targets = filter
                    .known_targets
                    .values()
                    .filter(|item| item.session == *session && item.window == *window)
                    .map(|item| item.target.clone())
                    .collect::<Vec<_>>();
                let enabled = targets
                    .iter()
                    .filter(|target| filter.is_allowed(target))
                    .count();
                let mark = if enabled == 0 {
                    mark_off
                } else if enabled == targets.len() {
                    mark_on
                } else {
                    mark_partial
                };
                let pointer = if matches!(active_column, ActiveListColumn::Window)
                    && idx == filter.cursor.window_idx
                {
                    focus_selected
                } else if matches!(active_column, ActiveListColumn::Window) {
                    focus_column
                } else {
                    focus_none
                };
                format!(
                    "{pointer}{mark} {session}:{window} ({enabled}/{})",
                    targets.len()
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let pane_cell = if let Some(target) = panes.get(idx) {
            let mark = if filter.is_allowed(target) {
                mark_on
            } else {
                mark_off
            };
            let pointer = if matches!(active_column, ActiveListColumn::Pane)
                && idx == filter.cursor.pane_idx
            {
                focus_selected
            } else if matches!(active_column, ActiveListColumn::Pane) {
                focus_column
            } else {
                focus_none
            };
            format!("{pointer}{mark} {target}")
        } else {
            String::new()
        };

        lines.push(format!(
            "{}{}{}{}{}",
            pad_to_width(&fit_line(&session_cell, col_width, use_unicode), col_width),
            spacer,
            pad_to_width(&fit_line(&window_cell, col_width, use_unicode), col_width),
            spacer,
            fit_line(&pane_cell, col_width, use_unicode)
        ));
    }

    lines
}

fn render_status_bar(args: &StatusBarRenderArgs<'_>) -> String {
    let state = args.state;
    let layout = args.layout;
    let icon_mode = args.icon_mode;
    let style = args.style;
    let width = args.width;
    let config = args.config;
    let current = args.current;
    let total = args.total;
    let rule_id = args.rule_id;
    let elapsed = args.elapsed;
    let remaining_duration = args.remaining_duration;
    let process_usage = args.process_usage;
    let (icon, label) = state_label(state, icon_mode);
    let progress = if config.infinite {
        "inf".to_string()
    } else {
        format!("{}/{}", current, total)
    };
    let percent = if config.infinite || total == 0 {
        "--".to_string()
    } else {
        format!("{}%", (current * 100 / total))
    };
    let bar = render_progress_bar(current, total, layout, style.use_unicode_ellipsis);
    let trigger = rule_id.unwrap_or("-");
    let profile = config.profile_id.as_deref().unwrap_or("-");

    let icon_glyph = if style.use_unicode_ellipsis {
        icon
    } else {
        ascii_icon(icon)
    };
    let state_text = format!("{icon_glyph} {label}");
    let iter_text = if config.infinite {
        "iter ∞".to_string()
    } else {
        format!("iter {progress}")
    };
    let trigger_text = truncate_text(
        trigger,
        match layout {
            LayoutMode::Compact => 16,
            LayoutMode::Standard => 28,
            LayoutMode::Wide => 44,
        },
        style.use_unicode_ellipsis,
    );
    let trigger_prefix = if config.exec_command.is_some() {
        "evt"
    } else {
        "trg"
    };

    let sep_text = if style.use_unicode_ellipsis {
        " · "
    } else {
        " . "
    };

    let mut left_parts = Vec::new();
    left_parts.push(state_text.clone());
    left_parts.push(iter_text);
    left_parts.push(format!("{bar} {percent}"));

    let mut right_parts = Vec::new();
    match layout {
        LayoutMode::Compact => {
            if let Some(next_scan) = args.next_scan_remaining {
                right_parts.push(format!("next {next_scan}"));
            }
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("run {profile}"));
            if let Some(usage) = process_usage {
                right_parts.push(usage.to_string());
            }
            right_parts.push(format!("{trigger_prefix} {trigger_text}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(config.target_label.clone());
        }
        LayoutMode::Standard => {
            if let Some(next_scan) = args.next_scan_remaining {
                right_parts.push(format!("next {next_scan}"));
            }
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("run {profile}"));
            if let Some(usage) = process_usage {
                right_parts.push(usage.to_string());
            }
            right_parts.push(format!("{trigger_prefix} {trigger_text}"));
            right_parts.push(format!("last {elapsed}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(config.target_label.clone());
        }
        LayoutMode::Wide => {
            if let Some(next_scan) = args.next_scan_remaining {
                right_parts.push(format!("next {next_scan}"));
            }
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("run {profile}"));
            if let Some(usage) = process_usage {
                right_parts.push(usage.to_string());
            }
            right_parts.push(format!("{trigger_prefix} {trigger_text}"));
            right_parts.push(format!("last {elapsed}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(format!("target {}", config.target_label));
        }
    }

    let left_sep_text = if matches!(layout, LayoutMode::Compact) {
        " "
    } else {
        sep_text
    };
    let left_text = left_parts.join(left_sep_text);
    let right_sep_text = if matches!(layout, LayoutMode::Compact) {
        " "
    } else {
        sep_text
    };
    let mut right_text = right_parts.join(right_sep_text);
    let mut line = if right_text.is_empty() {
        left_text.clone()
    } else {
        let width_usize = width as usize;
        let left_len = left_text.chars().count();
        let right_len = right_text.chars().count();
        let gap = 1;
        if left_len + gap + right_len > width_usize {
            let available = width_usize.saturating_sub(left_len + gap);
            if available > 0 {
                right_text = truncate_text(&right_text, available, style.use_unicode_ellipsis);
                format!("{left_text}{}{}", " ".repeat(gap), right_text)
            } else {
                left_text.clone()
            }
        } else {
            let padding = width_usize.saturating_sub(left_len + gap + right_len);
            format!(
                "{left_text}{}{}{}",
                " ".repeat(gap),
                " ".repeat(padding),
                right_text
            )
        }
    };
    line = pad_to_width(&line, width as usize);

    if style.use_color {
        let label_color = state_color(state);
        let base_prefix = style_prefix(Some(248), style.use_bg.then_some(236), false);
        let state_prefix = format!("\x1B[38;5;{label_color}m");
        let sep_prefix = style_prefix(Some(240), style.use_bg.then_some(236), false);
        let colored_state = format!("{state_prefix}{state_text}{base_prefix}");
        let mut colored_line = line.replacen(&state_text, &colored_state, 1);
        colored_line =
            colored_line.replace(sep_text, &format!("{sep_prefix}{sep_text}{base_prefix}"));
        format!("{base_prefix}{colored_line}\x1B[0m")
    } else {
        line
    }
}

fn parse_process_usage_summary(ps_stdout: &str) -> Option<String> {
    let line = ps_stdout
        .lines()
        .find(|line| !line.trim().is_empty())?
        .trim();
    let mut fields = line.split_whitespace();
    let cpu = fields.next()?.parse::<f64>().ok()?;
    let rss_kb = fields.next()?.parse::<u64>().ok()?;
    let mem_mb = rss_kb as f64 / 1024.0;
    Some(format!("cpu {:.1}% mem {:.1}mb", cpu, mem_mb))
}

fn state_label(state: LoopState, icon_mode: IconMode) -> (&'static str, &'static str) {
    match (state, icon_mode) {
        (LoopState::Running, IconMode::Nerd) => ("󰐊", "RUN"),
        (LoopState::Holding, IconMode::Nerd) => ("󰏤", "HOLD"),
        (LoopState::Delay, IconMode::Nerd) => ("󰔟", "DELAY"),
        (LoopState::Error, IconMode::Nerd) => ("󰅚", "ERROR"),
        (LoopState::Stopped, IconMode::Nerd) => ("󰩈", "STOP"),
        (LoopState::Waiting, IconMode::Nerd) => ("󰔟", "WAIT"),
        (LoopState::Sending, IconMode::Nerd) => ("󰐊", "SEND"),
        (LoopState::Running, IconMode::Ascii) => (">", "RUN"),
        (LoopState::Holding, IconMode::Ascii) => ("||", "HOLD"),
        (LoopState::Delay, IconMode::Ascii) => ("...", "DELAY"),
        (LoopState::Error, IconMode::Ascii) => ("!", "ERROR"),
        (LoopState::Stopped, IconMode::Ascii) => ("x", "STOP"),
        (LoopState::Waiting, IconMode::Ascii) => ("...", "WAIT"),
        (LoopState::Sending, IconMode::Ascii) => (">", "SEND"),
    }
}

fn render_progress_bar(current: u32, total: u32, layout: LayoutMode, unicode: bool) -> String {
    let width = match layout {
        LayoutMode::Compact => 6,
        LayoutMode::Standard => 10,
        LayoutMode::Wide => 14,
    };
    if total == 0 {
        return if unicode {
            "░".repeat(width)
        } else {
            ".".repeat(width)
        };
    }
    let filled = ((current as f64 / total as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    let filled_char = if unicode { "▰" } else { "=" };
    let empty_char = if unicode { "▱" } else { "." };
    format!(
        "{}{}",
        filled_char.repeat(filled),
        empty_char.repeat(width - filled)
    )
}

fn truncate_text(text: &str, max: usize, use_unicode: bool) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let suffix = if use_unicode { "…" } else { "..." };
    let suffix_len = suffix.chars().count();
    if max <= suffix_len {
        return text.chars().take(max).collect();
    }
    let mut s = text
        .chars()
        .take(max.saturating_sub(suffix_len))
        .collect::<String>();
    s.push_str(suffix);
    s
}

fn pad_to_width(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.chars().take(width).collect();
    }
    let padding = width - len;
    format!("{text}{}", " ".repeat(padding))
}

fn ascii_icon(icon: &str) -> &str {
    match icon {
        "󰐊" => ">",
        "󰏤" => "||",
        "󰔟" => "...",
        "󰅚" => "!",
        "󰩈" => "x",
        _ => ">",
    }
}

fn state_color(state: LoopState) -> u8 {
    match state {
        LoopState::Running => 71,
        LoopState::Holding => 179,
        LoopState::Waiting | LoopState::Delay => 109,
        LoopState::Error => 166,
        LoopState::Stopped => 246,
        LoopState::Sending => 109,
    }
}

fn style_prefix(fg: Option<u8>, bg: Option<u8>, bold: bool) -> String {
    let mut prefix = String::new();
    if bold {
        prefix.push_str("\x1B[1m");
    }
    if let Some(fg) = fg {
        prefix.push_str(&format!("\x1B[38;5;{fg}m"));
    }
    if let Some(bg) = bg {
        prefix.push_str(&format!("\x1B[48;5;{bg}m"));
    }
    prefix
}

fn fit_line(text: &str, width: usize, use_unicode: bool) -> String {
    if text.chars().count() <= width {
        return pad_to_width(text, width);
    }
    truncate_text(text, width, use_unicode)
}

fn timestamp_now() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

fn timestamp_local_now() -> String {
    OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

fn log_line_color(line: &str) -> u8 {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    log_line_color_at(line, now)
}

fn log_line_color_at(line: &str, now: OffsetDateTime) -> u8 {
    if let Some(timestamp) = parse_log_timestamp(line) {
        let local_timestamp = timestamp.to_offset(now.offset());
        if local_timestamp.date() == now.date() {
            return 251;
        }
        return 244;
    }
    if looks_like_compact_time_prefix(line) {
        return 249;
    }
    245
}

#[cfg(test)]
fn log_line_date(line: &str) -> Option<&str> {
    if !line.starts_with('[') {
        return None;
    }
    let close = line.find(']')?;
    let ts = line.get(1..close)?;
    let date = ts.split('T').next()?;
    if date.len() == 10 { Some(date) } else { None }
}

fn parse_log_timestamp(line: &str) -> Option<OffsetDateTime> {
    if !line.starts_with('[') {
        return None;
    }
    let close = line.find(']')?;
    let ts = line.get(1..close)?;
    OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok()
}

fn looks_like_compact_time_prefix(line: &str) -> bool {
    let mut parts = line.split(':');
    let (Some(h), Some(m), Some(s)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    h.len() == 2
        && m.len() == 2
        && s.len() >= 2
        && h.chars().all(|ch| ch.is_ascii_digit())
        && m.chars().all(|ch| ch.is_ascii_digit())
        && s.chars().take(2).all(|ch| ch.is_ascii_digit())
}

fn parse_duration(value: &str) -> Result<Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("duration is empty");
    }
    let mut number_part = String::new();
    let mut unit_part = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            if !unit_part.is_empty() {
                bail!("invalid duration: {value}");
            }
            number_part.push(ch);
        } else if !ch.is_whitespace() {
            unit_part.push(ch);
        }
    }
    if number_part.is_empty() || unit_part.is_empty() {
        bail!("invalid duration: {value}");
    }
    let amount: f64 = number_part
        .parse()
        .with_context(|| format!("invalid duration number: {value}"))?;
    if amount <= 0.0 {
        bail!("duration must be > 0: {value}");
    }
    let unit = unit_part.to_lowercase();
    let seconds = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => amount,
        "m" | "min" | "mins" | "minute" | "minutes" => amount * 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => amount * 3600.0,
        "d" | "day" | "days" => amount * 86_400.0,
        "w" | "wk" | "wks" | "week" | "weeks" => amount * 604_800.0,
        "mon" | "month" | "months" => amount * 2_592_000.0,
        "y" | "yr" | "yrs" | "year" | "years" => amount * 31_536_000.0,
        _ => bail!("invalid duration unit: {unit_part}"),
    };
    Ok(Duration::from_secs_f64(seconds))
}

#[derive(Debug, Clone)]
struct LoggingConfigResolved {
    path: Option<PathBuf>,
    format: LogFormatResolved,
}

#[derive(Debug, Clone, Copy)]
enum LogFormatResolved {
    Text,
    Jsonl,
}

#[derive(Debug)]
struct BackoffState {
    attempts: u32,
    last_sent: Option<OffsetDateTime>,
}

struct ExecInFlight {
    command: String,
    child: std::process::Child,
    started_at: std::time::Instant,
}

struct ResolveConfigArgs {
    target_override: Option<Vec<String>>,
    iterations_override: Option<u32>,
    skip_tmux: bool,
    tail_override: Option<usize>,
    head_override: Option<usize>,
    once: bool,
    single_line: bool,
    tui: bool,
    trigger_edge_override: Option<bool>,
    recheck_before_send_override: Option<bool>,
    debug_trigger: bool,
    profile_id: Option<String>,
}

fn resolve_config(mut config: Config, args: ResolveConfigArgs) -> Result<ResolvedConfig> {
    let ResolveConfigArgs {
        target_override,
        iterations_override,
        skip_tmux,
        tail_override,
        head_override,
        once,
        single_line,
        tui,
        trigger_edge_override,
        recheck_before_send_override,
        debug_trigger,
        profile_id,
    } = args;
    if let Some(targets) = target_override
        && let Some(first) = targets.first()
    {
        config.target = Some(first.clone());
        config.targets = Some(targets);
    }
    if let Some(iterations) = iterations_override {
        config.iterations = Some(iterations);
        config.infinite = Some(false);
    }

    let exec_command = config
        .exec
        .as_ref()
        .map(|value| value.command.trim().to_string())
        .filter(|value| !value.is_empty());
    if config.exec.is_some() && exec_command.is_none() {
        bail!("exec.command is required and cannot be empty");
    }

    if exec_command.is_some()
        && (config.default_action.is_some()
            || config.rules.as_ref().is_some_and(|rules| !rules.is_empty())
            || config.target.is_some()
            || config
                .targets
                .as_ref()
                .is_some_and(|targets| !targets.is_empty())
            || config.files.as_ref().is_some_and(|files| !files.is_empty()))
    {
        bail!(
            "exec mode cannot be combined with target/targets/files/default_action/rules in the same config"
        );
    }

    let requested_targets = if exec_command.is_some() {
        Vec::new()
    } else {
        config
            .targets
            .clone()
            .unwrap_or_else(|| config.target.clone().into_iter().collect())
    };
    if exec_command.is_none()
        && let Some(files) = &config.files
    {
        validate_file_sources(files)?;
    }

    let explicit_targets = if exec_command.is_some() {
        None
    } else if requested_targets.len() > 1 {
        Some(resolve_explicit_targets(&requested_targets, skip_tmux)?)
    } else {
        None
    };
    let (target_scope, target_label) = if let Some(command) = exec_command.as_ref() {
        (TargetScope::All, format!("exec://{command}"))
    } else if let Some(targets) = explicit_targets.as_ref() {
        (TargetScope::All, targets.join(","))
    } else {
        let target_input = requested_targets.first().map(String::as_str);
        if skip_tmux {
            resolve_target_scope_offline(target_input)?
        } else {
            resolve_target_scope(target_input)?
        }
    };

    let infinite = config.infinite.unwrap_or(false);
    let iterations = config.iterations;
    if infinite && iterations.is_some() {
        bail!("iterations must be omitted when infinite is true");
    }
    if !infinite && iterations.unwrap_or(0) == 0 {
        bail!("iterations must be > 0 unless infinite is true");
    }

    let duration = if let Some(ref value) = config.duration {
        Some(parse_duration(value).with_context(|| "invalid duration")?)
    } else {
        None
    };

    let (default_action, has_prompt, prompt_placeholders, template_var_keys, rule_eval, rules) =
        if exec_command.is_some() {
            (
                Action {
                    pre: None,
                    prompt: None,
                    post: None,
                },
                false,
                Vec::new(),
                Vec::new(),
                RuleEval::FirstMatch,
                Vec::new(),
            )
        } else {
            let Some(default_action) = config.default_action else {
                bail!("default_action.prompt is required");
            };
            let has_prompt = default_action.prompt.as_ref().is_some();
            if !has_prompt {
                bail!("default_action.prompt is required");
            }
            let prompt_placeholders = collect_template_placeholders(&default_action, &config.rules);
            let template_vars = config.template_vars.unwrap_or_default();
            let template_var_keys = template_vars.keys().cloned().collect::<Vec<_>>();
            let missing_template_vars = find_missing_vars(&prompt_placeholders, &template_vars);
            if !missing_template_vars.is_empty() {
                bail!(
                    "missing template_vars: {}",
                    missing_template_vars.join(", ")
                );
            }
            let rule_eval = config.rule_eval.unwrap_or(RuleEval::FirstMatch);
            let rules = config.rules.unwrap_or_default();
            validate_rules(&rules)?;
            (
                default_action,
                true,
                prompt_placeholders,
                template_var_keys,
                rule_eval,
                rules,
            )
        };
    let logging = resolve_logging(config.logging);

    let delay = config.delay;
    if let Some(ref delay) = delay {
        validate_delay(delay)?;
    }

    let poll = config.poll.unwrap_or(5).max(1);
    let initial_poll = config
        .initial_poll
        .unwrap_or(DEFAULT_INITIAL_POLL_SECONDS)
        .max(1);
    let trigger_confirm_seconds = config
        .trigger_confirm_seconds
        .unwrap_or(DEFAULT_TRIGGER_CONFIRM_SECONDS);
    let trigger_edge = trigger_edge_override.unwrap_or(config.trigger_edge.unwrap_or(true));
    let recheck_before_send =
        recheck_before_send_override.unwrap_or(config.recheck_before_send.unwrap_or(true));
    let log_preview_lines = config.log_preview_lines.unwrap_or(3).max(1);
    let prompt_edit_max_chars = config
        .prompt_edit_max_chars
        .unwrap_or(DEFAULT_PROMPT_EDIT_MAX_CHARS)
        .max(1);

    let fanout = config.fanout.unwrap_or(FanoutMode::Matched);

    if exec_command.is_none() && !skip_tmux {
        if let Some(targets) = explicit_targets.as_ref() {
            validate_tmux_targets(targets)?;
        }
        validate_tmux_scope(&target_scope)?;
    }

    if tail_override.is_some() && head_override.is_some() {
        bail!("--tail and --head are mutually exclusive");
    }
    let tail = tail_override.or(config.tail).unwrap_or(1);
    let once = once || config.once.unwrap_or(false);
    let single_line = single_line || config.single_line.unwrap_or(false);
    let tui = tui || config.tui.unwrap_or(false);
    let window = CaptureWindow::from_overrides(tail_override.or(Some(tail)), head_override);

    Ok(ResolvedConfig {
        profile_id,
        exec_command,
        target_scope,
        target_label,
        explicit_targets,
        file_sources: config.files.unwrap_or_default(),
        iterations,
        infinite,
        has_prompt,
        poll,
        initial_poll,
        trigger_confirm_seconds,
        log_preview_lines,
        trigger_edge,
        recheck_before_send,
        debug_trigger,
        fanout,
        duration,
        prompt_edit_max_chars,
        rule_eval,
        rules,
        delay,
        prompt_placeholders,
        template_vars: template_var_keys,
        default_action,
        logging,
        capture_window: window,
        once,
        single_line,
        tui,
    })
}

fn print_validation(config: &ResolvedConfig) {
    println!("Validation OK");
    println!("- target: {}", config.target_label);
    if let Some(command) = config.exec_command.as_deref() {
        println!("- exec.command: {command}");
    }
    if !config.file_sources.is_empty() {
        println!("- file_sources: {}", config.file_sources.join(", "));
    }
    if config.infinite {
        println!("- iterations: infinite");
    } else if let Some(iterations) = config.iterations {
        println!("- iterations: {iterations}");
    }
    println!("- prompt: {}", if config.has_prompt { "yes" } else { "no" });
    if config.exec_command.is_none() {
        println!("- rule_eval: {}", rule_eval_label(&config.rule_eval));
        println!("- rules: {}", config.rules.len());
        if let Some(delay) = &config.delay {
            println!("- delay: {}", delay_summary(delay));
        }
        if !config.prompt_placeholders.is_empty() {
            println!("- template vars: {}", config.prompt_placeholders.join(", "));
        }
        if !config.template_vars.is_empty() {
            println!("- template_vars: {}", config.template_vars.join(", "));
        }
    }
    if let Some(path) = &config.logging.path {
        println!(
            "- logging: {} ({})",
            path.display(),
            log_format_label(config.logging.format)
        );
    } else {
        println!(
            "- logging: stdout ({})",
            log_format_label(config.logging.format)
        );
    }
    if config.exec_command.is_none() {
        match config.capture_window {
            CaptureWindow::Tail(lines) => println!("- tail: {lines}"),
            CaptureWindow::Head(lines) => println!("- head: {lines}"),
        }
    }
    println!("- poll: {}s", config.poll);
    println!("- initial_poll: {}s", config.initial_poll);
    println!("- prompt_edit_max_chars: {}", config.prompt_edit_max_chars);
    println!(
        "- trigger_confirm_seconds: {}s",
        config.trigger_confirm_seconds
    );
    println!("- log_preview_lines: {}", config.log_preview_lines);
    println!(
        "- trigger_edge: {}",
        if config.trigger_edge { "yes" } else { "no" }
    );
    println!(
        "- recheck_before_send: {}",
        if config.recheck_before_send {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "- debug_trigger: {}",
        if config.debug_trigger { "yes" } else { "no" }
    );
    println!("- fanout: {}", fanout_label(config.fanout));
    if let Some(duration) = config.duration {
        println!("- duration: {}s", duration.as_secs_f64());
    }
    println!("- once: {}", if config.once { "yes" } else { "no" });
    println!(
        "- single_line: {}",
        if config.single_line { "yes" } else { "no" }
    );
    println!("- tui: {}", if config.tui { "yes" } else { "no" });
    println!("- note: dry-run only, no tmux commands sent");
}

fn rule_eval_label(rule_eval: &RuleEval) -> &'static str {
    match rule_eval {
        RuleEval::FirstMatch => "first_match",
        RuleEval::MultiMatch => "multi_match",
        RuleEval::Priority => "priority",
    }
}

fn fanout_label(mode: FanoutMode) -> &'static str {
    match mode {
        FanoutMode::Matched => "matched",
        FanoutMode::Broadcast => "broadcast",
    }
}

fn log_format_label(format: LogFormatResolved) -> &'static str {
    match format {
        LogFormatResolved::Text => "text",
        LogFormatResolved::Jsonl => "jsonl",
    }
}

fn delay_summary(delay: &DelayConfig) -> String {
    match delay.mode {
        DelayMode::Fixed => format!("fixed {}s", delay.value.unwrap_or(0)),
        DelayMode::Range => {
            let min = delay.min.unwrap_or(0);
            let max = delay.max.unwrap_or(0);
            format!("range {min}-{max}s")
        }
        DelayMode::Jitter => {
            let min = delay.min.unwrap_or(0);
            let max = delay.max.unwrap_or(0);
            let jitter = delay.jitter.unwrap_or(0.0);
            format!("jitter {min}-{max}s {jitter}")
        }
        DelayMode::Backoff => {
            if let Some(backoff) = &delay.backoff {
                let max = backoff.max.map_or(String::new(), |v| format!(", max {v}s"));
                format!("backoff base {}s x{}{}", backoff.base, backoff.factor, max)
            } else {
                "backoff".to_string()
            }
        }
    }
}

fn resolve_logging(config: Option<LoggingConfig>) -> LoggingConfigResolved {
    let config = config.unwrap_or(LoggingConfig {
        path: None,
        format: None,
    });
    let format = match config.format.unwrap_or(LogFormat::Text) {
        LogFormat::Text => LogFormatResolved::Text,
        LogFormat::Jsonl => LogFormatResolved::Jsonl,
    };
    LoggingConfigResolved {
        path: config.path,
        format,
    }
}

fn validate_rules(rules: &[Rule]) -> Result<()> {
    let mut ids = HashSet::new();
    let mut has_ids = false;
    for (idx, rule) in rules.iter().enumerate() {
        let id = rule.id.as_deref().unwrap_or("<unnamed>");
        if let Some(id_value) = rule.id.as_ref() {
            has_ids = true;
            if !ids.insert(id_value.clone()) {
                bail!("duplicate rule id: {id_value}");
            }
        }
        let match_defined = rule.match_.as_ref().map(has_match).unwrap_or(false);
        let exclude_defined = rule.exclude.as_ref().map(has_match).unwrap_or(false);
        if !match_defined && !exclude_defined {
            bail!("rule {idx} ({id}) requires match or exclude");
        }
    }
    if has_ids {
        for (idx, rule) in rules.iter().enumerate() {
            if let Some(next) = &rule.next {
                if next == "stop" {
                    continue;
                }
                if !ids.contains(next) {
                    let id = rule.id.as_deref().unwrap_or("<unnamed>");
                    bail!("rule {idx} ({id}) references unknown next: {next}");
                }
            }
        }
    }
    Ok(())
}

fn has_match(criteria: &MatchCriteria) -> bool {
    has_text(&criteria.regex)
        || has_text(&criteria.trigger_expr)
        || has_text(&criteria.exact_line)
        || has_text(&criteria.contains)
        || has_text(&criteria.starts_with)
}

fn has_text(value: &Option<String>) -> bool {
    value
        .as_ref()
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false)
}

fn validate_tmux_scope(scope: &TargetScope) -> Result<()> {
    let output = std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .context("failed to run tmux -V")?;
    if !output.status.success() {
        bail!("tmux not available on PATH");
    }

    if matches!(scope, TargetScope::All) {
        return Ok(());
    }

    let panes = list_tmux_panes()?;
    let candidates = select_targets_for_scope(scope, &panes);
    if candidates.is_empty() {
        bail!("tmux target scope not found: {}", target_scope_label(scope));
    }
    Ok(())
}

fn validate_tmux_targets(targets: &[String]) -> Result<()> {
    let panes = list_tmux_panes()?;
    let available = panes
        .iter()
        .map(|pane| pane.target.as_str())
        .collect::<HashSet<_>>();
    for target in targets {
        if !available.contains(target.as_str()) {
            bail!("tmux target not found: {target}");
        }
    }
    Ok(())
}

fn validate_file_sources(files: &[String]) -> Result<()> {
    for file in files {
        let path = PathBuf::from(file);
        if !path.exists() {
            bail!("file source not found: {}", path.display());
        }
        if !path.is_file() {
            bail!("file source is not a regular file: {}", path.display());
        }
        std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read file source: {}", path.display()))?;
    }
    Ok(())
}

fn file_source_key(path: &str) -> String {
    format!("file://{path}")
}

fn file_source_path(key: &str) -> Option<&str> {
    key.strip_prefix("file://")
}

fn list_tmux_panes() -> Result<Vec<TmuxPane>> {
    let output = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_index}\t#{pane_index}\t#{session_name}:#{window_index}.#{pane_index}",
        ])
        .output()
        .context("failed to run tmux list-panes")?;
    if !output.status.success() {
        bail!("tmux list-panes failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut panes = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split('\t');
        let session = parts.next().unwrap_or("").trim();
        let window = parts.next().unwrap_or("").trim();
        let _pane = parts.next().unwrap_or("").trim();
        let target = parts.next().unwrap_or("").trim();
        if session.is_empty() || window.is_empty() || target.is_empty() {
            continue;
        }
        panes.push(TmuxPane {
            target: target.to_string(),
            session: session.to_string(),
            window: window.to_string(),
        });
    }
    Ok(panes)
}

fn resolve_target(target: &str) -> Result<String> {
    resolve_target_with_current(target, tmux_current_target)
}

fn resolve_target_offline(target: &str) -> Result<String> {
    if target.contains(':') {
        return Ok(target.to_string());
    }
    bail!("target shorthand requires tmux; use session:window.pane")
}

fn resolve_target_scope(target: Option<&str>) -> Result<(TargetScope, String)> {
    resolve_target_scope_with(target, resolve_target)
}

fn resolve_target_scope_offline(target: Option<&str>) -> Result<(TargetScope, String)> {
    resolve_target_scope_with(target, resolve_target_offline)
}

fn resolve_explicit_targets(targets: &[String], skip_tmux: bool) -> Result<Vec<String>> {
    let mut resolved = Vec::with_capacity(targets.len());
    for target in targets {
        let candidate = if skip_tmux {
            resolve_target_offline(target)?
        } else {
            resolve_target(target)?
        };
        parse_target(&candidate)?;
        resolved.push(candidate);
    }
    Ok(dedupe_preserve_order(resolved))
}

fn resolve_target_scope_with(
    target: Option<&str>,
    pane_resolver: fn(&str) -> Result<String>,
) -> Result<(TargetScope, String)> {
    let Some(raw) = target
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return Ok((TargetScope::All, "all sessions/windows/panes".to_string()));
    };

    if raw.eq_ignore_ascii_case("all") {
        return Ok((TargetScope::All, "all sessions/windows/panes".to_string()));
    }

    if raw.contains(':') {
        if raw.contains('.') {
            let resolved = pane_resolver(raw)?;
            parse_target(&resolved)?;
            return Ok((TargetScope::Pane(resolved.clone()), resolved));
        }

        let (session, window) = parse_session_window(raw)?;
        let label = format!("{session}:{window}.*");
        return Ok((
            TargetScope::Window {
                session: session.to_string(),
                window: window.to_string(),
            },
            label,
        ));
    }

    if raw.contains('.') || raw.chars().all(|c| c.is_ascii_digit()) {
        let resolved = pane_resolver(raw)?;
        parse_target(&resolved)?;
        return Ok((TargetScope::Pane(resolved.clone()), resolved));
    }

    Ok((TargetScope::Session(raw.to_string()), format!("{raw}:*.*")))
}

fn parse_session_window(value: &str) -> Result<(&str, &str)> {
    let mut parts = value.splitn(2, ':');
    let session = parts.next().unwrap_or("").trim();
    let window = parts.next().unwrap_or("").trim();
    if session.is_empty() || window.is_empty() {
        bail!("target must be in the format session, session:window, or session:window.pane");
    }
    Ok((session, window))
}

fn select_targets_for_scope(scope: &TargetScope, panes: &[TmuxPane]) -> Vec<String> {
    panes
        .iter()
        .filter(|pane| match scope {
            TargetScope::All => true,
            TargetScope::Session(session) => &pane.session == session,
            TargetScope::Window { session, window } => {
                &pane.session == session && &pane.window == window
            }
            TargetScope::Pane(target) => &pane.target == target,
        })
        .map(|pane| pane.target.clone())
        .collect()
}

fn target_scope_label(scope: &TargetScope) -> String {
    match scope {
        TargetScope::All => "all sessions/windows/panes".to_string(),
        TargetScope::Session(session) => format!("{session}:*.*"),
        TargetScope::Window { session, window } => format!("{session}:{window}.*"),
        TargetScope::Pane(target) => target.clone(),
    }
}

fn resolve_target_with_current(target: &str, current_fn: fn() -> Result<String>) -> Result<String> {
    if target.contains(':') {
        return Ok(target.to_string());
    }

    let current = current_fn()
        .map_err(|_| anyhow::anyhow!("target shorthand requires tmux; use session:window.pane"))?;
    let (session, window, _pane) = parse_target(&current)?;

    if target.contains('.') {
        return Ok(format!("{session}:{target}"));
    }

    if target.chars().all(|c| c.is_ascii_digit()) {
        return Ok(format!("{session}:{window}.{target}"));
    }

    bail!("invalid target format: {target}");
}

fn tmux_current_target() -> Result<String> {
    let output = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "#{session_name}:#{window_index}.#{pane_index}",
        ])
        .output()
        .context("failed to query current tmux target")?;
    if !output.status.success() {
        bail!("tmux not available for target shorthand");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_target(target: &str) -> Result<(&str, &str, &str)> {
    let mut parts = target.splitn(2, ':');
    let session = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    if session.trim().is_empty() || rest.trim().is_empty() {
        bail!("target must be in the format session:window.pane");
    }

    let mut rest_parts = rest.splitn(2, '.');
    let window = rest_parts.next().unwrap_or("");
    let pane = rest_parts.next().unwrap_or("");
    if window.trim().is_empty() || pane.trim().is_empty() {
        bail!("target must be in the format session:window.pane");
    }

    Ok((session, window, pane))
}

fn validate_delay(delay: &DelayConfig) -> Result<()> {
    match delay.mode {
        DelayMode::Fixed => {
            if delay.value.unwrap_or(0) == 0 {
                bail!("delay.mode=fixed requires value > 0");
            }
        }
        DelayMode::Range | DelayMode::Jitter => {
            let min = delay.min.unwrap_or(0);
            let max = delay.max.unwrap_or(0);
            if min == 0 || max == 0 || min > max {
                bail!("delay.mode range/jitter requires min/max with min <= max and > 0");
            }
            if let DelayMode::Jitter = delay.mode {
                let jitter = delay.jitter.unwrap_or(0.0);
                if !(0.0..=1.0).contains(&jitter) {
                    bail!("delay.mode=jitter requires jitter between 0.0 and 1.0");
                }
            }
        }
        DelayMode::Backoff => {
            let backoff = delay
                .backoff
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("delay.mode=backoff requires backoff"))?;
            if backoff.base == 0 {
                bail!("delay.backoff.base must be > 0");
            }
            if backoff.factor < 1.0 {
                bail!("delay.backoff.factor must be >= 1.0");
            }
            if let Some(max) = backoff.max
                && max < backoff.base
            {
                bail!("delay.backoff.max must be >= base");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct LogEvent {
    event: String,
    timestamp: String,
    target: String,
    rule_id: Option<String>,
    detail: Option<String>,
    sends: Option<u32>,
}

impl LogEvent {
    fn started(config: &ResolvedConfig, timestamp: String) -> Self {
        Self {
            event: "started".to_string(),
            timestamp,
            target: config.target_label.clone(),
            rule_id: None,
            detail: None,
            sends: None,
        }
    }

    fn sent(
        config: &ResolvedConfig,
        rule_id: Option<&str>,
        timestamp: String,
        prompt: &str,
    ) -> Self {
        Self {
            event: "sent".to_string(),
            timestamp,
            target: config.target_label.clone(),
            rule_id: rule_id.map(|value| value.to_string()),
            detail: Some(prompt.to_string()),
            sends: None,
        }
    }

    fn delay_scheduled(config: &ResolvedConfig, rule_id: Option<&str>, detail: String) -> Self {
        Self {
            event: "delay".to_string(),
            timestamp: String::new(),
            target: config.target_label.clone(),
            rule_id: rule_id.map(|value| value.to_string()),
            detail: Some(detail),
            sends: None,
        }
    }

    fn stopped(config: &ResolvedConfig, detail: &str, sends: u32) -> Self {
        Self {
            event: "stopped".to_string(),
            timestamp: String::new(),
            target: config.target_label.clone(),
            rule_id: None,
            detail: Some(detail.to_string()),
            sends: Some(sends),
        }
    }

    fn matched(config: &ResolvedConfig, rule_id: Option<&str>) -> Self {
        Self {
            event: "match".to_string(),
            timestamp: String::new(),
            target: config.target_label.clone(),
            rule_id: rule_id.map(|value| value.to_string()),
            detail: None,
            sends: None,
        }
    }

    fn error(config: &ResolvedConfig, detail: String) -> Self {
        Self {
            event: "error".to_string(),
            timestamp: String::new(),
            target: config.target_label.clone(),
            rule_id: None,
            detail: Some(detail),
            sends: None,
        }
    }

    fn status(config: &ResolvedConfig, detail: String) -> Self {
        Self {
            event: "status".to_string(),
            timestamp: String::new(),
            target: config.target_label.clone(),
            rule_id: None,
            detail: Some(detail),
            sends: None,
        }
    }

    fn exec(config: &ResolvedConfig, event: &str, detail: String) -> Self {
        Self {
            event: event.to_string(),
            timestamp: String::new(),
            target: config.target_label.clone(),
            rule_id: None,
            detail: Some(detail),
            sends: None,
        }
    }
}

fn effective_elapsed(
    run_started: std::time::Instant,
    held_total: std::time::Duration,
    hold_started: Option<std::time::Instant>,
) -> std::time::Duration {
    let mut total_held = held_total;
    if let Some(started_at) = hold_started {
        total_held += started_at.elapsed();
    }
    run_started.elapsed().saturating_sub(total_held)
}

fn format_std_duration(duration: std::time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_clock_countdown(duration: std::time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn status_line(
    config: &ResolvedConfig,
    send_count: u32,
    max_sends: u32,
    rule_id: Option<&str>,
    elapsed: &str,
) -> String {
    let progress = if config.infinite {
        String::from("infinite")
    } else {
        format!("{}/{}", send_count, max_sends)
    };
    let rule = rule_id.unwrap_or("<unnamed>");
    let (selector_label, selector_value) = if config.exec_command.is_some() {
        ("event", format_exec_event_label(rule))
    } else {
        ("rule", rule.to_string())
    };
    let profile = config.profile_id.as_deref().unwrap_or("-");
    let icon = ">";
    let color = "\u{001B}[32m";
    let reset = "\u{001B}[0m";
    format!(
        "{}{} status:{} profile={} target={} progress={} {}={} elapsed={}{}",
        color,
        icon,
        reset,
        profile,
        config.target_label,
        progress,
        selector_label,
        selector_value,
        elapsed,
        reset
    )
}

fn format_exec_event_label(value: &str) -> String {
    match value {
        "exec:started" => "started".to_string(),
        "exec:running" => "running".to_string(),
        "exec:ok" => "ok".to_string(),
        "exec:fail" => "fail".to_string(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET_ALPHA: &str = "target-alpha";
    const TARGET_BETA: &str = "target-beta";

    fn edge_test_key(target: &str, index: usize) -> String {
        format!("{target}|inline|{index}")
    }

    fn rule_with(match_: Option<MatchCriteria>, exclude: Option<MatchCriteria>) -> Rule {
        Rule {
            id: None,
            match_,
            exclude,
            action: None,
            delay: None,
            confirm_seconds: None,
            next: None,
            priority: None,
        }
    }

    fn match_regex(pattern: &str) -> MatchCriteria {
        MatchCriteria {
            regex: Some(pattern.to_string()),
            trigger_expr: None,
            exact_line: None,
            contains: None,
            starts_with: None,
        }
    }

    fn match_contains(value: &str) -> MatchCriteria {
        MatchCriteria {
            regex: None,
            trigger_expr: None,
            exact_line: None,
            contains: Some(value.to_string()),
            starts_with: None,
        }
    }

    fn test_history_entry(prompt: &str, last_run: &str) -> HistoryEntry {
        HistoryEntry {
            last_run: last_run.to_string(),
            target: "ai:1.0".to_string(),
            prompt: prompt.to_string(),
            trigger: "DONE".to_string(),
            trigger_expr: None,
            trigger_exact_line: Some(false),
            exclude: None,
            pre: None,
            post: None,
            iterations: Some(1),
            tail: Some(1),
            head: None,
            once: false,
            poll: Some(5),
            initial_poll: Some(5),
            trigger_confirm_seconds: Some(DEFAULT_TRIGGER_CONFIRM_SECONDS),
            log_preview_lines: Some(3),
            trigger_edge: Some(true),
            recheck_before_send: Some(true),
            fanout: Some(FanoutMode::Matched),
            duration: None,
        }
    }

    #[test]
    fn trigger_expr_respects_precedence() {
        let expr = "A || B && C";
        assert!(matches_trigger_expr(expr, "A only").unwrap());
        assert!(!matches_trigger_expr(expr, "B only").unwrap());
        assert!(matches_trigger_expr(expr, "B C").unwrap());
    }

    #[test]
    fn trigger_expr_respects_parentheses() {
        let expr = "(A || B) && C";
        assert!(!matches_trigger_expr(expr, "B only").unwrap());
        assert!(matches_trigger_expr(expr, "B C").unwrap());
    }

    #[test]
    fn trigger_expr_trailing_operator_error() {
        let err = parse_trigger_expr("READY &&").unwrap_err();
        assert!(err.to_string().contains("trailing operator"));
    }

    #[test]
    fn trigger_expr_empty_term_error() {
        let err = parse_trigger_expr("READY && || DONE").unwrap_err();
        assert!(err.to_string().contains("expected term after '&&'"));
    }

    #[test]
    fn trigger_expr_missing_paren_error() {
        let err = parse_trigger_expr("(READY || DONE").unwrap_err();
        assert!(err.to_string().contains("missing right parenthesis"));
    }

    #[test]
    fn trigger_expr_unexpected_token_error() {
        let err = parse_trigger_expr(") READY").unwrap_err();
        assert!(err.to_string().contains("unexpected token"));
    }

    #[test]
    fn trigger_expr_invalid_regex_error() {
        let err = parse_trigger_expr("[").unwrap_err();
        assert!(err.to_string().contains("invalid regex term"));
    }

    #[test]
    fn wildcard_match_handles_star_patterns() {
        assert!(wildcard_match("/tmp/*/repo", "/tmp/demo/repo"));
        assert!(wildcard_match(
            "/Users/*/Codes/*",
            "/Users/diego/Codes/Projects"
        ));
        assert!(!wildcard_match("/tmp/*/repo", "/tmp/demo/repo/sub"));
    }

    #[test]
    fn workspace_loader_merges_main_runs_events_and_imports() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-workspace-test-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let imported = root.join("imported.yaml");
        let main = root.join("config.yaml");

        std::fs::write(
            &imported,
            r#"
runs:
  - id: imported-run
    target: "ai:9.0"
    iterations: 1
    default_action:
      prompt: "imported"
"#,
        )
        .unwrap();

        std::fs::write(
            &main,
            format!(
                r#"
id: main-run
target: "ai:1.0"
iterations: 1
default_action:
  prompt: "main"
imports:
  - {}
runs:
  - id: child-run
    target: "ai:2.0"
    iterations: 1
    default_action:
      prompt: "child"
events:
  - id: event-run
    target: "ai:3.0"
    iterations: 1
    default_action:
      prompt: "event"
"#,
                imported.display()
            ),
        )
        .unwrap();

        let profiles = load_workspace_profiles(&main).unwrap();
        let ids = profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"main-run".to_string()));
        assert!(ids.contains(&"child-run".to_string()));
        assert!(ids.contains(&"event-run".to_string()));
        assert!(ids.contains(&"imported-run".to_string()));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_workspace_profiles_respects_enabled_and_cwd() {
        let cwd = PathBuf::from("/tmp/demo");
        let profiles = vec![
            ResolvedRunProfile {
                id: "match-enabled".to_string(),
                source_path: PathBuf::from("/tmp/config.yaml"),
                config: Config::default(),
                enabled: true,
                when: RunProfileWhen {
                    cwd_matches: Some(vec!["/tmp/*".to_string()]),
                },
            },
            ResolvedRunProfile {
                id: "match-disabled".to_string(),
                source_path: PathBuf::from("/tmp/config.yaml"),
                config: Config::default(),
                enabled: false,
                when: RunProfileWhen {
                    cwd_matches: Some(vec!["/tmp/*".to_string()]),
                },
            },
            ResolvedRunProfile {
                id: "non-match-enabled".to_string(),
                source_path: PathBuf::from("/tmp/config.yaml"),
                config: Config::default(),
                enabled: true,
                when: RunProfileWhen {
                    cwd_matches: Some(vec!["/repo/*".to_string()]),
                },
            },
        ];

        let startup = selected_workspace_profiles(&profiles, &cwd, false)
            .into_iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        assert_eq!(startup, vec!["match-enabled".to_string()]);

        let all = selected_workspace_profiles(&profiles, &cwd, true)
            .into_iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn resolve_workspace_config_path_uses_override() {
        let path = PathBuf::from("/tmp/loopmux-custom.yaml");
        let resolved = resolve_workspace_config_path(Some(&path)).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn ensure_default_workspace_config_creates_template_with_continue_loop_event() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-ensure-config-create-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let config_path = root.join("loopmux").join("config.yaml");

        ensure_default_workspace_config_exists(&config_path).unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("exact_line: \"<CONTINUE-LOOP>\""));
        assert!(contents.contains("continue-loop"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_default_workspace_config_keeps_existing_file() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-ensure-config-existing-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.yaml");
        std::fs::write(&config_path, "id: existing\n").unwrap();

        ensure_default_workspace_config_exists(&config_path).unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(contents, "id: existing\n");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_doctor_reports_duplicate_profile_ids() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-doctor-dup-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.yaml");
        std::fs::write(
            &config_path,
            r#"
runs:
  - id: same
    target: "ai:1.0"
    iterations: 1
    default_action:
      prompt: "a"
  - id: same
    target: "ai:2.0"
    iterations: 1
    default_action:
      prompt: "b"
"#,
        )
        .unwrap();

        let err = config_doctor(Some(&config_path), true).unwrap_err();
        assert!(err.to_string().contains("duplicate profile id"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_doctor_reports_multiple_tui_profiles() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-doctor-tui-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.yaml");
        std::fs::write(
            &config_path,
            r#"
runs:
  - id: one
    target: "ai:1.0"
    iterations: 1
    tui: true
    default_action:
      prompt: "a"
  - id: two
    target: "ai:2.0"
    iterations: 1
    tui: true
    default_action:
      prompt: "b"
"#,
        )
        .unwrap();

        let err = config_doctor(Some(&config_path), true).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("multiple selected profiles enable `tui`")
                || message.contains("tmux list-panes failed"),
            "{message}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_test_reports_missing_profile() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-config-test-missing-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.yaml");
        std::fs::write(
            &config_path,
            r#"
runs:
  - id: one
    target: "ai:1.0"
    iterations: 1
    default_action:
      prompt: "a"
"#,
        )
        .unwrap();

        let err = config_test(Some(&config_path), "missing").unwrap_err();
        assert!(err.to_string().contains("not found"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_test_reports_duplicate_profile_ids() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-config-test-dup-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.yaml");
        std::fs::write(
            &config_path,
            r#"
runs:
  - id: same
    target: "ai:1.0"
    iterations: 1
    default_action:
      prompt: "a"
  - id: same
    target: "ai:2.0"
    iterations: 1
    default_action:
      prompt: "b"
"#,
        )
        .unwrap();

        let err = config_test(Some(&config_path), "same").unwrap_err();
        assert!(err.to_string().contains("duplicated"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matches_criteria_regex_and_contains() {
        let output = "hello world";
        assert!(matches_criteria(&match_regex("hello"), output).unwrap());
        assert!(matches_criteria(&match_contains("world"), output).unwrap());
        assert!(!matches_criteria(&match_contains("missing"), output).unwrap());
    }

    #[test]
    fn matches_criteria_exact_line() {
        let criteria = MatchCriteria {
            regex: None,
            trigger_expr: None,
            exact_line: Some("<CONTINUE-LOOP>".to_string()),
            contains: None,
            starts_with: None,
        };
        assert!(matches_criteria(&criteria, "foo\n  <CONTINUE-LOOP>  \nbar").unwrap());
        assert!(!matches_criteria(&criteria, "foo <CONTINUE-LOOP> bar").unwrap());
    }

    #[test]
    fn matches_criteria_trigger_expr() {
        let criteria = MatchCriteria {
            regex: None,
            trigger_expr: Some("(READY || DONE) && GO".to_string()),
            exact_line: None,
            contains: None,
            starts_with: None,
        };
        assert!(matches_criteria(&criteria, "READY GO").unwrap());
        assert!(!matches_criteria(&criteria, "READY").unwrap());
    }

    #[test]
    fn matches_criteria_invalid_regex() {
        let output = "hello";
        assert!(matches_criteria(&match_regex("["), output).is_err());
    }

    #[test]
    fn matches_rule_respects_exclude() {
        let rule = rule_with(Some(match_regex("hello")), Some(match_regex("world")));
        let output = "hello world";
        assert!(!matches_rule(&rule, output).unwrap());
    }

    #[test]
    fn matches_rule_exclude_only() {
        let rule = rule_with(None, Some(match_regex("skip")));
        assert!(matches_rule(&rule, "ok").unwrap());
        assert!(!matches_rule(&rule, "skip this").unwrap());
    }

    #[test]
    fn select_rules_priority() {
        let mut rule_a = rule_with(Some(match_contains("hit")), None);
        rule_a.priority = Some(1);
        let mut rule_b = rule_with(Some(match_contains("hit")), None);
        rule_b.priority = Some(2);
        let rules = vec![rule_a, rule_b];
        let matches = select_rules("hit", &rules, &RuleEval::Priority, None).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].index, 1);
    }

    #[test]
    fn select_rules_multi_match() {
        let rule_a = rule_with(Some(match_contains("hit")), None);
        let rule_b = rule_with(Some(match_contains("hit")), None);
        let rules = vec![rule_a, rule_b];
        let matches = select_rules("hit", &rules, &RuleEval::MultiMatch, None).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].index, 0);
        assert_eq!(matches[1].index, 1);
    }

    #[test]
    fn resolve_run_config_requires_trigger() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: None,
            trigger_expr: None,
            trigger_exact_line: false,
            exclude: None,
            pre: None,
            post: None,
            exec: None,
            target: vec!["ai:5.0".to_string()],
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(1),
            tail: None,
            head: None,
            once: false,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            prompt_edit_max_chars: None,
            name: None,
        };
        assert!(resolve_run_config(&args).is_err());
    }

    #[test]
    fn resolve_run_config_inline_exec_builds_exec_config() {
        let args = RunArgs {
            config: None,
            prompt: None,
            trigger: None,
            trigger_expr: None,
            trigger_exact_line: false,
            exclude: None,
            pre: None,
            post: None,
            exec: Some("gw-watch-comp --mode check".to_string()),
            target: Vec::new(),
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(3),
            tail: None,
            head: None,
            once: false,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: Some(5),
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: Some("30s".to_string()),
            history_limit: None,
            prompt_edit_max_chars: None,
            name: Some("gw-watch".to_string()),
        };

        let config = resolve_run_config(&args).unwrap();
        assert_eq!(
            config.exec.as_ref().map(|value| value.command.as_str()),
            Some("gw-watch-comp --mode check")
        );
        assert!(config.default_action.is_none());
        assert!(config.rules.is_none());
    }

    #[test]
    fn resolve_run_config_rejects_exec_with_prompt_mode_flags() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("Done".to_string()),
            trigger_expr: None,
            trigger_exact_line: false,
            exclude: None,
            pre: None,
            post: None,
            exec: Some("gw-watch-comp".to_string()),
            target: Vec::new(),
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(1),
            tail: None,
            head: None,
            once: false,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            prompt_edit_max_chars: None,
            name: None,
        };

        let err = resolve_run_config(&args).unwrap_err();
        assert!(err.to_string().contains("--exec cannot be combined"));
    }

    #[test]
    fn resolve_run_config_inline_builds_rule() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("Done".to_string()),
            trigger_expr: None,
            trigger_exact_line: false,
            exclude: Some("PROD".to_string()),
            pre: Some("pre".to_string()),
            post: Some("post".to_string()),
            exec: None,
            target: vec!["ai:5.0".to_string()],
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(2),
            tail: Some(123),
            head: None,
            once: true,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            prompt_edit_max_chars: None,
            name: None,
        };
        let config = resolve_run_config(&args).unwrap();
        let resolved = resolve_config(
            config,
            ResolveConfigArgs {
                target_override: None,
                iterations_override: None,
                skip_tmux: true,
                tail_override: args.tail,
                head_override: args.head,
                once: args.once,
                single_line: false,
                tui: false,
                trigger_edge_override: None,
                recheck_before_send_override: None,
                debug_trigger: false,
                profile_id: None,
            },
        )
        .unwrap();
        assert!(matches!(resolved.capture_window, CaptureWindow::Tail(123)));
        assert!(resolved.once);
        assert_eq!(resolved.rules.len(), 1);
        assert_eq!(
            resolved.trigger_confirm_seconds,
            DEFAULT_TRIGGER_CONFIRM_SECONDS
        );
        assert_eq!(
            resolved.rules[0].match_.as_ref().unwrap().regex.as_deref(),
            Some("Done")
        );
        assert_eq!(
            resolved.rules[0].exclude.as_ref().unwrap().regex.as_deref(),
            Some("PROD")
        );
    }

    #[test]
    fn resolve_run_config_inline_trigger_expr_mode() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: None,
            trigger_expr: Some("READY && GO".to_string()),
            trigger_exact_line: false,
            exclude: None,
            pre: None,
            post: None,
            exec: None,
            target: vec!["ai:5.0".to_string()],
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(1),
            tail: Some(1),
            head: None,
            once: false,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            prompt_edit_max_chars: None,
            name: None,
        };
        let config = resolve_run_config(&args).unwrap();
        let mut rules = config.rules.unwrap();
        let matcher = rules.remove(0).match_.unwrap();
        assert!(matcher.regex.is_none());
        assert_eq!(matcher.trigger_expr.as_deref(), Some("READY && GO"));
        assert!(matcher.exact_line.is_none());
    }

    #[test]
    fn resolve_run_config_inline_exact_line_mode() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("<CONTINUE-LOOP>".to_string()),
            trigger_expr: None,
            trigger_exact_line: true,
            exclude: None,
            pre: None,
            post: None,
            exec: None,
            target: vec!["ai:5.0".to_string()],
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(2),
            tail: Some(1),
            head: None,
            once: true,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            prompt_edit_max_chars: None,
            name: None,
        };
        let config = resolve_run_config(&args).unwrap();
        let mut rules = config.rules.unwrap();
        let rule = rules.remove(0);
        let matcher = rule.match_.unwrap();
        assert!(matcher.regex.is_none());
        assert_eq!(matcher.exact_line.as_deref(), Some("<CONTINUE-LOOP>"));
    }

    #[test]
    fn resolve_config_prefers_head_window_when_set() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("Done".to_string()),
            trigger_expr: None,
            trigger_exact_line: false,
            exclude: None,
            pre: None,
            post: None,
            exec: None,
            target: vec!["ai:5.0".to_string()],
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(1),
            tail: None,
            head: Some(7),
            once: false,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            prompt_edit_max_chars: None,
            name: None,
        };
        let config = resolve_run_config(&args).unwrap();
        let resolved = resolve_config(
            config,
            ResolveConfigArgs {
                target_override: None,
                iterations_override: None,
                skip_tmux: true,
                tail_override: args.tail,
                head_override: args.head,
                once: false,
                single_line: false,
                tui: false,
                trigger_edge_override: None,
                recheck_before_send_override: None,
                debug_trigger: false,
                profile_id: None,
            },
        )
        .unwrap();
        assert!(matches!(resolved.capture_window, CaptureWindow::Head(7)));
    }

    #[test]
    fn resolve_config_supports_multiple_explicit_tmux_targets() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("Done".to_string()),
            trigger_expr: None,
            trigger_exact_line: false,
            exclude: None,
            pre: None,
            post: None,
            exec: None,
            target: vec!["ai:5.0".to_string(), "codex:1.0".to_string()],
            targets_file: Vec::new(),
            file: Vec::new(),
            files_file: Vec::new(),
            iterations: Some(1),
            tail: Some(5),
            head: None,
            once: false,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            prompt_edit_max_chars: None,
            name: None,
        };
        let config = resolve_run_config(&args).unwrap();
        let resolved = resolve_config(
            config,
            ResolveConfigArgs {
                target_override: None,
                iterations_override: None,
                skip_tmux: true,
                tail_override: args.tail,
                head_override: args.head,
                once: false,
                single_line: false,
                tui: false,
                trigger_edge_override: None,
                recheck_before_send_override: None,
                debug_trigger: false,
                profile_id: None,
            },
        )
        .unwrap();
        assert_eq!(
            resolved.explicit_targets,
            Some(vec!["ai:5.0".to_string(), "codex:1.0".to_string()])
        );
    }

    #[test]
    fn resolve_config_rejects_missing_file_source() {
        let config = Config {
            target: Some("ai:5.0".to_string()),
            targets: None,
            files: Some(vec!["/tmp/loopmux-missing-source.log".to_string()]),
            exec: None,
            iterations: Some(1),
            infinite: None,
            poll: Some(1),
            initial_poll: None,
            trigger_confirm_seconds: Some(0),
            log_preview_lines: Some(1),
            trigger_edge: Some(true),
            recheck_before_send: Some(true),
            fanout: Some(FanoutMode::Matched),
            duration: None,
            prompt_edit_max_chars: None,
            rule_eval: Some(RuleEval::FirstMatch),
            default_action: Some(Action {
                pre: None,
                prompt: Some(PromptBlock::Single("go".to_string())),
                post: None,
            }),
            delay: None,
            rules: Some(vec![rule_with(Some(match_contains("ok")), None)]),
            logging: None,
            template_vars: None,
            tail: Some(1),
            once: Some(false),
            single_line: Some(false),
            tui: Some(false),
            name: Some("test".to_string()),
        };
        let err = resolve_config(
            config,
            ResolveConfigArgs {
                target_override: None,
                iterations_override: None,
                skip_tmux: true,
                tail_override: Some(1),
                head_override: None,
                once: false,
                single_line: false,
                tui: false,
                trigger_edge_override: None,
                recheck_before_send_override: None,
                debug_trigger: false,
                profile_id: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("file source not found"));
    }

    #[test]
    fn resolve_config_exec_mode_without_prompt_target_or_rules() {
        let config = Config {
            target: None,
            targets: None,
            files: None,
            exec: Some(ExecConfig {
                command: "gw-watch-comp".to_string(),
            }),
            iterations: Some(3),
            infinite: None,
            poll: Some(7),
            initial_poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            trigger_edge: None,
            recheck_before_send: None,
            fanout: None,
            duration: Some("30s".to_string()),
            prompt_edit_max_chars: None,
            rule_eval: None,
            default_action: None,
            delay: None,
            rules: None,
            logging: None,
            template_vars: None,
            tail: None,
            once: None,
            single_line: None,
            tui: None,
            name: Some("watcher".to_string()),
        };

        let resolved = resolve_config(
            config,
            ResolveConfigArgs {
                target_override: None,
                iterations_override: None,
                skip_tmux: true,
                tail_override: None,
                head_override: None,
                once: false,
                single_line: false,
                tui: false,
                trigger_edge_override: None,
                recheck_before_send_override: None,
                debug_trigger: false,
                profile_id: Some("watcher".to_string()),
            },
        )
        .unwrap();

        assert_eq!(resolved.exec_command.as_deref(), Some("gw-watch-comp"));
        assert_eq!(resolved.target_label, "exec://gw-watch-comp");
        assert!(!resolved.has_prompt);
        assert!(resolved.rules.is_empty());
    }

    #[test]
    fn parse_target_valid() {
        let (session, window, pane) = parse_target("ai:5.0").unwrap();
        assert_eq!(session, "ai");
        assert_eq!(window, "5");
        assert_eq!(pane, "0");
    }

    #[test]
    fn parse_target_invalid() {
        assert!(parse_target("ai").is_err());
        assert!(parse_target("ai:5").is_err());
        assert!(parse_target("ai:.0").is_err());
    }

    #[test]
    fn resolve_target_shorthand_pane_only() {
        let resolved = resolve_target_with_current("0", || Ok("ai:5.2".to_string())).unwrap();
        assert_eq!(resolved, "ai:5.0");
    }

    #[test]
    fn resolve_target_shorthand_window_pane() {
        let resolved = resolve_target_with_current("2.1", || Ok("ai:5.2".to_string())).unwrap();
        assert_eq!(resolved, "ai:2.1");
    }

    #[test]
    fn resolve_target_scope_defaults_to_all() {
        let (scope, label) =
            resolve_target_scope_with(None, |value| Ok(value.to_string())).unwrap();
        assert!(matches!(scope, TargetScope::All));
        assert_eq!(label, "all sessions/windows/panes");
    }

    #[test]
    fn resolve_target_scope_session() {
        let (scope, label) =
            resolve_target_scope_with(Some("ai"), |value| Ok(value.to_string())).unwrap();
        assert!(matches!(scope, TargetScope::Session(ref value) if value == "ai"));
        assert_eq!(label, "ai:*.*");
    }

    #[test]
    fn resolve_target_scope_window() {
        let (scope, label) =
            resolve_target_scope_with(Some("ai:5"), |value| Ok(value.to_string())).unwrap();
        assert!(
            matches!(scope, TargetScope::Window { ref session, ref window } if session == "ai" && window == "5")
        );
        assert_eq!(label, "ai:5.*");
    }

    #[test]
    fn resolve_explicit_targets_dedupes_preserving_order() {
        let targets = vec![
            "ai:5.0".to_string(),
            "codex:1.0".to_string(),
            "ai:5.0".to_string(),
        ];
        let resolved = resolve_explicit_targets(&targets, true).unwrap();
        assert_eq!(resolved, vec!["ai:5.0", "codex:1.0"]);
    }

    #[test]
    fn collect_source_inputs_merges_and_dedupes_in_order() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let targets_file = root.join("targets.txt");
        std::fs::write(&targets_file, "# comment\nai:5.0\nclaude:2.0\nai:5.0\n").unwrap();
        let files_file = root.join("files.txt");
        std::fs::write(
            &files_file,
            "# comment\n/tmp/a.log\n/tmp/b.log\n/tmp/a.log\n",
        )
        .unwrap();

        let sources = collect_source_inputs(
            &["codex:1.0".to_string(), "ai:5.0".to_string()],
            std::slice::from_ref(&targets_file),
            &[PathBuf::from("/tmp/a.log")],
            std::slice::from_ref(&files_file),
        )
        .unwrap();

        assert_eq!(
            sources.tmux_targets,
            vec!["codex:1.0", "ai:5.0", "claude:2.0"]
        );
        assert_eq!(sources.file_paths, vec!["/tmp/a.log", "/tmp/b.log"]);

        let _ = std::fs::remove_file(targets_file);
        let _ = std::fs::remove_file(files_file);
        let _ = std::fs::remove_dir(root);
    }

    #[test]
    fn collect_source_inputs_errors_for_missing_list_file() {
        let missing = PathBuf::from("/tmp/loopmux-missing-targets-file.txt");
        let err = collect_source_inputs(&[], &[missing], &[], &[]).unwrap_err();
        assert!(err.to_string().contains("failed to read list file"));
    }

    #[test]
    fn capture_file_respects_head_and_tail_windows() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-capture-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("sample.log");
        std::fs::write(&file, "A\nB\nC\nD\n").unwrap();

        let tail = capture_file(&file.display().to_string(), CaptureWindow::Tail(2)).unwrap();
        let head = capture_file(&file.display().to_string(), CaptureWindow::Head(2)).unwrap();

        assert_eq!(tail, "C\nD");
        assert_eq!(head, "A\nB");

        let _ = std::fs::remove_file(file);
        let _ = std::fs::remove_dir(root);
    }

    #[test]
    fn file_source_key_round_trip() {
        let key = file_source_key("/tmp/a.log");
        assert_eq!(file_source_path(&key), Some("/tmp/a.log"));
        assert!(file_source_path("ai:5.0").is_none());
    }

    #[test]
    fn sanitize_run_name_normalizes_chars() {
        assert_eq!(sanitize_run_name(" My Run #1 "), "my-run--1");
        assert_eq!(sanitize_run_name("alpha_beta"), "alpha_beta");
    }

    #[test]
    fn external_control_renew_resets_runtime_state() {
        let mut loop_state = LoopState::Running;
        let mut hold_started = None;
        let mut held_total = std::time::Duration::from_secs(0);
        let mut send_count = 9;
        let mut last_hash_by_target = std::collections::HashMap::new();
        last_hash_by_target.insert("ai:1.0".to_string(), "abc".to_string());
        let mut trigger_edge_active = HashSet::from(["ai:1.0|rule-a".to_string()]);
        let mut trigger_confirm_pending_since = std::collections::HashMap::new();
        trigger_confirm_pending_since
            .insert("ai:1.0|rule-a".to_string(), std::time::Instant::now());
        let mut active_rule = Some("next".to_string());
        let mut active_rule_by_target = std::collections::HashMap::new();
        active_rule_by_target.insert("ai:1.0".to_string(), Some("next".to_string()));
        let mut backoff_state = std::collections::HashMap::new();
        backoff_state.insert(
            "rule-a".to_string(),
            BackoffState {
                attempts: 1,
                last_sent: None,
            },
        );

        let should_stop = apply_external_control(
            FleetControlCommand::Renew,
            ExternalControlState {
                loop_state: &mut loop_state,
                hold_started: &mut hold_started,
                held_total: &mut held_total,
                send_count: &mut send_count,
                last_hash_by_target: &mut last_hash_by_target,
                trigger_edge_active: &mut trigger_edge_active,
                trigger_confirm_pending_since: &mut trigger_confirm_pending_since,
                active_rule: &mut active_rule,
                active_rule_by_target: &mut active_rule_by_target,
                backoff_state: &mut backoff_state,
            },
        );

        assert!(!should_stop);
        assert_eq!(send_count, 0);
        assert!(last_hash_by_target.is_empty());
        assert!(trigger_edge_active.is_empty());
        assert!(trigger_confirm_pending_since.is_empty());
        assert!(active_rule.is_none());
        assert!(active_rule_by_target.is_empty());
        assert!(backoff_state.is_empty());
    }

    #[test]
    fn external_control_next_clears_trigger_and_backoff_state() {
        let mut loop_state = LoopState::Running;
        let mut hold_started = None;
        let mut held_total = std::time::Duration::from_secs(0);
        let mut send_count = 9;
        let mut last_hash_by_target = std::collections::HashMap::new();
        last_hash_by_target.insert("ai:1.0".to_string(), "abc".to_string());
        let mut trigger_edge_active = HashSet::from(["ai:1.0|rule-a".to_string()]);
        let mut trigger_confirm_pending_since = std::collections::HashMap::new();
        trigger_confirm_pending_since
            .insert("ai:1.0|rule-a".to_string(), std::time::Instant::now());
        let mut active_rule = Some("next".to_string());
        let mut active_rule_by_target = std::collections::HashMap::new();
        active_rule_by_target.insert("ai:1.0".to_string(), Some("next".to_string()));
        let mut backoff_state = std::collections::HashMap::new();
        backoff_state.insert(
            "rule-a".to_string(),
            BackoffState {
                attempts: 1,
                last_sent: None,
            },
        );

        let should_stop = apply_external_control(
            FleetControlCommand::Next,
            ExternalControlState {
                loop_state: &mut loop_state,
                hold_started: &mut hold_started,
                held_total: &mut held_total,
                send_count: &mut send_count,
                last_hash_by_target: &mut last_hash_by_target,
                trigger_edge_active: &mut trigger_edge_active,
                trigger_confirm_pending_since: &mut trigger_confirm_pending_since,
                active_rule: &mut active_rule,
                active_rule_by_target: &mut active_rule_by_target,
                backoff_state: &mut backoff_state,
            },
        );

        assert!(!should_stop);
        assert_eq!(send_count, 9);
        assert!(last_hash_by_target.is_empty());
        assert!(trigger_edge_active.is_empty());
        assert!(trigger_confirm_pending_since.is_empty());
        assert!(active_rule.is_none());
        assert!(active_rule_by_target.is_empty());
        assert!(backoff_state.is_empty());
    }

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("5s").unwrap().as_secs(), 5);
        assert_eq!(parse_duration("2m").unwrap().as_secs(), 120);
        assert_eq!(parse_duration("1h").unwrap().as_secs(), 3600);
        assert_eq!(parse_duration("1d").unwrap().as_secs(), 86_400);
        assert_eq!(parse_duration("1w").unwrap().as_secs(), 604_800);
        assert_eq!(parse_duration("1mon").unwrap().as_secs(), 2_592_000);
        assert_eq!(parse_duration("1y").unwrap().as_secs(), 31_536_000);
    }

    #[test]
    fn parse_duration_rejects_invalid() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("s").is_err());
        assert!(parse_duration("5x").is_err());
    }

    fn status_bar_test_config() -> ResolvedConfig {
        ResolvedConfig {
            profile_id: None,
            exec_command: None,
            target_scope: TargetScope::Pane("ai:5.0".to_string()),
            target_label: "ai:5.0".to_string(),
            explicit_targets: None,
            file_sources: Vec::new(),
            iterations: Some(10),
            infinite: false,
            has_prompt: true,
            rule_eval: RuleEval::FirstMatch,
            rules: Vec::new(),
            delay: None,
            trigger_confirm_seconds: DEFAULT_TRIGGER_CONFIRM_SECONDS,
            prompt_placeholders: Vec::new(),
            template_vars: Vec::new(),
            default_action: Action {
                pre: None,
                prompt: Some(PromptBlock::Single("hi".to_string())),
                post: None,
            },
            logging: LoggingConfigResolved {
                path: None,
                format: LogFormatResolved::Text,
            },
            capture_window: CaptureWindow::Tail(200),
            once: false,
            single_line: false,
            tui: false,
            poll: 5,
            initial_poll: 5,
            log_preview_lines: 3,
            trigger_edge: true,
            recheck_before_send: true,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            prompt_edit_max_chars: DEFAULT_PROMPT_EDIT_MAX_CHARS,
            duration: None,
        }
    }

    #[test]
    fn legacy_status_bar_renderer_matches_direct_render() {
        let config = status_bar_test_config();
        let args = StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Standard,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 120,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: None,
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        };

        let direct = render_status_bar(&args);
        let via_adapter = LegacyStatusBarRenderer.render(args);
        assert_eq!(via_adapter, direct);
    }

    #[test]
    fn legacy_footer_renderer_matches_direct_render() {
        let args = FooterRenderArgs {
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 120,
            summary: Some("iter 5/10"),
            note: Some("view chrono"),
            overlay_help: None,
        };
        let direct = render_footer(
            args.style,
            args.width,
            args.summary,
            args.note,
            args.overlay_help,
        );
        let via_adapter = LegacyFooterRenderer.render(args);
        assert_eq!(via_adapter, direct);
    }

    fn assert_contains_in_order(line: &str, tokens: &[&str]) {
        let mut start = 0usize;
        for token in tokens {
            let found = line[start..]
                .find(token)
                .unwrap_or_else(|| panic!("token '{token}' missing from line: {line}"));
            start += found + token.len();
        }
    }

    #[test]
    fn render_status_bar_golden_compact_segments() {
        let config = status_bar_test_config();
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Compact,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: false,
                dim_logs: true,
            },
            width: 120,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: None,
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        });

        assert_contains_in_order(
            &line,
            &[
                "RUN",
                "iter 5/10",
                "50%",
                "rem 1m20s",
                "run -",
                "cpu 12.3% mem 42.0mb",
                "trg Concluded",
                &format!("v{LOOPMUX_VERSION}"),
                "ai:5.0",
            ],
        );
        assert!(!line.contains("last 00:10"));
        assert!(!line.contains("target ai:5.0"));
    }

    #[test]
    fn render_status_bar_golden_standard_segments() {
        let config = status_bar_test_config();
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Standard,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 160,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: None,
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        });

        assert_contains_in_order(
            &line,
            &[
                "RUN",
                "iter 5/10",
                "50%",
                "rem 1m20s",
                "run -",
                "cpu 12.3% mem 42.0mb",
                "trg Concluded",
                "last 00:10",
                &format!("v{LOOPMUX_VERSION}"),
                "ai:5.0",
            ],
        );
        assert!(!line.contains("target ai:5.0"));
    }

    #[test]
    fn render_status_bar_includes_next_scan_countdown() {
        let config = status_bar_test_config();
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Standard,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 160,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: Some("00:04"),
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        });

        assert_contains_in_order(
            &line,
            &[
                "RUN",
                "iter 5/10",
                "50%",
                "next 00:04",
                "rem 1m20s",
                "run -",
                "trg Concluded",
                "last 00:10",
            ],
        );
    }

    #[test]
    fn render_status_bar_includes_next_scan_countdown_compact() {
        let config = status_bar_test_config();
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Compact,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: false,
                dim_logs: true,
            },
            width: 120,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: Some("00:03"),
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        });

        assert_contains_in_order(&line, &["50%", "next 00:03", "rem 1m20s", "run -"]);
    }

    #[test]
    fn render_status_bar_includes_next_scan_countdown_wide() {
        let config = status_bar_test_config();
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Wide,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 200,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: Some("00:03"),
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        });

        assert_contains_in_order(
            &line,
            &["50%", "next 00:03", "rem 1m20s", "run -", "target ai:5.0"],
        );
    }

    #[test]
    fn render_status_bar_golden_wide_segments() {
        let config = status_bar_test_config();
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Wide,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 200,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: None,
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        });

        assert_contains_in_order(
            &line,
            &[
                "RUN",
                "iter 5/10",
                "50%",
                "rem 1m20s",
                "run -",
                "cpu 12.3% mem 42.0mb",
                "trg Concluded",
                "last 00:10",
                &format!("v{LOOPMUX_VERSION}"),
                "target ai:5.0",
            ],
        );
    }

    #[test]
    fn render_status_bar_unicode_snapshot_contract() {
        let mut config = status_bar_test_config();
        config.infinite = true;
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Standard,
            icon_mode: IconMode::Nerd,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 160,
            config: &config,
            current: 2,
            total: 0,
            rule_id: Some("This is a very long trigger string that should truncate"),
            elapsed: "00:10",
            remaining_duration: Some("2m10s"),
            next_scan_remaining: None,
            process_usage: Some("cpu 9.1% mem 22.4mb"),
        });

        assert!(line.contains("iter ∞"));
        assert!(line.contains(" · "));
        assert!(line.contains("…"));
        assert!(line.contains("cpu 9.1% mem 22.4mb"));
    }

    #[test]
    fn render_status_bar_no_color_snapshot_contract() {
        let config = status_bar_test_config();
        let line = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Holding,
            layout: LayoutMode::Wide,
            icon_mode: IconMode::Nerd,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 180,
            config: &config,
            current: 3,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:12",
            remaining_duration: Some("45s"),
            next_scan_remaining: None,
            process_usage: None,
        });

        assert!(!line.contains("\x1B["));
        assert!(line.contains("target ai:5.0"));
        assert!(line.contains("trg Concluded"));
        assert!(line.contains("last 00:12"));
    }

    #[test]
    fn render_status_bar_compact() {
        let config = ResolvedConfig {
            profile_id: None,
            exec_command: None,
            target_scope: TargetScope::Pane("ai:5.0".to_string()),
            target_label: "ai:5.0".to_string(),
            explicit_targets: None,
            file_sources: Vec::new(),
            iterations: Some(10),
            infinite: false,
            has_prompt: true,
            rule_eval: RuleEval::FirstMatch,
            rules: Vec::new(),
            delay: None,
            trigger_confirm_seconds: DEFAULT_TRIGGER_CONFIRM_SECONDS,
            prompt_placeholders: Vec::new(),
            template_vars: Vec::new(),
            default_action: Action {
                pre: None,
                prompt: Some(PromptBlock::Single("hi".to_string())),
                post: None,
            },
            logging: LoggingConfigResolved {
                path: None,
                format: LogFormatResolved::Text,
            },
            capture_window: CaptureWindow::Tail(200),
            once: false,
            single_line: false,
            tui: false,
            poll: 5,
            initial_poll: 5,
            log_preview_lines: 3,
            trigger_edge: true,
            recheck_before_send: true,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            prompt_edit_max_chars: DEFAULT_PROMPT_EDIT_MAX_CHARS,
            duration: None,
        };
        let bar = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Compact,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: false,
                dim_logs: true,
            },
            width: 80,
            config: &config,
            current: 5,
            total: 10,
            rule_id: Some("Concluded"),
            elapsed: "00:10",
            remaining_duration: None,
            next_scan_remaining: None,
            process_usage: None,
        });
        assert!(bar.contains("RUN"));
        assert!(bar.contains("5/10"));
        assert!(bar.contains("ai:5.0"));
    }

    #[test]
    fn render_status_bar_standard_truncates_trigger() {
        let config = ResolvedConfig {
            profile_id: None,
            exec_command: None,
            target_scope: TargetScope::Pane("ai:5.0".to_string()),
            target_label: "ai:5.0".to_string(),
            explicit_targets: None,
            file_sources: Vec::new(),
            iterations: Some(10),
            infinite: false,
            has_prompt: true,
            rule_eval: RuleEval::FirstMatch,
            rules: Vec::new(),
            delay: None,
            trigger_confirm_seconds: DEFAULT_TRIGGER_CONFIRM_SECONDS,
            prompt_placeholders: Vec::new(),
            template_vars: Vec::new(),
            default_action: Action {
                pre: None,
                prompt: Some(PromptBlock::Single("hi".to_string())),
                post: None,
            },
            logging: LoggingConfigResolved {
                path: None,
                format: LogFormatResolved::Text,
            },
            capture_window: CaptureWindow::Tail(200),
            once: false,
            single_line: false,
            tui: false,
            poll: 5,
            initial_poll: 5,
            log_preview_lines: 3,
            trigger_edge: true,
            recheck_before_send: true,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            prompt_edit_max_chars: DEFAULT_PROMPT_EDIT_MAX_CHARS,
            duration: None,
        };
        let bar = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Standard,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 160,
            config: &config,
            current: 1,
            total: 10,
            rule_id: Some("This is a very long trigger string that should truncate"),
            elapsed: "00:10",
            remaining_duration: Some("1m20s"),
            next_scan_remaining: None,
            process_usage: None,
        });
        assert!(bar.contains("trg"));
        assert!(bar.contains("rem 1m20s"));
        assert!(bar.contains("…"));
    }

    #[test]
    fn render_status_bar_exec_uses_event_label() {
        let config = ResolvedConfig {
            profile_id: Some("watcher".to_string()),
            exec_command: Some("gw-watch-comp".to_string()),
            target_scope: TargetScope::All,
            target_label: "exec://gw-watch-comp".to_string(),
            explicit_targets: None,
            file_sources: Vec::new(),
            iterations: Some(3),
            infinite: false,
            has_prompt: false,
            rule_eval: RuleEval::FirstMatch,
            rules: Vec::new(),
            delay: None,
            trigger_confirm_seconds: DEFAULT_TRIGGER_CONFIRM_SECONDS,
            prompt_placeholders: Vec::new(),
            template_vars: Vec::new(),
            default_action: Action {
                pre: None,
                prompt: None,
                post: None,
            },
            logging: LoggingConfigResolved {
                path: None,
                format: LogFormatResolved::Text,
            },
            capture_window: CaptureWindow::Tail(1),
            once: false,
            single_line: false,
            tui: true,
            poll: 10,
            initial_poll: 5,
            log_preview_lines: 3,
            trigger_edge: true,
            recheck_before_send: true,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            prompt_edit_max_chars: DEFAULT_PROMPT_EDIT_MAX_CHARS,
            duration: None,
        };
        let bar = render_status_bar(&StatusBarRenderArgs {
            state: LoopState::Running,
            layout: LayoutMode::Standard,
            icon_mode: IconMode::Ascii,
            style: StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            width: 120,
            config: &config,
            current: 1,
            total: 3,
            rule_id: Some("exec:running"),
            elapsed: "00:05",
            remaining_duration: None,
            next_scan_remaining: None,
            process_usage: Some("cpu 12.3% mem 42.0mb"),
        });
        assert!(bar.contains("evt exec:running"));
        assert!(bar.contains("cpu 12.3% mem 42.0mb"));
    }

    #[test]
    fn render_status_bar_stopped_shows_consistent_core_tokens() {
        let config = status_bar_test_config();
        for layout in [LayoutMode::Compact, LayoutMode::Standard, LayoutMode::Wide] {
            let line = render_status_bar(&StatusBarRenderArgs {
                state: LoopState::Stopped,
                layout,
                icon_mode: IconMode::Ascii,
                style: StyleConfig {
                    use_color: false,
                    use_bg: false,
                    use_unicode_ellipsis: true,
                    dim_logs: true,
                },
                width: 200,
                config: &config,
                current: 3,
                total: 10,
                rule_id: Some("manual_stop"),
                elapsed: "00:12",
                remaining_duration: None,
                next_scan_remaining: None,
                process_usage: None,
            });
            assert!(line.contains("STOP"));
            assert!(line.contains("iter 3/10"));
            assert!(line.contains("trg manual_stop"));
            assert!(!line.contains("rem "));
        }
    }

    #[test]
    fn parse_process_usage_summary_parses_cpu_and_mem() {
        let summary = parse_process_usage_summary(" 12.5  20480\n").unwrap();
        assert_eq!(summary, "cpu 12.5% mem 20.0mb");
    }

    #[test]
    fn sanitize_tui_log_line_removes_controls_and_limits_spaces() {
        let raw = "\u{1b}[31mfoo\u{1b}[0m\t\tbar    baz\u{0007}";
        let sanitized = sanitize_tui_log_line(raw);
        assert_eq!(sanitized, "foo  bar  baz");
    }

    #[test]
    fn sanitize_tui_log_line_drops_replacement_char() {
        let raw = "ok\u{fffd}value";
        let sanitized = sanitize_tui_log_line(raw);
        assert_eq!(sanitized, "okvalue");
    }

    #[test]
    fn extract_log_target_prefers_target_field() {
        let line = "[2026-02-24T20:51:12Z] sent target=ai:2.0 sends=4";
        assert_eq!(tui::extract_log_target(line).as_deref(), Some("ai:2.0"));
    }

    #[test]
    fn grouped_log_lines_fold_same_target() {
        let logs = vec![
            "[a] matched target=ai:2.0".to_string(),
            "[b] delay target=ai:2.0".to_string(),
            "[c] matched target=codex:1.0".to_string(),
        ];
        let lines = build_grouped_log_lines(&logs, 10, true);
        assert!(lines.iter().any(|line| line.starts_with("ai:2.0 x2 ")));
        assert!(lines.iter().any(|line| line.starts_with("codex:1.0 x1 ")));
    }

    #[test]
    fn status_line_exec_uses_friendly_event_label() {
        let config = ResolvedConfig {
            profile_id: Some("watcher".to_string()),
            exec_command: Some("gw-watch-comp".to_string()),
            target_scope: TargetScope::All,
            target_label: "exec://gw-watch-comp".to_string(),
            explicit_targets: None,
            file_sources: Vec::new(),
            iterations: Some(3),
            infinite: false,
            has_prompt: false,
            rule_eval: RuleEval::FirstMatch,
            rules: Vec::new(),
            delay: None,
            trigger_confirm_seconds: DEFAULT_TRIGGER_CONFIRM_SECONDS,
            prompt_placeholders: Vec::new(),
            template_vars: Vec::new(),
            default_action: Action {
                pre: None,
                prompt: None,
                post: None,
            },
            logging: LoggingConfigResolved {
                path: None,
                format: LogFormatResolved::Text,
            },
            capture_window: CaptureWindow::Tail(1),
            once: false,
            single_line: true,
            tui: false,
            poll: 10,
            initial_poll: 5,
            log_preview_lines: 3,
            trigger_edge: true,
            recheck_before_send: true,
            debug_trigger: false,
            fanout: FanoutMode::Matched,
            prompt_edit_max_chars: DEFAULT_PROMPT_EDIT_MAX_CHARS,
            duration: None,
        };

        let line = status_line(&config, 1, 3, Some("exec:running"), "5s");
        assert!(line.contains("event=running"));
        assert!(!line.contains("rule=exec:running"));
    }

    #[test]
    fn format_exec_event_label_maps_known_values() {
        assert_eq!(format_exec_event_label("exec:started"), "started");
        assert_eq!(format_exec_event_label("exec:running"), "running");
        assert_eq!(format_exec_event_label("exec:ok"), "ok");
        assert_eq!(format_exec_event_label("exec:fail"), "fail");
        assert_eq!(format_exec_event_label("custom"), "custom");
    }

    #[test]
    fn trigger_edge_rearms_after_clear() {
        let mut active = HashSet::new();
        let alpha_edge = edge_test_key(TARGET_ALPHA, 0);
        let beta_edge = edge_test_key(TARGET_BETA, 0);
        active.insert(alpha_edge.clone());

        let matched_now = HashSet::new();
        refresh_trigger_edges_for_target(&mut active, TARGET_ALPHA, &matched_now, false, true);
        assert!(!active.contains(&alpha_edge));

        active.insert(beta_edge.clone());
        refresh_trigger_edges_for_target(&mut active, TARGET_ALPHA, &matched_now, false, true);
        assert!(active.contains(&beta_edge));
    }

    #[test]
    fn trigger_edge_rearms_when_output_hash_changes() {
        let mut active = HashSet::new();
        let alpha_edge = edge_test_key(TARGET_ALPHA, 0);
        let beta_edge = edge_test_key(TARGET_BETA, 0);
        active.insert(alpha_edge.clone());
        active.insert(beta_edge.clone());

        let matched_now = HashSet::from([alpha_edge.clone()]);
        refresh_trigger_edges_for_target(&mut active, TARGET_ALPHA, &matched_now, true, true);

        assert!(!active.contains(&alpha_edge));
        assert!(active.contains(&beta_edge));
    }

    #[test]
    fn edge_guard_allowance_respects_toggle() {
        let mut active = HashSet::new();
        let alpha_edge = edge_test_key(TARGET_ALPHA, 0);
        let alpha_other_edge = edge_test_key(TARGET_ALPHA, 1);
        active.insert(alpha_edge.clone());
        assert!(!edge_guard_allows(&active, &alpha_edge, true));
        assert!(edge_guard_allows(&active, &alpha_edge, false));
        assert!(edge_guard_allows(&active, &alpha_other_edge, true));
    }

    #[test]
    fn trigger_edge_contract_rearms_on_hash_change_with_persistent_marker() {
        let alpha_edge = edge_test_key(TARGET_ALPHA, 0);

        let mut active = HashSet::from([alpha_edge.clone()]);
        let matched_now = HashSet::from([alpha_edge.clone()]);

        refresh_trigger_edges_for_target(&mut active, TARGET_ALPHA, &matched_now, false, true);
        assert!(!edge_guard_allows(&active, &alpha_edge, true));

        refresh_trigger_edges_for_target(&mut active, TARGET_ALPHA, &matched_now, true, true);
        assert!(edge_guard_allows(&active, &alpha_edge, true));
    }

    #[test]
    fn prompt_editor_enforces_max_chars() {
        let mut editor = PromptEditorState::new("seed".to_string(), 5);
        editor.current.clear();
        editor.input_char('a');
        editor.input_char('b');
        editor.input_char('c');
        editor.input_char('d');
        editor.input_char('e');
        editor.input_char('f');
        assert_eq!(editor.current, "abcde");
    }

    #[test]
    fn prompt_editor_delete_selected_and_undo_restore_history() {
        let mut editor = PromptEditorState::new("original".to_string(), 100);
        editor.history = vec![
            PromptHistoryItem {
                created_at: "2026-02-25T00:00:00Z".to_string(),
                text: "first".to_string(),
            },
            PromptHistoryItem {
                created_at: "2026-02-25T00:01:00Z".to_string(),
                text: "second".to_string(),
            },
        ];

        editor.selected_idx = 0;
        editor.request_delete_selected();
        assert!(editor.confirm.is_none());

        editor.selected_idx = 1;
        editor.request_delete_selected();
        assert_eq!(editor.confirm, Some(PromptEditorConfirm::DeleteSelected));

        editor.confirm_yes();
        assert_eq!(editor.history.len(), 1);
        assert_eq!(editor.history[0].text, "second");

        editor.undo();
        assert_eq!(editor.history.len(), 2);
        assert_eq!(editor.history[0].text, "first");
        assert_eq!(editor.history[1].text, "second");
    }

    #[test]
    fn prompt_editor_clear_all_and_undo_restore_history() {
        let mut editor = PromptEditorState::new("original".to_string(), 100);
        editor.history = vec![PromptHistoryItem {
            created_at: "2026-02-25T00:00:00Z".to_string(),
            text: "first".to_string(),
        }];

        editor.request_clear_history();
        assert_eq!(editor.confirm, Some(PromptEditorConfirm::ClearAll));

        editor.confirm_yes();
        assert!(editor.history.is_empty());

        editor.undo();
        assert_eq!(editor.history.len(), 1);
        assert_eq!(editor.history[0].text, "first");
    }

    #[test]
    fn prompt_history_loader_falls_back_when_persisted_json_corrupt() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-prompt-history-corrupt-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let persisted_path = root.join("prompt_editor_history.json");
        let run_history_path = root.join("history.json");

        std::fs::write(&persisted_path, "{not-valid-json").unwrap();
        let run_history = RunHistory {
            entries: vec![test_history_entry(
                "from-run-history",
                "2026-02-25T00:00:00Z",
            )],
        };
        std::fs::write(
            &run_history_path,
            serde_json::to_string_pretty(&run_history).unwrap(),
        )
        .unwrap();

        let items =
            load_prompt_history_items_from_paths(10, &persisted_path, &run_history_path).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "from-run-history");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prompt_history_loader_falls_back_when_persisted_file_empty() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-prompt-history-empty-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let persisted_path = root.join("prompt_editor_history.json");
        let run_history_path = root.join("history.json");

        std::fs::write(&persisted_path, "").unwrap();
        let run_history = RunHistory {
            entries: vec![test_history_entry(
                "fallback-run-history",
                "2026-02-25T00:00:00Z",
            )],
        };
        std::fs::write(
            &run_history_path,
            serde_json::to_string_pretty(&run_history).unwrap(),
        )
        .unwrap();

        let items =
            load_prompt_history_items_from_paths(10, &persisted_path, &run_history_path).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "fallback-run-history");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prompt_history_save_truncates_to_limit() {
        let root = std::env::temp_dir().join(format!(
            "loopmux-prompt-history-save-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let persisted_path = root.join("prompt_editor_history.json");

        let items = vec![
            PromptHistoryItem {
                created_at: "2026-02-25T00:00:00Z".to_string(),
                text: "first".to_string(),
            },
            PromptHistoryItem {
                created_at: "2026-02-25T00:01:00Z".to_string(),
                text: "second".to_string(),
            },
            PromptHistoryItem {
                created_at: "2026-02-25T00:02:00Z".to_string(),
                text: "third".to_string(),
            },
        ];

        save_prompt_history_items_to_path(&items, 2, &persisted_path).unwrap();
        let content = std::fs::read_to_string(&persisted_path).unwrap();
        let saved: Vec<PromptHistoryItem> = serde_json::from_str(&content).unwrap();

        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].text, "first");
        assert_eq!(saved[1].text, "second");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hash_skip_depends_on_trigger_edge_mode() {
        assert!(should_skip_scan_by_hash(true, "same", "same", false));
        assert!(!should_skip_scan_by_hash(true, "same", "same", true));
        assert!(!should_skip_scan_by_hash(false, "same", "same", false));
        assert!(!should_skip_scan_by_hash(true, "new", "old", false));
    }

    #[test]
    fn pending_confirm_detected_per_target() {
        let mut pending = std::collections::HashMap::new();
        let now = std::time::Instant::now();
        pending.insert(edge_test_key(TARGET_ALPHA, 0), now);
        pending.insert(edge_test_key(TARGET_BETA, 0), now);
        assert!(has_pending_confirm_for_target(&pending, TARGET_ALPHA));
        assert!(has_pending_confirm_for_target(&pending, TARGET_BETA));
        assert!(!has_pending_confirm_for_target(&pending, "target-gamma"));
    }

    #[test]
    fn injection_filter_tracks_triggered_targets() {
        let mut filter = InjectionFilterState::default();
        filter.observe_trigger_target("ai:5.0");
        filter.observe_trigger_target("ai:5.1");
        filter.observe_trigger_target("ai:5.0");
        assert_eq!(filter.known_targets.len(), 2);
        assert_eq!(filter.active_counts(), (2, 2));
    }

    #[test]
    fn injection_filter_disable_all_blocks_known_targets() {
        let mut filter = InjectionFilterState::default();
        filter.observe_trigger_target("ai:5.0");
        filter.observe_trigger_target("codex:1.0");
        filter.disable_all();
        assert!(!filter.is_allowed("ai:5.0"));
        assert!(!filter.is_allowed("codex:1.0"));
        assert_eq!(filter.active_counts(), (0, 2));
    }

    #[test]
    fn injection_filter_toggle_current_selection_toggles_scope() {
        let mut filter = InjectionFilterState::default();
        filter.observe_trigger_target("ai:5.0");
        filter.observe_trigger_target("ai:5.1");
        filter.observe_trigger_target("codex:1.0");
        filter.open_popup();
        filter.cursor.column = ActiveListColumn::Session;
        filter.cursor.session_idx = 0;
        filter.toggle_current_selection();
        assert!(!filter.is_allowed("ai:5.0"));
        assert!(!filter.is_allowed("ai:5.1"));
        assert!(filter.is_allowed("codex:1.0"));
    }

    #[test]
    fn injection_filter_window_toggle_only_affects_window_targets() {
        let mut filter = InjectionFilterState::default();
        filter.observe_trigger_target("ai:5.0");
        filter.observe_trigger_target("ai:5.1");
        filter.observe_trigger_target("ai:6.0");
        filter.observe_trigger_target("codex:1.0");
        filter.open_popup();
        filter.cursor.column = ActiveListColumn::Window;
        filter.cursor.session_idx = 0;
        filter.cursor.window_idx = 0;
        filter.toggle_current_selection();
        assert!(!filter.is_allowed("ai:5.0"));
        assert!(!filter.is_allowed("ai:5.1"));
        assert!(filter.is_allowed("ai:6.0"));
        assert!(filter.is_allowed("codex:1.0"));
    }

    #[test]
    fn injection_filter_pane_toggle_only_affects_selected_pane() {
        let mut filter = InjectionFilterState::default();
        filter.observe_trigger_target("ai:5.0");
        filter.observe_trigger_target("ai:5.1");
        filter.observe_trigger_target("ai:6.0");
        filter.open_popup();
        filter.cursor.column = ActiveListColumn::Pane;
        filter.cursor.session_idx = 0;
        filter.cursor.window_idx = 0;
        filter.cursor.pane_idx = 1;
        filter.toggle_current_selection();
        assert!(filter.is_allowed("ai:5.0"));
        assert!(!filter.is_allowed("ai:5.1"));
        assert!(filter.is_allowed("ai:6.0"));
    }

    #[test]
    fn confirm_window_elapsed_requires_persisted_match() {
        let mut pending = std::collections::HashMap::new();
        let edge_key = edge_test_key(TARGET_ALPHA, 0);
        let now = std::time::Instant::now();
        assert!(!confirm_window_elapsed(
            5,
            None,
            &edge_key,
            &mut pending,
            now
        ));
        assert!(!confirm_window_elapsed(
            5,
            Some(3),
            &edge_key,
            &mut pending,
            now + std::time::Duration::from_secs(2),
        ));
        assert!(confirm_window_elapsed(
            5,
            Some(3),
            &edge_key,
            &mut pending,
            now + std::time::Duration::from_secs(3),
        ));
    }

    #[test]
    fn confirm_window_elapsed_zero_is_immediate() {
        let mut pending = std::collections::HashMap::new();
        let edge_key = edge_test_key(TARGET_ALPHA, 0);
        assert!(confirm_window_elapsed(
            5,
            Some(0),
            &edge_key,
            &mut pending,
            std::time::Instant::now(),
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn skip_scan_by_hash_requires_initialized_last_hash() {
        assert!(!should_skip_scan_by_hash(true, "", "", false));
        assert!(should_skip_scan_by_hash(true, "abc", "abc", false));
        assert!(!should_skip_scan_by_hash(true, "abc", "abc", true));
        assert!(!should_skip_scan_by_hash(false, "abc", "abc", false));
    }

    #[test]
    fn truncate_text_respects_ascii_max_width() {
        let truncated = truncate_text("abcdefghijk", 8, false);
        assert_eq!(truncated.chars().count(), 8);
        assert_eq!(truncated, "abcde...");
    }

    #[test]
    fn retry_enter_when_tail_shows_unsent_prompt() {
        let before = "shell ready\n";
        let after = "shell ready\nloopmux test command";
        assert!(should_retry_enter_submit(
            Some(before),
            Some(after),
            "loopmux test command"
        ));
    }

    #[test]
    fn no_retry_when_tail_no_longer_contains_prompt() {
        let before = "shell ready\n";
        let after = "shell ready\ncommand output\n";
        assert!(!should_retry_enter_submit(
            Some(before),
            Some(after),
            "loopmux test command"
        ));
    }

    #[test]
    fn extract_trigger_preview_ascii_separator() {
        let output = "line1\nline2\nline3\n";
        let (_, preview) = extract_trigger_preview(output, 2, false);
        assert!(preview.contains(" | "));
        assert!(!preview.contains(" │ "));
    }

    #[test]
    fn log_line_date_extracts_rfc3339_prefix() {
        let line = "[2026-02-17T00:12:34Z] started target=ai:7.0";
        assert_eq!(log_line_date(line), Some("2026-02-17"));
        assert_eq!(log_line_date("23:11:04 > ai:7.0"), None);
    }

    #[test]
    fn compact_time_prefix_detection() {
        assert!(looks_like_compact_time_prefix("23:11:04 > ai:7.0"));
        assert!(!looks_like_compact_time_prefix(
            "[2026-02-17T00:12:34Z] sent"
        ));
    }

    #[test]
    fn latest_stop_reason_prefers_most_recent_reason() {
        let logs = vec![
            "[t1] started target=ai:1.0".to_string(),
            "[t2] stopped reason=manual".to_string(),
            "[t3] stopped reason=once sends=1 elapsed=0s".to_string(),
        ];
        assert_eq!(latest_stop_reason(&logs).as_deref(), Some("once"));
    }

    #[test]
    fn latest_stop_reason_ignores_missing_reason_token() {
        let logs = vec![
            "[t1] stopped reason=".to_string(),
            "[t2] status target=ai:1.0".to_string(),
        ];
        assert!(latest_stop_reason(&logs).is_none());
    }

    #[test]
    fn tui_frame_signature_changes_when_footer_changes() {
        let lines = vec!["line a".to_string(), "line b".to_string()];
        let a = tui_frame_signature(120, 30, "bar", &lines, "footer a", false);
        let b = tui_frame_signature(120, 30, "bar", &lines, "footer b", false);
        assert_ne!(a, b);
    }

    #[test]
    fn tui_frame_signature_changes_when_overlay_visibility_changes() {
        let lines = vec!["line a".to_string()];
        let a = tui_frame_signature(100, 20, "bar", &lines, "footer", false);
        let b = tui_frame_signature(100, 20, "bar", &lines, "footer", true);
        assert_ne!(a, b);
    }

    #[test]
    fn tui_frame_signature_changes_when_status_bar_changes() {
        let lines = vec!["line a".to_string()];
        let a = tui_frame_signature(100, 20, "bar one", &lines, "footer", false);
        let b = tui_frame_signature(100, 20, "bar two", &lines, "footer", false);
        assert_ne!(a, b);
    }

    #[test]
    fn tui_frame_signature_changes_when_log_lines_change() {
        let a = tui_frame_signature(100, 20, "bar", &["line a".to_string()], "footer", false);
        let b = tui_frame_signature(100, 20, "bar", &["line b".to_string()], "footer", false);
        assert_ne!(a, b);
    }

    #[test]
    fn fleet_heartbeat_interval_scales_and_is_bounded() {
        assert_eq!(fleet_heartbeat_interval_seconds(1), 30);
        assert_eq!(fleet_heartbeat_interval_seconds(3), 36);
        assert_eq!(fleet_heartbeat_interval_seconds(60), 300);
    }

    #[test]
    fn fleet_heartbeat_emission_respects_interval_and_bad_timestamps() {
        let now = OffsetDateTime::parse(
            "2026-03-01T00:01:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(!should_emit_fleet_heartbeat(
            now,
            Some("2026-03-01T00:00:20Z"),
            5,
        ));
        assert!(should_emit_fleet_heartbeat(
            now,
            Some("2026-03-01T00:00:00Z"),
            5,
        ));
        assert!(!should_emit_fleet_heartbeat(now, None, 5));
        assert!(should_emit_fleet_heartbeat(now, Some("not-a-ts"), 5));
    }

    #[test]
    fn fleet_heartbeat_emission_does_not_trigger_for_future_timestamps() {
        let now = OffsetDateTime::parse(
            "2026-03-01T00:01:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(!should_emit_fleet_heartbeat(
            now,
            Some("2026-03-01T00:02:30Z"),
            5,
        ));
    }

    #[test]
    fn fleet_heartbeat_emission_with_zero_poll_uses_minimum_interval() {
        let now = OffsetDateTime::parse(
            "2026-03-01T00:01:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(!should_emit_fleet_heartbeat(
            now,
            Some("2026-03-01T00:00:31Z"),
            0,
        ));
        assert!(should_emit_fleet_heartbeat(
            now,
            Some("2026-03-01T00:00:30Z"),
            0,
        ));
    }

    #[test]
    fn fleet_heartbeat_drift_seconds_tracks_overdue_amount() {
        let now = OffsetDateTime::parse(
            "2026-03-01T00:01:20Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(
            fleet_heartbeat_drift_seconds(now, Some("2026-03-01T00:00:00Z"), 60),
            20,
        );
        assert_eq!(
            fleet_heartbeat_drift_seconds(now, Some("2026-03-01T00:00:50Z"), 60),
            0,
        );
        assert_eq!(
            fleet_heartbeat_drift_seconds(now, Some("2026-03-01T00:02:20Z"), 60),
            0,
        );
        assert_eq!(fleet_heartbeat_drift_seconds(now, None, 60), 0);
        assert_eq!(fleet_heartbeat_drift_seconds(now, Some("bad-ts"), 60), 0);
    }

    #[test]
    fn fleet_heartbeat_drift_severity_is_compact_and_stable() {
        assert_eq!(fleet_heartbeat_drift_severity(0), "ok");
        assert_eq!(fleet_heartbeat_drift_severity(10), "warn");
        assert_eq!(fleet_heartbeat_drift_severity(30), "warn");
        assert_eq!(fleet_heartbeat_drift_severity(31), "critical");
    }

    #[test]
    fn fleet_heartbeat_metric_marks_idle_and_active_modes() {
        let idle = format_fleet_heartbeat_metric(LoopState::Running, 10, 0, 5, 60, 0);
        assert!(idle.contains("state=running"));
        assert!(idle.contains("activity=idle"));
        assert!(idle.contains("progress=stalled"));
        assert!(idle.contains("poll=5s"));
        assert!(idle.contains("window=60s"));
        assert!(idle.contains("drift=0s"));
        assert!(idle.contains("severity=ok"));
        let active = format_fleet_heartbeat_metric(LoopState::Running, 12, 2, 5, 60, 20);
        assert!(active.contains("activity=active"));
        assert!(active.contains("progress=progressing"));
        assert!(active.contains("sends_total=12"));
        assert!(active.contains("sends_delta=2"));
        assert!(active.contains("drift=20s"));
        assert!(active.contains("severity=warn"));
        let stopped = format_fleet_heartbeat_metric(LoopState::Stopped, 12, 0, 5, 60, 0);
        assert!(stopped.contains("state=stopped"));
        assert!(stopped.contains("activity=idle"));
    }

    #[test]
    fn fleet_heartbeat_metric_marks_critical_severity_for_large_drift() {
        let metric = format_fleet_heartbeat_metric(LoopState::Running, 20, 0, 5, 60, 61);
        assert!(metric.contains("drift=61s"));
        assert!(metric.contains("severity=critical"));
    }

    #[test]
    fn fleet_heartbeat_metric_contract_includes_required_keys_for_all_states() {
        for state in [LoopState::Running, LoopState::Holding, LoopState::Stopped] {
            let metric = format_fleet_heartbeat_metric(state, 7, 1, 5, 60, 0);
            assert!(metric.contains("fleet-heartbeat"));
            assert!(metric.contains("state="));
            assert!(metric.contains("activity="));
            assert!(metric.contains("progress="));
            assert!(metric.contains("sends_total="));
            assert!(metric.contains("sends_delta="));
            assert!(metric.contains("poll="));
            assert!(metric.contains("window="));
            assert!(metric.contains("drift="));
            assert!(metric.contains("severity="));
        }
    }

    #[test]
    fn log_line_color_same_and_prior_day() {
        let now = OffsetDateTime::parse(
            "2026-02-17T10:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(log_line_color_at("[2026-02-17T01:02:03Z] sent", now), 251);
        assert_eq!(log_line_color_at("[2026-02-16T23:59:59Z] sent", now), 244);
    }

    #[test]
    fn log_line_color_handles_timezone_offsets() {
        let now = OffsetDateTime::parse(
            "2026-02-17T00:30:00+00:00",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(
            log_line_color_at("[2026-02-16T23:30:00-02:00] sent", now),
            251
        );
    }

    #[test]
    fn log_line_color_compact_prefix_still_dimmed() {
        let now = OffsetDateTime::parse(
            "2026-02-17T00:30:00+00:00",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(log_line_color_at("23:11:04 > ai:7.0", now), 249);
    }

    fn fleet_test_record(
        id: &str,
        name: &str,
        state: &str,
        sends: u32,
        version: &str,
    ) -> FleetRunRecord {
        FleetRunRecord {
            id: id.to_string(),
            name: name.to_string(),
            profile_id: name.to_string(),
            pid: 1,
            host: "local".to_string(),
            target: "ai:1.0".to_string(),
            state: state.to_string(),
            sends,
            poll_seconds: 5,
            started_at: "2026-02-17T00:00:00Z".to_string(),
            last_seen: "2026-02-17T00:00:00Z".to_string(),
            version: version.to_string(),
            events: Vec::new(),
            heartbeat_sends_reported: sends,
            heartbeat_reported_at: None,
        }
    }

    fn fleet_listed(record: FleetRunRecord, stale: bool, version_mismatch: bool) -> FleetListedRun {
        let (health_score, health_label) = fleet_health(&record, stale, version_mismatch);
        FleetListedRun {
            record,
            stale,
            version_mismatch,
            health_score,
            health_label,
            needs_attention: stale || version_mismatch || health_score < 70,
        }
    }

    #[test]
    fn fleet_manager_hides_stale_by_default() {
        let active = fleet_listed(
            fleet_test_record("run-1", "alpha", "waiting", 1, LOOPMUX_VERSION),
            false,
            false,
        );
        let stale = fleet_listed(
            fleet_test_record("run-2", "beta", "waiting", 1, LOOPMUX_VERSION),
            true,
            false,
        );

        let hidden = fleet_manager_visible_runs(FleetVisibleArgs {
            runs: &[active.clone(), stale.clone()],
            profile_filter: None,
            show_stale: false,
            mismatch_only: false,
            state_filter: FleetStateFilter::All,
            search_query: "",
            sort_mode: FleetSortMode::LastSeen,
            view_preset: FleetViewPreset::Default,
        });
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].record.id, "run-1");

        let all = fleet_manager_visible_runs(FleetVisibleArgs {
            runs: &[active, stale],
            profile_filter: None,
            show_stale: true,
            mismatch_only: false,
            state_filter: FleetStateFilter::All,
            search_query: "",
            sort_mode: FleetSortMode::LastSeen,
            view_preset: FleetViewPreset::Default,
        });
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn version_mismatch_detection_uses_local_version() {
        assert!(!is_version_mismatch(LOOPMUX_VERSION));
        assert!(is_version_mismatch("0.0.1"));
        assert!(is_version_mismatch(""));
    }

    #[test]
    fn fleet_manager_mismatch_filter_works() {
        let run_match = fleet_listed(
            fleet_test_record("run-1", "alpha", "waiting", 1, LOOPMUX_VERSION),
            false,
            false,
        );
        let run_mismatch = fleet_listed(
            fleet_test_record("run-2", "beta", "holding", 2, "0.0.1"),
            false,
            true,
        );
        let filtered = fleet_manager_visible_runs(FleetVisibleArgs {
            runs: &[run_match, run_mismatch.clone()],
            profile_filter: None,
            show_stale: true,
            mismatch_only: true,
            state_filter: FleetStateFilter::All,
            search_query: "",
            sort_mode: FleetSortMode::LastSeen,
            view_preset: FleetViewPreset::Default,
        });
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].record.id, run_mismatch.record.id);
    }

    #[test]
    fn fleet_manager_state_filter_holding_only() {
        let waiting = fleet_listed(
            fleet_test_record("run-1", "alpha", "waiting", 1, LOOPMUX_VERSION),
            false,
            false,
        );
        let holding = fleet_listed(
            fleet_test_record("run-2", "beta", "holding", 2, LOOPMUX_VERSION),
            false,
            false,
        );
        let filtered = fleet_manager_visible_runs(FleetVisibleArgs {
            runs: &[waiting, holding.clone()],
            profile_filter: None,
            show_stale: true,
            mismatch_only: false,
            state_filter: FleetStateFilter::Holding,
            search_query: "",
            sort_mode: FleetSortMode::LastSeen,
            view_preset: FleetViewPreset::Default,
        });
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].record.id, holding.record.id);
    }

    #[test]
    fn fleet_manager_search_matches_name_or_target() {
        let run = fleet_listed(
            fleet_test_record("run-1", "planner-a", "waiting", 1, LOOPMUX_VERSION),
            false,
            false,
        );
        let by_name = fleet_manager_visible_runs(FleetVisibleArgs {
            runs: std::slice::from_ref(&run),
            profile_filter: None,
            show_stale: true,
            mismatch_only: false,
            state_filter: FleetStateFilter::All,
            search_query: "planner",
            sort_mode: FleetSortMode::LastSeen,
            view_preset: FleetViewPreset::Default,
        });
        assert_eq!(by_name.len(), 1);

        let by_target = fleet_manager_visible_runs(FleetVisibleArgs {
            runs: &[run],
            profile_filter: None,
            show_stale: true,
            mismatch_only: false,
            state_filter: FleetStateFilter::All,
            search_query: "ai:1",
            sort_mode: FleetSortMode::LastSeen,
            view_preset: FleetViewPreset::Default,
        });
        assert_eq!(by_target.len(), 1);
    }

    #[test]
    fn fleet_profile_filter_matches_profile_or_name() {
        let run = fleet_listed(
            fleet_test_record("run-1", "planner-a", "waiting", 1, LOOPMUX_VERSION),
            false,
            false,
        );
        assert!(run_matches_profile_filter(&run, "planner-a"));
        assert!(!run_matches_profile_filter(&run, "docs"));
    }

    #[test]
    fn fleet_status_line_includes_profile_and_search() {
        let line = fleet_status_line(
            FleetViewPreset::Default,
            FleetSortMode::LastSeen,
            FleetStateFilter::All,
            false,
            true,
            "needle",
            Some("docs"),
        );
        assert!(line.contains("search=needle"));
        assert!(line.contains("profile=docs"));
        assert!(line.contains("mismatch=on"));
    }

    #[test]
    fn fleet_run_list_line_includes_mismatch_tag_and_version() {
        let run = fleet_listed(
            fleet_test_record("run-2", "beta", "holding", 2, "0.0.1"),
            false,
            true,
        );
        let line = fleet_run_list_line(&run, true, true, true);
        assert!(line.contains(">[x] beta"));
        assert!(line.contains("[active"));
        assert!(line.contains("mismatch"));
        assert!(line.contains("ver=0.0.1"));
    }

    #[test]
    fn fleet_stop_snippet_uses_run_id() {
        let snippet = fleet_stop_snippet("run-123");
        assert_eq!(snippet, "loopmux runs stop run-123");
    }

    #[test]
    fn fleet_resize_marks_refresh_flags_idempotently() {
        let mut force_full_redraw = false;
        let mut needs_refresh = false;

        for _ in 0..1_000 {
            fleet_mark_resize(&mut force_full_redraw, &mut needs_refresh);
        }

        assert!(force_full_redraw);
        assert!(needs_refresh);
    }

    #[test]
    fn fleet_selection_burst_navigation_wraps_without_panic() {
        let len = 7;
        let mut selected = 0;

        for _ in 0..10_000 {
            selected = fleet_step_selection_right(selected, len);
        }
        assert_eq!(selected, 10_000 % len);

        for _ in 0..10_000 {
            selected = fleet_step_selection_left(selected, len);
        }
        assert_eq!(selected, 0);
    }

    #[test]
    fn fleet_selection_steps_are_safe_for_empty_lists() {
        assert_eq!(fleet_step_selection_left(0, 0), 0);
        assert_eq!(fleet_step_selection_right(0, 0), 0);
        assert_eq!(fleet_step_selection_left(5, 0), 0);
        assert_eq!(fleet_step_selection_right(5, 0), 0);
    }

    #[test]
    fn legacy_fleet_pane_renderer_list_matches_direct_render() {
        let runs = vec![
            fleet_listed(
                fleet_test_record("run-1", "alpha", "running", 2, LOOPMUX_VERSION),
                false,
                false,
            ),
            fleet_listed(
                fleet_test_record("run-2", "beta", "holding", 5, LOOPMUX_VERSION),
                false,
                false,
            ),
        ];
        let selected_ids = std::collections::HashSet::from(["run-2".to_string()]);
        let direct = fleet_run_list_lines(&runs, 4, 1, &selected_ids);
        let via_adapter = LegacyFleetPaneRenderer.render_list_lines(&runs, 4, 1, &selected_ids);
        assert_eq!(via_adapter, direct);
    }

    #[test]
    fn legacy_fleet_pane_renderer_details_matches_direct_render() {
        let selected = fleet_listed(
            fleet_test_record("run-1", "alpha", "running", 2, LOOPMUX_VERSION),
            false,
            false,
        );
        let pending = PendingFleetAction::SingleStop {
            run_id: "run-1".to_string(),
            run_name: "alpha".to_string(),
        };

        let args = FleetDetailRenderArgs {
            selected_run: Some(&selected),
            profile_filter: Some("alpha"),
            show_stale: true,
            mismatch_only: false,
            state_filter: FleetStateFilter::All,
            search_query: "alpha",
            counts: (1, 1, 0, 0),
            sort_mode: FleetSortMode::LastSeen,
            view_preset: FleetViewPreset::Default,
            marked_count: 1,
            pending_action: Some(&pending),
        };
        let direct = fleet_detail_lines(&args);
        let via_adapter = LegacyFleetPaneRenderer.render_detail_lines(args);
        assert_eq!(via_adapter, direct);
    }

    #[test]
    fn run_keymap_contract_includes_legacy_hotkeys() {
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('h'), KeyModifiers::NONE, false, false),
            Some(TuiAction::HoldToggle)
        );
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('p'), KeyModifiers::NONE, false, false),
            Some(TuiAction::Pause)
        );
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('r'), KeyModifiers::NONE, false, false),
            Some(TuiAction::Resume)
        );
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('s'), KeyModifiers::NONE, false, false),
            Some(TuiAction::Stop)
        );
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('q'), KeyModifiers::NONE, false, false),
            Some(TuiAction::Quit)
        );
    }

    #[test]
    fn run_keymap_contract_keeps_prompt_editor_confirmation_keys() {
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('y'), KeyModifiers::NONE, true, true),
            Some(TuiAction::PromptEditorConfirmYes)
        );
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('N'), KeyModifiers::NONE, true, true),
            Some(TuiAction::PromptEditorConfirmNo)
        );
        assert_eq!(
            map_run_tui_key_action(KeyCode::Char('c'), KeyModifiers::NONE, true, false),
            Some(TuiAction::PromptEditorClearHistory)
        );
    }

    #[test]
    fn runs_action_next_maps_to_next_fleet_command() {
        let action = RunsAction::Next {
            target: "run-123".to_string(),
        };
        let (target, command) =
            runs_action_fleet_command(&action).expect("next action should map to a fleet command");
        assert_eq!(target, "run-123");
        assert!(matches!(command, FleetControlCommand::Next));
    }

    #[test]
    fn cli_runs_next_command_path_maps_to_next_fleet_command() {
        let cli = Cli::try_parse_from(["loopmux", "runs", "next", "planner-a"]).unwrap();
        let command = cli.command.expect("command should parse");
        let Command::Runs(runs_args) = command else {
            panic!("expected runs command");
        };
        let action = runs_args.action.expect("runs action should parse");
        let (target, fleet_command) = runs_action_fleet_command(&action)
            .expect("runs next action should produce fleet command");
        assert_eq!(target, "planner-a");
        assert!(matches!(fleet_command, FleetControlCommand::Next));
    }

    #[test]
    fn resolve_ui_mode_prefers_tui_on_interactive_terminal() {
        assert_eq!(resolve_ui_mode(true, false, true), UiMode::Tui);
        assert_eq!(resolve_ui_mode(true, true, true), UiMode::Tui);
    }

    #[test]
    fn resolve_ui_mode_falls_back_when_not_interactive() {
        assert_eq!(resolve_ui_mode(true, false, false), UiMode::Plain);
        assert_eq!(resolve_ui_mode(true, true, false), UiMode::SingleLine);
    }

    #[test]
    fn resolve_ui_mode_respects_single_line_without_tui() {
        assert_eq!(resolve_ui_mode(false, true, false), UiMode::SingleLine);
        assert_eq!(resolve_ui_mode(false, true, true), UiMode::SingleLine);
        assert_eq!(resolve_ui_mode(false, false, false), UiMode::Plain);
    }

    #[test]
    fn fleet_keymap_contract_keeps_navigation_aliases() {
        assert_eq!(
            fleet_control_key(&KeyCode::Left),
            Some(FleetControlKey::MoveLeft)
        );
        assert_eq!(
            fleet_control_key(&KeyCode::Char('<')),
            Some(FleetControlKey::MoveLeft)
        );
        assert_eq!(
            fleet_control_key(&KeyCode::Right),
            Some(FleetControlKey::MoveRight)
        );
        assert_eq!(
            fleet_control_key(&KeyCode::Char('>')),
            Some(FleetControlKey::MoveRight)
        );
        assert_eq!(
            fleet_control_key(&KeyCode::Char('q')),
            Some(FleetControlKey::Quit)
        );
        assert_eq!(
            fleet_control_key(&KeyCode::Esc),
            Some(FleetControlKey::Quit)
        );
    }

    #[test]
    fn fleet_confirmation_message_contracts_are_stable() {
        assert_eq!(
            fleet_single_stop_confirmation("planner-a"),
            "confirm stop planner-a: press Enter, or c to cancel"
        );
        assert_eq!(
            fleet_bulk_confirmation(FleetControlCommand::Hold, 3),
            "confirm bulk hold for 3 run(s): press Enter, or c to cancel"
        );
        assert_eq!(
            fleet_pending_action_cleared_message(),
            "pending action cleared"
        );
    }

    #[test]
    fn arm_bulk_action_contract_uses_marked_runs_and_sorted_names() {
        let runs = vec![
            fleet_listed(
                fleet_test_record("run-2", "zeta", "holding", 1, LOOPMUX_VERSION),
                false,
                false,
            ),
            fleet_listed(
                fleet_test_record("run-1", "alpha", "waiting", 2, LOOPMUX_VERSION),
                false,
                false,
            ),
        ];
        let selected_ids =
            std::collections::HashSet::from(["run-2".to_string(), "run-1".to_string()]);
        let mut message = String::new();

        let pending = arm_bulk_action(
            FleetControlCommand::Stop,
            &selected_ids,
            &runs,
            0,
            &mut message,
        );

        assert_eq!(
            message,
            "confirm bulk stop for 2 run(s): press Enter, or c to cancel"
        );
        let pending = pending.expect("pending bulk action expected");
        match pending {
            PendingFleetAction::Bulk {
                command,
                run_ids,
                run_names,
            } => {
                assert!(matches!(command, FleetControlCommand::Stop));
                assert_eq!(run_names, vec!["alpha".to_string(), "zeta".to_string()]);
                assert_eq!(run_ids, vec!["run-1".to_string(), "run-2".to_string()]);
            }
            _ => panic!("expected bulk pending action"),
        }
    }

    #[test]
    fn arm_bulk_action_contract_requires_selected_target() {
        let runs = Vec::<FleetListedRun>::new();
        let selected_ids = std::collections::HashSet::new();
        let mut message = String::new();

        let pending = arm_bulk_action(
            FleetControlCommand::Renew,
            &selected_ids,
            &runs,
            0,
            &mut message,
        );

        assert!(pending.is_none());
        assert_eq!(message, "no runs selected for bulk action");
    }

    #[test]
    fn plan_hold_action_resume_sets_force_rescan() {
        let plan = plan_hold_action(TuiAction::Resume, false, true).unwrap();
        assert_eq!(plan.transition, HoldTransition::Unchanged);
        assert!(plan.force_rescan);
        assert!(plan.break_wait);
    }

    #[test]
    fn plan_hold_action_toggle_exits_when_holding() {
        let plan = plan_hold_action(TuiAction::HoldToggle, true, true).unwrap();
        assert_eq!(plan.transition, HoldTransition::ExitHolding);
        assert!(plan.force_rescan);
        assert!(plan.break_wait);
    }

    #[test]
    fn plan_hold_action_pause_while_holding_is_stable() {
        let plan = plan_hold_action(TuiAction::Pause, true, true).unwrap();
        assert_eq!(plan.transition, HoldTransition::Unchanged);
        assert!(!plan.force_rescan);
        assert!(!plan.break_wait);
    }

    #[test]
    fn plan_hold_action_toggle_entering_hold_does_not_force_rescan() {
        let plan = plan_hold_action(TuiAction::HoldToggle, false, true).unwrap();
        assert_eq!(plan.transition, HoldTransition::EnterHolding);
        assert!(!plan.force_rescan);
        assert!(!plan.break_wait);
    }

    #[test]
    fn apply_hold_transition_updates_state_consistently() {
        let mut loop_state = LoopState::Running;
        let mut hold_started = None;
        let mut held_total = std::time::Duration::from_secs(0);

        apply_hold_transition(
            HoldTransition::EnterHolding,
            &mut loop_state,
            &mut hold_started,
            &mut held_total,
        );
        assert_eq!(loop_state, LoopState::Holding);
        assert!(hold_started.is_some());

        apply_hold_transition(
            HoldTransition::ExitHolding,
            &mut loop_state,
            &mut hold_started,
            &mut held_total,
        );
        assert_eq!(loop_state, LoopState::Running);
        assert!(hold_started.is_none());
    }

    #[test]
    fn raw_mode_guard_nested_release_enables_once_disables_once() {
        let _test_guard = RAW_MODE_TEST_LOCK.lock().unwrap();
        reset_raw_mode_test_state();

        let mut outer = RawModeGuard::acquire("outer acquire failed").unwrap();
        let mut inner = RawModeGuard::acquire("inner acquire failed").unwrap();

        assert_eq!(RAW_MODE_DEPTH.load(Ordering::SeqCst), 2);
        assert_eq!(RAW_MODE_ENABLE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(RAW_MODE_DISABLE_CALLS.load(Ordering::SeqCst), 0);

        outer.release().unwrap();
        assert_eq!(RAW_MODE_DEPTH.load(Ordering::SeqCst), 1);
        assert_eq!(RAW_MODE_DISABLE_CALLS.load(Ordering::SeqCst), 0);

        inner.release().unwrap();
        assert_eq!(RAW_MODE_DEPTH.load(Ordering::SeqCst), 0);
        assert_eq!(RAW_MODE_DISABLE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn raw_mode_guard_release_is_idempotent() {
        let _test_guard = RAW_MODE_TEST_LOCK.lock().unwrap();
        reset_raw_mode_test_state();

        let mut guard = RawModeGuard::acquire("acquire failed").unwrap();
        guard.release().unwrap();
        guard.release().unwrap();

        assert_eq!(RAW_MODE_DEPTH.load(Ordering::SeqCst), 0);
        assert_eq!(RAW_MODE_ENABLE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(RAW_MODE_DISABLE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn raw_mode_guard_acquire_failure_restores_depth() {
        let _test_guard = RAW_MODE_TEST_LOCK.lock().unwrap();
        reset_raw_mode_test_state();

        RAW_MODE_FAIL_ENABLE.store(true, Ordering::SeqCst);
        let err = match RawModeGuard::acquire("failed to enable raw mode for test") {
            Ok(_) => panic!("expected guard acquire failure"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains("failed to enable raw mode for test"),
            "error should include caller context"
        );
        assert_eq!(RAW_MODE_DEPTH.load(Ordering::SeqCst), 0);
        assert_eq!(RAW_MODE_ENABLE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(RAW_MODE_DISABLE_CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn raw_mode_guard_panic_hook_restores_without_underflow() {
        let _test_guard = RAW_MODE_TEST_LOCK.lock().unwrap();
        reset_raw_mode_test_state();

        let _ = std::panic::catch_unwind(|| {
            let _guard = RawModeGuard::acquire("panic acquire failed").unwrap();
            panic!("simulated panic while raw mode active");
        });

        assert_eq!(RAW_MODE_DEPTH.load(Ordering::SeqCst), 0);
        assert_eq!(RAW_MODE_ENABLE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(RAW_MODE_DISABLE_CALLS.load(Ordering::SeqCst), 1);
    }
}
