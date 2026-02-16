use std::collections::{BTreeMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use crossterm::QueueableCommand;
use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_yaml::Number;
use time::OffsetDateTime;

#[derive(Debug, Parser)]
#[command(name = "loopmux")]
#[command(about = "Loop prompts into tmux panes with triggers and delays.")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a loop against a tmux target.
    Run(RunArgs),
    /// Validate configuration without sending anything.
    Validate(ValidateArgs),
    /// Print a starter YAML config to stdout.
    Init(InitArgs),
    /// Simulate pane output for trigger testing.
    Simulate(SimulateArgs),
}

#[derive(Debug, Parser)]
#[command(
    after_help = "Examples:\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --once\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --exclude \"PROD\"\n  loopmux run --config loop.yaml --duration 2h\n\nDefaults:\n  tail=1 (last non-blank line)\n  poll=5s\n\nDuration units: s, m, h, d, w, mon (30d), y (365d)\n"
)]
struct RunArgs {
    /// Path to the YAML config file.
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    /// Inline prompt (mutually exclusive with --config).
    #[arg(long, conflicts_with = "config", requires = "target")]
    prompt: Option<String>,
    /// Inline trigger regex (requires --prompt).
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    trigger: Option<String>,
    /// Inline exclude regex.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    exclude: Option<String>,
    /// Optional pre block for inline prompt.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    pre: Option<String>,
    /// Optional post block for inline prompt.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    post: Option<String>,
    /// tmux target (session:window.pane), overrides config.
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
    /// Stop after a duration (e.g. 5m, 2h, 1d, 1w, 1mon, 1y).
    #[arg(long)]
    duration: Option<String>,
}

#[derive(Debug, Parser)]
struct ValidateArgs {
    /// Path to the YAML config file.
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    /// tmux target (session:window.pane), overrides config.
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

#[derive(Debug, Deserialize)]
struct Config {
    target: Option<String>,
    iterations: Option<u32>,
    infinite: Option<bool>,
    poll: Option<u64>,
    duration: Option<String>,
    rule_eval: Option<RuleEval>,
    default_action: Option<Action>,
    delay: Option<DelayConfig>,
    rules: Option<Vec<Rule>>,
    logging: Option<LoggingConfig>,
    template_vars: Option<TemplateVars>,
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
    next: Option<String>,
    priority: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct MatchCriteria {
    regex: Option<String>,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run(args) => run(args),
        Command::Validate(args) => validate(args),
        Command::Init(args) => init(args),
        Command::Simulate(args) => simulate(args),
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
    )?;

    if args.dry_run {
        print_validation(&resolved);
        return Ok(());
    }

    run_loop(resolved)
}

fn run_loop(config: ResolvedConfig) -> Result<()> {
    let mut send_count: u32 = 0;
    let max_sends = config.iterations.unwrap_or(u32::MAX);
    let mut last_hash = String::new();
    let mut active_rule: Option<String> = None;
    let mut backoff_state: std::collections::HashMap<String, BackoffState> =
        std::collections::HashMap::new();
    let mut logger = Logger::new(config.logging.clone())?;
    let tui_enabled = config.tui && std::io::stdout().is_terminal();
    let ui_mode = if tui_enabled {
        UiMode::Tui
    } else if config.single_line {
        UiMode::SingleLine
    } else {
        UiMode::Plain
    };
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
        println!("loopmux: running on {}", config.target);
        if config.infinite {
            println!("loopmux: iterations = infinite");
        } else {
            println!("loopmux: iterations = {max_sends}");
        }
        println!("loopmux: started at {start_timestamp}");
    } else if ui_mode == UiMode::Tui {
        if let Some(tui_state) = tui.as_mut() {
            tui_state.push_log(format!(
                "[{}] started target={}",
                start_timestamp, config.target
            ));
        }
    }
    logger.log(LogEvent::started(&config, start_timestamp.clone()))?;

    let deadline = config
        .duration
        .map(|duration| OffsetDateTime::now_utc() + duration);

    while config.infinite || send_count < max_sends {
        if let Some(deadline) = deadline {
            if OffsetDateTime::now_utc() >= deadline {
                if ui_mode == UiMode::Tui {
                    if let Some(tui_state) = tui.as_mut() {
                        let elapsed = format_duration(start, OffsetDateTime::now_utc());
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
                            &elapsed,
                            "",
                        )?;
                    }
                }
                logger.log(LogEvent::stopped(&config, "duration", send_count))?;
                break;
            }
        }
        let output = match capture_pane(&config.target, config.tail) {
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

        if hash != last_hash {
            let rule_matches =
                evaluate_rules(&config, &mut logger, &output, active_rule.as_deref())?;
            if !rule_matches.is_empty() {
                let mut stop_after = false;
                for rule_match in rule_matches {
                    if loop_state == LoopState::Paused {
                        loop_state = LoopState::Paused;
                        break;
                    }
                    if rule_match.rule.next.as_deref() == Some("stop") {
                        stop_after = true;
                    }
                    let action = rule_match
                        .rule
                        .action
                        .as_ref()
                        .unwrap_or(&config.default_action);
                    let prompt = build_prompt(action);
                    let delay = rule_match.rule.delay.as_ref().or(config.delay.as_ref());
                    if let Some(delay) = delay {
                        if ui_mode == UiMode::Tui {
                            loop_state = LoopState::Delay;
                        }
                        let delay_seconds =
                            sleep_for_delay(delay, &rule_match, &mut backoff_state)?;
                        let detail = format!("delay {}s", delay_seconds);
                        logger.log(LogEvent::delay_scheduled(
                            &config,
                            rule_match.rule.id.as_deref(),
                            detail,
                        ))?;
                        if let Some(tui_state) = tui.as_mut() {
                            tui_state.push_log(format!(
                                "[{}] delay rule={} detail=\"delay {}s\"",
                                timestamp_now(),
                                rule_match.rule.id.as_deref().unwrap_or("<unnamed>"),
                                delay_seconds
                            ));
                            tui_state.update(
                                loop_state,
                                &config,
                                send_count,
                                max_sends,
                                rule_match.rule.id.as_deref(),
                                &format_duration(start, OffsetDateTime::now_utc()),
                                "",
                            )?;
                        }
                    }
                    if ui_mode == UiMode::Tui {
                        loop_state = LoopState::Sending;
                    }
                    if let Err(err) = send_prompt(&config.target, &prompt) {
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
                                    rule_match.rule.id.as_deref(),
                                    &format_duration(start, OffsetDateTime::now_utc()),
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
                    last_hash = hash.clone();
                    active_rule = rule_match.rule.next.clone();
                    let now = OffsetDateTime::now_utc();
                    let timestamp = now
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_else(|_| "unknown".into());
                    let elapsed = format_duration(start, now);
                    let status = status_line(
                        &config,
                        send_count,
                        max_sends,
                        rule_match.rule.id.as_deref(),
                        &elapsed,
                    );
                    if ui_mode == UiMode::SingleLine {
                        print!("\r{status}");
                        let _ = std::io::stdout().flush();
                    } else if ui_mode == UiMode::Tui {
                        if let Some(tui_state) = tui.as_mut() {
                            tui_state.push_log(format!(
                                "[{timestamp}] sent rule={} prompt=\"{}\"",
                                rule_match.rule.id.as_deref().unwrap_or("<unnamed>"),
                                truncate_text(&prompt, 80, true)
                            ));
                            tui_state.update(
                                loop_state,
                                &config,
                                send_count,
                                max_sends,
                                rule_match.rule.id.as_deref(),
                                &elapsed,
                                &status,
                            )?;
                        }
                    } else {
                        println!(
                            "[{}/{}] sent via rule {} at {timestamp} (elapsed {elapsed})",
                            send_count,
                            if config.infinite { 0 } else { max_sends },
                            rule_match.rule.id.as_deref().unwrap_or("<unnamed>")
                        );
                        println!("{status}");
                    }
                    logger.log(LogEvent::status(&config, status))?;
                    logger.log(LogEvent::sent(
                        &config,
                        rule_match.rule.id.as_deref(),
                        timestamp,
                        &prompt,
                    ))?;
                    if config.once || (!config.infinite && send_count >= max_sends) {
                        break;
                    }
                }
                if stop_after {
                    if ui_mode == UiMode::Tui {
                        if let Some(tui_state) = tui.as_mut() {
                            tui_state.push_log(format!(
                                "[{}] stopped reason=stop_rule",
                                timestamp_now()
                            ));
                            tui_state.update(
                                LoopState::Stopped,
                                &config,
                                send_count,
                                max_sends,
                                active_rule.as_deref(),
                                &format_duration(start, OffsetDateTime::now_utc()),
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
                            tui_state
                                .push_log(format!("[{}] stopped reason=once", timestamp_now()));
                            tui_state.update(
                                LoopState::Stopped,
                                &config,
                                send_count,
                                max_sends,
                                active_rule.as_deref(),
                                &format_duration(start, OffsetDateTime::now_utc()),
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
                if matches!(config.rule_eval, RuleEval::MultiMatch) {
                    active_rule = None;
                }
            }
        } else if ui_mode == UiMode::Tui {
            loop_state = LoopState::Waiting;
        }

        if ui_mode == UiMode::Tui {
            if let Some(tui_state) = tui.as_mut() {
                if let Some(action) = tui_state.poll_input()? {
                    match action {
                        TuiAction::Pause => loop_state = LoopState::Paused,
                        TuiAction::Resume => loop_state = LoopState::Running,
                        TuiAction::Stop => {
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] stopped reason=manual",
                                    timestamp_now()
                                ));
                                tui_state.update(
                                    LoopState::Stopped,
                                    &config,
                                    send_count,
                                    max_sends,
                                    active_rule.as_deref(),
                                    &format_duration(start, OffsetDateTime::now_utc()),
                                    "",
                                )?;
                            }
                            break;
                        }
                        TuiAction::Next => {
                            last_hash.clear();
                        }
                        TuiAction::Quit => break,
                    }
                }
                let elapsed = format_duration(start, OffsetDateTime::now_utc());
                tui_state.update(
                    loop_state,
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    &elapsed,
                    "",
                )?;
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(config.poll));
    }

    let end = OffsetDateTime::now_utc();
    let elapsed = format_duration(start, end);
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
                &elapsed,
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

fn sleep_for_delay(
    delay: &DelayConfig,
    rule_match: &RuleMatch<'_>,
    backoff_state: &mut std::collections::HashMap<String, BackoffState>,
) -> Result<u64> {
    let seconds = compute_delay_seconds(delay, rule_match, backoff_state)?;
    if seconds > 0 {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
    Ok(seconds)
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
            regex: Some(trigger.clone()),
            contains: None,
            starts_with: None,
        }),
        exclude: args.exclude.as_ref().map(|value| MatchCriteria {
            regex: Some(value.clone()),
            contains: None,
            starts_with: None,
        }),
        action: None,
        delay: None,
        next: None,
        priority: None,
    };

    Ok(Config {
        target: args.target.clone(),
        iterations: args.iterations,
        infinite: None,
        poll: args.poll,
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
    target: String,
    iterations: Option<u32>,
    infinite: bool,
    has_prompt: bool,
    poll: u64,
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
    Paused,
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
    Stop,
    Next,
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
        elapsed: &str,
        _last_status: &str,
    ) -> Result<()> {
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
            elapsed,
        );

        let log_height = if width < 60 { 0 } else { self.max_logs };

        let mut out = std::io::stdout();
        let _ = out.queue(MoveTo(0, 0));
        let _ = out.queue(Clear(ClearType::All));
        let _ = write!(out, "{bar}");

        for idx in 0..log_height {
            let mut line = self
                .logs
                .iter()
                .rev()
                .take(log_height)
                .rev()
                .nth(idx)
                .map(|value| fit_line(value, width as usize, self.style.use_unicode_ellipsis))
                .unwrap_or_else(|| "".to_string());
            if self.style.use_color && self.style.dim_logs && !line.is_empty() {
                let log_prefix = style_prefix(Some(245), None, false);
                line = format!("{log_prefix}{line}\x1B[0m");
            }
            let _ = out.queue(MoveTo(0, (idx + 1) as u16));
            let _ = out.queue(Clear(ClearType::CurrentLine));
            let _ = write!(out, "{line}");
        }

        let footer_row = self.height.saturating_sub(1);
        let footer_summary = if state == LoopState::Stopped {
            Some(render_footer_summary(config, current, total, elapsed))
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
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                return Ok(match code {
                    KeyCode::Char('p') => Some(TuiAction::Pause),
                    KeyCode::Char('r') => Some(TuiAction::Resume),
                    KeyCode::Char('s') => Some(TuiAction::Stop),
                    KeyCode::Char('n') => Some(TuiAction::Next),
                    KeyCode::Char('q') => Some(TuiAction::Quit),
                    _ => None,
                });
            }
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
        format!("p pause{sep_text}r resume{sep_text}s stop{sep_text}n next{sep_text}q quit")
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
            right_parts.push(format!("trg {trigger_text}"));
            right_parts.push(config.target.clone());
        }
        LayoutMode::Standard => {
            right_parts.push(format!("trg {trigger_text}"));
            right_parts.push(format!("last {elapsed}"));
            right_parts.push(config.target.clone());
        }
        LayoutMode::Wide => {
            right_parts.push(format!("trg {trigger_text}"));
            right_parts.push(format!("last {elapsed}"));
            right_parts.push(format!("target {}", config.target));
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
        (LoopState::Paused, IconMode::Nerd) => ("󰏤", "PAUSE"),
        (LoopState::Delay, IconMode::Nerd) => ("󰔟", "DELAY"),
        (LoopState::Error, IconMode::Nerd) => ("󰅚", "ERROR"),
        (LoopState::Stopped, IconMode::Nerd) => ("󰩈", "STOP"),
        (LoopState::Waiting, IconMode::Nerd) => ("󰔟", "WAIT"),
        (LoopState::Sending, IconMode::Nerd) => ("󰐊", "SEND"),
        (LoopState::Running, IconMode::Ascii) => (">", "RUN"),
        (LoopState::Paused, IconMode::Ascii) => ("||", "PAUSE"),
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
    let mut s = text.chars().take(max.saturating_sub(1)).collect::<String>();
    if use_unicode {
        s.push('…');
    } else {
        s.push_str("...");
    }
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
        LoopState::Paused => 179,
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
) -> Result<ResolvedConfig> {
    if let Some(target) = target_override {
        config.target = Some(target);
    }
    if let Some(iterations) = iterations_override {
        config.iterations = Some(iterations);
        config.infinite = Some(false);
    }

    let target = config
        .target
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("target is required"))?
        .to_string();
    let target = if skip_tmux {
        resolve_target_offline(&target)?
    } else {
        resolve_target(&target)?
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

    if !skip_tmux {
        validate_target(&target)?;
        validate_tmux_target(&target)?;
    }

    let tail = tail_override.unwrap_or(1);
    Ok(ResolvedConfig {
        target,
        iterations,
        infinite,
        has_prompt,
        poll,
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
    println!("- target: {}", config.target);
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
    has_text(&criteria.regex) || has_text(&criteria.contains) || has_text(&criteria.starts_with)
}

fn has_text(value: &Option<String>) -> bool {
    value
        .as_ref()
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false)
}

fn validate_target(target: &str) -> Result<()> {
    parse_target(target).map(|_| ())
}

fn validate_tmux_target(target: &str) -> Result<()> {
    let output = std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .context("failed to run tmux -V")?;
    if !output.status.success() {
        bail!("tmux not available on PATH");
    }
    let output = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}:#{window_index}.#{pane_index}",
        ])
        .output()
        .context("failed to run tmux list-panes")?;
    if !output.status.success() {
        bail!("tmux list-panes failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.trim() == target {
            return Ok(());
        }
    }
    bail!("tmux target not found: {target}");
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
            target: config.target.clone(),
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
            target: config.target.clone(),
            rule_id: rule_id.map(|value| value.to_string()),
            detail: Some(prompt.to_string()),
            sends: None,
        }
    }

    fn delay_scheduled(config: &ResolvedConfig, rule_id: Option<&str>, detail: String) -> Self {
        Self {
            event: "delay".to_string(),
            timestamp: String::new(),
            target: config.target.clone(),
            rule_id: rule_id.map(|value| value.to_string()),
            detail: Some(detail),
            sends: None,
        }
    }

    fn stopped(config: &ResolvedConfig, detail: &str, sends: u32) -> Self {
        Self {
            event: "stopped".to_string(),
            timestamp: String::new(),
            target: config.target.clone(),
            rule_id: None,
            detail: Some(detail.to_string()),
            sends: Some(sends),
        }
    }

    fn matched(config: &ResolvedConfig, rule_id: Option<&str>) -> Self {
        Self {
            event: "match".to_string(),
            timestamp: String::new(),
            target: config.target.clone(),
            rule_id: rule_id.map(|value| value.to_string()),
            detail: None,
            sends: None,
        }
    }

    fn error(config: &ResolvedConfig, detail: String) -> Self {
        Self {
            event: "error".to_string(),
            timestamp: String::new(),
            target: config.target.clone(),
            rule_id: None,
            detail: Some(detail),
            sends: None,
        }
    }

    fn status(config: &ResolvedConfig, detail: String) -> Self {
        Self {
            event: "status".to_string(),
            timestamp: String::new(),
            target: config.target.clone(),
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

fn format_duration(start: OffsetDateTime, end: OffsetDateTime) -> String {
    let duration = end - start;
    let total_seconds = duration.whole_seconds().max(0) as u64;
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
        color, icon, reset, config.target, progress, rule, elapsed, reset
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
            next: None,
            priority: None,
        }
    }

    fn match_regex(pattern: &str) -> MatchCriteria {
        MatchCriteria {
            regex: Some(pattern.to_string()),
            contains: None,
            starts_with: None,
        }
    }

    fn match_contains(value: &str) -> MatchCriteria {
        MatchCriteria {
            regex: None,
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
            duration: None,
        };
        assert!(resolve_run_config(&args).is_err());
    }

    #[test]
    fn resolve_run_config_inline_builds_rule() {
        let args = RunArgs {
            config: None,
            prompt: Some("Do it".to_string()),
            trigger: Some("Done".to_string()),
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
            duration: None,
        };
        let config = resolve_run_config(&args).unwrap();
        let resolved =
            resolve_config(config, None, None, true, args.tail, args.once, false, false).unwrap();
        assert_eq!(resolved.tail, 123);
        assert!(resolved.once);
        assert_eq!(resolved.rules.len(), 1);
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
            target: "ai:5.0".to_string(),
            iterations: Some(10),
            infinite: false,
            has_prompt: true,
            rule_eval: RuleEval::FirstMatch,
            rules: Vec::new(),
            delay: None,
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
        );
        assert!(bar.contains("RUN"));
        assert!(bar.contains("5/10"));
        assert!(bar.contains("ai:5.0"));
    }

    #[test]
    fn render_status_bar_standard_truncates_trigger() {
        let config = ResolvedConfig {
            target: "ai:5.0".to_string(),
            iterations: Some(10),
            infinite: false,
            has_prompt: true,
            rule_eval: RuleEval::FirstMatch,
            rules: Vec::new(),
            delay: None,
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
        );
        assert!(bar.contains("trg"));
        assert!(bar.contains("…"));
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
