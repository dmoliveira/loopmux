use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_yaml::Number;

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
struct RunArgs {
    /// Path to the YAML config file.
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    /// tmux target (session:window.pane), overrides config.
    #[arg(long, short = 't')]
    target: Option<String>,
    /// Iterations to run, overrides config.
    #[arg(long, short = 'n')]
    iterations: Option<u32>,
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
#[serde(untagged)]
enum PromptBlock {
    Single(String),
    Multi(Vec<String>),
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
    let config = load_config(args.config.as_ref())?;
    let resolved = resolve_config(config, args.target, args.iterations)?;

    if args.dry_run {
        print_validation(&resolved);
        return Ok(());
    }

    println!("loopmux: run is not implemented yet (dry-run only).");
    print_validation(&resolved);
    Ok(())
}

fn validate(args: ValidateArgs) -> Result<()> {
    let config = load_config(args.config.as_ref())?;
    let resolved = resolve_config(config, args.target, args.iterations)?;
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
}

fn resolve_config(
    mut config: Config,
    target_override: Option<String>,
    iterations_override: Option<u32>,
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

    let has_prompt = config
        .default_action
        .as_ref()
        .and_then(|action| action.prompt.as_ref())
        .is_some();
    if !has_prompt {
        bail!("default_action.prompt is required");
    }

    let prompt_placeholders = collect_template_placeholders(&config);
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

    let delay = config.delay;
    if let Some(ref delay) = delay {
        validate_delay(delay)?;
    }

    validate_target(&target)?;

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
    println!("- note: dry-run only, no tmux commands sent");
}

fn rule_eval_label(rule_eval: &RuleEval) -> &'static str {
    match rule_eval {
        RuleEval::FirstMatch => "first_match",
        RuleEval::MultiMatch => "multi_match",
        RuleEval::Priority => "priority",
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
        }
    }
    Ok(())
}

fn collect_template_placeholders(config: &Config) -> Vec<String> {
    let mut vars = HashSet::new();
    if let Some(action) = &config.default_action {
        collect_action_placeholders(action, &mut vars);
    }
    if let Some(rules) = &config.rules {
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
