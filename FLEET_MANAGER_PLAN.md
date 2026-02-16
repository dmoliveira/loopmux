# Fleet Manager Delivery Plan (bd-efr)

## Goal
Ship a separate fleet manager for local loopmux processes, with named runs (`--name` + auto-generated fallback), local run registry, and manager controls to rotate/select runs and send commands (`stop`, `hold`, `resume`, `next`, `renew`).

## Stage Tracker

| Stage | Item | Status | Notes |
|---|---|---|---|
| 0 | Worktree + br issue setup | done | branch `loopmux-fleet-manager`, issue `bd-efr` in progress |
| 1 | Define data model + file layout for registry/control | in_progress | decide schema/paths/heartbeat cadence |
| 2 | Add run identity (`--name`, auto-name, run id) | pending | include in startup + validation output |
| 3 | Implement local fleet registry + heartbeat lifecycle | pending | register/update/remove run records |
| 4 | Implement control inbox + run-loop command handling | pending | consume control commands with low-latency polling |
| 5 | Add manager CLI (`runs ls`, `runs <action>`, `runs tui`) | pending | local listing and command dispatch |
| 6 | Build fleet TUI navigation + controls (`<`, `>`, stop, hold/resume) | pending | rotate among active runs and act on selected run |
| 7 | Update docs + tests for fleet features | pending | README examples and focused unit tests |
| 8 | Verify (tests + runnable CLI), review, and finalize PR flow | pending | commit sequence, push, PR, merge, cleanup |

## Execution Order
1. Finalize file conventions and schema in code comments/types.
2. Implement run identity and registry writer first.
3. Implement control command ingestion in `run_loop`.
4. Add manager command surface and non-TUI listing/control.
5. Add fleet TUI manager and keyboard rotation/actions.
6. Update README and tests.
7. Run `cargo fmt`, `cargo test`, and a real CLI check.
8. Ship with incremental commits and full worktree E2E flow.
