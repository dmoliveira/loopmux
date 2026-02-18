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

### Config-first startup (no subcommand)
- Run `loopmux` with no subcommand to auto-start matching profiles from `~/.config/loopmux/config.yaml`.
- Profiles in `runs:` (or `events:` alias) can be enabled/disabled and filtered by current directory.
- Multiple matching profiles are started together (each as an independent run process).

Example:

```yaml
imports:
  - ~/.config/loopmux/runs/work.yaml

id: main
enabled: true
when:
  cwd_matches:
    - ~/Codes/Projects/*
target: "ai:8.1"
iterations: 50
tail: 3
poll: 5
default_action:
  prompt: "Do the next iteration."
rules:
  - id: continue
    match:
      regex: "Concluded|What is next"

runs:
  - id: docs
    enabled: false
    target: "ai:6.1"
    iterations: 20
    default_action:
      prompt: "Polish docs and examples."
```

Notes:
- Imported files can contribute extra `runs`/`events` profiles.
- Each profile uses the same run-config schema as normal YAML runs (`target`, `rules`, `poll`, `tail`, etc.).
- Startup validates all selected profiles before launch and prints clear per-profile errors.

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
trigger_confirm_seconds: 5
recheck_before_send: true

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
    confirm_seconds: 3
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
loopmux runs ls
loopmux runs tui
loopmux runs stop <run-id-or-name>
```

## Lean Mode (no YAML)

Use inline flags to run a quick loop without a config file.

```bash
loopmux run -t ai:5.0 -n 5 \
  --prompt "Do the next iteration." \
  --trigger-expr "(Concluded || READY) && NEXT" \
  --exclude "PROD" \
  --once
```

### Lean flags
- `--prompt`: required prompt body.
- `--trigger`: regex to match source output.
- `--trigger-expr`: boolean expression trigger mode using regex atoms, `&&`, `||`, and parentheses.
- `--trigger` and `--trigger-expr` are mutually exclusive; provide one of them.
- `--trigger-exact-line`: treat `--trigger` as an exact trimmed line match (good for sentinel tokens like `<CONTINUE-LOOP>`).
- `--exclude`: regex to skip matches (optional).
- `--pre` / `--post`: optional prompt blocks.
- `--once`: send a single prompt and exit.
- `-t, --target`: tmux target selector (repeatable).
- `--targets-file PATH`: load tmux targets from a file (`#` comments and blank lines ignored).
- `--file PATH`: add a file source to scan for triggers.
- `--files-file PATH`: load file sources from a file (`#` comments and blank lines ignored).
- `--tail N`: scan the last `N` lines from each source (default `1`, applies `last non-blank line` shortcut when `N=1`).
- `--head N`: scan the first `N` lines from each source (mutually exclusive with `--tail`).
- `--single-line`: update status output on a single line.
- `--poll N`: polling interval in seconds while waiting for matches (default 5).
- `--trigger-confirm-seconds N`: require trigger to stay matched for N seconds before send (default 5).
- `--log-preview-lines N`: number of captured lines shown in folded sent-log previews (default 3).
- `--no-trigger-edge`: opt out of edge-guard (default guard is ON to avoid repeated queue injections while trigger stays true).
- `--no-recheck-before-send`: skip the default pre-send trigger recheck (default is ON).
- `--fanout matched|broadcast`: send to matched panes only (default) or broadcast to all panes in scope.
- `--tui`: enable the interactive terminal UI.
- `--history-limit N`: max history entries to keep/show in TUI picker (default 50).
- `--name`: optional codename for this run; auto-generated if omitted.

### Trigger expression quick reference
- Use regex terms joined by boolean operators:
  - `&&` logical AND
  - `||` logical OR
  - `(` `)` grouping
- Precedence: parentheses > `&&` > `||` (left-associative).
- `--trigger-exact-line` applies only to `--trigger` (not `--trigger-expr`).

Examples:

```bash
loopmux run -t ai:5.0 \
  --prompt "Continue iteration" \
  --trigger-expr "(READY || DONE) && NEXT"

loopmux run -t ai:5.0 \
  --prompt "Continue iteration" \
  --trigger-expr "<CONTINUE-LOOP> && (LGTM || APPROVED)"
```

### Migration notes (`--trigger` -> `--trigger-expr`)
- Keep `--trigger` when a single regex is enough.
- Move to `--trigger-expr` when you need explicit boolean logic.
- A direct migration pattern is to wrap existing regex terms as atoms and compose with operators:
  - before: `--trigger "READY|DONE"`
  - after: `--trigger-expr "READY || DONE"`
- If you previously used `--trigger-exact-line`, keep that mode on `--trigger`; expression mode remains regex-atom based.

### Mixed source examples
Use tmux + files together:

```bash
loopmux run \
  --target ai:5.0 \
  --targets-file ./targets.txt \
  --file ./logs/assistant.log \
  --files-file ./watch-files.txt \
  --head 20 \
  --prompt "Continue iteration and summarize updates." \
  --trigger-expr "<CONTINUE-LOOP> || Ready for next step"
```

`targets.txt` format:

```text
# comments are ignored
ai:5.0
codex:1.0
```

`watch-files.txt` format:

```text
# comments are ignored
./logs/assistant.log
./logs/review.log
```

### TUI history picker
- Run `loopmux run --tui` with no prompt/config to pick from recent commands.
- Entries are stored in `~/.loopmux/history.json`, newest first, deduplicated by command shape.
- TUI controls: `h` hold/resume (non-consuming, alias `p`/`r`), `f` open fleet manager view, `R` renew counter, `n` next, `s`/`Ctrl+C` stop run, `q` quit run view.
- When `--duration` is set, the TUI status bar shows remaining time (`rem ...`) and it freezes while HOLD is active.
- Run TUI status bar includes current loopmux version (`vX.Y.Z`) for quick parity checks.
- Sent logs are compact and include a folded trigger preview (`N` lines from capture tail) to keep long prompts readable.
- TUI log timestamps use subtle date-aware coloring to make same-day activity easier to scan.

### Fleet manager (local)
- Every running `loopmux run` writes a local registry entry under `~/.loopmux/runs/state/`.
- Each run has an id plus a codename (`--name` or auto-generated like `amber-fox-0421`).
- Quick command workflow:
  ```bash
  loopmux runs ls
  loopmux runs hold <id-or-name>
  loopmux runs resume <id-or-name>
  loopmux runs next <id-or-name>
  loopmux runs renew <id-or-name>
  loopmux runs stop <id-or-name>
  loopmux runs tui
  ```
- List runs:
  ```bash
  loopmux runs ls
  ```
  - Output includes each run version and whether it matches local version.
- Send controls to a run by id or name:
  ```bash
  loopmux runs hold <id-or-name>
  loopmux runs resume <id-or-name>
  loopmux runs next <id-or-name>
  loopmux runs renew <id-or-name>
  loopmux runs stop <id-or-name>
  ```
- Open the fleet manager TUI:
  ```bash
  loopmux runs tui
  ```
  - On wide terminals, fleet manager uses a split layout (runs list on left, selected-run details on right).
  - Controls: `<`/`Left` previous, `>`/`Right` next, `space` mark/unmark selected run, `a` clear marks, `x` toggle stale visibility (hidden by default), `v` mismatch-only filter, `f` cycle state filter (`all/active/holding/stale`), `/` search mode (name/id/target/state/version), `p` cycle presets (`default`, `needs-attention`, `mismatch-only`, `holding-focus`), `1-4` jump directly to those presets, `o` cycle sort (`last_seen/sends/health/name/state`), `s` arm single-run stop, `S`/`H`/`P`/`N`/`U` arm bulk stop/hold/resume/next/renew for marked runs (or selected run when none are marked), `Enter` confirm pending action (or jump when no action is armed), `c` cancel pending action, `i` copy selected run id, `y` copy `loopmux runs stop <id>` snippet, `h` hold, `r` resume, `n` next, `R` renew, `q`/`Esc` quit manager.
  - When opened from `run --tui` via `f`, `q`/`Esc` returns to the run view.
  - Header includes local version plus counts (`active`, `holding`, `stale`, `mismatch`).

### Version checks
- `loopmux --version` shows the active binary version.
- `loopmux run --help` and `loopmux runs --help` include version references.
- Fleet manager and run TUI surfaces include version labels to spot mismatched runs quickly.

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

By default, loopmux also requires matches to remain present for `5s` before sending (`trigger_confirm_seconds`). Set it to `0` for immediate behavior, or override per rule with `confirm_seconds`.

## Troubleshooting

### tmux target not found
- Verify the target: `tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index}'`
- Ensure the session/window/pane exists and is attached.
- Confirm the assistant is running in the target pane.

### No triggers firing
- Check your regex terms or expression atoms.
- Confirm the source output actually includes the trigger text (`tmux` pane or file window).
- If you use file sources, validate paths are readable regular files.
- For sentinel lines, prefer `--trigger-exact-line` and a unique token.
- For expression mode, validate operator precedence (`&&` before `||`) and add parentheses when intent is ambiguous.
- Use `multi_match` if you expect more than one rule to fire.

### File source gotchas
- Use `--tail` for append-only logs and `--head` when the important marker stays near the top.
- `--tail` and `--head` are mutually exclusive.
- In `fanout matched` mode, a file match sends to configured tmux recipients.

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
