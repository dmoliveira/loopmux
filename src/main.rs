use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;

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
    #[arg(long)]
    config: Option<PathBuf>,
    /// tmux target (session:window.pane), overrides config.
    #[arg(long)]
    target: Option<String>,
    /// Iterations to run, overrides config.
    #[arg(long)]
    iterations: Option<u32>,
    /// Validate config and tmux target without sending.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Parser)]
struct ValidateArgs {
    /// Path to the YAML config file.
    #[arg(long)]
    config: Option<PathBuf>,
    /// tmux target (session:window.pane), overrides config.
    #[arg(long)]
    target: Option<String>,
    /// Iterations to run, overrides config.
    #[arg(long)]
    iterations: Option<u32>,
}

#[derive(Debug, Parser)]
struct InitArgs {
    /// Path to write the YAML config file. If omitted, prints to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct Config {
    target: Option<String>,
    iterations: Option<u32>,
    infinite: Option<bool>,
    default_action: Option<Action>,
}

#[derive(Debug, Deserialize)]
struct Action {
    pre: Option<PromptBlock>,
    prompt: Option<PromptBlock>,
    post: Option<PromptBlock>,
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
        bail!("--config is required for now");
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
        .ok_or_else(|| anyhow::anyhow!("target is required"))?;

    let infinite = config.infinite.unwrap_or(false);
    let iterations = config.iterations;
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

    Ok(ResolvedConfig {
        target,
        iterations,
        infinite,
        has_prompt,
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
}

fn default_template() -> String {
    let template = r#"target: "ai:5.0"
iterations: 10

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
