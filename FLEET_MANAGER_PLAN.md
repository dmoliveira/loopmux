# Fleet Manager Delivery Plan (bd-efr)

## Goal
Ship a separate fleet manager for local loopmux processes, with named runs (`--name` + auto-generated fallback), local run registry, and manager controls to rotate/select runs and send commands (`stop`, `hold`, `resume`, `next`, `renew`).

## Stage Tracker

| Stage | Item | Status | Notes |
|---|---|---|---|
| 0 | Worktree + br issue setup | done | branch `loopmux-fleet-manager`, issue `bd-efr` in progress |
| 1 | Define data model + file layout for registry/control | done | local files under `~/.loopmux/runs/{state,control}` |
| 2 | Add run identity (`--name`, auto-name, run id) | done | `--name` added with sanitized/auto codename + run id |
| 3 | Implement local fleet registry + heartbeat lifecycle | done | state file written each loop and cleaned on exit |
| 4 | Implement control inbox + run-loop command handling | done | control commands consumed from control file |
| 5 | Add manager CLI (`runs ls`, `runs <action>`, `runs tui`) | done | command surface implemented |
| 6 | Build fleet TUI navigation + controls (`<`, `>`, stop, hold/resume) | done | manager TUI rotates and dispatches controls |
| 7 | Update docs + tests for fleet features | in_progress | docs and extra tests pending |
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
