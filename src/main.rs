use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
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
}

#[derive(Debug, Parser)]
#[command(
    after_help = "Lean mode:\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --once\n  loopmux run -t ai:5.0 -n 5 --prompt \"Do the next iteration.\" --trigger \"Concluded|What is next\" --exclude \"PROD\"\n"
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
    /// Tail lines from tmux capture (default 200).
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    tail: Option<usize>,
    /// Run a single send and exit.
    #[arg(long, requires = "prompt", conflicts_with = "config")]
    once: bool,
    /// Validate config and tmux target without sending.
    #[arg(long)]
    dry_run: bool,
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

#[derive(Debug, Deserialize)]
struct Config {
    target: Option<String>,
    iterations: Option<u32>,
    infinite: Option<bool>,
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
    }
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

    let start = OffsetDateTime::now_utc();
    let start_timestamp = start
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into());
    println!("loopmux: running on {}", config.target);
    if config.infinite {
        println!("loopmux: iterations = infinite");
    } else {
        println!("loopmux: iterations = {max_sends}");
    }
    println!("loopmux: started at {start_timestamp}");
    logger.log(LogEvent::started(&config, start_timestamp.clone()))?;

    while config.infinite || send_count < max_sends {
        let output = match capture_pane(&config.target, config.tail) {
            Ok(output) => output,
            Err(err) => {
                let detail = err.to_string();
                logger.log(LogEvent::error(&config, detail))?;
                return Err(err);
            }
        };
        let hash = hash_output(&output);

        if hash != last_hash {
            let rule_matches =
                evaluate_rules(&config, &mut logger, &output, active_rule.as_deref())?;
            if !rule_matches.is_empty() {
                let mut stop_after = false;
                for rule_match in rule_matches {
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
                        let delay_seconds =
                            sleep_for_delay(delay, &rule_match, &mut backoff_state)?;
                        let detail = format!("delay {}s", delay_seconds);
                        logger.log(LogEvent::delay_scheduled(
                            &config,
                            rule_match.rule.id.as_deref(),
                            detail,
                        ))?;
                    }
                    if let Err(err) = send_prompt(&config.target, &prompt) {
                        let detail = err.to_string();
                        logger.log(LogEvent::error(&config, detail))?;
                        return Err(err);
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
                    println!(
                        "[{}/{}] sent via rule {} at {timestamp} (elapsed {elapsed})",
                        send_count,
                        if config.infinite { 0 } else { max_sends },
                        rule_match.rule.id.as_deref().unwrap_or("<unnamed>")
                    );
                    println!("{status}");
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
                    println!("loopmux: stopping due to stop rule");
                    logger.log(LogEvent::stopped(&config, "stop rule matched", send_count))?;
                    break;
                }
                if config.once {
                    println!("loopmux: stopping after single send");
                    logger.log(LogEvent::stopped(&config, "once", send_count))?;
                    break;
                }
                if matches!(config.rule_eval, RuleEval::MultiMatch) {
                    active_rule = None;
                }
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }

    let end = OffsetDateTime::now_utc();
    let elapsed = format_duration(start, end);
    println!("loopmux: stopped after {send_count} sends (elapsed {elapsed})");
    logger.log(LogEvent::stopped(&config, "completed", send_count))?;
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

fn send_prompt(target: &str, prompt: &str) -> Result<()> {
    let output = std::process::Command::new("tmux")
        .args(["send-keys", "-t", target, prompt, "Enter"])
        .output()
        .context("failed to send tmux keys")?;
    if !output.status.success() {
        bail!("tmux send-keys failed");
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
    rule_eval: RuleEval,
    rules: Vec<Rule>,
    delay: Option<DelayConfig>,
    prompt_placeholders: Vec<String>,
    template_vars: Vec<String>,
    default_action: Action,
    logging: LoggingConfigResolved,
    tail: usize,
    once: bool,
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

    let infinite = config.infinite.unwrap_or(false);
    let iterations = config.iterations;
    if infinite && iterations.is_some() {
        bail!("iterations must be omitted when infinite is true");
    }
    if !infinite && iterations.unwrap_or(0) == 0 {
        bail!("iterations must be > 0 unless infinite is true");
    }

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

    if !skip_tmux {
        validate_target(&target)?;
        validate_tmux_target(&target)?;
    }

    let tail = tail_override.unwrap_or(200);
    Ok(ResolvedConfig {
        target,
        iterations,
        infinite,
        has_prompt,
        rule_eval,
        rules,
        delay,
        prompt_placeholders,
        template_vars: template_var_keys,
        default_action,
        logging,
        tail,
        once,
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
    println!("- once: {}", if config.once { "yes" } else { "no" });
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
    Ok(())
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
