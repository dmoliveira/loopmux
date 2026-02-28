# RFC: ratatui Feasibility and Migration Entry Criteria

## Context

loopmux currently runs a stable `crossterm`-based TUI with hardened lifecycle handling, render I/O fallback policy, deterministic loop-phase boundaries, and regression contracts for key interactions and fallback paths.

This RFC defines whether and when we should introduce `ratatui` for rendering while preserving current operator contracts.

## Scope

- Compare current manual renderer vs `ratatui` for reliability, maintainability, and testability.
- Define compatibility and rollback strategy for phased migration.
- Define migration entry criteria and stop conditions.

Out of scope:

- Full visual redesign.
- Input/event stack replacement (`crossterm` remains the event source).
- Feature expansion beyond parity-focused migration slices.

## Cost-Benefit Assessment

### Current renderer (manual crossterm)

Benefits:

- Zero new rendering dependency and no immediate migration risk.
- Full control over output and terminal compatibility behavior.
- Existing behavior contracts and tests already reflect production usage.

Costs:

- Layout logic remains custom and easy to regress when adding panes/metadata.
- Rendering/diff concerns are distributed, increasing maintenance burden.
- Future UI composition work has higher implementation overhead.

### ratatui renderer (phased)

Benefits:

- Declarative layout/widgets reduce manual cursor and string composition complexity.
- Stronger structure for snapshot-style rendering tests.
- Cleaner separation between model/state and view composition.

Costs:

- Temporary dual-renderer complexity during migration window.
- Potential parity regressions (spacing, truncation, ordering, hint text).
- Additional crate surface and update maintenance.

## Compatibility Strategy

1. Keep command/UI contracts fixed during migration:
   - keymaps and aliases,
   - confirmation prompts,
   - non-interactive fallback behavior,
   - status/footer semantic fields.
2. Keep `crossterm` as input/event backend; swap draw backend incrementally.
3. Gate migration behind explicit draw adapter boundary and parity checks per slice.
4. Require parity tests to pass before removing legacy draw path.

## Rollback Strategy

- Maintain legacy renderer as fallback until at least one full release cycle of parity and stability checks passes.
- If parity or stability regresses in any migrated slice:
  - rollback that slice to legacy renderer path,
  - keep adapter interface intact,
  - capture regression as a contract test before reattempting.
- Never remove legacy renderer in same PR that introduces first migrated pane.

## Migration Entry Criteria

All must be true before E4.T2 implementation work starts:

1. E1 and E3 acceptance criteria are satisfied and merged.
2. Lifecycle restoration and render fallback policies remain green in current tests.
3. Contract tests cover:
   - keymaps,
   - destructive confirmations,
   - non-interactive fallback,
   - layout token ordering across compact/standard/wide and unicode/no-color.
4. Adapter seam exists for draw backend routing with no behavior change at introduction.

## Stop Conditions

Pause migration and revert to hardening if any occurs:

1. Repeated parity regressions across two consecutive migration slices.
2. Increased terminal recovery/render failure incidents above E0 SLO tolerances.
3. Persistent performance regressions violating established p95/p99 frame/input guardrails.
4. Dual-backend maintenance window exceeds planned timebox without clear burn-down.

## Recommended Execution Sequence

1. Add draw adapter interface (no behavior change).
2. Port status/header/footer with parity checks.
3. Port list/detail panes with parity checks.
4. Remove legacy path after parity and stability prove-out window.

## Validation Requirements Per Migration PR

- `cargo fmt --check`
- `cargo test -q`
- targeted layout/contract tests for touched surfaces
- PR evidence table with:
  - parity checks passed/failed,
  - perf deltas (if measured),
  - rollback trigger status
