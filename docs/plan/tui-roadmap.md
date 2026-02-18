# TUI Roadmap

This document captures the remaining TUI direction after the major TUI and fleet manager work already shipped in recent releases.

## Current baseline (implemented)

- Run view supports interactive controls (`h`, `r`, `n`, `R`, `s`, `q`) and status updates.
- Fleet manager exists (`loopmux runs tui`) with filtering, sorting, search, presets, and bulk actions.
- Status/log output includes compact rendering, trigger previews, and version visibility.
- Help and README now document fleet and TUI usage flows.

## Product goals

- Keep the TUI legible under narrow terminal widths.
- Keep controls discoverable without cluttering the screen.
- Preserve robust fallback behavior for non-interactive terminals.
- Improve operator speed for common multi-run workflows.

## Planned improvements

### 1) Run view ergonomics

- Add explicit width tiers (compact/standard/wide) with tested truncation rules.
- Improve footer discoverability for less common actions.
- Add a log visibility toggle to maximize signal in narrow terminals.

### 2) Fleet manager workflow

- Improve first-use affordances for marked/bulk actions.
- Add clearer action confirmation copy for destructive commands.
- Add optional jump shortcuts to common filter/preset combinations.

### 3) Styling and compatibility

- Keep a conservative color strategy that degrades cleanly with `NO_COLOR`.
- Keep icon fallback behavior deterministic when Nerd Font glyphs are unavailable.
- Validate all critical UI paths in low-color and narrow-width environments.

### 4) Runtime editing (future)

- Evaluate safe in-session edits for prompt/trigger values.
- Define guardrails for edits while confirm windows or hold states are active.
- Decide whether edits are ephemeral or persisted to history/config.

## Non-goals (for now)

- Rewriting the current TUI stack.
- Introducing heavy visual effects that reduce terminal compatibility.
- Expanding feature scope before core readability and control flows are tightened.

## Validation strategy

- Add deterministic render tests for compact/standard/wide status formatting.
- Add targeted behavior tests for key-control flows and fallback paths.
- Use recorded terminal sessions for release-note demos when behavior changes.

## Milestone slices

1. Width-tier polish and truncation rules.
2. Fleet bulk-action clarity pass.
3. Compatibility hardening and regression tests.
4. Runtime editing RFC and prototype.
