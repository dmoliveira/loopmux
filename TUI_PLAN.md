# loopmux TUI Plan

## Goals
- Provide a concise, always-visible status bar with progress and state.
- Keep updates readable without spamming the terminal.
- Support runtime controls (pause, resume, stop, next, edit).
- Preserve non-TTY fallback to plain logs.

## Layout (Initial)
- **Top status bar** (single line)
  - `RUNNING | 5/10 [=====.....] 50% | trigger: "Concluded|What is next" | last: 08:28:11 | target: ai:5.0`
  - Trigger text truncated to max length with ellipsis.
- **Body log** (scrolling)
  - Recent events: match, delay, sent, error.
  - Each entry: timestamp, rule id, prompt preview (trimmed).
- **Footer** (shortcuts)
  - `p:pause r:resume s:stop n:next e:edit i:iters t:trigger c:reload q:quit`

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

## Implementation Approach
### Option A: Minimal ANSI TUI (fast)
- Custom rendering with ANSI clear + cursor positioning.
- Manual input handling with non-blocking stdin.

### Option B: ratatui + crossterm (robust)
- Proper layout management, easier future expansion.
- More dependencies, but standard for Rust TUIs.

## Suggested MVP Scope
- TTY detection + ANSI layout (Option A)
- Minimal input handling (`p`, `r`, `s`, `q`)
- Status bar + body log + footer
- No config editing yet (stub commands)

## Next Iterations
- Edit prompt/trigger at runtime
- Progress bar gradient and color states
- Expand to ratatui if needed
