# TUI Stability and Migration Plan

This plan is the source of truth for hardening loopmux TUI reliability and evaluating an incremental migration from manual `crossterm` rendering to `ratatui`.

## Status legend

- `` not started
- `` doing
- `` done
- `` blocked

## Index

- [Scope and outcomes](#scope-and-outcomes)
- [Assumptions](#assumptions)
- [Engineering conventions](#engineering-conventions)
- [Definition of done](#definition-of-done)
- [Validation and testing strategy](#validation-and-testing-strategy)
- [Acceptance criteria](#acceptance-criteria)
- [Epic roadmap](#epic-roadmap)
- [Task closure protocol](#task-closure-protocol)
- [Decision log](#decision-log)

## Scope and outcomes

### In scope

- Stabilize current TUI lifecycle (raw mode, rendering, failure handling) before large refactors.
- Introduce deterministic tests for rendering and key-driven state behavior.
- Improve runtime observability and graceful degradation under terminal and I/O faults.
- Design and execute a low-risk, phased `ratatui` migration where benefits are concrete.

### Out of scope

- Full visual redesign unrelated to stability.
- Feature expansion that increases control surface before reliability baseline is green.
- Breaking CLI semantics for existing operators.

## Assumptions

1. We keep terminal compatibility first (macOS/Linux, SSH sessions, CI logs).
2. `crossterm` remains the input/event foundation during early hardening.
3. `ratatui` is optional until objective stability metrics justify migration effort.
4. Existing run and fleet controls (`h`, `r`, `n`, `R`, `s`, `q`, fleet actions) are contract behavior.
5. We can ship in thin vertical slices with independent roll-forward safety.

## Engineering conventions

1. Single writer flow for TUI core changes: `src/main.rs` updates are scoped and reviewed in small batches.
2. Every closed task must append:
   - decision taken,
   - tests/validation run,
   - assumption/convention updates if changed by evidence.
3. No hidden behavior toggles: all runtime switches are documented in README/specs before close.
4. Prefer deterministic tests over ad-hoc manual verification; keep flaky timing assertions out of CI.
5. New abstractions must pay for themselves with at least one testability or failure-isolation win.
6. Raw mode ownership must be centralized behind a single guard path; ad-hoc enable/disable calls are transitional only.

## Definition of done

A task is done only when all are true:

1. Code/doc scope for the task is complete and merged in branch sequence.
2. Targeted tests pass locally and in CI for touched behavior.
3. Failure modes are handled explicitly (no silent state corruption, no terminal wedging).
4. Plan entry is updated with status, completion note, and decision link.
5. Any changed assumption/convention in this file is updated in the same commit.

## Validation and testing strategy

### Test layers

1. Unit tests
   - formatting/truncation helpers,
   - key-to-action mapping,
   - state transition functions.
2. Snapshot tests
   - compact/standard/wide render output,
   - footer/help/overlay composition.
3. Integration tests
   - TUI startup/shutdown lifecycle,
   - fallback mode behavior when terminal is non-interactive.
4. Smoke/manual checks
   - `loopmux run --tui` on narrow and wide terminals,
   - `loopmux runs tui` list navigation and bulk action confirmation path.

### Validation commands (minimum)

- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`
- focused manual smoke for TUI/fleet interactions on changed paths

## Acceptance criteria

1. TUI never leaves terminal in raw mode after normal exit, recoverable error, or panic.
2. Render path has explicit error policy (retry budget + controlled fallback/exit), not ignored writes.
3. Render/update/input responsibilities are isolated enough to test deterministically.
4. Key operator paths are covered by tests and remain backward compatible.
5. Migration steps (if chosen) preserve behavior parity with measurable regressions tracked.

## Epic roadmap

## Epic E0 - Baseline and guardrails

Status: `` doing

Goal: lock current behavior, remove blind spots, and define objective reliability baseline.

### Task E0.T1 - Baseline behavior inventory

Status: `` done

Subtasks:

- `` E0.T1.S1 Capture current controls and mode matrix (run/fleet/plain/single-line).
- `` E0.T1.S2 Document terminal lifecycle entry/exit paths and known failure gaps.
- `` E0.T1.S3 Capture current redraw/update/input coupling map for refactor planning.

Lifecycle baseline notes (E0.T1.S2):

- Entry points: raw mode is enabled in run TUI init (`src/main.rs:6362`) and fleet manager entry (`src/main.rs:2473`).
- Normal exits: raw mode is disabled by `TuiState::shutdown` (`src/main.rs:6605`) on run-loop completion (`src/main.rs:4952`) and after fleet manager returns (`src/main.rs:2475`).
- Gap: run loop has many fallible `?` operations after TUI init (example `src/main.rs:3585`, `src/main.rs:3625`, `src/main.rs:4947`) before explicit shutdown (`src/main.rs:4953`), so early errors can skip restoration.
- Gap: there is no explicit panic restoration hook around TUI lifecycle; panic behavior relies on process teardown and can wedge the terminal in practice.
- Decision: E1.T1 will implement a centralized RAII terminal guard with panic-safe restoration as mandatory before broader refactors.

Coupling baseline notes (E0.T1.S3):

- Run mode coupling: one control loop interleaves input polling (`src/main.rs:3625`, `src/main.rs:4447`) and state transitions with rendering via `tui_state.update(...)` (`src/main.rs:3577`, `src/main.rs:4888`).
- Render coupling: `TuiState::update` computes view model and performs terminal writes in the same function (`src/main.rs:6429` onward), making draw failures and state updates non-separable.
- Hot-path side effect: render update calls `process_usage_summary()` (`src/main.rs:6449`) which may execute `ps` (`src/main.rs:6394`), introducing external command latency into frame generation.
- Fleet manager coupling: redraw, diffing, input poll, and action dispatch share a single loop (`src/main.rs:2506` onward; input poll at `src/main.rs:2656`).
- Decision: E2.T2 will introduce explicit tick/input/render phases and move process sampling out of the frame path before any backend migration.

Closure update (2026-02-28):

- Validation evidence: docs-only change validated with `git diff --check`.
- Outcome: E0.T1 baseline inventory is complete; next active item is E0.T2 reliability metrics definition.

Important decisions to remember:

- Preserve current keymap compatibility unless an explicit migration note is approved.
- Keep baseline capture small and factual; avoid speculative redesign in this task.

### Task E0.T2 - Reliability metrics definition

Status: `` not started

Subtasks:

- `` E0.T2.S1 Define stability SLOs (terminal recovery success, redraw failure tolerance).
- `` E0.T2.S2 Define perf guardrails (max frame/update budget under nominal load).
- `` E0.T2.S3 Add lightweight instrumentation plan for local/CI evidence.

Important decisions to remember:

- SLOs must be observable with existing tooling; no heavy telemetry dependency.

## Epic E1 - Lifecycle hardening on crossterm

Status: `` not started

Goal: make current architecture resilient before introducing `ratatui`.

### Task E1.T1 - Terminal lifecycle guard

Status: `` not started

Subtasks:

- `` E1.T1.S1 Introduce RAII terminal guard for raw mode lifecycle.
- `` E1.T1.S2 Add panic-safe restoration hook and idempotent shutdown logic.
- `` E1.T1.S3 Add tests for restoration guarantees on error and panic simulation.

Important decisions to remember:

- Guard implementation must not require global mutable state for correctness.

### Task E1.T2 - Render I/O error policy

Status: `` not started

Subtasks:

- `` E1.T2.S1 Remove ignored write/flush results in render paths.
- `` E1.T2.S2 Implement bounded retry and fallback policy.
- `` E1.T2.S3 Add logs/events for render degradation and fallback transitions.

Important decisions to remember:

- Favor explicit controlled exit over continuing in corrupted terminal state.

## Epic E2 - Architecture split for determinism

Status: `` not started

Goal: separate model/update/draw/input to reduce coupling and increase testability.

### Task E2.T1 - Loop state model extraction

Status: `` not started

Subtasks:

- `` E2.T1.S1 Extract pure state structs and transition functions.
- `` E2.T1.S2 Isolate side effects behind trait boundaries.
- `` E2.T1.S3 Add deterministic transition tests for key flows.

Important decisions to remember:

- Extract only proven seams first; avoid broad file split churn in one PR.

### Task E2.T2 - Event loop rationalization

Status: `` not started

Subtasks:

- `` E2.T2.S1 Introduce explicit tick/input/render phases.
- `` E2.T2.S2 Decouple expensive sampling from frame rendering.
- `` E2.T2.S3 Stress test resize and burst-input behavior.

Important decisions to remember:

- Rendering must be side-effect minimal; no slow external process calls in hot path.

## Epic E3 - Test matrix and regression net

Status: `` not started

Goal: establish fast confidence loops for TUI changes.

### Task E3.T1 - Render and layout snapshots

Status: `` not started

Subtasks:

- `` E3.T1.S1 Add compact/standard/wide golden snapshots.
- `` E3.T1.S2 Add unicode/no-color snapshots.
- `` E3.T1.S3 Add snapshot update guidance and diff policy.

Important decisions to remember:

- Snapshot churn must stay intentional; include rationale on updates.

### Task E3.T2 - Interaction contract tests

Status: `` not started

Subtasks:

- `` E3.T2.S1 Keymap compatibility tests for run and fleet modes.
- `` E3.T2.S2 Confirmation flow tests for destructive actions.
- `` E3.T2.S3 Non-interactive fallback tests.

Important decisions to remember:

- Contract tests define compatibility promise for future `ratatui` parity.

## Epic E4 - ratatui feasibility and phased migration

Status: `` not started

Goal: migrate only when hardening baseline is met and parity can be maintained.

### Task E4.T1 - Feasibility RFC

Status: `` not started

Subtasks:

- `` E4.T1.S1 Compare manual renderer vs `ratatui` cost/benefit with evidence.
- `` E4.T1.S2 Define compatibility and rollback strategy.
- `` E4.T1.S3 Set migration entry criteria and stop conditions.

Important decisions to remember:

- Migration starts only after E1 + E3 acceptance criteria are met.

### Task E4.T2 - Incremental adapter implementation

Status: `` not started

Subtasks:

- `` E4.T2.S1 Introduce adapter interface for draw backend.
- `` E4.T2.S2 Port status/header/footer first with parity checks.
- `` E4.T2.S3 Port list/detail panes and remove dual-path debt.

Important decisions to remember:

- Keep dual backend window short and time-boxed to avoid long-lived complexity.

## Task closure protocol

When any task/subtask changes to `` done, update this file in the same PR:

1. Set status marker(s) in Epic section.
2. Append one bullet to Decision log with task id and rationale.
3. Add test/validation evidence snippet reference.
4. If behavior assumptions changed, update Assumptions and Engineering conventions sections.

## Decision log

- `2026-02-28` `PLAN-001` Adopt hardening-first sequence: stabilize `crossterm` lifecycle before committing to `ratatui` migration.
- `2026-02-28` `PLAN-002` Define explicit acceptance criteria around terminal recovery, render error policy, and testability parity.
- `2026-02-28` `PLAN-003` E0.T1.S2 baseline confirms raw-mode restoration risk on early `?` exits; E1.T1 must land RAII + panic-safe restore first.
- `2026-02-28` `PLAN-004` E0.T1.S3 baseline confirms render/input/update coupling and hot-path process sampling; E2.T2 must phase-separate loop responsibilities.
