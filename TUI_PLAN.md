# loopmux TUI Plan

## Goals
- [ ] Provide a concise, always-visible status bar with progress and state.
- [ ] Keep updates readable without spamming the terminal.
- [ ] Support runtime controls (pause, resume, stop, next, edit).
- [ ] Preserve non-TTY fallback to plain logs.

## Layout (Initial)
- **Top status bar** (single line)
  - `RUNNING | 5/10 [=====.....] 50% | trigger: "Concluded|What is next" | last: 08:28:11 | target: ai:5.0`
  - Trigger text truncated to max length with ellipsis.
- **Body log** (scrolling)
  - Recent events: match, delay, sent, error.
  - Each entry: timestamp, rule id, prompt preview (trimmed).
- **Footer** (shortcuts)
  - `p:pause r:resume s:stop n:next e:edit i:iters t:trigger c:reload q:quit`

## Visual Design
- Nerd Font icons for state and actions.
- Use a small, consistent color palette that works in common terminal themes.
- Prefer bold for labels and normal weight for values.
- Use separators and padding for clarity without wasting columns.

### Theme Compatibility
- Avoid hardcoded background colors; use foreground colors only.
- Use ANSI 8-color palette with bright variants for contrast.
- Provide a no-color fallback when `NO_COLOR` is set.

### Responsive Status Bar
- **Compact** (<= 80 cols)
  - `▶ RUN 5/10 [===..] 50% | trg: Concluded… | ai:5.0`
- **Standard** (81-120 cols)
  - `▶ RUNNING 5/10 [=====.....] 50% | trigger: Concluded… | last: 08:28:11 | ai:5.0`
- **Wide** (> 120 cols)
  - `▶ RUNNING | iter 5/10 [=====.....] 50% | trigger: Concluded… | last: 08:28:11 | start: 08:10:02 | target: ai:5.0`

### Truncation + Priority Rules
- Always show: state, progress, target.
- If space is limited, drop: start time, last time, then trigger.
- Trigger text max length: 24 (compact), 40 (standard), 60 (wide).
- Prompt preview max length: 80 chars in log.

### Icon + Color Guide (Nerd Font)
- Running: `󰐊` green
- Paused: `󰏤` yellow
- Waiting/Delay: `󰔟` blue
- Error: `󰅚` red
- Stopped: `󰩈` gray

### Icon Fallbacks
- If Nerd Font not detected, use ASCII: `>` (run), `||` (pause), `...` (delay), `!` (error), `x` (stop).

## Interaction Model
- `p`: pause (no sends, still updating status)
- `r`: resume
- `s`: stop (graceful)
- `n`: force next send
- `e`: edit prompt (in-memory)
- `i`: update remaining iterations
- `t`: update trigger regex
- `c`: reload config from file
- `q`: quit immediately

## States
- `RUNNING`, `PAUSED`, `WAITING`, `DELAY`, `SENDING`, `ERROR`, `STOPPED`

## Data Model
- loop status: state, start time, last match, last send
- counters: current iteration, total iterations
- target: session/window/pane
- trigger: active match rule + pattern
- delays: remaining time, delay strategy

## Output Behavior
- TTY: draw top bar + footer; body log scrolls.
- Non-TTY: fall back to current log output.
- Single-line mode remains available as a lightweight alternative.

### Log Line Format
- `[08:28:11] sent rule=success-path prompt="Continue with next iteration"`
- `[08:28:22] delay rule=review-path detail="delay 300s"`
- `[08:28:30] match rule=failure-path`

## Implementation Approach
### Option A: Minimal ANSI TUI (fast)
- Custom rendering with ANSI clear + cursor positioning.
- Manual input handling with non-blocking stdin.

### Option B: ratatui + crossterm (robust)
- Proper layout management, easier future expansion.
- More dependencies, but standard for Rust TUIs.

## Suggested MVP Scope
- [ ] TTY detection + ANSI layout (Option A)
- [ ] Minimal input handling (`p`, `r`, `s`, `q`)
- [ ] Status bar + body log + footer
- [ ] No config editing yet (stub commands)
- [ ] Responsive bar sizing for compact/standard/wide
- [ ] Nerd Font icons with safe fallback
- [ ] Hide/show log body with `l`

## Tests and Validation
- [ ] Snapshot tests for rendered bars (compact/standard/wide).
- [ ] Terminal capability detection tests (color + unicode).
- [ ] Fallback tests when Nerd Font icons are unavailable.
- [ ] Optional: use `vhs` or `asciinema` scripts for E2E captures.

### Snapshot Strategy
- Render status bar to strings and compare using a snapshot tool (e.g., `insta`).
- Capture TTY sessions with `vhs` for demo artifacts.

## Next Iterations
- [ ] Edit prompt/trigger at runtime
- [ ] Progress bar gradient and color states
- [ ] Expand to ratatui if needed
- [ ] Config persistence for edits
