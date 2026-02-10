# loopmux TUI Plan

## Goals
- [ ] Provide a concise, always-visible status bar with progress and state.
- [ ] Keep updates readable without spamming the terminal.
- [ ] Support runtime controls (pause, resume, stop, next, edit).
- [ ] Preserve non-TTY fallback to plain logs.
- [ ] Maintain a clean single-bar layout across terminal sizes.

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
- Prefer foreground-only styling unless 256-color support is detected.
- Use ANSI 8-color palette with bright variants for contrast.
- Provide a no-color fallback when `NO_COLOR` is set.
- Disable background fills when `TERM=dumb` or 256 colors are unavailable.
- Use bold only when ANSI bold is supported; otherwise plain text.

### Visual Bar Styling (Reference)
- Single status bar line with subtle background fill (only when supported).
- State color accent on label/icon (RUN green, PAUSE yellow, WAIT blue, ERROR red).
- Bold labels, normal values.
- Use Nerd Font state icon only; ASCII fallback.
- Ellipsis fallback: use `...` when Unicode ellipsis is not supported.

### Layout Refinements
- Top bar is always a single line; body log starts immediately after.
- Footer is a single line at bottom with dim shortcuts.
- No extra spacer rows unless terminal height is large.
- Log area height = terminal height - 2 (bar + footer).

### Bar Format (Preferred)
- `󰐊 RUN | iter 5/10 [=====.....] 50% | trigger: Concluded… | last: 15s | target: ai:5.0`
- Compact: `RUN 5/10 [===..] 50% | trg: Concl… | ai:5.0`

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
- If width < 60, collapse to single-line mode and hide log body.

### Icon + Color Guide (Nerd Font)
- Running: `󰐊` green
- Paused: `󰏤` yellow
- Waiting/Delay: `󰔟` blue
- Error: `󰅚` red
- Stopped: `󰩈` gray

### Icon Fallbacks
- If Nerd Font not detected, use ASCII: `>` (run), `||` (pause), `...` (delay), `!` (error), `x` (stop).

### Color Palette (256-color safe)
- Background bar: `48;5;236`
- Foreground text: `38;5;250`
- Dim footer: `38;5;244`
- State accents: green `38;5;71`, yellow `38;5;180`, red `38;5;203`, blue `38;5;75`
 
### Capability Detection
- 256-color if `TERM` contains `256color` or `COLORTERM` is set.
- Nerd Font icons if `LOOPMUX_NO_NERD_FONT` is not set.
- No-color if `NO_COLOR` is set.
- Unicode ellipsis if locale is UTF-8; otherwise ASCII.

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
- If width is too small for the bar, fall back to single-line mode.

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
- [ ] Single-line fallback when width < 60

## Acceptance Criteria
- [ ] Bar renders in a single line with background fill when supported.
- [ ] Compact/standard/wide layouts switch correctly at 80/120 columns.
- [ ] Colors degrade gracefully with `NO_COLOR` and non-256 terminals.
- [ ] Logs never push footer off-screen.
- [ ] Single-line fallback works in narrow terminals.

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
