# Remote tmux discovery over SSH (future)

## Goal
- Support optional discovery of remote tmux sessions over SSH for workflows that run terminals such as Ghostty or iTerm2 while hopping across machines.

## Why
- Local tmux-only discovery misses sessions on remote hosts reached through SSH.
- Operators need one place to see candidate tmux targets without manually checking each host.

## Scope (proposed)
- Add opt-in remote scan capability (disabled by default).
- Discover tmux sessions/windows/panes on selected hosts only.
- Merge remote results with local discovery output while preserving host identity.

## Config ideas
- `remote_scan.enabled` (bool; default `false`)
- `remote_scan.hosts` (allowlist of hosts)
- `remote_scan.exclude_hosts` (denylist override)
- `remote_scan.interval_seconds` (scan cadence)
- `remote_scan.timeout_seconds` (per-host timeout)
- `remote_scan.max_concurrency` (parallel SSH fanout limit)
- `remote_scan.hosts[].enabled` (per-host on/off)
- `remote_scan.hosts[].ssh_user` / `remote_scan.hosts[].ssh_port`

## Safety and cost controls
- Keep scans off unless explicitly enabled.
- Use non-interactive SSH defaults (`BatchMode=yes`) to avoid hangs.
- Bound time and concurrency to reduce network/host load.
- Log per-host latency/failures for observability.

## UX notes
- Show discovered targets with host prefix (for example `host-a:session.window.pane`).
- Provide clear indicators for unreachable hosts and skipped hosts.
- Offer command flags to temporarily disable remote scanning even when configured.

## Non-goals (initial)
- Automatic host discovery across the entire network.
- Credential provisioning/secret management beyond standard SSH setup.

## Acceptance criteria (future implementation)
- Remote scanning can be enabled/disabled globally and per host.
- Only allowlisted hosts are scanned.
- Discovery remains responsive under timeout/concurrency caps.
- Tests cover config parsing, host filtering, and failure/timeouts.
