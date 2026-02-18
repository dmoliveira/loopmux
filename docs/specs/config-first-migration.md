# Migration Guide: Single Run to Config-First Profiles

## Who this is for

Use this guide if you currently run loopmux with:

- `loopmux run --config loop.yaml`
- inline flags like `loopmux run -t ... --prompt ... --trigger ...`

and want to move to `loopmux` commandless startup from `~/.config/loopmux/config.yaml`.

## Before and after

Before:

```bash
loopmux run --config loop.yaml
```

After:

```bash
loopmux
```

Loopmux loads `~/.config/loopmux/config.yaml`, selects enabled profiles that match current cwd, validates them, then starts each selected profile.

## Step 1: move your current config into the default path

```bash
mkdir -p ~/.config/loopmux
cp loop.yaml ~/.config/loopmux/config.yaml
```

If your existing file defines one run, it can stay top-level as your main profile.

## Step 2: add profile metadata

```yaml
id: main
enabled: true
when:
  cwd_matches:
    - ~/Codes/Projects/*
```

- `id` identifies the profile in logs and TUI status.
- `enabled: false` keeps a profile in config without auto-starting it.
- `when.cwd_matches` supports `*` wildcard matching.

## Step 3: split optional runs into `runs` (or `events`)

```yaml
runs:
  - id: docs
    enabled: false
    target: "ai:6.1"
    iterations: 20
    default_action:
      prompt: "Polish docs and examples."
```

`events` is accepted as an alias of `runs`.

## Step 4: reuse multiple files with `imports`

```yaml
imports:
  - ~/.config/loopmux/runs/work.yaml
  - ~/.config/loopmux/runs/personal.yaml
```

Imported files can define additional `runs` or `events` blocks.

## Step 5: validate and inspect before startup

Use the new commands:

```bash
loopmux config list
loopmux config validate
```

For full audits (including disabled/non-matching profiles):

```bash
loopmux config list --all
loopmux config validate --all
```

## Field parity notes

Profiles reuse run YAML options. Common fields include:

- routing: `target`, `targets`, `files`
- cadence: `iterations`, `poll`, `trigger_confirm_seconds`
- matching: `rules`, `rule_eval`, `trigger_edge`, `recheck_before_send`
- runtime UX: `tail`, `head`, `once`, `single_line`, `tui`, `name`

## Troubleshooting

- No profile starts: check `enabled` and `when.cwd_matches` against current cwd.
- Startup aborts on one bad profile: run `loopmux config validate --all` to see per-profile errors.
- Multiple TUI profiles selected: only one profile should enable `tui` for the same terminal session.
