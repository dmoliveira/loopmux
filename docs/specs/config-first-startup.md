# Config-First Startup

## Goal

Allow `loopmux` to start without subcommands by loading `~/.config/loopmux/config.yaml` and launching all matching enabled profiles.

## Profile model

- Top-level config can define one main run profile using the existing run config schema.
- Additional profiles are declared in `runs:`.
- `events:` is supported as an alias of `runs:`.
- `imports:` can include additional YAML files that contribute `runs:`/`events:` profiles.

## Profile controls

- `id`: stable profile identifier.
- `enabled`: defaults to `true`; disabled profiles are ignored.
- `when.cwd_matches`: optional wildcard patterns (supports `*`) for path-based selection.
- Remaining keys reuse existing loop run config fields (`target`, `rules`, `poll`, `tail`, `trigger_confirm_seconds`, `once`, `single_line`, `tui`, etc.).

## Runtime behavior

1. `loopmux` loads the default config and all imported profiles.
2. It filters enabled profiles that match current working directory.
3. It validates all selected profiles first.
4. If validation succeeds, it starts each selected profile as an independent `loopmux run --config <runtime-file>` process.
5. If any selected profile fails validation, startup aborts with per-profile errors.

## UX updates

- Status output includes `profile=<id>` plus matched rule details.
- TUI status bar includes `run <id>` to clarify which profile is active.
- `loopmux config doctor` surfaces common workspace profile issues with fix-oriented messages.

## Acceptance criteria

- `loopmux` (no subcommand) starts all enabled cwd-matching profiles from default path.
- `loopmux run --config ...` behavior stays backward compatible.
- Imports work and do not loop on cyclic references.
- `runs` and `events` profile lists are both supported.
- Tests cover wildcard matching and workspace profile loading/merging.
