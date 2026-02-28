# Changelog

## 2026-02-28

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
