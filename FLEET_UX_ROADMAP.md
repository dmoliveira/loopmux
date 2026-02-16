# Fleet UX Roadmap (bd-1j1)

Track progress for fleet-manager improvements with explicit statuses.

Status legend: `pending`, `in_progress`, `completed`, `blocked`.

## Epic 0 - Quit key clarity in fleet TUI (current)

| ID | Task | Status | Notes |
|---|---|---|---|
| E0-T1 | Make quit wording explicit as "quit manager" in fleet TUI header/footer | completed | avoid confusion with run stop action |
| E0-T2 | Add `Esc` as quit-manager alias in fleet TUI | completed | keep `q` for quick exit |
| E0-T3 | Update docs/help for new quit semantics | completed | README updated |
| E0-T4 | Verify behavior and ship | completed | tests + runnable CLI checks |

## Epic 1 - Version visibility everywhere

| ID | Task | Status | Notes |
|---|---|---|---|
| E1-T1 | Show version in top-level help and subcommand help sections used most (`run`, `runs`) | pending | keep discoverable in operator workflows |
| E1-T2 | Show local version in run TUI header | pending | quick visual parity check |
| E1-T3 | Show local version in fleet TUI header | pending | compare manager vs runs |
| E1-T4 | Add docs section for version checks (`--version`, help, TUI header) | pending | reduce operator ambiguity |

## Epic 1b - Embedded fleet manager in run TUI

| ID | Task | Status | Notes |
|---|---|---|---|
| E1b-T1 | Add `f` shortcut in run TUI to open fleet manager view | completed | reuse existing fleet control flow |
| E1b-T2 | Keep standalone `runs tui` semantics and embedded return semantics | completed | embedded `q`/`Esc` returns to run view |
| E1b-T3 | Support `Enter` jump-to-target in fleet manager | completed | handles pane/session/window/all scopes |
| E1b-T4 | Update docs and verify behavior | completed | README + test/smoke validation |

## Epic 2 - Cross-run version consistency checks

| ID | Task | Status | Notes |
|---|---|---|---|
| E2-T1 | Persist run binary version in registry state record | pending | include in `~/.loopmux/runs/state/*.json` |
| E2-T2 | Show run version in `loopmux runs ls` output | pending | compact format |
| E2-T3 | Highlight mismatches in fleet TUI list/details | pending | visual warning when versions differ |
| E2-T4 | Add optional filter for mismatches or same-version-only view | pending | operator triage |

## Epic 3 - Fleet TUI UX upgrades

| ID | Task | Status | Notes |
|---|---|---|---|
| E3-T1 | Split layout into list panel + details panel | pending | clearer at-a-glance context |
| E3-T2 | Add search/filter controls by name/id/target/state | pending | faster navigation with many runs |
| E3-T3 | Add safer destructive flow (stop confirmation) | pending | reduce accidental stops |
| E3-T4 | Add copy helpers (`id`, command snippet) | pending | operator speed |
| E3-T5 | Add summary counters (active/holding/stale/mismatch) | pending | fleet health overview |

## Execution order

1. Finish Epic 0 (this branch).
2. Implement Epic 1 + Epic 2 together (shared version surface).
3. Implement Epic 3 in iterative slices.
