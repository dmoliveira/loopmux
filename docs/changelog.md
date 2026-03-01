# Changelog

## 2026-02-28

### References
- PR #114: Complete clippy structural debt pass 2 (`https://github.com/dmoliveira/loopmux/pull/114`)
- PR #115: Remove unused TUI status argument from update loop (`https://github.com/dmoliveira/loopmux/pull/115`)
- PR #116: Add changelog notes for recent clippy and TUI cleanups (`https://github.com/dmoliveira/loopmux/pull/116`)

### Executive Summary
- Completed structural clippy debt cleanup and a follow-up TUI API cleanup while keeping runtime behavior stable.
- Restored strict Rust quality gates on main with formatting, tests, and clippy all green.

### Adds
- Added typed argument objects for fleet filtering/control and config resolution paths to reduce parameter sprawl and improve maintainability.

### Changes
- Boxed `Command::Run` payload to remove enum-size imbalance flagged by clippy.
- Simplified `TuiState::update` by removing an unused trailing status argument and trimming all callsites.

### Fixes
- Cleared strict lint blockers tied to large enum variants and high-argument APIs across key execution paths.
- Removed dead parameter plumbing in the TUI update loop without changing behavior.

### Validation
- `cargo fmt --check`
- `cargo test -q` (124 passed)
- `cargo clippy -- -D warnings`

### Post-Release Smoke
- Real tmux smoke passed on a temporary target (`loopmux-smoke:1.0`) with one successful send and clean stop.
- Command used: `cargo run -- run -t loopmux-smoke:1.0 -n 1 --prompt "echo LOOPMUX_SMOKE_SENT" --trigger ".*" --once --no-trigger-edge --trigger-confirm-seconds 0`.
- Pane evidence captured: `LOOPMUX_SMOKE_SENT`.

### Consolidated Validation (v0.1.36 Prep)
- Full sweep passed on `main`: `cargo fmt --check`, `cargo test -q` (127 passed), `cargo clippy -- -D warnings`, `make smoke-post-release`.
- Smoke helper resolved target `loopmux-smoke:1.0` and captured `LOOPMUX_SMOKE_SENT`.

## 2026-03-01

### v0.1.37 Prep References
- PR #124: Reduce TUI redraw churn for unchanged frames (`https://github.com/dmoliveira/loopmux/pull/124`)
- PR #125: Surface skipped TUI redraw count in footer (`https://github.com/dmoliveira/loopmux/pull/125`)
- PR #126: Emit periodic metric logs for skipped TUI redraws (`https://github.com/dmoliveira/loopmux/pull/126`)
- PR #127: Scaffold v0.1.37 draft release notes (`https://github.com/dmoliveira/loopmux/pull/127`)

### v0.1.37 Release
- Release published: `https://github.com/dmoliveira/loopmux/releases/tag/v0.1.37`.
- Tag: `v0.1.37`.
- Preflight and publish checks passed: `cargo fmt --check`, `cargo test -q` (131 passed), `cargo clippy -- -D warnings`, `make smoke-post-release`.

### v0.1.37 Follow-Up References
- PR #128: Log v0.1.37 prep PR references in changelog (`https://github.com/dmoliveira/loopmux/pull/128`)
- PR #129: Update v0.1.37 draft with current validation evidence (`https://github.com/dmoliveira/loopmux/pull/129`)
- PR #130: Add v0.1.37 release command block to draft notes (`https://github.com/dmoliveira/loopmux/pull/130`)
- PR #131: Prepare v0.1.37 release execution runbook (`https://github.com/dmoliveira/loopmux/pull/131`)
- PR #132: Refresh v0.1.37 draft PR references (`https://github.com/dmoliveira/loopmux/pull/132`)
