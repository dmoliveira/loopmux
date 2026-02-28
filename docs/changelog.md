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
