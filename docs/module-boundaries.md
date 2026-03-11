# Module Boundaries

This note describes the current high-level module split after the recent `src/main.rs` cleanup passes.

## Current layout

- `src/main.rs`: CLI wiring, config resolution, run loop orchestration, shared types, and cross-module glue.
- `src/logging.rs`: structured log writing and sent-prompt redaction.
- `src/source_inputs.rs`: explicit target/file source loading and dedupe helpers.
- `src/template.rs`: template placeholder discovery and default config template generation.
- `src/tui.rs`: generic TUI mode/style/layout and shared rendering helpers.
- `src/fleet.rs`: fleet run records, fleet loading/filtering/counting, and control-envelope dispatch.
- `src/fleet_tui.rs`: fleet manager list/detail/header/status rendering helpers.
- `src/fleet_runtime.rs`: fleet manager interactive runtime loop, selection flow, key handling, and bulk/single control actions.
- `src/prompt_editor.rs`: prompt editor state plus prompt history load/save behavior.

## Why this split exists

- Keep domain logic close to the subsystem that owns it instead of growing a single file.
- Preserve the existing behavior contracts while making targeted areas easier to review and extend.
- Let tests keep covering stable module seams such as fleet filtering, fleet TUI rendering, prompt history persistence, and log redaction.

## Practical guidance

- Put new fleet record/control logic in `src/fleet.rs`.
- Put fleet manager screen rendering changes in `src/fleet_tui.rs`.
- Put fleet manager event-loop or key-flow changes in `src/fleet_runtime.rs`.
- Put prompt editor/history behavior in `src/prompt_editor.rs`.
- Keep `src/main.rs` focused on wiring modules together unless the code is truly cross-cutting.

## Remaining concentration

- `src/main.rs` still owns the primary run loop and several shared runtime/TUI integration paths.
- Future refactors should prefer extracting one coherent responsibility at a time and preserving the existing test contracts.
