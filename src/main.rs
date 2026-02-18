use std::collections::{BTreeMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use crossterm::QueueableCommand;
use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_yaml::Number;
use time::OffsetDateTime;

const LOOPMUX_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(name = "loopmux")]
#[command(about = "Loop prompts into tmux panes with triggers and delays.")]
#[command(
    help_template = "{before-help}{name} {version}\n{about-with-newline}\n{usage-heading} {usage}\n\nCommands:\n{subcommands}\nOptions:\n{options}\n{after-help}"
)]
#[command(
    after_help = "Quick orientation:\n  - Runs against tmux pane scope (`all`, `session`, `session:window`, or `session:window.pane`)\n  - Default safety: trigger-edge ON (sends on false->true trigger transitions)\n\nCommon commands:\n  - run: start looping prompts into target panes\n  - validate: check config/scope without sending\n  - init: print starter YAML template\n  - runs: inspect/stop local loopmux processes\n\nTry next:\n  loopmux run --help\n"
)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a loop against a tmux target scope.
    Run(RunArgs),
    /// Validate configuration without sending anything.
    Validate(ValidateArgs),
    /// Print a starter YAML config to stdout.
    Init(InitArgs),
    /// Simulate pane output for trigger testing.
    Simulate(SimulateArgs),
    /// Manage active local loopmux runs.
    Runs(RunsArgs),
}

#[derive(Debug, Parser)]
#[command(
    after_help = concat!(
        "Examples:\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --once\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --exclude \"PROD\"\n  loopmux run --config loop.yaml --duration 2h\n  loopmux run --tui\n\nDefaults:\n  tail=1 (last non-blank line)\n  poll=5s\n  trigger-confirm-seconds=5\n  history-limit=50\n  log-preview-lines=3\n  trigger-edge=on\n  recheck-before-send=on\n\nDuration units: s, m, h, d, w, mon (30d), y (365d)\n\n",
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
    /// Treat --trigger as an exact line match (trimmed comparison).
    #[arg(long, requires = "trigger", conflicts_with = "config")]
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
    /// tmux target scope (session, session:window, or session:window.pane), overrides config.
    #[arg(long, short = 't')]
    target: Option<String>,
    /// Iterations to run, overrides config.
    #[arg(long, short = 'n')]
    iterations: Option<u32>,
    /// Tail lines from tmux capture (default 1).
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    tail: Option<usize>,
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
    /// Fanout mode for matched panes.
    #[arg(long, default_value = "matched")]
    fanout: FanoutMode,
    /// Stop after a duration (e.g. 5m, 2h, 1d, 1w, 1mon, 1y).
    #[arg(long)]
    duration: Option<String>,
    /// Max history entries to keep/show for TUI picker.
    #[arg(long)]
    history_limit: Option<usize>,
    /// Optional run codename (auto-generated when omitted).
    #[arg(long)]
    name: Option<String>,
}

const DEFAULT_HISTORY_LIMIT: usize = 50;
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
    trigger_exact_line: Option<bool>,
    exclude: Option<String>,
    pre: Option<String>,
    post: Option<String>,
    iterations: Option<u32>,
    tail: Option<usize>,
    once: bool,
    poll: Option<u64>,
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
    target: Option<String>,
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
#[command(after_help = concat!("Version: ", env!("CARGO_PKG_VERSION"), "\n"))]
struct RunsArgs {
    #[command(subcommand)]
    action: Option<RunsAction>,
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

#[derive(Debug, Deserialize)]
struct Config {
    target: Option<String>,
    iterations: Option<u32>,
    infinite: Option<bool>,
    poll: Option<u64>,
    trigger_confirm_seconds: Option<u64>,
    log_preview_lines: Option<usize>,
    trigger_edge: Option<bool>,
    recheck_before_send: Option<bool>,
    fanout: Option<FanoutMode>,
    duration: Option<String>,
    rule_eval: Option<RuleEval>,
    default_action: Option<Action>,
    delay: Option<DelayConfig>,
    rules: Option<Vec<Rule>>,
    logging: Option<LoggingConfig>,
    template_vars: Option<TemplateVars>,
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

#[derive(Debug, Deserialize)]
struct Action {
    pre: Option<PromptBlock>,
    prompt: Option<PromptBlock>,
    post: Option<PromptBlock>,
}

type TemplateVars = BTreeMap<String, TemplateValue>;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum TemplateValue {
    String(String),
    Number(Number),
    Bool(bool),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuleEval {
    FirstMatch,
    MultiMatch,
    Priority,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
struct MatchCriteria {
    regex: Option<String>,
    exact_line: Option<String>,
    contains: Option<String>,
    starts_with: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DelayConfig {
    mode: DelayMode,
    value: Option<u64>,
    min: Option<u64>,
    max: Option<u64>,
    jitter: Option<f64>,
    backoff: Option<BackoffConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DelayMode {
    Fixed,
    Range,
    Jitter,
    Backoff,
}

#[derive(Debug, Deserialize)]
struct BackoffConfig {
    base: u64,
    factor: f64,
    max: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct LoggingConfig {
    path: Option<PathBuf>,
    format: Option<LogFormat>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LogFormat {
    Text,
    Jsonl,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FleetRunRecord {
    id: String,
    name: String,
    pid: u32,
    host: String,
    target: String,
    state: String,
    sends: u32,
    poll_seconds: u64,
    started_at: String,
    last_seen: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    events: Vec<FleetRunEvent>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FleetRunEvent {
    timestamp: String,
    kind: String,
    detail: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct FleetControlEnvelope {
    token: String,
    command: FleetControlCommand,
    issued_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum FleetControlCommand {
    Stop,
    Hold,
    Resume,
    Next,
    Renew,
}

struct FleetRunRegistry {
    identity: RunIdentity,
    state_path: PathBuf,
    control_path: PathBuf,
    last_control_token: Option<String>,
}

#[derive(Debug, Clone)]
struct FleetListedRun {
    record: FleetRunRecord,
    stale: bool,
    version_mismatch: bool,
    health_score: u8,
    health_label: &'static str,
    needs_attention: bool,
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
        Command::Run(args) => run(args),
        Command::Validate(args) => validate(args),
        Command::Init(args) => init(args),
        Command::Simulate(args) => simulate(args),
        Command::Runs(args) => runs(args),
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
    let identity = resolve_run_identity(args.name.as_deref());
    let config = resolve_run_config(&args)?;
    let resolved = resolve_config(
        config,
        args.target.clone(),
        args.iterations,
        false,
        args.tail,
        args.once,
        args.single_line,
        args.tui,
        args.no_trigger_edge.then_some(false),
        args.no_recheck_before_send.then_some(false),
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
    match args.action.unwrap_or(RunsAction::Ls) {
        RunsAction::Ls => print_fleet_runs(),
        RunsAction::Tui => run_fleet_manager_tui(),
        RunsAction::Stop { target } => send_fleet_command(&target, FleetControlCommand::Stop),
        RunsAction::Hold { target } => send_fleet_command(&target, FleetControlCommand::Hold),
        RunsAction::Resume { target } => send_fleet_command(&target, FleetControlCommand::Resume),
        RunsAction::Next { target } => send_fleet_command(&target, FleetControlCommand::Next),
        RunsAction::Renew { target } => send_fleet_command(&target, FleetControlCommand::Renew),
    }
}

fn hydrate_run_args_from_history(mut args: RunArgs) -> Result<RunArgs> {
    let needs_history =
        args.tui && args.config.is_none() && args.prompt.is_none() && args.trigger.is_none();
    if !needs_history {
        return Ok(args);
    }

    let entry = select_history_entry(args.history_limit.unwrap_or(DEFAULT_HISTORY_LIMIT))?;
    args.target = Some(entry.target);
    args.prompt = Some(entry.prompt);
    args.trigger = Some(entry.trigger);
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
    if !args.once {
        args.once = entry.once;
    }
    if args.poll.is_none() {
        args.poll = entry.poll;
    }
    if args.trigger_confirm_seconds.is_none() {
        args.trigger_confirm_seconds = entry.trigger_confirm_seconds;
    }
    if args.log_preview_lines.is_none() {
        args.log_preview_lines = entry.log_preview_lines;
    }
    if !args.no_trigger_edge {
        if let Some(trigger_edge) = entry.trigger_edge {
            args.no_trigger_edge = !trigger_edge;
        }
    }
    if !args.no_recheck_before_send {
        if let Some(recheck_before_send) = entry.recheck_before_send {
            args.no_recheck_before_send = !recheck_before_send;
        }
    }
    if args.fanout == FanoutMode::Matched {
        if let Some(fanout) = entry.fanout {
            args.fanout = fanout;
        }
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
        .map(|value| sanitize_run_name(value))
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
    fn new(identity: RunIdentity) -> Result<Self> {
        std::fs::create_dir_all(fleet_state_dir()?)?;
        std::fs::create_dir_all(fleet_control_dir()?)?;
        Ok(Self {
            state_path: fleet_state_path(&identity.id)?,
            control_path: fleet_control_path(&identity.id)?,
            identity,
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
                    if events.len() > 24 {
                        let keep_from = events.len() - 24;
                        events.drain(0..keep_from);
                    }
                    FleetRunRecord {
                        started_at: existing.started_at,
                        events,
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

fn load_fleet_runs() -> Result<Vec<FleetListedRun>> {
    let dir = fleet_state_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)?;
        let Ok(record) = serde_json::from_str::<FleetRunRecord>(&raw) else {
            continue;
        };
        let stale = is_fleet_record_stale(&record);
        let version_mismatch = is_version_mismatch(&record.version);
        let (health_score, health_label) = fleet_health(&record, stale, version_mismatch);
        let needs_attention = stale
            || version_mismatch
            || health_score < 70
            || record.state == "error"
            || record.state == "stopped";
        runs.push(FleetListedRun {
            stale,
            version_mismatch,
            health_score,
            health_label,
            needs_attention,
            record,
        });
    }
    Ok(runs)
}

fn is_version_mismatch(run_version: &str) -> bool {
    run_version.trim().is_empty() || run_version.trim() != LOOPMUX_VERSION
}

fn fleet_health(
    record: &FleetRunRecord,
    stale: bool,
    version_mismatch: bool,
) -> (u8, &'static str) {
    if stale {
        return (20, "critical");
    }

    let mut score: i32 = 100;
    if version_mismatch {
        score -= 25;
    }
    if record.state == "holding" {
        score -= 8;
    }
    if record.state == "error" {
        score -= 35;
    }

    if let Some(age_seconds) = fleet_last_seen_age_seconds(record) {
        let budget = (record.poll_seconds.max(1) * 3 + 5) as i64;
        if age_seconds > budget {
            score -= 25;
        } else if age_seconds > budget / 2 {
            score -= 10;
        }
    } else {
        score -= 20;
    }

    let score = score.clamp(0, 100) as u8;
    let label = if score >= 85 {
        "healthy"
    } else if score >= 65 {
        "watch"
    } else {
        "critical"
    };
    (score, label)
}

fn fleet_last_seen_age_seconds(record: &FleetRunRecord) -> Option<i64> {
    let last_seen = OffsetDateTime::parse(
        &record.last_seen,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()?;
    Some((OffsetDateTime::now_utc() - last_seen).whole_seconds())
}

fn is_fleet_record_stale(record: &FleetRunRecord) -> bool {
    if !pid_alive(record.pid) {
        return true;
    }
    let Ok(last_seen) = OffsetDateTime::parse(
        &record.last_seen,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return true;
    };
    let now = OffsetDateTime::now_utc();
    let age = now - last_seen;
    let max_age = (record.poll_seconds.max(1) * 3 + 5) as i64;
    age.whole_seconds() > max_age
}

fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn fleet_manager_visible_runs(
    runs: &[FleetListedRun],
    show_stale: bool,
    mismatch_only: bool,
    state_filter: FleetStateFilter,
    search_query: &str,
    sort_mode: FleetSortMode,
    view_preset: FleetViewPreset,
) -> Vec<FleetListedRun> {
    let search = search_query.trim().to_ascii_lowercase();
    let mut visible: Vec<FleetListedRun> = runs
        .iter()
        .filter(|run| show_stale || !run.stale)
        .filter(|run| !mismatch_only || run.version_mismatch)
        .filter(|run| state_filter.allows(run))
        .filter(|run| {
            if view_preset == FleetViewPreset::NeedsAttention {
                run.needs_attention
            } else {
                true
            }
        })
        .filter(|run| search.is_empty() || run_matches_query(run, &search))
        .cloned()
        .collect();

    visible.sort_by(|a, b| match sort_mode {
        FleetSortMode::LastSeen => b.record.last_seen.cmp(&a.record.last_seen),
        FleetSortMode::Sends => b.record.sends.cmp(&a.record.sends),
        FleetSortMode::Health => a.health_score.cmp(&b.health_score),
        FleetSortMode::Name => a.record.name.cmp(&b.record.name),
        FleetSortMode::State => a.record.state.cmp(&b.record.state),
    });
    visible
}

fn run_matches_query(run: &FleetListedRun, query: &str) -> bool {
    let version = if run.record.version.is_empty() {
        "unknown"
    } else {
        run.record.version.as_str()
    };
    [
        run.record.name.as_str(),
        run.record.id.as_str(),
        run.record.target.as_str(),
        run.record.state.as_str(),
        version,
    ]
    .iter()
    .any(|value| value.to_ascii_lowercase().contains(query))
}

fn fleet_manager_counts(runs: &[FleetListedRun]) -> (usize, usize, usize, usize) {
    let mut active = 0;
    let mut holding = 0;
    let mut stale = 0;
    let mut mismatch = 0;
    for run in runs {
        if run.stale {
            stale += 1;
        } else {
            active += 1;
        }
        if run.record.state == "holding" {
            holding += 1;
        }
        if run.version_mismatch {
            mismatch += 1;
        }
    }
    (active, holding, stale, mismatch)
}

fn fleet_detail_lines(
    selected_run: Option<&FleetListedRun>,
    show_stale: bool,
    mismatch_only: bool,
    state_filter: FleetStateFilter,
    search_query: &str,
    counts: (usize, usize, usize, usize),
    sort_mode: FleetSortMode,
    view_preset: FleetViewPreset,
    marked_count: usize,
    pending_action: Option<&PendingFleetAction>,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("Details".to_string());
    lines.push(format!(
        "preset={} stale={} mismatch_only={} state={} sort={} search={}",
        view_preset.label(),
        if show_stale { "on" } else { "off" },
        if mismatch_only { "on" } else { "off" },
        state_filter.label(),
        sort_mode.label(),
        if search_query.trim().is_empty() {
            "<none>"
        } else {
            search_query.trim()
        }
    ));
    lines.push(format!(
        "summary active={} holding={} stale={} mismatch={} marked={}",
        counts.0, counts.1, counts.2, counts.3, marked_count
    ));

    if let Some(action) = pending_action {
        match action {
            PendingFleetAction::SingleStop { run_name, .. } => lines.push(format!(
                "pending: stop {} (press Enter to confirm, c to cancel)",
                run_name
            )),
            PendingFleetAction::Bulk {
                command, run_names, ..
            } => {
                lines.push(format!(
                    "pending: bulk {} for {} run(s)",
                    fleet_command_label(*command),
                    run_names.len()
                ));
                lines.push(format!(
                    "targets: {}",
                    truncate_text(&run_names.join(", "), 70, true)
                ));
                lines.push("press Enter to confirm, c to cancel".to_string());
            }
        }
    }
    lines.push(String::new());

    if let Some(run) = selected_run {
        let version = if run.record.version.is_empty() {
            "unknown"
        } else {
            run.record.version.as_str()
        };
        lines.push(format!("name: {}", run.record.name));
        lines.push(format!("id: {}", run.record.id));
        lines.push(format!("pid: {}", run.record.pid));
        lines.push(format!("host: {}", run.record.host));
        lines.push(format!("state: {}", run.record.state));
        lines.push(format!("target: {}", run.record.target));
        lines.push(format!("sends: {}", run.record.sends));
        lines.push(format!(
            "version: {} ({})",
            version,
            if run.version_mismatch {
                "mismatch"
            } else {
                "match"
            }
        ));
        lines.push(format!(
            "health: {} ({}){}",
            run.health_label,
            run.health_score,
            if run.needs_attention {
                " attention"
            } else {
                ""
            }
        ));
        lines.push(format!("started: {}", run.record.started_at));
        lines.push(format!("last_seen: {}", run.record.last_seen));

        lines.push(String::new());
        lines.push("timeline (latest)".to_string());
        if run.record.events.is_empty() {
            lines.push("- no events yet".to_string());
        } else {
            for event in run.record.events.iter().rev().take(6) {
                lines.push(format!(
                    "- {} {} {}",
                    truncate_text(&event.timestamp, 19, false),
                    event.kind,
                    truncate_text(&event.detail, 48, true)
                ));
            }
        }
    } else {
        lines.push("no run selected".to_string());
    }

    lines.push(String::new());
    lines.push("actions".to_string());
    lines.push("space mark/unmark selected run, a clears marks".to_string());
    lines.push("S/H/P/N/U arm bulk stop/hold/resume/next/renew".to_string());
    lines.push("1-4 presets, p cycles presets, o cycles sort".to_string());
    lines.push("/ enter search mode (name/id/target/state/ver)".to_string());
    lines.push("h/r/n/R single control, s safe stop, enter jump/confirm".to_string());
    lines.push("i copy run id, y copy stop snippet, x/v/f filters".to_string());
    lines
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

fn print_fleet_runs() -> Result<()> {
    let mut runs = load_fleet_runs()?;
    runs.sort_by(|a, b| b.record.last_seen.cmp(&a.record.last_seen));
    if runs.is_empty() {
        println!("No local loopmux runs found.");
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
            "- {} ({}) id={} pid={} state={} sends={} target={} version={} ({}) last_seen={}",
            run.record.name,
            stale,
            run.record.id,
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

fn send_fleet_command(target: &str, command: FleetControlCommand) -> Result<()> {
    let run = dispatch_fleet_command(target, command)?;
    println!(
        "Sent {} to {} ({})",
        fleet_command_label(command),
        run.record.name,
        run.record.id
    );
    Ok(())
}

fn dispatch_fleet_command(target: &str, command: FleetControlCommand) -> Result<FleetListedRun> {
    let runs = load_fleet_runs()?;
    if runs.is_empty() {
        bail!("no active local loopmux runs found");
    }
    let run = resolve_fleet_target(target, &runs)?;
    let path = fleet_control_path(&run.record.id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let token = format!(
        "{}-{}",
        OffsetDateTime::now_utc().unix_timestamp_nanos(),
        std::process::id()
    );
    let envelope = FleetControlEnvelope {
        token,
        command,
        issued_at: timestamp_now(),
    };
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&envelope)?)?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(run)
}

fn fleet_command_label(command: FleetControlCommand) -> &'static str {
    match command {
        FleetControlCommand::Stop => "stop",
        FleetControlCommand::Hold => "hold",
        FleetControlCommand::Resume => "resume",
        FleetControlCommand::Next => "next",
        FleetControlCommand::Renew => "renew",
    }
}

fn apply_external_control(
    command: FleetControlCommand,
    loop_state: &mut LoopState,
    hold_started: &mut Option<std::time::Instant>,
    held_total: &mut std::time::Duration,
    send_count: &mut u32,
    last_hash_by_target: &mut std::collections::HashMap<String, String>,
    active_rule: &mut Option<String>,
    active_rule_by_target: &mut std::collections::HashMap<String, Option<String>>,
) -> bool {
    match command {
        FleetControlCommand::Stop => true,
        FleetControlCommand::Hold => {
            if hold_started.is_none() {
                *hold_started = Some(std::time::Instant::now());
            }
            *loop_state = LoopState::Holding;
            false
        }
        FleetControlCommand::Resume => {
            if let Some(started_at) = hold_started.take() {
                *held_total += started_at.elapsed();
            }
            *loop_state = LoopState::Running;
            false
        }
        FleetControlCommand::Next => {
            last_hash_by_target.clear();
            false
        }
        FleetControlCommand::Renew => {
            *send_count = 0;
            last_hash_by_target.clear();
            *active_rule = None;
            active_rule_by_target.clear();
            false
        }
    }
}

fn sleep_with_heartbeat(
    registry: &FleetRunRegistry,
    target: &str,
    state: LoopState,
    sends: u32,
    poll_seconds: u64,
    seconds: u64,
) -> Result<()> {
    if seconds == 0 {
        return Ok(());
    }
    for _ in 0..seconds {
        std::thread::sleep(std::time::Duration::from_secs(1));
        registry.update(target, state, sends, poll_seconds)?;
    }
    Ok(())
}

fn run_fleet_manager_tui() -> Result<()> {
    enable_raw_mode().context("failed to enable raw mode for fleet manager")?;
    let result = run_fleet_manager_tui_inner(false);
    let _ = disable_raw_mode();
    result
}

fn run_fleet_manager_tui_embedded() -> Result<()> {
    run_fleet_manager_tui_inner(true)
}

fn run_fleet_manager_tui_inner(embedded: bool) -> Result<()> {
    let mut selected: usize = 0;
    let mut selected_run_id: Option<String> = None;
    let mut message = String::from("fleet manager ready");
    let mut show_stale = false;
    let mut mismatch_only = false;
    let mut state_filter = FleetStateFilter::All;
    let mut sort_mode = FleetSortMode::LastSeen;
    let mut view_preset = FleetViewPreset::Default;
    let mut search_query = String::new();
    let mut search_mode = false;
    let mut selected_ids: HashSet<String> = HashSet::new();
    let mut pending_action: Option<PendingFleetAction> = None;
    let mut last_lines: Vec<String> = Vec::new();
    let mut force_full_redraw = true;
    let mut last_refresh = std::time::Instant::now() - Duration::from_secs(1);
    let refresh_interval = Duration::from_millis(450);
    let mut needs_refresh = true;

    let mut all_runs: Vec<FleetListedRun> = Vec::new();
    let mut runs: Vec<FleetListedRun> = Vec::new();
    let mut counts = (0, 0, 0, 0);

    loop {
        if needs_refresh || last_refresh.elapsed() >= refresh_interval {
            all_runs = load_fleet_runs()?;
            runs = fleet_manager_visible_runs(
                &all_runs,
                show_stale,
                mismatch_only,
                state_filter,
                &search_query,
                sort_mode,
                view_preset,
            );
            counts = fleet_manager_counts(&all_runs);
            last_refresh = std::time::Instant::now();
            needs_refresh = false;

            let run_ids: HashSet<&str> =
                all_runs.iter().map(|run| run.record.id.as_str()).collect();
            selected_ids.retain(|id| run_ids.contains(id.as_str()));

            if runs.is_empty() {
                selected = 0;
                selected_run_id = None;
                pending_action = None;
            } else if let Some(id) = selected_run_id.as_deref() {
                if let Some(pos) = runs.iter().position(|run| run.record.id == id) {
                    selected = pos;
                } else {
                    selected = selected.min(runs.len() - 1);
                    selected_run_id = Some(runs[selected].record.id.clone());
                }
            } else {
                selected = selected.min(runs.len() - 1);
                selected_run_id = Some(runs[selected].record.id.clone());
            }
        }

        let (width, height) = crossterm::terminal::size().unwrap_or((120, 30));
        let header = format!(
            "loopmux v{} fleet manager | runs={}/{}{}{} | preset={} filter={} sort={} search={} | active={} holding={} stale={} mismatch={} | selected={} | q/esc {}",
            LOOPMUX_VERSION,
            runs.len(),
            all_runs.len(),
            if show_stale { "" } else { " (hide stale)" },
            if mismatch_only {
                " (mismatch only)"
            } else {
                ""
            },
            view_preset.label(),
            state_filter.label(),
            sort_mode.label(),
            if search_query.is_empty() {
                "<none>"
            } else {
                search_query.as_str()
            },
            counts.0,
            counts.1,
            counts.2,
            counts.3,
            if runs.is_empty() { 0 } else { selected + 1 },
            if embedded {
                "return to run"
            } else {
                "quit manager"
            }
        );

        let content_rows = height.saturating_sub(2) as usize;
        let mut lines = Vec::new();
        for (idx, run) in runs.iter().take(content_rows).enumerate() {
            let marker = if idx == selected { ">" } else { " " };
            let selected_mark = if selected_ids.contains(&run.record.id) {
                "[x]"
            } else {
                "[ ]"
            };
            let stale = if run.stale { "stale" } else { "active" };
            let version = if run.record.version.is_empty() {
                "unknown"
            } else {
                run.record.version.as_str()
            };
            let mismatch = if run.version_mismatch { " !" } else { "" };
            let line = format!(
                "{}{} {} [{}{} {}] sends={} ver={} health={}({}) target={}",
                marker,
                selected_mark,
                run.record.name,
                stale,
                mismatch,
                run.record.state,
                run.record.sends,
                version,
                run.health_label,
                run.health_score,
                truncate_text(&run.record.target, 28, true)
            );
            lines.push(line);
        }

        let selected_run = runs.get(selected);
        let details = fleet_detail_lines(
            selected_run,
            show_stale,
            mismatch_only,
            state_filter,
            &search_query,
            counts,
            sort_mode,
            view_preset,
            selected_ids.len(),
            pending_action.as_ref(),
        );

        let footer = format!(
            "<-/> nav · space mark · a clear-mark · p/1-4 presets · o sort · x stale · v mismatch · f state · / search · enter jump/confirm · i id · y stop-cmd · h/r/n/R single · S/H/P/N/U bulk · s arm stop · c cancel · q/esc {} · {}",
            if embedded {
                "return to run"
            } else {
                "quit manager"
            },
            truncate_text(&message, width.saturating_sub(80) as usize, true)
        );

        let split_mode = width >= 120;
        let left_width = ((width as usize) * 58 / 100)
            .max(52)
            .min((width as usize).saturating_sub(20));
        let right_width = (width as usize).saturating_sub(left_width + 1);
        let mut screen_lines = vec![String::new(); height as usize];
        if !screen_lines.is_empty() {
            screen_lines[0] = fit_line(&header, width as usize, true);
        }
        for idx in 0..content_rows {
            let row = idx + 1;
            if row >= screen_lines.len().saturating_sub(1) {
                break;
            }
            if split_mode {
                let left = lines.get(idx).map(|value| value.as_str()).unwrap_or("");
                let right = details.get(idx).map(|value| value.as_str()).unwrap_or("");
                screen_lines[row] = fit_line(
                    &format!(
                        "{} {}",
                        pad_to_width(&fit_line(left, left_width, true), left_width),
                        fit_line(right, right_width, true)
                    ),
                    width as usize,
                    true,
                );
            } else {
                let line = lines.get(idx).map(|value| value.as_str()).unwrap_or("");
                screen_lines[row] = fit_line(line, width as usize, true);
            }
        }
        if height > 0 {
            let footer_row = height.saturating_sub(1) as usize;
            screen_lines[footer_row] = fit_line(&footer, width as usize, true);
        }

        if force_full_redraw || screen_lines != last_lines {
            let mut out = std::io::stdout();
            if force_full_redraw {
                let _ = out.queue(MoveTo(0, 0));
                let _ = out.queue(Clear(ClearType::All));
            }
            for (row, line) in screen_lines.iter().enumerate() {
                if force_full_redraw || last_lines.get(row) != Some(line) {
                    let _ = out.queue(MoveTo(0, row as u16));
                    let _ = out.queue(Clear(ClearType::CurrentLine));
                    let _ = write!(out, "{}", line);
                }
            }
            let _ = out.flush();
            last_lines = screen_lines;
            force_full_redraw = false;
        }

        if event::poll(Duration::from_millis(80)).context("fleet manager poll failed")? {
            match event::read()? {
                Event::Resize(_, _) => {
                    force_full_redraw = true;
                    needs_refresh = true;
                }
                Event::Key(KeyEvent { code, .. }) => {
                    if search_mode {
                        match code {
                            KeyCode::Esc => {
                                search_mode = false;
                                message = "search cancelled".to_string();
                            }
                            KeyCode::Enter => {
                                search_mode = false;
                                message = if search_query.is_empty() {
                                    "search cleared".to_string()
                                } else {
                                    format!("search applied: {}", search_query)
                                };
                            }
                            KeyCode::Backspace => {
                                search_query.pop();
                                selected = 0;
                                selected_run_id = runs.first().map(|run| run.record.id.clone());
                                pending_action = None;
                                message = format!("search: {}", search_query);
                            }
                            KeyCode::Char(c) => {
                                search_query.push(c);
                                selected = 0;
                                selected_run_id = runs.first().map(|run| run.record.id.clone());
                                pending_action = None;
                                message = format!("search: {}", search_query);
                            }
                            _ => {}
                        }
                        needs_refresh = true;
                        continue;
                    }

                    match code {
                        KeyCode::Esc | KeyCode::Char('q') => break,
                        KeyCode::Enter => {
                            if let Some(action) = pending_action.take() {
                                message = apply_pending_fleet_action(&action);
                            } else {
                                message = apply_selected_fleet_jump(&runs, selected);
                            }
                        }
                        KeyCode::Char('<') | KeyCode::Left => {
                            if !runs.is_empty() {
                                selected = if selected == 0 {
                                    runs.len() - 1
                                } else {
                                    selected - 1
                                };
                                selected_run_id = Some(runs[selected].record.id.clone());
                            }
                            pending_action = None;
                        }
                        KeyCode::Char('>') | KeyCode::Right => {
                            if !runs.is_empty() {
                                selected = (selected + 1) % runs.len();
                                selected_run_id = Some(runs[selected].record.id.clone());
                            }
                            pending_action = None;
                        }
                        KeyCode::Char(' ') => {
                            if let Some(run) = runs.get(selected) {
                                if !selected_ids.insert(run.record.id.clone()) {
                                    selected_ids.remove(&run.record.id);
                                }
                                message = format!("marked runs={}", selected_ids.len());
                            } else {
                                message = "no run selected".to_string();
                            }
                            pending_action = None;
                        }
                        KeyCode::Char('a') => {
                            selected_ids.clear();
                            pending_action = None;
                            message = "cleared marked runs".to_string();
                        }
                        KeyCode::Char('x') => {
                            show_stale = !show_stale;
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = if show_stale {
                                "showing stale + active runs".to_string()
                            } else {
                                "showing active runs only".to_string()
                            };
                        }
                        KeyCode::Char('v') => {
                            mismatch_only = !mismatch_only;
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = if mismatch_only {
                                "showing version mismatches only".to_string()
                            } else {
                                "showing all version states".to_string()
                            };
                        }
                        KeyCode::Char('f') => {
                            state_filter = state_filter.next();
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("state filter={}", state_filter.label());
                        }
                        KeyCode::Char('o') => {
                            sort_mode = sort_mode.next();
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("sort={}", sort_mode.label());
                        }
                        KeyCode::Char('p') => {
                            view_preset = view_preset.next();
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('1') => {
                            view_preset = FleetViewPreset::Default;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('2') => {
                            view_preset = FleetViewPreset::NeedsAttention;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('3') => {
                            view_preset = FleetViewPreset::MismatchOnly;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('4') => {
                            view_preset = FleetViewPreset::Holding;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('/') => {
                            search_mode = true;
                            pending_action = None;
                            message = format!("search: {}", search_query);
                        }
                        KeyCode::Char('s') => {
                            if let Some(run) = runs.get(selected) {
                                pending_action = Some(PendingFleetAction::SingleStop {
                                    run_id: run.record.id.clone(),
                                    run_name: run.record.name.clone(),
                                });
                                message = format!(
                                    "confirm stop {}: press Enter, or c to cancel",
                                    run.record.name
                                );
                            } else {
                                message = "no run selected".to_string();
                            }
                        }
                        KeyCode::Char('S') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Stop,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('H') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Hold,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('P') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Resume,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('N') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Next,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('U') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Renew,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('c') => {
                            pending_action = None;
                            message = "pending action cleared".to_string();
                        }
                        KeyCode::Char('i') => {
                            pending_action = None;
                            message = copy_selected_run_id(&runs, selected);
                        }
                        KeyCode::Char('y') => {
                            pending_action = None;
                            message = copy_selected_run_command(&runs, selected);
                        }
                        KeyCode::Char('h') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Hold,
                            );
                        }
                        KeyCode::Char('r') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Resume,
                            );
                        }
                        KeyCode::Char('n') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Next,
                            );
                        }
                        KeyCode::Char('R') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Renew,
                            );
                        }
                        _ => {}
                    }
                    needs_refresh = true;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn apply_view_preset(
    preset: FleetViewPreset,
    show_stale: &mut bool,
    mismatch_only: &mut bool,
    state_filter: &mut FleetStateFilter,
    sort_mode: &mut FleetSortMode,
) {
    match preset {
        FleetViewPreset::Default => {
            *show_stale = false;
            *mismatch_only = false;
            *state_filter = FleetStateFilter::All;
            *sort_mode = FleetSortMode::LastSeen;
        }
        FleetViewPreset::NeedsAttention => {
            *show_stale = true;
            *mismatch_only = false;
            *state_filter = FleetStateFilter::All;
            *sort_mode = FleetSortMode::Health;
        }
        FleetViewPreset::MismatchOnly => {
            *show_stale = true;
            *mismatch_only = true;
            *state_filter = FleetStateFilter::All;
            *sort_mode = FleetSortMode::LastSeen;
        }
        FleetViewPreset::Holding => {
            *show_stale = true;
            *mismatch_only = false;
            *state_filter = FleetStateFilter::Holding;
            *sort_mode = FleetSortMode::Sends;
        }
    }
}

fn arm_bulk_action(
    command: FleetControlCommand,
    selected_ids: &HashSet<String>,
    runs: &[FleetListedRun],
    selected: usize,
    message: &mut String,
) -> Option<PendingFleetAction> {
    let mut targets: Vec<&FleetListedRun> = if selected_ids.is_empty() {
        runs.get(selected).map(|run| vec![run]).unwrap_or_default()
    } else {
        runs.iter()
            .filter(|run| selected_ids.contains(&run.record.id))
            .collect()
    };
    if targets.is_empty() {
        *message = "no runs selected for bulk action".to_string();
        return None;
    }
    targets.sort_by(|a, b| a.record.name.cmp(&b.record.name));
    let run_ids = targets.iter().map(|run| run.record.id.clone()).collect();
    let run_names: Vec<String> = targets.iter().map(|run| run.record.name.clone()).collect();
    *message = format!(
        "confirm bulk {} for {} run(s): press Enter, or c to cancel",
        fleet_command_label(command),
        run_names.len()
    );
    Some(PendingFleetAction::Bulk {
        command,
        run_ids,
        run_names,
    })
}

fn apply_pending_fleet_action(action: &PendingFleetAction) -> String {
    match action {
        PendingFleetAction::SingleStop { run_id, run_name } => {
            match dispatch_fleet_command(run_id, FleetControlCommand::Stop) {
                Ok(_) => format!("sent stop to {}", run_name),
                Err(err) => format!("stop failed: {err}"),
            }
        }
        PendingFleetAction::Bulk {
            command,
            run_ids,
            run_names,
        } => {
            let mut ok = 0usize;
            let mut errors = Vec::new();
            for run_id in run_ids {
                match dispatch_fleet_command(run_id, *command) {
                    Ok(_) => ok += 1,
                    Err(err) => errors.push(format!("{}: {}", run_id, err)),
                }
            }
            if errors.is_empty() {
                format!(
                    "sent {} to {} run(s): {}",
                    fleet_command_label(*command),
                    ok,
                    truncate_text(&run_names.join(", "), 100, true)
                )
            } else {
                format!(
                    "{} sent to {} run(s), {} failed ({})",
                    fleet_command_label(*command),
                    ok,
                    errors.len(),
                    truncate_text(&errors.join("; "), 100, true)
                )
            }
        }
    }
}

fn apply_selected_fleet_command(
    runs: &[FleetListedRun],
    selected: usize,
    command: FleetControlCommand,
) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    match dispatch_fleet_command(&run.record.id, command) {
        Ok(_) => format!(
            "sent {} to {}",
            fleet_command_label(command),
            run.record.name
        ),
        Err(err) => format!("command failed: {err}"),
    }
}

fn apply_selected_fleet_jump(runs: &[FleetListedRun], selected: usize) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    match jump_to_tmux_target(&run.record.target) {
        Ok(()) => format!("jumped to {} ({})", run.record.target, run.record.name),
        Err(err) => format!("jump failed: {err}"),
    }
}

fn copy_selected_run_id(runs: &[FleetListedRun], selected: usize) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    match copy_to_clipboard(&run.record.id) {
        Ok(()) => format!("copied run id: {}", run.record.id),
        Err(err) => format!("copy failed: {err}"),
    }
}

fn copy_selected_run_command(runs: &[FleetListedRun], selected: usize) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    let snippet = fleet_stop_snippet(&run.record.id);
    match copy_to_clipboard(&snippet) {
        Ok(()) => format!("copied snippet: {}", snippet),
        Err(err) => format!("copy failed: {err}"),
    }
}

fn fleet_stop_snippet(run_id: &str) -> String {
    format!("loopmux runs stop {run_id}")
}

fn copy_to_clipboard(value: &str) -> Result<()> {
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to start pbcopy")?;
    let Some(stdin) = child.stdin.as_mut() else {
        bail!("failed to open pbcopy stdin");
    };
    stdin
        .write_all(value.as_bytes())
        .context("failed to write clipboard value")?;
    let status = child.wait().context("failed to wait for pbcopy")?;
    if !status.success() {
        bail!("pbcopy exited with status {status}");
    }
    Ok(())
}

fn jump_to_tmux_target(target: &str) -> Result<()> {
    if std::env::var("TMUX").is_err() {
        bail!("not inside tmux; run this from a tmux client");
    }

    if target == "all sessions/windows/panes" {
        let panes = list_tmux_panes()?;
        let first = panes
            .first()
            .map(|pane| pane.target.clone())
            .ok_or_else(|| anyhow::anyhow!("no tmux panes found to jump"))?;
        return jump_to_tmux_target(&first);
    }

    if let Some(session) = target.strip_suffix(":*.*") {
        let switch_status = std::process::Command::new("tmux")
            .args(["switch-client", "-t", session])
            .status()
            .context("failed to run tmux switch-client")?;
        if !switch_status.success() {
            bail!("tmux switch-client failed for session: {session}");
        }
        return Ok(());
    }

    if let Some(window_target) = target.strip_suffix(".*") {
        let (session, _window) = parse_session_window(window_target)?;
        let switch_status = std::process::Command::new("tmux")
            .args(["switch-client", "-t", session])
            .status()
            .context("failed to run tmux switch-client")?;
        if !switch_status.success() {
            bail!("tmux switch-client failed for session: {session}");
        }
        let window_status = std::process::Command::new("tmux")
            .args(["select-window", "-t", window_target])
            .status()
            .context("failed to run tmux select-window")?;
        if !window_status.success() {
            bail!("tmux select-window failed for {window_target}");
        }
        return Ok(());
    }

    let (session, window, _pane) = parse_target(target)?;
    let window_target = format!("{session}:{window}");
    let switch_status = std::process::Command::new("tmux")
        .args(["switch-client", "-t", session])
        .status()
        .context("failed to run tmux switch-client")?;
    if !switch_status.success() {
        bail!("tmux switch-client failed for session: {session}");
    }
    let window_status = std::process::Command::new("tmux")
        .args(["select-window", "-t", &window_target])
        .status()
        .context("failed to run tmux select-window")?;
    if !window_status.success() {
        bail!("tmux select-window failed for {window_target}");
    }
    let pane_status = std::process::Command::new("tmux")
        .args(["select-pane", "-t", target])
        .status()
        .context("failed to run tmux select-pane")?;
    if !pane_status.success() {
        bail!("tmux select-pane failed for {target}");
    }
    Ok(())
}

fn load_run_history() -> Result<RunHistory> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(RunHistory::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read history file: {}", path.display()))?;
    let history: RunHistory = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse history file: {}", path.display()))?;
    Ok(history)
}

fn save_run_history(history: &RunHistory) -> Result<()> {
    let dir = history_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create history dir: {}", dir.display()))?;
    let path = history_path()?;
    let content = serde_json::to_string_pretty(history).context("failed to serialize history")?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write history file: {}", path.display()))?;
    Ok(())
}

fn history_signature(args: &RunArgs) -> Option<String> {
    let target = args.target.as_ref()?;
    let prompt = args.prompt.as_ref()?;
    let trigger = args.trigger.as_ref()?;
    Some(format!(
        "target={target}|prompt={prompt}|trigger={trigger}|trigger_exact_line={}|exclude={}|pre={}|post={}|iterations={}|tail={}|once={}|poll={}|trigger_confirm_seconds={}|log_preview_lines={}|trigger_edge={}|recheck_before_send={}|fanout={}|duration={}",
        args.trigger_exact_line,
        args.exclude.as_deref().unwrap_or(""),
        args.pre.as_deref().unwrap_or(""),
        args.post.as_deref().unwrap_or(""),
        args.iterations.map(|v| v.to_string()).unwrap_or_default(),
        args.tail.map(|v| v.to_string()).unwrap_or_default(),
        args.once,
        args.poll.map(|v| v.to_string()).unwrap_or_default(),
        args.trigger_confirm_seconds
            .map(|v| v.to_string())
            .unwrap_or_default(),
        args.log_preview_lines
            .map(|v| v.to_string())
            .unwrap_or_default(),
        !args.no_trigger_edge,
        !args.no_recheck_before_send,
        fanout_label(args.fanout),
        args.duration.as_deref().unwrap_or("")
    ))
}

fn store_run_history(args: &RunArgs) -> Result<()> {
    let Some(signature) = history_signature(args) else {
        return Ok(());
    };

    let mut history = load_run_history()?;
    let limit = args.history_limit.unwrap_or(DEFAULT_HISTORY_LIMIT).max(1);
    history.entries.retain(|entry| {
        history_entry_signature(entry)
            .map(|existing| existing != signature)
            .unwrap_or(true)
    });

    history.entries.insert(
        0,
        HistoryEntry {
            last_run: timestamp_now(),
            target: args.target.clone().unwrap_or_default(),
            prompt: args.prompt.clone().unwrap_or_default(),
            trigger: args.trigger.clone().unwrap_or_default(),
            trigger_exact_line: Some(args.trigger_exact_line),
            exclude: args.exclude.clone(),
            pre: args.pre.clone(),
            post: args.post.clone(),
            iterations: args.iterations,
            tail: args.tail,
            once: args.once,
            poll: args.poll,
            trigger_confirm_seconds: args.trigger_confirm_seconds,
            log_preview_lines: args.log_preview_lines,
            trigger_edge: Some(!args.no_trigger_edge),
            recheck_before_send: Some(!args.no_recheck_before_send),
            fanout: Some(args.fanout),
            duration: args.duration.clone(),
        },
    );
    if history.entries.len() > limit {
        history.entries.truncate(limit);
    }
    save_run_history(&history)
}

fn history_entry_signature(entry: &HistoryEntry) -> Option<String> {
    Some(format!(
        "target={}|prompt={}|trigger={}|trigger_exact_line={}|exclude={}|pre={}|post={}|iterations={}|tail={}|once={}|poll={}|trigger_confirm_seconds={}|log_preview_lines={}|trigger_edge={}|recheck_before_send={}|fanout={}|duration={}",
        entry.target,
        entry.prompt,
        entry.trigger,
        entry.trigger_exact_line.unwrap_or(false),
        entry.exclude.as_deref().unwrap_or(""),
        entry.pre.as_deref().unwrap_or(""),
        entry.post.as_deref().unwrap_or(""),
        entry.iterations.map(|v| v.to_string()).unwrap_or_default(),
        entry.tail.map(|v| v.to_string()).unwrap_or_default(),
        entry.once,
        entry.poll.map(|v| v.to_string()).unwrap_or_default(),
        entry
            .trigger_confirm_seconds
            .map(|v| v.to_string())
            .unwrap_or_default(),
        entry
            .log_preview_lines
            .map(|v| v.to_string())
            .unwrap_or_default(),
        entry.trigger_edge.unwrap_or(true),
        entry.recheck_before_send.unwrap_or(true),
        fanout_label(entry.fanout.unwrap_or(FanoutMode::Matched)),
        entry.duration.as_deref().unwrap_or("")
    ))
}

fn select_history_entry(limit: usize) -> Result<HistoryEntry> {
    let history = load_run_history()?;
    if history.entries.is_empty() {
        bail!("no run history found; run a command once before using --tui history picker");
    }

    println!("loopmux history (most recent first):");
    let visible = history
        .entries
        .iter()
        .take(limit.max(1))
        .collect::<Vec<_>>();
    for (idx, entry) in visible.iter().enumerate() {
        let prompt = truncate_text(&entry.prompt, 70, true);
        println!(
            "{}. [{}] target={} trigger={} prompt={}",
            idx + 1,
            entry.last_run,
            entry.target,
            entry.trigger,
            prompt
        );
    }

    loop {
        print!("Select history number (1-{}, q to cancel): ", visible.len());
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("failed to read history selection")?;
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("q") {
            bail!("history selection cancelled");
        }
        let Ok(index) = trimmed.parse::<usize>() else {
            println!("Invalid selection: {trimmed}");
            continue;
        };
        if index == 0 || index > visible.len() {
            println!("Selection out of range: {index}");
            continue;
        }
        return Ok(visible[index - 1].clone());
    }
}

fn run_loop(config: ResolvedConfig, identity: RunIdentity) -> Result<()> {
    let mut send_count: u32 = 0;
    let max_sends = config.iterations.unwrap_or(u32::MAX);
    let mut last_hash_by_target: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut trigger_edge_active: HashSet<String> = HashSet::new();
    let mut trigger_confirm_pending_since: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    let mut active_rule_by_target: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut active_rule: Option<String> = None;
    let mut backoff_state: std::collections::HashMap<String, BackoffState> =
        std::collections::HashMap::new();
    let mut logger = Logger::new(config.logging.clone())?;
    let mut fleet_registry = FleetRunRegistry::new(identity.clone())?;
    let tui_enabled = config.tui && std::io::stdout().is_terminal();
    let ui_mode = if tui_enabled {
        UiMode::Tui
    } else if config.single_line {
        UiMode::SingleLine
    } else {
        UiMode::Plain
    };
    let log_icon_mode = detect_icon_mode();
    let log_use_unicode = supports_unicode();
    let mut loop_state = LoopState::Running;
    let mut tui = if ui_mode == UiMode::Tui {
        Some(TuiState::new(&config)?)
    } else {
        None
    };

    let start = OffsetDateTime::now_utc();
    let start_timestamp = start
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into());
    if ui_mode == UiMode::Plain {
        println!("loopmux: running on {}", config.target_label);
        println!("loopmux: version {}", LOOPMUX_VERSION);
        println!("loopmux: run {} ({})", identity.name, identity.id);
        if config.infinite {
            println!("loopmux: iterations = infinite");
        } else {
            println!("loopmux: iterations = {max_sends}");
        }
        println!("loopmux: started at {start_timestamp}");
    } else if ui_mode == UiMode::Tui {
        if let Some(tui_state) = tui.as_mut() {
            tui_state.push_log(format!(
                "[{}] started target={} run={} ({})",
                start_timestamp, config.target_label, identity.name, identity.id
            ));
        }
    }
    logger.log(LogEvent::started(&config, start_timestamp.clone()))?;
    let run_started = std::time::Instant::now();
    let mut held_total = std::time::Duration::from_secs(0);
    let mut hold_started: Option<std::time::Instant> = None;
    fleet_registry.update(&config.target_label, loop_state, send_count, config.poll)?;

    while config.infinite || send_count < max_sends {
        fleet_registry.update(&config.target_label, loop_state, send_count, config.poll)?;
        let mut force_rescan = false;
        let active_elapsed = effective_elapsed(run_started, held_total, hold_started);
        if let Some(limit) = config.duration {
            if active_elapsed >= limit {
                if ui_mode == UiMode::Tui {
                    if let Some(tui_state) = tui.as_mut() {
                        let elapsed = format_std_duration(active_elapsed);
                        tui_state.push_log(format!(
                            "[{}] stopped reason=duration sends={} elapsed={}",
                            timestamp_now(),
                            send_count,
                            elapsed
                        ));
                        tui_state.update(
                            LoopState::Stopped,
                            &config,
                            send_count,
                            max_sends,
                            active_rule.as_deref(),
                            active_elapsed,
                            "",
                        )?;
                    }
                }
                logger.log(LogEvent::stopped(&config, "duration", send_count))?;
                break;
            }
        }

        if let Some(command) = fleet_registry.consume_control_command()? {
            let stop = apply_external_control(
                command,
                &mut loop_state,
                &mut hold_started,
                &mut held_total,
                &mut send_count,
                &mut last_hash_by_target,
                &mut active_rule,
                &mut active_rule_by_target,
            );
            if let Some(tui_state) = tui.as_mut() {
                tui_state.push_log(format!(
                    "[{}] control command={} source=fleet-manager",
                    timestamp_now(),
                    fleet_command_label(command)
                ));
            }
            logger.log(LogEvent::status(
                &config,
                format!("control command={}", fleet_command_label(command)),
            ))?;
            if stop {
                logger.log(LogEvent::stopped(&config, "external stop", send_count))?;
                break;
            }
        }
        if ui_mode == UiMode::Tui && loop_state == LoopState::Holding {
            let mut open_fleet_manager = false;
            if let Some(tui_state) = tui.as_mut() {
                if let Some(action) = tui_state.poll_input()? {
                    match action {
                        TuiAction::Pause => {}
                        TuiAction::Resume => {
                            if let Some(started_at) = hold_started.take() {
                                held_total += started_at.elapsed();
                            }
                            loop_state = LoopState::Running;
                        }
                        TuiAction::HoldToggle => {
                            if let Some(started_at) = hold_started.take() {
                                held_total += started_at.elapsed();
                                loop_state = LoopState::Running;
                            } else {
                                hold_started = Some(std::time::Instant::now());
                                loop_state = LoopState::Holding;
                            }
                        }
                        TuiAction::Fleet => {
                            open_fleet_manager = true;
                        }
                        TuiAction::Stop => {
                            tui_state
                                .push_log(format!("[{}] stopped reason=manual", timestamp_now()));
                            logger.log(LogEvent::stopped(&config, "manual", send_count))?;
                            tui_state.update(
                                LoopState::Stopped,
                                &config,
                                send_count,
                                max_sends,
                                active_rule.as_deref(),
                                effective_elapsed(run_started, held_total, hold_started),
                                "",
                            )?;
                            break;
                        }
                        TuiAction::Quit => {
                            tui_state
                                .push_log(format!("[{}] stopped reason=quit", timestamp_now()));
                            logger.log(LogEvent::stopped(&config, "quit", send_count))?;
                            break;
                        }
                        TuiAction::Next => {
                            last_hash_by_target.clear();
                            trigger_edge_active.clear();
                            trigger_confirm_pending_since.clear();
                            active_rule = None;
                            active_rule_by_target.clear();
                            backoff_state.clear();
                            loop_state = LoopState::Running;
                            force_rescan = true;
                        }
                        TuiAction::Renew => {
                            send_count = 0;
                            last_hash_by_target.clear();
                            trigger_edge_active.clear();
                            trigger_confirm_pending_since.clear();
                            active_rule = None;
                            active_rule_by_target.clear();
                            backoff_state.clear();
                            tui_state.push_log(format!(
                                "[{}] renewed counter reason=manual",
                                timestamp_now()
                            ));
                        }
                        TuiAction::Redraw => {}
                    }
                }
                tui_state.update(
                    loop_state,
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    effective_elapsed(run_started, held_total, hold_started),
                    "",
                )?;
            }
            if open_fleet_manager {
                if let Err(err) = run_fleet_manager_tui_embedded() {
                    if let Some(tui_state) = tui.as_mut() {
                        tui_state.push_log(format!(
                            "[{}] fleet manager error=\"{}\"",
                            timestamp_now(),
                            truncate_text(&err.to_string(), 100, true)
                        ));
                    }
                }
                if let Some(tui_state) = tui.as_mut() {
                    tui_state
                        .push_log(format!("[{}] returned from fleet manager", timestamp_now()));
                }
                continue;
            }
            if force_rescan {
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        let mut plans: Vec<SendPlan> = Vec::new();
        let mut matched_sources: HashSet<String> = HashSet::new();
        let mut poll_targets: Vec<String> = Vec::new();
        if loop_state != LoopState::Holding {
            let panes = match list_tmux_panes() {
                Ok(value) => value,
                Err(err) => {
                    let detail = err.to_string();
                    logger.log(LogEvent::error(&config, detail))?;
                    return Err(err);
                }
            };
            poll_targets = select_targets_for_scope(&config.target_scope, &panes);
            let mut broadcast_plan_keys: HashSet<String> = HashSet::new();

            for target in &poll_targets {
                let output = match capture_pane(target, config.tail) {
                    Ok(output) => output,
                    Err(err) => {
                        let detail = err.to_string();
                        logger.log(LogEvent::error(&config, detail))?;
                        return Err(err);
                    }
                };
                let output = if config.tail == 1 {
                    last_non_empty_line(&output)
                } else {
                    output
                };
                let hash = hash_output(&output);
                let last_hash = last_hash_by_target.get(target).cloned().unwrap_or_default();
                if should_skip_scan_by_hash(config.trigger_edge, &hash, &last_hash) {
                    continue;
                }

                let active = active_rule_by_target
                    .get(target)
                    .and_then(|value| value.as_deref());
                let rule_matches = evaluate_rules(&config, &mut logger, &output, active)?;

                let matched_edge_keys = rule_matches
                    .iter()
                    .map(|rule_match| trigger_edge_key(target, rule_match))
                    .collect::<HashSet<_>>();
                refresh_trigger_edges_for_target(
                    &mut trigger_edge_active,
                    target,
                    &matched_edge_keys,
                    config.trigger_edge,
                );
                refresh_trigger_confirm_for_target(
                    &mut trigger_confirm_pending_since,
                    target,
                    &matched_edge_keys,
                );

                if rule_matches.is_empty() {
                    continue;
                }

                matched_sources.insert(target.clone());
                for rule_match in rule_matches {
                    let edge_key = trigger_edge_key(target, &rule_match);
                    if !edge_guard_allows(&trigger_edge_active, &edge_key, config.trigger_edge) {
                        continue;
                    }
                    if !confirm_window_elapsed(
                        config.trigger_confirm_seconds,
                        rule_match.rule.confirm_seconds,
                        &edge_key,
                        &mut trigger_confirm_pending_since,
                        std::time::Instant::now(),
                    ) {
                        continue;
                    }

                    let (trigger_preview_lines, trigger_preview) =
                        extract_trigger_preview(&output, config.log_preview_lines, log_use_unicode);

                    let action = rule_match
                        .rule
                        .action
                        .as_ref()
                        .unwrap_or(&config.default_action);
                    let prompt = build_prompt(action);
                    if config.fanout == FanoutMode::Broadcast {
                        let key = format!(
                            "{}|{}",
                            rule_match.rule.id.as_deref().unwrap_or("<unnamed>"),
                            prompt
                        );
                        if !broadcast_plan_keys.insert(key) {
                            continue;
                        }
                    }
                    let delay = rule_match.rule.delay.as_ref().or(config.delay.as_ref());
                    let delay_seconds = if let Some(delay) = delay {
                        Some(compute_delay_seconds(
                            delay,
                            &rule_match,
                            &mut backoff_state,
                        )?)
                    } else {
                        None
                    };
                    plans.push(SendPlan {
                        source_target: target.clone(),
                        rule_id: rule_match.rule.id.clone(),
                        rule_index: rule_match.index,
                        next_rule: rule_match.rule.next.clone(),
                        edge_key,
                        prompt,
                        trigger_preview,
                        trigger_preview_lines,
                        stop_after: rule_match.rule.next.as_deref() == Some("stop"),
                        delay_seconds,
                    });
                }
                if config.trigger_edge {
                    last_hash_by_target.insert(target.clone(), hash);
                }

                if matches!(config.rule_eval, RuleEval::MultiMatch) {
                    active_rule_by_target.insert(target.clone(), None);
                }
            }
        }

        if plans.is_empty() {
            if ui_mode == UiMode::Tui {
                loop_state = LoopState::Waiting;
            }
        } else {
            let mut stop_after = false;
            for plan in plans {
                if loop_state == LoopState::Holding {
                    break;
                }

                if let Some(delay_seconds) = plan.delay_seconds {
                    if delay_seconds > 0 {
                        if ui_mode == UiMode::Tui {
                            loop_state = LoopState::Delay;
                        }
                        let detail = format!("delay {}s", delay_seconds);
                        logger.log(LogEvent::delay_scheduled(
                            &config,
                            plan.rule_id.as_deref(),
                            detail,
                        ))?;
                        if let Some(tui_state) = tui.as_mut() {
                            tui_state.push_log(format!(
                                "[{}] delay rule={} detail=\"delay {}s\"",
                                timestamp_now(),
                                plan.rule_id.as_deref().unwrap_or("<unnamed>"),
                                delay_seconds
                            ));
                            tui_state.update(
                                loop_state,
                                &config,
                                send_count,
                                max_sends,
                                plan.rule_id.as_deref(),
                                effective_elapsed(run_started, held_total, hold_started),
                                "",
                            )?;
                        }
                        sleep_with_heartbeat(
                            &fleet_registry,
                            &config.target_label,
                            loop_state,
                            send_count,
                            config.poll,
                            delay_seconds,
                        )?;
                    }
                }

                let recipients = match config.fanout {
                    FanoutMode::Matched => vec![plan.source_target.clone()],
                    FanoutMode::Broadcast => poll_targets.clone(),
                };
                if recipients.is_empty() {
                    continue;
                }

                let mut sent_any_for_plan = false;
                for target in recipients {
                    if config.recheck_before_send {
                        let output = capture_pane(&target, config.tail)?;
                        let output = if config.tail == 1 {
                            last_non_empty_line(&output)
                        } else {
                            output
                        };
                        let Some(rule) = config.rules.get(plan.rule_index) else {
                            continue;
                        };
                        if !matches_rule(rule, &output)? {
                            let (recheck_preview_lines, recheck_preview) = extract_trigger_preview(
                                &output,
                                config.log_preview_lines,
                                log_use_unicode,
                            );
                            let detail = format!(
                                "suppressed stale trigger target={} rule={} preview={}L {}",
                                target,
                                plan.rule_id.as_deref().unwrap_or("<unnamed>"),
                                recheck_preview_lines,
                                truncate_text(&recheck_preview, 70, log_use_unicode)
                            );
                            logger.log(LogEvent::status(&config, detail.clone()))?;
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] {}",
                                    timestamp_now(),
                                    truncate_text(&detail, 120, log_use_unicode)
                                ));
                            }
                            continue;
                        }
                    }
                    if ui_mode == UiMode::Tui {
                        loop_state = LoopState::Sending;
                    }
                    if let Err(err) = send_prompt(&target, &plan.prompt) {
                        let detail = err.to_string();
                        logger.log(LogEvent::error(&config, detail.clone()))?;
                        if ui_mode == UiMode::Tui {
                            loop_state = LoopState::Error;
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] error detail=\"{}\"",
                                    timestamp_now(),
                                    truncate_text(&detail, 120, true)
                                ));
                                tui_state.update(
                                    loop_state,
                                    &config,
                                    send_count,
                                    max_sends,
                                    plan.rule_id.as_deref(),
                                    effective_elapsed(run_started, held_total, hold_started),
                                    "",
                                )?;
                            }
                        }
                        return Err(err);
                    }
                    if ui_mode == UiMode::Tui {
                        loop_state = LoopState::Running;
                    }
                    send_count = send_count.saturating_add(1);
                    sent_any_for_plan = true;
                    active_rule = plan.next_rule.clone();
                    active_rule_by_target
                        .insert(plan.source_target.clone(), plan.next_rule.clone());
                    let now = OffsetDateTime::now_utc();
                    let timestamp = now
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_else(|_| "unknown".into());
                    let elapsed = format_std_duration(effective_elapsed(
                        run_started,
                        held_total,
                        hold_started,
                    ));
                    let status = status_line(
                        &config,
                        send_count,
                        max_sends,
                        plan.rule_id.as_deref(),
                        &elapsed,
                    );
                    if ui_mode == UiMode::SingleLine {
                        print!("\r{status}");
                        let _ = std::io::stdout().flush();
                    } else if ui_mode == UiMode::Tui {
                        if let Some(tui_state) = tui.as_mut() {
                            tui_state.push_log(compact_sent_log(
                                &timestamp,
                                target.as_str(),
                                plan.rule_id.as_deref(),
                                &plan.trigger_preview,
                                plan.trigger_preview_lines,
                                log_use_unicode,
                                log_icon_mode,
                            ));
                            tui_state.update(
                                loop_state,
                                &config,
                                send_count,
                                max_sends,
                                plan.rule_id.as_deref(),
                                effective_elapsed(run_started, held_total, hold_started),
                                &status,
                            )?;
                        }
                    } else {
                        println!(
                            "[{}/{}] sent target={} via rule {} at {timestamp} (elapsed {elapsed})",
                            send_count,
                            if config.infinite { 0 } else { max_sends },
                            target,
                            plan.rule_id.as_deref().unwrap_or("<unnamed>")
                        );
                        println!("{status}");
                    }
                    logger.log(LogEvent::status(&config, status))?;
                    logger.log(LogEvent::sent(
                        &config,
                        plan.rule_id.as_deref(),
                        timestamp,
                        &format!("target={target} prompt={}", plan.prompt),
                    ))?;

                    if !config.infinite && send_count >= max_sends {
                        break;
                    }
                }
                if config.trigger_edge && sent_any_for_plan {
                    trigger_edge_active.insert(plan.edge_key.clone());
                }
                if plan.stop_after {
                    stop_after = true;
                }
                if config.once || (!config.infinite && send_count >= max_sends) {
                    break;
                }
            }

            if stop_after {
                if ui_mode == UiMode::Tui {
                    if let Some(tui_state) = tui.as_mut() {
                        tui_state
                            .push_log(format!("[{}] stopped reason=stop_rule", timestamp_now()));
                        tui_state.update(
                            LoopState::Stopped,
                            &config,
                            send_count,
                            max_sends,
                            active_rule.as_deref(),
                            effective_elapsed(run_started, held_total, hold_started),
                            "",
                        )?;
                    }
                }
                if ui_mode == UiMode::Plain {
                    println!("loopmux: stopping due to stop rule");
                }
                logger.log(LogEvent::stopped(&config, "stop rule matched", send_count))?;
                break;
            }
            if config.once {
                if ui_mode == UiMode::Tui {
                    if let Some(tui_state) = tui.as_mut() {
                        tui_state.push_log(format!("[{}] stopped reason=once", timestamp_now()));
                        tui_state.update(
                            LoopState::Stopped,
                            &config,
                            send_count,
                            max_sends,
                            active_rule.as_deref(),
                            effective_elapsed(run_started, held_total, hold_started),
                            "",
                        )?;
                    }
                }
                if ui_mode == UiMode::Plain {
                    println!("loopmux: stopping after single send");
                }
                logger.log(LogEvent::stopped(&config, "once", send_count))?;
                break;
            }
            if ui_mode == UiMode::Tui && matched_sources.is_empty() {
                loop_state = LoopState::Waiting;
            }
        }

        if ui_mode == UiMode::Tui {
            let mut open_fleet_manager = false;
            if let Some(tui_state) = tui.as_mut() {
                if let Some(action) = tui_state.poll_input()? {
                    match action {
                        TuiAction::Pause => {
                            if hold_started.is_none() {
                                hold_started = Some(std::time::Instant::now());
                            }
                            loop_state = LoopState::Holding;
                        }
                        TuiAction::Resume => {
                            if let Some(started_at) = hold_started.take() {
                                held_total += started_at.elapsed();
                            }
                            loop_state = LoopState::Running;
                        }
                        TuiAction::HoldToggle => {
                            if let Some(started_at) = hold_started.take() {
                                held_total += started_at.elapsed();
                                loop_state = LoopState::Running;
                            } else {
                                hold_started = Some(std::time::Instant::now());
                                loop_state = LoopState::Holding;
                            }
                        }
                        TuiAction::Fleet => {
                            open_fleet_manager = true;
                        }
                        TuiAction::Stop => {
                            tui_state
                                .push_log(format!("[{}] stopped reason=manual", timestamp_now()));
                            tui_state.update(
                                LoopState::Stopped,
                                &config,
                                send_count,
                                max_sends,
                                active_rule.as_deref(),
                                effective_elapsed(run_started, held_total, hold_started),
                                "",
                            )?;
                            logger.log(LogEvent::stopped(&config, "manual", send_count))?;
                            break;
                        }
                        TuiAction::Next => {
                            last_hash_by_target.clear();
                            trigger_edge_active.clear();
                            trigger_confirm_pending_since.clear();
                            active_rule = None;
                            active_rule_by_target.clear();
                            backoff_state.clear();
                            loop_state = LoopState::Running;
                            force_rescan = true;
                        }
                        TuiAction::Renew => {
                            send_count = 0;
                            last_hash_by_target.clear();
                            trigger_edge_active.clear();
                            trigger_confirm_pending_since.clear();
                            active_rule = None;
                            active_rule_by_target.clear();
                            backoff_state.clear();
                            tui_state.push_log(format!(
                                "[{}] renewed counter reason=manual",
                                timestamp_now()
                            ));
                        }
                        TuiAction::Redraw => {}
                        TuiAction::Quit => {
                            tui_state
                                .push_log(format!("[{}] stopped reason=quit", timestamp_now()));
                            logger.log(LogEvent::stopped(&config, "quit", send_count))?;
                            break;
                        }
                    }
                }
                tui_state.update(
                    loop_state,
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    effective_elapsed(run_started, held_total, hold_started),
                    "",
                )?;
            }
            if open_fleet_manager {
                if let Err(err) = run_fleet_manager_tui_embedded() {
                    if let Some(tui_state) = tui.as_mut() {
                        tui_state.push_log(format!(
                            "[{}] fleet manager error=\"{}\"",
                            timestamp_now(),
                            truncate_text(&err.to_string(), 100, true)
                        ));
                    }
                }
                if let Some(tui_state) = tui.as_mut() {
                    tui_state
                        .push_log(format!("[{}] returned from fleet manager", timestamp_now()));
                }
                continue;
            }
            if force_rescan {
                continue;
            }
        }

        if ui_mode == UiMode::Tui {
            let sleep_until =
                std::time::Instant::now() + std::time::Duration::from_secs(config.poll);
            let mut should_exit_loop = false;
            while std::time::Instant::now() < sleep_until {
                if let Some(tui_state) = tui.as_mut() {
                    if let Some(action) = tui_state.poll_input()? {
                        match action {
                            TuiAction::Pause => {
                                if hold_started.is_none() {
                                    hold_started = Some(std::time::Instant::now());
                                }
                                loop_state = LoopState::Holding;
                            }
                            TuiAction::Resume => {
                                if let Some(started_at) = hold_started.take() {
                                    held_total += started_at.elapsed();
                                }
                                loop_state = LoopState::Running;
                            }
                            TuiAction::HoldToggle => {
                                if let Some(started_at) = hold_started.take() {
                                    held_total += started_at.elapsed();
                                    loop_state = LoopState::Running;
                                } else {
                                    hold_started = Some(std::time::Instant::now());
                                    loop_state = LoopState::Holding;
                                }
                            }
                            TuiAction::Fleet => {
                                if let Err(err) = run_fleet_manager_tui_embedded() {
                                    tui_state.push_log(format!(
                                        "[{}] fleet manager error=\"{}\"",
                                        timestamp_now(),
                                        truncate_text(&err.to_string(), 100, true)
                                    ));
                                }
                                tui_state.push_log(format!(
                                    "[{}] returned from fleet manager",
                                    timestamp_now()
                                ));
                                force_rescan = true;
                                break;
                            }
                            TuiAction::Next => {
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                loop_state = LoopState::Running;
                                force_rescan = true;
                                break;
                            }
                            TuiAction::Renew => {
                                send_count = 0;
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                tui_state.push_log(format!(
                                    "[{}] renewed counter reason=manual",
                                    timestamp_now()
                                ));
                            }
                            TuiAction::Stop => {
                                tui_state.push_log(format!(
                                    "[{}] stopped reason=manual",
                                    timestamp_now()
                                ));
                                logger.log(LogEvent::stopped(&config, "manual", send_count))?;
                                tui_state.update(
                                    LoopState::Stopped,
                                    &config,
                                    send_count,
                                    max_sends,
                                    active_rule.as_deref(),
                                    effective_elapsed(run_started, held_total, hold_started),
                                    "",
                                )?;
                                should_exit_loop = true;
                                break;
                            }
                            TuiAction::Quit => {
                                tui_state
                                    .push_log(format!("[{}] stopped reason=quit", timestamp_now()));
                                logger.log(LogEvent::stopped(&config, "quit", send_count))?;
                                should_exit_loop = true;
                                break;
                            }
                            TuiAction::Redraw => {}
                        }
                    }
                    tui_state.update(
                        loop_state,
                        &config,
                        send_count,
                        max_sends,
                        active_rule.as_deref(),
                        effective_elapsed(run_started, held_total, hold_started),
                        "",
                    )?;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if should_exit_loop {
                break;
            }
            if force_rescan {
                continue;
            }
        } else {
            sleep_with_heartbeat(
                &fleet_registry,
                &config.target_label,
                loop_state,
                send_count,
                config.poll,
                config.poll,
            )?;
        }
    }

    let elapsed = format_std_duration(effective_elapsed(run_started, held_total, hold_started));
    if ui_mode == UiMode::Tui {
        if let Some(tui_state) = tui.as_mut() {
            tui_state.push_log(format!(
                "[{}] stopped reason=completed sends={} elapsed={}",
                timestamp_now(),
                send_count,
                elapsed
            ));
            tui_state.update(
                LoopState::Stopped,
                &config,
                send_count,
                max_sends,
                active_rule.as_deref(),
                effective_elapsed(run_started, held_total, hold_started),
                "",
            )?;
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    }
    logger.log(LogEvent::stopped(&config, "completed", send_count))?;
    if let Some(mut tui_state) = tui {
        tui_state.shutdown()?;
    }
    if ui_mode == UiMode::SingleLine {
        println!();
    }
    println!("loopmux: stopped after {send_count} sends (elapsed {elapsed})");
    Ok(())
}

fn capture_pane(target: &str, lines: usize) -> Result<String> {
    let output = std::process::Command::new("tmux")
        .args(["capture-pane", "-p", "-S"])
        .arg(format!("-{lines}"))
        .args(["-t", target])
        .output()
        .context("failed to capture tmux pane")?;
    if !output.status.success() {
        bail!("tmux capture-pane failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn last_non_empty_line(output: &str) -> String {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

fn send_prompt(target: &str, prompt: &str) -> Result<()> {
    let output = std::process::Command::new("tmux")
        .args(["send-keys", "-t", target, "-l", prompt])
        .output()
        .context("failed to send tmux keys")?;
    if !output.status.success() {
        bail!("tmux send-keys failed");
    }

    let output = std::process::Command::new("tmux")
        .args(["send-keys", "-t", target, "Enter"])
        .output()
        .context("failed to submit tmux keys")?;
    if !output.status.success() {
        bail!("tmux send-keys submit failed");
    }
    Ok(())
}

fn hash_output(output: &str) -> String {
    let mut hash: u64 = 14695981039346656037;
    for byte in output.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:x}")
}

fn select_rules<'a>(
    output: &str,
    rules: &'a [Rule],
    rule_eval: &RuleEval,
    active_rule: Option<&str>,
) -> Result<Vec<RuleMatch<'a>>> {
    let mut candidates = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        if let Some(active) = active_rule {
            if rule.id.as_deref() != Some(active) {
                continue;
            }
        }
        if !matches_rule(rule, output)? {
            continue;
        }
        candidates.push(RuleMatch { rule, index });
        if matches!(rule_eval, RuleEval::FirstMatch) {
            break;
        }
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    match rule_eval {
        RuleEval::FirstMatch => Ok(vec![candidates.remove(0)]),
        RuleEval::MultiMatch => Ok(candidates),
        RuleEval::Priority => {
            let mut best = &candidates[0];
            for candidate in &candidates[1..] {
                let priority = candidate.rule.priority.unwrap_or(0);
                let best_priority = best.rule.priority.unwrap_or(0);
                if priority > best_priority {
                    best = candidate;
                } else if priority == best_priority && candidate.index < best.index {
                    best = candidate;
                }
            }
            Ok(vec![RuleMatch {
                rule: best.rule,
                index: best.index,
            }])
        }
    }
}

fn evaluate_rules<'a>(
    config: &'a ResolvedConfig,
    logger: &mut Logger,
    output: &str,
    active_rule: Option<&str>,
) -> Result<Vec<RuleMatch<'a>>> {
    let matches = select_rules(output, &config.rules, &config.rule_eval, active_rule)?;
    for rule_match in &matches {
        logger.log(LogEvent::matched(config, rule_match.rule.id.as_deref()))?;
    }
    Ok(matches)
}

fn trigger_edge_key(target: &str, rule_match: &RuleMatch<'_>) -> String {
    let rule_id = rule_match.rule.id.as_deref().unwrap_or("<unnamed>");
    format!("{target}|{rule_id}|{}", rule_match.index)
}

fn refresh_trigger_edges_for_target(
    active_edges: &mut HashSet<String>,
    target: &str,
    matched_keys: &HashSet<String>,
    enabled: bool,
) {
    if !enabled {
        return;
    }
    let prefix = format!("{target}|");
    active_edges.retain(|key| !key.starts_with(&prefix) || matched_keys.contains(key));
}

fn edge_guard_allows(active_edges: &HashSet<String>, edge_key: &str, enabled: bool) -> bool {
    !enabled || !active_edges.contains(edge_key)
}

fn refresh_trigger_confirm_for_target(
    pending_since: &mut std::collections::HashMap<String, std::time::Instant>,
    target: &str,
    matched_keys: &HashSet<String>,
) {
    let prefix = format!("{target}|");
    pending_since.retain(|key, _| !key.starts_with(&prefix) || matched_keys.contains(key));
}

fn confirm_window_elapsed(
    global_seconds: u64,
    rule_override_seconds: Option<u64>,
    edge_key: &str,
    pending_since: &mut std::collections::HashMap<String, std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    let seconds = rule_override_seconds.unwrap_or(global_seconds);
    if seconds == 0 {
        pending_since.remove(edge_key);
        return true;
    }

    let wait = std::time::Duration::from_secs(seconds);
    let Some(first_seen) = pending_since.get(edge_key).copied() else {
        pending_since.insert(edge_key.to_string(), now);
        return false;
    };
    if now.duration_since(first_seen) >= wait {
        pending_since.remove(edge_key);
        return true;
    }
    false
}

fn should_skip_scan_by_hash(trigger_edge_enabled: bool, hash: &str, last_hash: &str) -> bool {
    trigger_edge_enabled && hash == last_hash
}

fn extract_trigger_preview(output: &str, max_lines: usize, use_unicode: bool) -> (usize, String) {
    let lines = output
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| truncate_text(line, 60, use_unicode))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return (0, "<empty>".to_string());
    }
    let take = max_lines.max(1).min(lines.len());
    let start = lines.len().saturating_sub(take);
    let sep = if use_unicode { " │ " } else { " | " };
    let preview = lines[start..].join(sep);
    (take, preview)
}

fn compact_timestamp(timestamp: &str) -> String {
    let mut parts = timestamp.split('T');
    let _date = parts.next();
    let Some(time_part) = parts.next() else {
        return timestamp.to_string();
    };
    let time = time_part.trim_end_matches('Z');
    let time = time.split('+').next().unwrap_or(time);
    let time = time.split('-').next().unwrap_or(time);
    truncate_text(time, 12, false)
}

fn compact_sent_log(
    timestamp: &str,
    target: &str,
    rule_id: Option<&str>,
    trigger_preview: &str,
    trigger_preview_lines: usize,
    use_unicode: bool,
    icon_mode: IconMode,
) -> String {
    let rule = rule_id.unwrap_or("-");
    let ts = compact_timestamp(timestamp);
    let use_nerd = use_unicode && icon_mode == IconMode::Nerd;
    let send_icon = if use_nerd { "󰐊" } else { ">" };
    let fold_icon = if use_nerd { "" } else { ">" };
    format!(
        "{ts} {send_icon} {target} {rule} {fold_icon} {}L {}",
        trigger_preview_lines,
        truncate_text(trigger_preview, 70, use_unicode)
    )
}

fn matches_rule(rule: &Rule, output: &str) -> Result<bool> {
    let match_defined = rule.match_.as_ref().map(has_match).unwrap_or(false);
    let matches = if match_defined {
        rule.match_
            .as_ref()
            .map(|criteria| matches_criteria(criteria, output))
            .unwrap_or(Ok(false))?
    } else {
        true
    };
    if !matches {
        return Ok(false);
    }
    if let Some(exclude) = &rule.exclude {
        if matches_criteria(exclude, output)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn matches_criteria(criteria: &MatchCriteria, output: &str) -> Result<bool> {
    if let Some(exact_line) = &criteria.exact_line {
        let expected = exact_line.trim();
        if output.lines().any(|line| line.trim() == expected) {
            return Ok(true);
        }
    }
    if let Some(regex) = &criteria.regex {
        let re = Regex::new(regex).context("invalid regex")?;
        if re.is_match(output) {
            return Ok(true);
        }
    }
    if let Some(contains) = &criteria.contains {
        if output.contains(contains) {
            return Ok(true);
        }
    }
    if let Some(prefix) = &criteria.starts_with {
        if output.starts_with(prefix) {
            return Ok(true);
        }
    }
    Ok(false)
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
    let config = load_config(args.config.as_ref())?;
    let resolved = resolve_config(
        config,
        args.target,
        args.iterations,
        args.skip_tmux,
        None,
        false,
        false,
        false,
        None,
        None,
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

    let Some(prompt) = args.prompt.as_ref() else {
        bail!("--config or --prompt is required");
    };
    let Some(trigger) = args.trigger.as_ref() else {
        bail!("--trigger is required when using --prompt");
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
            regex: if args.trigger_exact_line {
                None
            } else {
                Some(trigger.clone())
            },
            exact_line: if args.trigger_exact_line {
                Some(trigger.clone())
            } else {
                None
            },
            contains: None,
            starts_with: None,
        }),
        exclude: args.exclude.as_ref().map(|value| MatchCriteria {
            regex: Some(value.clone()),
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
        target: args.target.clone(),
        iterations: args.iterations,
        infinite: None,
        poll: args.poll,
        trigger_confirm_seconds: args.trigger_confirm_seconds,
        log_preview_lines: args.log_preview_lines,
        trigger_edge: Some(!args.no_trigger_edge),
        recheck_before_send: Some(!args.no_recheck_before_send),
        fanout: Some(args.fanout),
        duration: args.duration.clone(),
        rule_eval: Some(RuleEval::FirstMatch),
        default_action: Some(default_action),
        delay: None,
        rules: Some(vec![rule]),
        logging: None,
        template_vars: None,
    })
}

#[derive(Debug)]
struct ResolvedConfig {
    target_scope: TargetScope,
    target_label: String,
    iterations: Option<u32>,
    infinite: bool,
    has_prompt: bool,
    poll: u64,
    trigger_confirm_seconds: u64,
    log_preview_lines: usize,
    trigger_edge: bool,
    recheck_before_send: bool,
    fanout: FanoutMode,
    duration: Option<Duration>,
    rule_eval: RuleEval,
    rules: Vec<Rule>,
    delay: Option<DelayConfig>,
    prompt_placeholders: Vec<String>,
    template_vars: Vec<String>,
    default_action: Action,
    logging: LoggingConfigResolved,
    tail: usize,
    once: bool,
    single_line: bool,
    tui: bool,
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
    Redraw,
    Quit,
}

struct TuiState {
    width: u16,
    height: u16,
    icon_mode: IconMode,
    style: StyleConfig,
    logs: Vec<String>,
    max_logs: usize,
}

impl TuiState {
    fn new(_config: &ResolvedConfig) -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        let style = detect_style();
        Ok(Self {
            width,
            height,
            icon_mode: detect_icon_mode(),
            style,
            logs: Vec::new(),
            max_logs: height.saturating_sub(3) as usize,
        })
    }

    fn update(
        &mut self,
        state: LoopState,
        config: &ResolvedConfig,
        current: u32,
        total: u32,
        rule_id: Option<&str>,
        active_elapsed: std::time::Duration,
        _last_status: &str,
    ) -> Result<()> {
        let elapsed = format_std_duration(active_elapsed);
        let remaining_duration = config
            .duration
            .map(|limit| format_std_duration(limit.saturating_sub(active_elapsed)));
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        self.width = width;
        self.height = height;
        self.max_logs = height.saturating_sub(3) as usize;

        let layout = layout_mode(width);
        let bar = render_status_bar(
            state,
            layout,
            self.icon_mode,
            self.style,
            width,
            config,
            current,
            total,
            rule_id,
            &elapsed,
            remaining_duration.as_deref(),
        );

        let log_height = if width < 60 { 0 } else { self.max_logs };

        let mut out = std::io::stdout();
        let _ = out.queue(MoveTo(0, 0));
        let _ = out.queue(Clear(ClearType::All));
        let _ = write!(out, "{bar}");

        for idx in 0..log_height {
            let raw_line = self
                .logs
                .iter()
                .rev()
                .take(log_height)
                .rev()
                .nth(idx)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "".to_string());
            let mut line = fit_line(&raw_line, width as usize, self.style.use_unicode_ellipsis);
            if self.style.use_color && self.style.dim_logs && !line.is_empty() {
                let log_prefix = style_prefix(Some(log_line_color(&raw_line)), None, false);
                line = format!("{log_prefix}{line}\x1B[0m");
            }
            let _ = out.queue(MoveTo(0, (idx + 1) as u16));
            let _ = out.queue(Clear(ClearType::CurrentLine));
            let _ = write!(out, "{line}");
        }

        let footer_row = self.height.saturating_sub(1);
        let footer_summary = if state == LoopState::Stopped {
            Some(render_footer_summary(config, current, total, &elapsed))
        } else {
            None
        };
        let footer = render_footer(self.style, width, footer_summary.as_deref());
        let _ = out.queue(MoveTo(0, footer_row));
        let _ = out.queue(Clear(ClearType::CurrentLine));
        let _ = write!(out, "{footer}");
        let _ = out.flush();
        Ok(())
    }

    fn push_log(&mut self, line: String) {
        self.logs.push(line);
        if self.logs.len() > 500 {
            self.logs.drain(0..self.logs.len().saturating_sub(500));
        }
    }

    fn poll_input(&self) -> Result<Option<TuiAction>> {
        if event::poll(Duration::from_millis(10)).context("poll input failed")? {
            let ev = event::read()?;
            return Ok(match ev {
                Event::Resize(_, _) => Some(TuiAction::Redraw),
                Event::Key(KeyEvent {
                    code, modifiers, ..
                }) => match code {
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        Some(TuiAction::Stop)
                    }
                    KeyCode::Char('p') => Some(TuiAction::Pause),
                    KeyCode::Char('r') => Some(TuiAction::Resume),
                    KeyCode::Char('h') => Some(TuiAction::HoldToggle),
                    KeyCode::Char('f') => Some(TuiAction::Fleet),
                    KeyCode::Char('R') => Some(TuiAction::Renew),
                    KeyCode::Char('s') => Some(TuiAction::Stop),
                    KeyCode::Char('n') => Some(TuiAction::Next),
                    KeyCode::Char('q') => Some(TuiAction::Quit),
                    _ => None,
                },
                _ => None,
            });
        }
        Ok(None)
    }

    fn shutdown(&mut self) -> Result<()> {
        disable_raw_mode().context("failed to disable raw mode")?;
        Ok(())
    }
}

fn layout_mode(width: u16) -> LayoutMode {
    if width <= 80 {
        LayoutMode::Compact
    } else if width <= 120 {
        LayoutMode::Standard
    } else {
        LayoutMode::Wide
    }
}

fn detect_icon_mode() -> IconMode {
    if std::env::var("LOOPMUX_NO_NERD_FONT").is_ok() {
        return IconMode::Ascii;
    }
    IconMode::Nerd
}

fn detect_style() -> StyleConfig {
    let no_color = std::env::var("NO_COLOR").is_ok();
    let term = std::env::var("TERM").unwrap_or_default();
    let color_term = std::env::var("COLORTERM").unwrap_or_default();
    let use_color = !no_color && term != "dumb";
    let use_bg = use_color && (term.contains("256color") || !color_term.is_empty());
    let use_unicode_ellipsis = supports_unicode();
    let dim_logs = std::env::var("LOOPMUX_TUI_BRIGHT_LOGS").is_err();
    StyleConfig {
        use_color,
        use_bg,
        use_unicode_ellipsis,
        dim_logs,
    }
}

fn supports_unicode() -> bool {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_CTYPE"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let locale = locale.to_lowercase();
    locale.contains("utf-8") || locale.contains("utf8")
}

fn render_footer(style: StyleConfig, width: u16, summary: Option<&str>) -> String {
    let sep_text = if style.use_unicode_ellipsis {
        " · "
    } else {
        " . "
    };
    let text = if let Some(summary) = summary {
        format!("stopped{sep_text}{summary}{sep_text}q quit")
    } else {
        format!(
            "h hold/resume (p/r){sep_text}f fleet{sep_text}R renew{sep_text}n next{sep_text}s/^C stop{sep_text}q quit"
        )
    };
    let line = pad_to_width(&text, width as usize);
    if style.use_color {
        let prefix = style_prefix(Some(240), style.use_bg.then_some(235), false);
        format!("{prefix}{line}\x1B[0m")
    } else {
        line
    }
}

fn render_footer_summary(
    config: &ResolvedConfig,
    current: u32,
    total: u32,
    elapsed: &str,
) -> String {
    if config.infinite || total == 0 || total == u32::MAX {
        format!("sends {current} elapsed {elapsed}")
    } else {
        format!("iter {current}/{total} elapsed {elapsed}")
    }
}

fn render_status_bar(
    state: LoopState,
    layout: LayoutMode,
    icon_mode: IconMode,
    style: StyleConfig,
    width: u16,
    config: &ResolvedConfig,
    current: u32,
    total: u32,
    rule_id: Option<&str>,
    elapsed: &str,
    remaining_duration: Option<&str>,
) -> String {
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
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("trg {trigger_text}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(config.target_label.clone());
        }
        LayoutMode::Standard => {
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("trg {trigger_text}"));
            right_parts.push(format!("last {elapsed}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(config.target_label.clone());
        }
        LayoutMode::Wide => {
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("trg {trigger_text}"));
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

fn resolve_config(
    mut config: Config,
    target_override: Option<String>,
    iterations_override: Option<u32>,
    skip_tmux: bool,
    tail_override: Option<usize>,
    once: bool,
    single_line: bool,
    tui: bool,
    trigger_edge_override: Option<bool>,
    recheck_before_send_override: Option<bool>,
) -> Result<ResolvedConfig> {
    if let Some(target) = target_override {
        config.target = Some(target);
    }
    if let Some(iterations) = iterations_override {
        config.iterations = Some(iterations);
        config.infinite = Some(false);
    }

    let (target_scope, target_label) = if skip_tmux {
        resolve_target_scope_offline(config.target.as_deref())?
    } else {
        resolve_target_scope(config.target.as_deref())?
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
    let logging = resolve_logging(config.logging);

    let delay = config.delay;
    if let Some(ref delay) = delay {
        validate_delay(delay)?;
    }

    let poll = config.poll.unwrap_or(5).max(1);
    let trigger_confirm_seconds = config
        .trigger_confirm_seconds
        .unwrap_or(DEFAULT_TRIGGER_CONFIRM_SECONDS);
    let trigger_edge = trigger_edge_override.unwrap_or(config.trigger_edge.unwrap_or(true));
    let recheck_before_send =
        recheck_before_send_override.unwrap_or(config.recheck_before_send.unwrap_or(true));
    let log_preview_lines = config.log_preview_lines.unwrap_or(3).max(1);

    let fanout = config.fanout.unwrap_or(FanoutMode::Matched);

    if !skip_tmux {
        validate_tmux_scope(&target_scope)?;
    }

    let tail = tail_override.unwrap_or(1);
    Ok(ResolvedConfig {
        target_scope,
        target_label,
        iterations,
        infinite,
        has_prompt,
        poll,
        trigger_confirm_seconds,
        log_preview_lines,
        trigger_edge,
        recheck_before_send,
        fanout,
        duration,
        rule_eval,
        rules,
        delay,
        prompt_placeholders,
        template_vars: template_var_keys,
        default_action,
        logging,
        tail,
        once,
        single_line,
        tui,
    })
}

fn print_validation(config: &ResolvedConfig) {
    println!("Validation OK");
    println!("- target: {}", config.target_label);
    if config.infinite {
        println!("- iterations: infinite");
    } else if let Some(iterations) = config.iterations {
        println!("- iterations: {iterations}");
    }
    println!("- prompt: {}", if config.has_prompt { "yes" } else { "no" });
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
    println!("- tail: {}", config.tail);
    println!("- poll: {}s", config.poll);
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
            if let Some(max) = backoff.max {
                if max < backoff.base {
                    bail!("delay.backoff.max must be >= base");
                }
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
}

struct Logger {
    config: LoggingConfigResolved,
    file: Option<std::fs::File>,
}

impl Logger {
    fn new(config: LoggingConfigResolved) -> Result<Self> {
        let file = if let Some(path) = &config.path {
            Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("failed to open log file {}", path.display()))?,
            )
        } else {
            None
        };
        Ok(Self { config, file })
    }

    fn log(&mut self, mut event: LogEvent) -> Result<()> {
        let timestamp = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into());
        if event.timestamp.is_empty() {
            event.timestamp = timestamp;
        }
        match self.config.format {
            LogFormatResolved::Text => self.log_text(&event),
            LogFormatResolved::Jsonl => self.log_json(&event),
        }
    }

    fn log_text(&mut self, event: &LogEvent) -> Result<()> {
        let mut line = format!(
            "[{}] {} target={}",
            event.timestamp, event.event, event.target
        );
        if let Some(rule_id) = event.rule_id.as_ref() {
            line.push_str(&format!(" rule={rule_id}"));
        }
        if let Some(detail) = event.detail.as_ref() {
            let sanitized = detail.replace('"', "'");
            line.push_str(&format!(" detail=\"{}\"", sanitized));
        }
        if let Some(sends) = event.sends {
            line.push_str(&format!(" sends={sends}"));
        }
        line.push('\n');
        self.write_line(&line)
    }

    fn log_json(&mut self, event: &LogEvent) -> Result<()> {
        let value = json!({
            "event": event.event,
            "timestamp": event.timestamp,
            "target": event.target,
            "rule_id": event.rule_id,
            "detail": event.detail,
            "sends": event.sends,
        });
        let mut line = serde_json::to_string(&value).context("failed to serialize log JSON")?;
        line.push('\n');
        self.write_line(&line)
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        if let Some(file) = &mut self.file {
            file.write_all(line.as_bytes())?;
        } else {
            print!("{line}");
        }
        Ok(())
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
    let icon = ">";
    let color = "\u{001B}[32m";
    let reset = "\u{001B}[0m";
    format!(
        "{}{} status:{} target={} progress={} rule={} elapsed={}{}",
        color, icon, reset, config.target_label, progress, rule, elapsed, reset
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
            exact_line: None,
            contains: None,
            starts_with: None,
        }
    }

    fn match_contains(value: &str) -> MatchCriteria {
        MatchCriteria {
            regex: None,
            exact_line: None,
            contains: Some(value.to_string()),
            starts_with: None,
        }
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
            exact_line: Some("<CONTINUE-LOOP>".to_string()),
            contains: None,
            starts_with: None,
        };
        assert!(matches_criteria(&criteria, "foo\n  <CONTINUE-LOOP>  \nbar").unwrap());
        assert!(!matches_criteria(&criteria, "foo <CONTINUE-LOOP> bar").unwrap());
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
            trigger_exact_line: false,
            exclude: None,
            pre: None,
            post: None,
            target: Some("ai:5.0".to_string()),
            iterations: Some(1),
            tail: None,
            once: false,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            name: None,
        };
        assert!(resolve_run_config(&args).is_err());
    }

    #[test]
    fn resolve_run_config_inline_builds_rule() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("Done".to_string()),
            trigger_exact_line: false,
            exclude: Some("PROD".to_string()),
            pre: Some("pre".to_string()),
            post: Some("post".to_string()),
            target: Some("ai:5.0".to_string()),
            iterations: Some(2),
            tail: Some(123),
            once: true,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
            name: None,
        };
        let config = resolve_run_config(&args).unwrap();
        let resolved = resolve_config(
            config, None, None, true, args.tail, args.once, false, false, None, None,
        )
        .unwrap();
        assert_eq!(resolved.tail, 123);
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
    fn resolve_run_config_inline_exact_line_mode() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("<CONTINUE-LOOP>".to_string()),
            trigger_exact_line: true,
            exclude: None,
            pre: None,
            post: None,
            target: Some("ai:5.0".to_string()),
            iterations: Some(2),
            tail: Some(1),
            once: true,
            dry_run: false,
            single_line: false,
            tui: false,
            poll: None,
            trigger_confirm_seconds: None,
            log_preview_lines: None,
            no_trigger_edge: false,
            no_recheck_before_send: false,
            fanout: FanoutMode::Matched,
            duration: None,
            history_limit: None,
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
        let mut active_rule = Some("next".to_string());
        let mut active_rule_by_target = std::collections::HashMap::new();
        active_rule_by_target.insert("ai:1.0".to_string(), Some("next".to_string()));

        let should_stop = apply_external_control(
            FleetControlCommand::Renew,
            &mut loop_state,
            &mut hold_started,
            &mut held_total,
            &mut send_count,
            &mut last_hash_by_target,
            &mut active_rule,
            &mut active_rule_by_target,
        );

        assert!(!should_stop);
        assert_eq!(send_count, 0);
        assert!(last_hash_by_target.is_empty());
        assert!(active_rule.is_none());
        assert!(active_rule_by_target.is_empty());
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

    #[test]
    fn render_status_bar_compact() {
        let config = ResolvedConfig {
            target_scope: TargetScope::Pane("ai:5.0".to_string()),
            target_label: "ai:5.0".to_string(),
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
            tail: 200,
            once: false,
            single_line: false,
            tui: false,
            poll: 5,
            log_preview_lines: 3,
            trigger_edge: true,
            recheck_before_send: true,
            fanout: FanoutMode::Matched,
            duration: None,
        };
        let bar = render_status_bar(
            LoopState::Running,
            LayoutMode::Compact,
            IconMode::Ascii,
            StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: false,
                dim_logs: true,
            },
            80,
            &config,
            5,
            10,
            Some("Concluded"),
            "00:10",
            None,
        );
        assert!(bar.contains("RUN"));
        assert!(bar.contains("5/10"));
        assert!(bar.contains("ai:5.0"));
    }

    #[test]
    fn render_status_bar_standard_truncates_trigger() {
        let config = ResolvedConfig {
            target_scope: TargetScope::Pane("ai:5.0".to_string()),
            target_label: "ai:5.0".to_string(),
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
            tail: 200,
            once: false,
            single_line: false,
            tui: false,
            poll: 5,
            log_preview_lines: 3,
            trigger_edge: true,
            recheck_before_send: true,
            fanout: FanoutMode::Matched,
            duration: None,
        };
        let bar = render_status_bar(
            LoopState::Running,
            LayoutMode::Standard,
            IconMode::Ascii,
            StyleConfig {
                use_color: false,
                use_bg: false,
                use_unicode_ellipsis: true,
                dim_logs: true,
            },
            120,
            &config,
            1,
            10,
            Some("This is a very long trigger string that should truncate"),
            "00:10",
            Some("1m20s"),
        );
        assert!(bar.contains("trg"));
        assert!(bar.contains("rem 1m20s"));
        assert!(bar.contains("…"));
    }

    #[test]
    fn trigger_edge_rearms_after_clear() {
        let mut active = HashSet::new();
        active.insert("ai:7.0|inline|0".to_string());

        let matched_now = HashSet::new();
        refresh_trigger_edges_for_target(&mut active, "ai:7.0", &matched_now, true);
        assert!(!active.contains("ai:7.0|inline|0"));

        active.insert("other:1.0|inline|0".to_string());
        refresh_trigger_edges_for_target(&mut active, "ai:7.0", &matched_now, true);
        assert!(active.contains("other:1.0|inline|0"));
    }

    #[test]
    fn edge_guard_allowance_respects_toggle() {
        let mut active = HashSet::new();
        active.insert("ai:7.0|inline|0".to_string());
        assert!(!edge_guard_allows(&active, "ai:7.0|inline|0", true));
        assert!(edge_guard_allows(&active, "ai:7.0|inline|0", false));
        assert!(edge_guard_allows(&active, "ai:7.0|inline|1", true));
    }

    #[test]
    fn hash_skip_depends_on_trigger_edge_mode() {
        assert!(should_skip_scan_by_hash(true, "same", "same"));
        assert!(!should_skip_scan_by_hash(false, "same", "same"));
        assert!(!should_skip_scan_by_hash(true, "new", "old"));
    }

    #[test]
    fn confirm_window_elapsed_requires_persisted_match() {
        let mut pending = std::collections::HashMap::new();
        let now = std::time::Instant::now();
        assert!(!confirm_window_elapsed(
            5,
            None,
            "ai:7.0|inline|0",
            &mut pending,
            now
        ));
        assert!(!confirm_window_elapsed(
            5,
            Some(3),
            "ai:7.0|inline|0",
            &mut pending,
            now + std::time::Duration::from_secs(2),
        ));
        assert!(confirm_window_elapsed(
            5,
            Some(3),
            "ai:7.0|inline|0",
            &mut pending,
            now + std::time::Duration::from_secs(3),
        ));
    }

    #[test]
    fn confirm_window_elapsed_zero_is_immediate() {
        let mut pending = std::collections::HashMap::new();
        assert!(confirm_window_elapsed(
            5,
            Some(0),
            "ai:7.0|inline|0",
            &mut pending,
            std::time::Instant::now(),
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn truncate_text_respects_ascii_max_width() {
        let truncated = truncate_text("abcdefghijk", 8, false);
        assert_eq!(truncated.chars().count(), 8);
        assert_eq!(truncated, "abcde...");
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

        let hidden = fleet_manager_visible_runs(
            &vec![active.clone(), stale.clone()],
            false,
            false,
            FleetStateFilter::All,
            "",
            FleetSortMode::LastSeen,
            FleetViewPreset::Default,
        );
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].record.id, "run-1");

        let all = fleet_manager_visible_runs(
            &vec![active, stale],
            true,
            false,
            FleetStateFilter::All,
            "",
            FleetSortMode::LastSeen,
            FleetViewPreset::Default,
        );
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
        let filtered = fleet_manager_visible_runs(
            &vec![run_match, run_mismatch.clone()],
            true,
            true,
            FleetStateFilter::All,
            "",
            FleetSortMode::LastSeen,
            FleetViewPreset::Default,
        );
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
        let filtered = fleet_manager_visible_runs(
            &vec![waiting, holding.clone()],
            true,
            false,
            FleetStateFilter::Holding,
            "",
            FleetSortMode::LastSeen,
            FleetViewPreset::Default,
        );
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
        let by_name = fleet_manager_visible_runs(
            &vec![run.clone()],
            true,
            false,
            FleetStateFilter::All,
            "planner",
            FleetSortMode::LastSeen,
            FleetViewPreset::Default,
        );
        assert_eq!(by_name.len(), 1);

        let by_target = fleet_manager_visible_runs(
            &vec![run],
            true,
            false,
            FleetStateFilter::All,
            "ai:1",
            FleetSortMode::LastSeen,
            FleetViewPreset::Default,
        );
        assert_eq!(by_target.len(), 1);
    }

    #[test]
    fn fleet_stop_snippet_uses_run_id() {
        let snippet = fleet_stop_snippet("run-123");
        assert_eq!(snippet, "loopmux runs stop run-123");
    }
}

fn collect_template_placeholders(
    default_action: &Action,
    rules: &Option<Vec<Rule>>,
) -> Vec<String> {
    let mut vars = HashSet::new();
    collect_action_placeholders(default_action, &mut vars);
    if let Some(rules) = rules {
        for rule in rules {
            if let Some(action) = &rule.action {
                collect_action_placeholders(action, &mut vars);
            }
        }
    }
    let mut values: Vec<String> = vars.into_iter().collect();
    values.sort();
    values
}

fn collect_action_placeholders(action: &Action, vars: &mut HashSet<String>) {
    collect_prompt_block_placeholders(action.pre.as_ref(), vars);
    collect_prompt_block_placeholders(action.prompt.as_ref(), vars);
    collect_prompt_block_placeholders(action.post.as_ref(), vars);
}

fn collect_prompt_block_placeholders(block: Option<&PromptBlock>, vars: &mut HashSet<String>) {
    let Some(block) = block else {
        return;
    };
    match block {
        PromptBlock::Single(text) => extract_placeholders(text, vars),
        PromptBlock::Multi(items) => {
            for item in items {
                extract_placeholders(item, vars);
            }
        }
    }
}

fn extract_placeholders(text: &str, vars: &mut HashSet<String>) {
    let mut remaining = text;
    while let Some(start) = remaining.find("{{") {
        if let Some(end) = remaining[start + 2..].find("}}") {
            let raw = &remaining[start + 2..start + 2 + end];
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                vars.insert(trimmed.to_string());
            }
            remaining = &remaining[start + 2 + end + 2..];
        } else {
            break;
        }
    }
}

fn find_missing_vars(required: &[String], available: &TemplateVars) -> Vec<String> {
    let mut missing = Vec::new();
    for key in required {
        if !available.contains_key(key) {
            missing.push(key.clone());
        }
    }
    missing
}

fn default_template() -> String {
    let template = r#"target: "ai:5.0"
iterations: 10
poll: 5
trigger_confirm_seconds: 5
log_preview_lines: 3
trigger_edge: true
recheck_before_send: true
duration: 2h

template_vars:
  project: loopmux

rule_eval: first_match

default_action:
  pre: "Keep context on UX simplification."
  prompt: "Do the next iteration."
  post: "Run lint/tests; fix failures."

delay:
  mode: range
  min: 5
  max: 120

rules:
  - id: success-path
    match:
      regex: "(All tests passed|LGTM)"
    exclude:
      regex: "PROD"
    action:
      prompt: "Continue with next iteration."
    next: review-path

  - id: failure-path
    match:
      regex: "(FAIL|Error|Exception)"
    action:
      pre: "Fix the errors before proceeding."
      prompt: "Repair and re-run tests."
      post: "Summarize fixes."
    next: success-path
"#;
    template.to_string()
}
