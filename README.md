# loopmux

[![Homebrew](https://img.shields.io/badge/homebrew-installable-2f9c5f)](https://github.com/dmoliveira/homebrew-tap)

Loop prompts into tmux panes with triggers, delays, and branching rules. Built to automate iterative workflows for code assistants running in tmux (OpenCode, Codex, Claude Code).

## Why loopmux
loopmux watches tmux output and injects prompts when a trigger matches. You can chain flows, add pre/post blocks, and control delays so your iterations feel deliberate instead of spammy.

## Features
- YAML config with `pre`, `prompt`, `post` blocks
- Ordered rule evaluation with `first_match`, `priority`, or `multi_match`
- Include/exclude match criteria (regex/contains/starts_with)
- Delay strategies: fixed, range, jitter, backoff
- Mid-flight loop runner (tmux capture + send)
- Structured logging (text or JSONL)

## Supported Code Assistants
loopmux is tmux-first and backend-agnostic. If your assistant runs in a tmux pane, loopmux can target it.

Example tmux targets:
- OpenCode: `ai:5.0`
- Codex: `codex:1.0`
- Claude Code: `claude:2.0`

## Install

### Homebrew
```bash
brew tap dmoliveira/tap
brew install loopmux
```

### Build from source
```bash
git clone https://github.com/dmoliveira/loopmux.git
cd loopmux
cargo build --release
./target/release/loopmux --help
```

## Quick Start

1) Create a config:
```bash
loopmux init --output loop.yaml
```

2) Update the tmux target and rules in `loop.yaml`.

3) Validate config:
```bash
loopmux validate --config loop.yaml
```

4) Run the loop:
```bash
loopmux run --config loop.yaml
```

### Quick Run (no YAML)
```bash
loopmux run -t ai:5.0 -n 5 \
  --prompt "Do the next iteration." \
  --trigger "Concluded|What is next" \
  --once
```

## Configuration

### Minimal example
```yaml
target: "ai:5.0"
iterations: 10

default_action:
  prompt: "Do the next iteration."
```

### Full example
```yaml
target: "ai:5.0"
iterations: 50

rule_eval: first_match

template_vars:
  project: loopmux

default_action:
  pre: "Keep context on UX simplification."
  prompt: "Do the next iteration for {{project}}."
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

  - id: review-path
    match:
      regex: "(Ready for review|PR created)"
    delay:
      mode: fixed
      value: 300
    action:
      prompt: "Audit UX for simplification."
    next: success-path

  - id: failure-path
    match:
      regex: "(FAIL|Error|Exception)"
    action:
      pre: "Fix the errors before proceeding."
      prompt: "Repair and re-run tests."
      post: "Summarize fixes."
    next: success-path

logging:
  path: "loopmux.log"
  format: "jsonl"
```

### Example files
- `examples/loopmux.example.yaml`
- `examples/loopmux.lean.yaml`

### Rule evaluation
- `first_match`: ordered rules; first match wins.
- `multi_match`: all matching rules fire in order.
- `priority`: highest priority wins (ties resolved by order).

### Delay strategies
- `fixed`: static delay in seconds.
- `range`: random delay between `min` and `max`.
- `jitter`: range plus +/- jitter factor (0.0..1.0).
- `backoff`: exponential backoff using `base`, `factor`, `max`.

## CLI

```text
loopmux run --config loop.yaml [--target ai:5.0] [--iterations 10]
loopmux run --config loop.yaml --dry-run
loopmux validate --config loop.yaml [--skip-tmux]
loopmux init --output loop.yaml
```

## Lean Mode (no YAML)

Use inline flags to run a quick loop without a config file.

```bash
loopmux run -t ai:5.0 -n 5 \
  --prompt "Do the next iteration." \
  --trigger "Concluded|What is next" \
  --exclude "PROD" \
  --once
```

### Lean flags
- `--prompt`: required prompt body.
- `--trigger`: regex to match tmux output (required).
- `--exclude`: regex to skip matches (optional).
- `--pre` / `--post`: optional prompt blocks.
- `--once`: send a single prompt and exit.
- `--tail N`: number of capture-pane lines (default 1, last non-blank line).
- `--single-line`: update status output on a single line.
- `--poll N`: polling interval in seconds while waiting for matches (default 5).
- `--no-trigger-edge`: opt out of edge-guard (default guard is ON to avoid repeated queue injections while trigger stays true).
- `--fanout matched|broadcast`: send to matched panes only (default) or broadcast to all panes in scope.
- `--tui`: enable the interactive terminal UI.
- `--history-limit N`: max history entries to keep/show in TUI picker (default 50).

### TUI history picker
- Run `loopmux run --tui` with no prompt/config to pick from recent commands.
- Entries are stored in `~/.loopmux/history.json`, newest first, deduplicated by command shape.
- TUI controls: `h` hold/resume (non-consuming, alias `p`/`r`), `R` renew counter, `n` next, `s`/`Ctrl+C` stop, `q` quit.
- When `--duration` is set, the TUI status bar shows remaining time (`rem ...`) and it freezes while HOLD is active.

### Common flags
- `-t, --target`: tmux scope selector.
  - omit `-t`: scan all sessions/windows/panes each poll
  - `session`: all panes in that session
  - `session:window`: all panes in that window
  - `session:window.pane`: one pane
- `-n, --iterations`: number of iterations (omit for infinite when using config).

### Target shorthand (inside tmux)
- `-t 0` expands to `current_session:current_window.0`
- `-t 2.1` expands to `current_session:2.1`
  - Shorthand requires tmux; otherwise provide full `session:window.pane`.

When no candidates are found in the selected scope, loopmux waits and re-scans on the next `--poll` interval.

By default, loopmux sends only on trigger state transitions (`false -> true`) per target/rule and waits for the trigger to clear before sending again.

## Troubleshooting

### tmux target not found
- Verify the target: `tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index}'`
- Ensure the session/window/pane exists and is attached.
- Confirm the assistant is running in the target pane.

### No triggers firing
- Check your match regex/contains.
- Confirm the pane output actually includes the trigger text.
- Use `multi_match` if you expect more than one rule to fire.

### Too fast or too slow
- Adjust `delay` (fixed/range/jitter/backoff).
- Increase `min`/`max` if output needs more time to settle.

### Homebrew build fails with `rust-objcopy`
- If you see `error: unable to run rust-objcopy`, ensure you have the latest tap:
  ```bash
  brew update
  brew reinstall loopmux
  ```
- The formula disables cargo stripping to avoid this dependency on macOS.

### Homebrew checksum mismatch
- If install fails with `Formula reports different checksum`, your local tap formula is stale or the formula SHA is outdated.
- Refresh your taps and retry:
  ```bash
  brew update
  brew untap dmoliveira/tap
  brew tap dmoliveira/tap
  brew reinstall loopmux
  ```
- Maintainers can regenerate `release/loopmux.rb` for a release tag:
  ```bash
  ./release/update_formula.sh v0.1.6
  ```

## Contributing

1) Fork the repo and create a feature branch.
2) Run `cargo fmt` and `cargo check` before opening a PR.
3) Keep commits focused and include a clear summary.

## Security
loopmux executes prompts into tmux. Treat configs as code, review commands, and avoid sensitive content in logs.

## License
MIT
