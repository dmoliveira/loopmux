# Ralph Loop for OpenCode - Lean Plan

## Goals
- Loop a prompt into an OpenCode tmux pane N times with configurable triggers.
- Support mid-flight control (pause/resume/stop/skip/update prompt/iterations).
- Improve visibility: status bar, progress bar, colored state, icons, timestamps.
- Allow YAML config with optional pre/post sections and trigger matching rules.
- Provide a clear CLI (`--help`) and config validation via dry-run.

## Non-Goals (initial cut)
- Distributed execution or remote tmux orchestration.
- Full TUI framework unless necessary (keep lean).
- Complex multi-agent orchestration beyond a single prompt loop.

## Core Concepts
- **Loop**: A run that sends prompts into a tmux target pane.
- **Prompt Script**: `pre` (optional), `prompt` (required), `post` (optional).
- **Trigger Rule**: A match rule that determines when to inject the next prompt.
- **Flow**: Optional branching between rules based on output matches.
- **Delay Strategy**: Always wait between sends, even after a trigger matches.
- **Negative Match**: Exclude matches when certain patterns appear.

## Name / Distribution
- Proposed binary + formula name: `loopmux`.
- Repo name should align with binary for clarity.

## Config (YAML) - v1.1
### Required
- `target`: tmux session/window/pane selector (e.g., `ai:5.0`).
- `iterations`: integer or `infinite: true`.
- `default_action.prompt`: required base prompt text.

### Optional
- `default_action.pre`: string or list.
- `default_action.post`: string or list.
- `rules[]`: array of trigger rules (ordered by default).
- `rule_eval`: how to resolve multiple matches.
- `delay`: default delay strategy (fixed, range, jitter, backoff).
- `logging`: path/format/verbosity.
- `ui`: colors, icons, layout.
- `template_vars`: map of variables for prompt templating.
- `cli`: help text + examples for `--help` output.

## Rule Evaluation (default: first match wins)
- `first_match` (default): rules are checked in order; the first match wins and stops evaluation.
- `multi_match`: evaluate all matching rules in one cycle (order still matters for output).
- `priority`: evaluate by `rules[].priority` first, then order.

## Match Semantics
- `match`: positive criteria (regex/contains/starts_with).
- `exclude`: negative criteria; if any exclude matches, the rule is skipped.
- Example: match `iteration` but exclude `PROD`.

## Runtime Flow
1. Parse config + CLI overrides.
2. Validate tmux target (session/window/pane).
3. Render pre + prompt + post templates.
4. Start loop timer + logger.
5. Capture pane output and compare with last cursor/marker.
6. Evaluate rules using `rule_eval` strategy.
7. On match:
   - resolve action (default or rule override)
   - apply delay strategy (even after match)
   - send prompt
   - update progress + UI
   - move to `next` rule if specified
8. Handle mid-flight commands (pause/resume/edit/stop/skip/change rule).

## Mid-flight Commands
- `p`: pause/resume
- `s`: stop (graceful)
- `n`: next (force trigger)
- `e`: edit prompt (in-memory)
- `i`: change iterations (remaining)
- `t`: change trigger
- `r`: reload config

## UX/TUI (Lean)
- Top area: current state + last trigger match + last action.
- Center: live tmux output snippet (tail capture).
- Bottom bar: iterations, elapsed time, start time, config file, target, shortcuts.
- Progress bar: percent of iterations (or spinner if infinite).
- Icons + colors: green running, yellow paused, red error, blue waiting.

## Logging
- Structured log with timestamps.
- Record: trigger matches, injections, errors, output excerpts.
- Optional JSONL for later analysis.

## Implementation Options
- **Rust**
  - Pros: robust, fast, good CLI/TUI ecosystem.
  - Cons: higher initial setup.
  - Suggested crates: `clap`, `serde_yaml`, `tokio`, `ratatui`, `crossterm`, `regex`.
- **Node/TS**
  - Faster to iterate, easier for JS users.
  - If minimal TUI, can be okay.

## Risks / Edge Cases
- tmux pane output capture limits.
- Trigger false positives.
- Long-running LLM outputs causing match delays.
- User edits mid-flight causing inconsistent state.

## Example YAML (lean)
```yaml
target: "ai:5.0"
iterations: 50

# Rule evaluation strategy:
# - first_match (default): order matters; first rule wins
# - multi_match: all matching rules can fire
# - priority: use rules[].priority then order
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

cli:
  help:
    examples:
      - "loopmux run --config loop.yaml"
      - "loopmux run --config loop.yaml --dry-run"
      - "loopmux run --config loop.yaml --target ai:5.0 --iterations 10"
```

## CLI / Help
- `loopmux --help`: explain main commands and config layout.
- `loopmux run --config loop.yaml`: execute loop (supports overrides like `--target`, `--iterations`).
- `loopmux run --dry-run`: validate config, tmux target, and templates without sending.

## Homebrew Distribution (outline)
- Formula name: `loopmux`.
- Tap: `brew tap <org>/loopmux`.
- Install: `brew install loopmux`.
- Binary: `loopmux`.

## CLI Command Sketch
- `loopmux run`: execute a loop (accepts `--config`, `--target`, `--iterations`, `--dry-run`).
- `loopmux validate`: validate config only (alias of `run --dry-run`).
- `loopmux init`: scaffold a YAML config template.

## Notes on Current Script
- The hash approach works but re-captures the same buffer.
- A cursor/marker to track last processed output will reduce duplicate triggers.
- Delay should be applied even after a trigger match to avoid rapid repeats.

## Next Steps
1. Confirm config format + feature priorities.
2. Decide on Rust MVP vs TS prototype.
3. Define minimal CLI/TUI layout + keyboard controls.
4. Implement tmux capture + cursor tracking + rule engine.
5. Add mid-flight controls.
6. Add logging + YAML templating.

## Execution Plan (Epics, Tasks, Subtasks)
### Epic 1: Repo + CLI Skeleton
- [x] Task 1.1: Initialize Rust project for `loopmux`.
- [x] Subtask 1.1.1: Add CLI with `run`, `validate`, `init`.
- [x] Subtask 1.1.2: `--help` content with config summary + examples.
- [x] Subtask 1.1.3: `--dry-run` validation path.

### Epic 2: Config + Rule Engine
- [x] Task 2.1: YAML schema parsing (`default_action`, `rules`, `delay`).
- [x] Subtask 2.1.1: Match `include` and `exclude` semantics.
- [x] Subtask 2.1.2: `rule_eval` strategies (first_match default).
- [x] Subtask 2.1.3: Template variable expansion.

### Epic 3: tmux Integration
- [ ] Task 3.1: Capture pane output with cursor tracking.
- [ ] Subtask 3.1.1: Trigger detection on incremental output.
- [ ] Subtask 3.1.2: Safe send-keys with delay strategies.
- [ ] Subtask 3.1.3: Target validation and error handling.

### Epic 4: UX/TUI
- [ ] Task 4.1: Minimal TUI layout (status, output, footer).
- [ ] Subtask 4.1.1: Progress bar + delay countdown.
- [ ] Subtask 4.1.2: Icons + color states.

### Epic 5: Logging + Observability
- [ ] Task 5.1: Structured logging (text + JSONL).
- [ ] Subtask 5.1.1: Event types (match, send, error, pause).

### Epic 6: Packaging
- [ ] Task 6.1: Homebrew formula metadata.
- [ ] Subtask 6.1.1: Release artifact naming.

### Epic 7: Delivery Workflow
- [ ] Task 7.1: Create a worktree branch.
- [ ] Task 7.2: Implement in order of epics.
- [ ] Task 7.3: Commit changes.
- [ ] Task 7.4: Open PR.
- [ ] Task 7.5: Review/fix x10.
- [ ] Task 7.6: Merge to main.
