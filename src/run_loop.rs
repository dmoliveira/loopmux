use std::collections::HashSet;

use anyhow::{Context, Result};

use crate::*;

pub(crate) struct ExternalControlState<'a> {
    pub(crate) loop_state: &'a mut LoopState,
    pub(crate) hold_started: &'a mut Option<std::time::Instant>,
    pub(crate) held_total: &'a mut std::time::Duration,
    pub(crate) send_count: &'a mut u32,
    pub(crate) last_hash_by_target: &'a mut std::collections::HashMap<String, String>,
    pub(crate) trigger_edge_active: &'a mut HashSet<String>,
    pub(crate) trigger_confirm_pending_since:
        &'a mut std::collections::HashMap<String, std::time::Instant>,
    pub(crate) active_rule: &'a mut Option<String>,
    pub(crate) active_rule_by_target: &'a mut std::collections::HashMap<String, Option<String>>,
    pub(crate) backoff_state: &'a mut std::collections::HashMap<String, BackoffState>,
}

pub(crate) fn apply_external_control(
    command: FleetControlCommand,
    state: ExternalControlState<'_>,
) -> bool {
    let ExternalControlState {
        loop_state,
        hold_started,
        held_total,
        send_count,
        last_hash_by_target,
        trigger_edge_active,
        trigger_confirm_pending_since,
        active_rule,
        active_rule_by_target,
        backoff_state,
    } = state;
    match command {
        FleetControlCommand::Stop => true,
        FleetControlCommand::Hold => {
            if hold_started.is_none() {
                *hold_started = Some(std::time::Instant::now());
            }
            *loop_state = LoopState::Holding;
            false
        }
        FleetControlCommand::Resume => {
            if let Some(started_at) = hold_started.take() {
                *held_total += started_at.elapsed();
            }
            *loop_state = LoopState::Running;
            false
        }
        FleetControlCommand::Next => {
            last_hash_by_target.clear();
            trigger_edge_active.clear();
            trigger_confirm_pending_since.clear();
            *active_rule = None;
            active_rule_by_target.clear();
            backoff_state.clear();
            false
        }
        FleetControlCommand::Renew => {
            *send_count = 0;
            last_hash_by_target.clear();
            trigger_edge_active.clear();
            trigger_confirm_pending_since.clear();
            *active_rule = None;
            active_rule_by_target.clear();
            backoff_state.clear();
            false
        }
    }
}

pub(crate) fn sleep_with_heartbeat(
    registry: &FleetRunRegistry,
    target: &str,
    state: LoopState,
    sends: u32,
    poll_seconds: u64,
    seconds: u64,
) -> Result<()> {
    if seconds == 0 {
        return Ok(());
    }
    for _ in 0..seconds {
        std::thread::sleep(std::time::Duration::from_secs(1));
        registry.update(target, state, sends, poll_seconds)?;
    }
    Ok(())
}

pub(crate) fn spawn_exec_in_flight(command: &str) -> Result<ExecInFlight> {
    let child = std::process::Command::new("sh")
        .args(["-lc", command])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start exec command: {command}"))?;
    Ok(ExecInFlight {
        command: command.to_string(),
        child,
        started_at: std::time::Instant::now(),
    })
}

pub(crate) fn summarize_exec_stream(bytes: &[u8], use_unicode: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    let first = trimmed.lines().next().unwrap_or("").trim();
    truncate_text(first, 120, use_unicode)
}

pub(crate) static RAW_MODE_DEPTH: AtomicUsize = AtomicUsize::new(0);
pub(crate) static RAW_MODE_HOOK_INIT: Once = Once::new();
#[cfg(test)]
pub(crate) static RAW_MODE_ENABLE_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static RAW_MODE_DISABLE_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static RAW_MODE_FAIL_ENABLE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static RAW_MODE_FAIL_DISABLE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static RAW_MODE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(not(test))]
pub(crate) fn enable_raw_mode_guarded() -> std::io::Result<()> {
    enable_raw_mode()
}

#[cfg(not(test))]
pub(crate) fn disable_raw_mode_guarded() -> std::io::Result<()> {
    disable_raw_mode()
}

#[cfg(test)]
pub(crate) fn enable_raw_mode_guarded() -> std::io::Result<()> {
    RAW_MODE_ENABLE_CALLS.fetch_add(1, Ordering::SeqCst);
    if RAW_MODE_FAIL_ENABLE.swap(false, Ordering::SeqCst) {
        Err(std::io::Error::other("simulated enable raw mode failure"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn disable_raw_mode_guarded() -> std::io::Result<()> {
    RAW_MODE_DISABLE_CALLS.fetch_add(1, Ordering::SeqCst);
    if RAW_MODE_FAIL_DISABLE.swap(false, Ordering::SeqCst) {
        Err(std::io::Error::other("simulated disable raw mode failure"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn reset_raw_mode_test_state() {
    RAW_MODE_DEPTH.store(0, Ordering::SeqCst);
    RAW_MODE_ENABLE_CALLS.store(0, Ordering::SeqCst);
    RAW_MODE_DISABLE_CALLS.store(0, Ordering::SeqCst);
    RAW_MODE_FAIL_ENABLE.store(false, Ordering::SeqCst);
    RAW_MODE_FAIL_DISABLE.store(false, Ordering::SeqCst);
}

pub(crate) fn install_raw_mode_panic_hook() {
    RAW_MODE_HOOK_INIT.call_once(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            if RAW_MODE_DEPTH.load(Ordering::SeqCst) > 0 {
                let _ = disable_raw_mode_guarded();
                RAW_MODE_DEPTH.store(0, Ordering::SeqCst);
            }
            previous_hook(panic_info);
        }));
    });
}

pub(crate) struct RawModeGuard {
    pub(crate) active: bool,
}

impl RawModeGuard {
    pub(crate) fn acquire(context: &str) -> Result<Self> {
        install_raw_mode_panic_hook();
        if RAW_MODE_DEPTH.fetch_add(1, Ordering::SeqCst) == 0
            && let Err(err) = enable_raw_mode_guarded()
        {
            RAW_MODE_DEPTH.fetch_sub(1, Ordering::SeqCst);
            return Err(err).context(context.to_string());
        }
        Ok(Self { active: true })
    }

    pub(crate) fn release(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let mut previous = RAW_MODE_DEPTH.load(Ordering::SeqCst);
        loop {
            if previous == 0 {
                return Ok(());
            }
            match RAW_MODE_DEPTH.compare_exchange(
                previous,
                previous - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => previous = current,
            }
        }
        if previous == 1 {
            disable_raw_mode_guarded().context("failed to disable raw mode")?;
        }
        Ok(())
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

const TUI_RENDER_RETRY_LIMIT: usize = 3;
const TUI_RENDER_RETRY_DELAY: Duration = Duration::from_millis(15);

pub(crate) fn render_with_retry(
    context: &str,
    mut render_step: impl FnMut() -> std::io::Result<()>,
) -> Result<()> {
    let mut last_error: Option<std::io::Error> = None;
    for attempt in 1..=TUI_RENDER_RETRY_LIMIT {
        match render_step() {
            Ok(()) => {
                if attempt > 1 {
                    eprintln!("loopmux: tui render recovered context={context} attempts={attempt}");
                }
                return Ok(());
            }
            Err(err) => {
                eprintln!(
                    "loopmux: tui render degraded context={context} attempt={attempt}/{TUI_RENDER_RETRY_LIMIT} error={err}"
                );
                last_error = Some(err);
                if attempt < TUI_RENDER_RETRY_LIMIT {
                    std::thread::sleep(TUI_RENDER_RETRY_DELAY);
                }
            }
        }
    }

    let err = last_error.unwrap_or_else(|| std::io::Error::other("unknown render failure"));
    bail!(
        "tui render fallback=exit context={context} attempts={TUI_RENDER_RETRY_LIMIT} error={err}"
    );
}

pub(crate) fn fleet_stop_snippet(run_id: &str) -> String {
    format!("loopmux runs stop {run_id}")
}

pub(crate) fn copy_to_clipboard(value: &str) -> Result<()> {
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to start pbcopy")?;
    let Some(stdin) = child.stdin.as_mut() else {
        bail!("failed to open pbcopy stdin");
    };
    stdin
        .write_all(value.as_bytes())
        .context("failed to write clipboard value")?;
    let status = child.wait().context("failed to wait for pbcopy")?;
    if !status.success() {
        bail!("pbcopy exited with status {status}");
    }
    Ok(())
}

pub(crate) fn jump_to_tmux_target(target: &str) -> Result<()> {
    if std::env::var("TMUX").is_err() {
        bail!("not inside tmux; run this from a tmux client");
    }

    if target == "all sessions/windows/panes" {
        let panes = list_tmux_panes()?;
        let first = panes
            .first()
            .map(|pane| pane.target.clone())
            .ok_or_else(|| anyhow::anyhow!("no tmux panes found to jump"))?;
        return jump_to_tmux_target(&first);
    }

    if let Some(session) = target.strip_suffix(":*.*") {
        let switch_status = std::process::Command::new("tmux")
            .args(["switch-client", "-t", session])
            .status()
            .context("failed to run tmux switch-client")?;
        if !switch_status.success() {
            bail!("tmux switch-client failed for session: {session}");
        }
        return Ok(());
    }

    if let Some(window_target) = target.strip_suffix(".*") {
        let (session, _window) = parse_session_window(window_target)?;
        let switch_status = std::process::Command::new("tmux")
            .args(["switch-client", "-t", session])
            .status()
            .context("failed to run tmux switch-client")?;
        if !switch_status.success() {
            bail!("tmux switch-client failed for session: {session}");
        }
        let window_status = std::process::Command::new("tmux")
            .args(["select-window", "-t", window_target])
            .status()
            .context("failed to run tmux select-window")?;
        if !window_status.success() {
            bail!("tmux select-window failed for {window_target}");
        }
        return Ok(());
    }

    let (session, window, _pane) = parse_target(target)?;
    let window_target = format!("{session}:{window}");
    let switch_status = std::process::Command::new("tmux")
        .args(["switch-client", "-t", session])
        .status()
        .context("failed to run tmux switch-client")?;
    if !switch_status.success() {
        bail!("tmux switch-client failed for session: {session}");
    }
    let window_status = std::process::Command::new("tmux")
        .args(["select-window", "-t", &window_target])
        .status()
        .context("failed to run tmux select-window")?;
    if !window_status.success() {
        bail!("tmux select-window failed for {window_target}");
    }
    let pane_status = std::process::Command::new("tmux")
        .args(["select-pane", "-t", target])
        .status()
        .context("failed to run tmux select-pane")?;
    if !pane_status.success() {
        bail!("tmux select-pane failed for {target}");
    }
    Ok(())
}

pub(crate) fn load_run_history() -> Result<RunHistory> {
    let path = history_path()?;
    load_run_history_from_path(&path)
}

pub(crate) fn load_run_history_from_path(path: &Path) -> Result<RunHistory> {
    if !path.exists() {
        return Ok(RunHistory::default());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read history file: {}", path.display()))?;
    let history: RunHistory = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse history file: {}", path.display()))?;
    Ok(history)
}

pub(crate) fn save_run_history(history: &RunHistory) -> Result<()> {
    let dir = history_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create history dir: {}", dir.display()))?;
    let path = history_path()?;
    let content = serde_json::to_string_pretty(history).context("failed to serialize history")?;
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write history file: {}", path.display()))?;
    Ok(())
}

pub(crate) fn history_signature(args: &RunArgs) -> Option<String> {
    let target = args.target.first()?;
    let prompt = args.prompt.as_ref()?;
    let trigger = args.trigger.as_deref().unwrap_or("");
    let trigger_expr = args.trigger_expr.as_deref().unwrap_or("");
    if trigger.is_empty() && trigger_expr.is_empty() {
        return None;
    }
    Some(format!(
        "target={target}|prompt={prompt}|trigger={trigger}|trigger_expr={trigger_expr}|trigger_exact_line={}|exclude={}|pre={}|post={}|iterations={}|tail={}|head={}|once={}|poll={}|initial_poll={}|trigger_confirm_seconds={}|log_preview_lines={}|trigger_edge={}|recheck_before_send={}|fanout={}|duration={}",
        args.trigger_exact_line,
        args.exclude.as_deref().unwrap_or(""),
        args.pre.as_deref().unwrap_or(""),
        args.post.as_deref().unwrap_or(""),
        args.iterations.map(|v| v.to_string()).unwrap_or_default(),
        args.tail.map(|v| v.to_string()).unwrap_or_default(),
        args.head.map(|v| v.to_string()).unwrap_or_default(),
        args.once,
        args.poll.map(|v| v.to_string()).unwrap_or_default(),
        args.initial_poll.map(|v| v.to_string()).unwrap_or_default(),
        args.trigger_confirm_seconds
            .map(|v| v.to_string())
            .unwrap_or_default(),
        args.log_preview_lines
            .map(|v| v.to_string())
            .unwrap_or_default(),
        !args.no_trigger_edge,
        !args.no_recheck_before_send,
        fanout_label(args.fanout),
        args.duration.as_deref().unwrap_or("")
    ))
}

pub(crate) fn store_run_history(args: &RunArgs) -> Result<()> {
    let Some(signature) = history_signature(args) else {
        return Ok(());
    };

    let mut history = load_run_history()?;
    let limit = args.history_limit.unwrap_or(DEFAULT_HISTORY_LIMIT).max(1);
    history.entries.retain(|entry| {
        history_entry_signature(entry)
            .map(|existing| existing != signature)
            .unwrap_or(true)
    });

    history.entries.insert(
        0,
        HistoryEntry {
            last_run: timestamp_now(),
            target: args.target.first().cloned().unwrap_or_default(),
            prompt: args.prompt.clone().unwrap_or_default(),
            trigger: args.trigger.clone().unwrap_or_default(),
            trigger_expr: args.trigger_expr.clone(),
            trigger_exact_line: Some(args.trigger_exact_line),
            exclude: args.exclude.clone(),
            pre: args.pre.clone(),
            post: args.post.clone(),
            iterations: args.iterations,
            tail: args.tail,
            head: args.head,
            once: args.once,
            poll: args.poll,
            initial_poll: args.initial_poll,
            trigger_confirm_seconds: args.trigger_confirm_seconds,
            log_preview_lines: args.log_preview_lines,
            trigger_edge: Some(!args.no_trigger_edge),
            recheck_before_send: Some(!args.no_recheck_before_send),
            fanout: Some(args.fanout),
            duration: args.duration.clone(),
        },
    );
    if history.entries.len() > limit {
        history.entries.truncate(limit);
    }
    save_run_history(&history)
}

pub(crate) fn history_entry_signature(entry: &HistoryEntry) -> Option<String> {
    Some(format!(
        "target={}|prompt={}|trigger={}|trigger_expr={}|trigger_exact_line={}|exclude={}|pre={}|post={}|iterations={}|tail={}|head={}|once={}|poll={}|initial_poll={}|trigger_confirm_seconds={}|log_preview_lines={}|trigger_edge={}|recheck_before_send={}|fanout={}|duration={}",
        entry.target,
        entry.prompt,
        entry.trigger,
        entry.trigger_expr.as_deref().unwrap_or(""),
        entry.trigger_exact_line.unwrap_or(false),
        entry.exclude.as_deref().unwrap_or(""),
        entry.pre.as_deref().unwrap_or(""),
        entry.post.as_deref().unwrap_or(""),
        entry.iterations.map(|v| v.to_string()).unwrap_or_default(),
        entry.tail.map(|v| v.to_string()).unwrap_or_default(),
        entry.head.map(|v| v.to_string()).unwrap_or_default(),
        entry.once,
        entry.poll.map(|v| v.to_string()).unwrap_or_default(),
        entry
            .initial_poll
            .map(|v| v.to_string())
            .unwrap_or_default(),
        entry
            .trigger_confirm_seconds
            .map(|v| v.to_string())
            .unwrap_or_default(),
        entry
            .log_preview_lines
            .map(|v| v.to_string())
            .unwrap_or_default(),
        entry.trigger_edge.unwrap_or(true),
        entry.recheck_before_send.unwrap_or(true),
        fanout_label(entry.fanout.unwrap_or(FanoutMode::Matched)),
        entry.duration.as_deref().unwrap_or("")
    ))
}

pub(crate) fn select_history_entry(limit: usize) -> Result<HistoryEntry> {
    let history = load_run_history()?;
    if history.entries.is_empty() {
        bail!("no run history found; run a command once before using --tui history picker");
    }

    println!("loopmux history (most recent first):");
    let visible = history
        .entries
        .iter()
        .take(limit.max(1))
        .collect::<Vec<_>>();
    for (idx, entry) in visible.iter().enumerate() {
        let prompt = truncate_text(&entry.prompt, 70, true);
        let trigger = if let Some(expr) = &entry.trigger_expr {
            format!("expr:{expr}")
        } else {
            entry.trigger.clone()
        };
        println!(
            "{}. [{}] target={} trigger={} prompt={}",
            idx + 1,
            entry.last_run,
            entry.target,
            trigger,
            prompt
        );
    }

    loop {
        print!("Select history number (1-{}, q to cancel): ", visible.len());
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("failed to read history selection")?;
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("q") {
            bail!("history selection cancelled");
        }
        let Ok(index) = trimmed.parse::<usize>() else {
            println!("Invalid selection: {trimmed}");
            continue;
        };
        if index == 0 || index > visible.len() {
            println!("Selection out of range: {index}");
            continue;
        }
        return Ok(visible[index - 1].clone());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldTransition {
    Unchanged,
    EnterHolding,
    ExitHolding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HoldActionPlan {
    pub(crate) transition: HoldTransition,
    pub(crate) force_rescan: bool,
    pub(crate) break_wait: bool,
}

pub(crate) fn plan_hold_action(
    action: TuiAction,
    currently_holding: bool,
    break_wait_on_resume: bool,
) -> Option<HoldActionPlan> {
    match action {
        TuiAction::Pause => Some(HoldActionPlan {
            transition: if currently_holding {
                HoldTransition::Unchanged
            } else {
                HoldTransition::EnterHolding
            },
            force_rescan: false,
            break_wait: false,
        }),
        TuiAction::Resume => Some(HoldActionPlan {
            transition: if currently_holding {
                HoldTransition::ExitHolding
            } else {
                HoldTransition::Unchanged
            },
            force_rescan: true,
            break_wait: break_wait_on_resume,
        }),
        TuiAction::HoldToggle => Some(HoldActionPlan {
            transition: if currently_holding {
                HoldTransition::ExitHolding
            } else {
                HoldTransition::EnterHolding
            },
            force_rescan: currently_holding,
            break_wait: currently_holding && break_wait_on_resume,
        }),
        _ => None,
    }
}

pub(crate) fn apply_hold_transition(
    transition: HoldTransition,
    loop_state: &mut LoopState,
    hold_started: &mut Option<std::time::Instant>,
    held_total: &mut std::time::Duration,
) {
    match transition {
        HoldTransition::Unchanged => {
            if hold_started.is_none() {
                *loop_state = LoopState::Running;
            }
        }
        HoldTransition::EnterHolding => {
            if hold_started.is_none() {
                *hold_started = Some(std::time::Instant::now());
            }
            *loop_state = LoopState::Holding;
        }
        HoldTransition::ExitHolding => {
            if let Some(started_at) = hold_started.take() {
                *held_total += started_at.elapsed();
            }
            *loop_state = LoopState::Running;
        }
    }
}

pub(crate) fn run_loop(config: ResolvedConfig, identity: RunIdentity) -> Result<()> {
    let mut send_count: u32 = 0;
    let max_sends = config.iterations.unwrap_or(u32::MAX);
    let mut last_hash_by_target: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut trigger_edge_active: HashSet<String> = HashSet::new();
    let mut trigger_confirm_pending_since: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    let mut active_rule_by_target: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut active_rule: Option<String> = None;
    let mut backoff_state: std::collections::HashMap<String, BackoffState> =
        std::collections::HashMap::new();
    let mut exec_in_flight: Option<ExecInFlight> = None;
    let mut exec_running_ticks: u32 = 0;
    let mut injection_filter = InjectionFilterState::default();
    let mut prompt_editor = PromptEditorState::new(
        build_prompt(&config.default_action),
        config.prompt_edit_max_chars,
    );
    let mut logger = Logger::new(config.logging.clone())?;
    let mut fleet_registry = FleetRunRegistry::new(identity.clone(), config.profile_id.clone())?;
    let ui_mode = resolve_ui_mode(
        config.tui,
        config.single_line,
        std::io::stdout().is_terminal(),
    );
    let log_icon_mode = detect_icon_mode();
    let log_use_unicode = supports_unicode();
    let mut loop_state = LoopState::Running;
    let mut tui = if ui_mode == UiMode::Tui {
        Some(TuiState::new(&config)?)
    } else {
        None
    };

    let start = OffsetDateTime::now_utc();
    let start_timestamp = start
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into());
    if ui_mode == UiMode::Plain {
        println!("loopmux: running on {}", config.target_label);
        println!("loopmux: version {}", LOOPMUX_VERSION);
        println!("loopmux: run {} ({})", identity.name, identity.id);
        if let Some(command) = config.exec_command.as_deref() {
            println!("loopmux: mode exec command=\"{}\"", command);
            println!("loopmux: successful command exits count as sends; non-zero exits are logged");
        }
        if config.infinite {
            println!("loopmux: iterations = infinite");
        } else {
            println!("loopmux: iterations = {max_sends}");
        }
        println!("loopmux: started at {start_timestamp}");
    } else if ui_mode == UiMode::Tui
        && let Some(tui_state) = tui.as_mut()
    {
        tui_state.push_log(format!(
            "[{}] started target={} run={} ({})",
            start_timestamp, config.target_label, identity.name, identity.id
        ));
    }
    logger.log(LogEvent::started(&config, start_timestamp.clone()))?;
    let run_started = std::time::Instant::now();
    let mut held_total = std::time::Duration::from_secs(0);
    let mut hold_started: Option<std::time::Instant> = None;
    let mut first_wait_cycle = true;
    fleet_registry.update(&config.target_label, loop_state, send_count, config.poll)?;

    while config.infinite || send_count < max_sends {
        fleet_registry.update(&config.target_label, loop_state, send_count, config.poll)?;
        let mut force_rescan = false;
        let active_elapsed = effective_elapsed(run_started, held_total, hold_started);
        if let Some(tui_state) = tui.as_mut() {
            tui_state.refresh_process_usage_summary();
        }
        if let Some(limit) = config.duration
            && active_elapsed >= limit
        {
            if ui_mode == UiMode::Tui
                && let Some(tui_state) = tui.as_mut()
            {
                let elapsed = format_std_duration(active_elapsed);
                tui_state.push_log(format!(
                    "[{}] stopped reason=duration sends={} elapsed={}",
                    timestamp_now(),
                    send_count,
                    elapsed
                ));
                tui_state.update(
                    LoopState::Stopped,
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    active_elapsed,
                    None,
                )?;
            }
            logger.log(LogEvent::stopped(&config, "duration", send_count))?;
            break;
        }

        if let Some(command) = fleet_registry.consume_control_command()? {
            let stop = apply_external_control(
                command,
                ExternalControlState {
                    loop_state: &mut loop_state,
                    hold_started: &mut hold_started,
                    held_total: &mut held_total,
                    send_count: &mut send_count,
                    last_hash_by_target: &mut last_hash_by_target,
                    trigger_edge_active: &mut trigger_edge_active,
                    trigger_confirm_pending_since: &mut trigger_confirm_pending_since,
                    active_rule: &mut active_rule,
                    active_rule_by_target: &mut active_rule_by_target,
                    backoff_state: &mut backoff_state,
                },
            );
            if let Some(tui_state) = tui.as_mut() {
                tui_state.push_log(format!(
                    "[{}] control command={} source=fleet-manager",
                    timestamp_now(),
                    fleet_command_label(command)
                ));
            }
            logger.log(LogEvent::status(
                &config,
                format!("control command={}", fleet_command_label(command)),
            ))?;
            if stop {
                logger.log(LogEvent::stopped(&config, "external stop", send_count))?;
                break;
            }
        }
        if ui_mode == UiMode::Tui && loop_state == LoopState::Holding {
            let mut open_fleet_manager = false;
            if let Some(tui_state) = tui.as_mut() {
                sync_tui_overlays(tui_state, &injection_filter, &prompt_editor);
                if let Some(action) =
                    tui_state.poll_input(prompt_editor.open, prompt_editor.confirm.is_some())?
                {
                    if let Some(plan) = plan_hold_action(action, hold_started.is_some(), false) {
                        apply_hold_transition(
                            plan.transition,
                            &mut loop_state,
                            &mut hold_started,
                            &mut held_total,
                        );
                        if plan.force_rescan {
                            force_rescan = true;
                        }
                    } else {
                        match action {
                            TuiAction::Fleet => {
                                open_fleet_manager = true;
                            }
                            TuiAction::Stop => {
                                tui_state.push_log(format!(
                                    "[{}] stopped reason=manual",
                                    timestamp_now()
                                ));
                                logger.log(LogEvent::stopped(&config, "manual", send_count))?;
                                tui_state.update(
                                    LoopState::Stopped,
                                    &config,
                                    send_count,
                                    max_sends,
                                    active_rule.as_deref(),
                                    effective_elapsed(run_started, held_total, hold_started),
                                    None,
                                )?;
                                break;
                            }
                            TuiAction::Quit => {
                                if prompt_editor.open {
                                    prompt_editor.close();
                                    continue;
                                }
                                if injection_filter.popup_open {
                                    injection_filter.close_popup();
                                    continue;
                                }
                                tui_state
                                    .push_log(format!("[{}] stopped reason=quit", timestamp_now()));
                                logger.log(LogEvent::stopped(&config, "quit", send_count))?;
                                break;
                            }
                            TuiAction::Next => {
                                if prompt_editor.open && prompt_editor.confirm.is_some() {
                                    prompt_editor.confirm_no();
                                    continue;
                                }
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                loop_state = LoopState::Running;
                                force_rescan = true;
                            }
                            TuiAction::Renew => {
                                send_count = 0;
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                tui_state.push_log(format!(
                                    "[{}] renewed counter reason=manual",
                                    timestamp_now()
                                ));
                            }
                            TuiAction::ActiveListToggle => {
                                if prompt_editor.open {
                                    continue;
                                }
                                if injection_filter.popup_open {
                                    injection_filter.close_popup();
                                } else {
                                    injection_filter.open_popup();
                                }
                            }
                            TuiAction::ActiveListUp => {
                                if prompt_editor.open {
                                    prompt_editor.select_up();
                                } else if injection_filter.popup_open {
                                    injection_filter.move_up();
                                }
                            }
                            TuiAction::ActiveListDown => {
                                if prompt_editor.open {
                                    prompt_editor.select_down();
                                } else if injection_filter.popup_open {
                                    injection_filter.move_down();
                                }
                            }
                            TuiAction::ActiveListLeft => {
                                if injection_filter.popup_open {
                                    injection_filter.move_left();
                                }
                            }
                            TuiAction::ActiveListRight => {
                                if injection_filter.popup_open {
                                    injection_filter.move_right();
                                }
                            }
                            TuiAction::ActiveListToggleSelection => {
                                if prompt_editor.open {
                                    prompt_editor.use_selection();
                                } else if injection_filter.popup_open {
                                    injection_filter.toggle_current_selection();
                                }
                            }
                            TuiAction::ActiveListEnableAll => {
                                if injection_filter.popup_open {
                                    injection_filter.enable_all();
                                }
                            }
                            TuiAction::ActiveListDisableAll => {
                                if injection_filter.popup_open {
                                    injection_filter.disable_all();
                                }
                            }
                            TuiAction::ActiveListClose => {
                                if prompt_editor.open {
                                    prompt_editor.close();
                                } else {
                                    injection_filter.close_popup();
                                }
                            }
                            TuiAction::PromptEditorToggle => {
                                injection_filter.close_popup();
                                prompt_editor.toggle_open();
                            }
                            TuiAction::PromptEditorClearHistory => {
                                if prompt_editor.open {
                                    prompt_editor.request_clear_history();
                                }
                            }
                            TuiAction::PromptEditorUndo => {
                                if prompt_editor.open {
                                    prompt_editor.undo();
                                }
                            }
                            TuiAction::PromptEditorConfirmYes => {
                                if prompt_editor.open {
                                    prompt_editor.confirm_yes();
                                }
                            }
                            TuiAction::PromptEditorConfirmNo => {
                                if prompt_editor.open {
                                    prompt_editor.confirm_no();
                                }
                            }
                            TuiAction::PromptEditorBackspace => {
                                if prompt_editor.open {
                                    prompt_editor.backspace();
                                }
                            }
                            TuiAction::PromptEditorInput(ch) => {
                                if prompt_editor.open && !ch.is_control() {
                                    prompt_editor.input_char(ch);
                                }
                            }
                            TuiAction::PromptEditorDeleteSelected => {
                                if prompt_editor.open {
                                    prompt_editor.request_delete_selected();
                                }
                            }
                            TuiAction::ToggleLogView => {
                                tui_state.toggle_log_view();
                            }
                            TuiAction::Pause | TuiAction::Resume | TuiAction::HoldToggle => {}
                            TuiAction::Redraw => {}
                        }
                    }
                }
                sync_tui_overlays(tui_state, &injection_filter, &prompt_editor);
                tui_state.update(
                    loop_state,
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    effective_elapsed(run_started, held_total, hold_started),
                    None,
                )?;
            }
            if open_fleet_manager {
                if let Err(err) = run_fleet_manager_tui_embedded()
                    && let Some(tui_state) = tui.as_mut()
                {
                    tui_state.push_log(format!(
                        "[{}] fleet manager error=\"{}\"",
                        timestamp_now(),
                        truncate_text(&err.to_string(), 100, true)
                    ));
                }
                if let Some(tui_state) = tui.as_mut() {
                    tui_state
                        .push_log(format!("[{}] returned from fleet manager", timestamp_now()));
                }
                continue;
            }
            if force_rescan {
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        if let Some(exec_command) = config.exec_command.as_deref() {
            if loop_state != LoopState::Holding {
                if let Some(in_flight) = exec_in_flight.as_mut() {
                    if let Some(status) = in_flight
                        .child
                        .try_wait()
                        .context("failed to poll exec command")?
                    {
                        let finished = exec_in_flight
                            .take()
                            .ok_or_else(|| anyhow::anyhow!("exec process state lost"))?;
                        let runtime = finished.started_at.elapsed();
                        let output = finished
                            .child
                            .wait_with_output()
                            .context("failed to read exec command output")?;
                        let stdout_preview = summarize_exec_stream(&output.stdout, log_use_unicode);
                        let stderr_preview = summarize_exec_stream(&output.stderr, log_use_unicode);
                        if status.success() {
                            exec_running_ticks = 0;
                            send_count = send_count.saturating_add(1);
                            active_rule = Some("exec:ok".to_string());
                            let detail = format!(
                                "command=\"{}\" exit=0 duration_ms={} stdout=\"{}\"",
                                finished.command,
                                runtime.as_millis(),
                                stdout_preview
                            );
                            if ui_mode == UiMode::Plain {
                                println!(
                                    "loopmux: exec triggered ({}/{}) {}",
                                    send_count,
                                    if config.infinite {
                                        "infinite".to_string()
                                    } else {
                                        max_sends.to_string()
                                    },
                                    detail
                                );
                            }
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] exec-triggered sends={} detail=\"{}\"",
                                    timestamp_now(),
                                    send_count,
                                    truncate_text(&detail, 120, log_use_unicode)
                                ));
                            }
                            logger.log(LogEvent::exec(&config, "exec-triggered", detail))?;
                        } else {
                            exec_running_ticks = 0;
                            active_rule = Some("exec:fail".to_string());
                            let exit_label = status
                                .code()
                                .map(|code| code.to_string())
                                .unwrap_or_else(|| "signal".to_string());
                            let detail = format!(
                                "command=\"{}\" exit={} duration_ms={} stderr=\"{}\" stdout=\"{}\"",
                                finished.command,
                                exit_label,
                                runtime.as_millis(),
                                stderr_preview,
                                stdout_preview
                            );
                            if ui_mode == UiMode::Plain {
                                println!("loopmux: exec failed {}", detail);
                            }
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] exec-failed detail=\"{}\"",
                                    timestamp_now(),
                                    truncate_text(&detail, 120, log_use_unicode)
                                ));
                            }
                            logger.log(LogEvent::exec(&config, "exec-failed", detail))?;
                        }
                    } else {
                        exec_running_ticks = exec_running_ticks.saturating_add(1);
                        active_rule = Some("exec:running".to_string());
                        let detail = format!(
                            "command=\"{}\" pid={} still-running elapsed={}s",
                            in_flight.command,
                            in_flight.child.id(),
                            in_flight.started_at.elapsed().as_secs()
                        );
                        if exec_running_ticks == 1 || exec_running_ticks.is_multiple_of(3) {
                            logger.log(LogEvent::exec(
                                &config,
                                "exec-still-running",
                                detail.clone(),
                            ))?;
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] {}",
                                    timestamp_now(),
                                    truncate_text(&detail, 120, log_use_unicode)
                                ));
                            }
                        }
                    }
                }
                if exec_in_flight.is_none() && (config.infinite || send_count < max_sends) {
                    let in_flight = spawn_exec_in_flight(exec_command)?;
                    active_rule = Some("exec:started".to_string());
                    let detail = format!(
                        "command=\"{}\" pid={} poll={}s",
                        in_flight.command,
                        in_flight.child.id(),
                        config.poll
                    );
                    logger.log(LogEvent::exec(&config, "exec-started", detail.clone()))?;
                    if ui_mode == UiMode::Plain {
                        println!("loopmux: {detail}");
                    }
                    if let Some(tui_state) = tui.as_mut() {
                        tui_state.push_log(format!(
                            "[{}] {}",
                            timestamp_now(),
                            truncate_text(&detail, 120, log_use_unicode)
                        ));
                    }
                    exec_in_flight = Some(in_flight);
                }
            }

            let elapsed = format_std_duration(active_elapsed);
            let status = status_line(
                &config,
                send_count,
                max_sends,
                active_rule.as_deref(),
                &elapsed,
            );
            if ui_mode == UiMode::SingleLine {
                print!("\r{status}");
                let _ = std::io::stdout().flush();
            }

            if let Some(tui_state) = tui.as_mut() {
                tui_state.update(
                    loop_state,
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    active_elapsed,
                    None,
                )?;
            }

            if ui_mode == UiMode::SingleLine {
                let elapsed =
                    format_std_duration(effective_elapsed(run_started, held_total, hold_started));
                let status = status_line(
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    &elapsed,
                );
                print!("\r{status}");
                let _ = std::io::stdout().flush();
            }

            let wait_seconds = if first_wait_cycle {
                config.initial_poll
            } else {
                config.poll
            };
            sleep_with_heartbeat(
                &fleet_registry,
                &config.target_label,
                loop_state,
                send_count,
                wait_seconds,
                config.poll,
            )?;
            first_wait_cycle = false;
            continue;
        }

        let mut plans: Vec<SendPlan> = Vec::new();
        let mut matched_sources: HashSet<String> = HashSet::new();
        let mut tmux_recipients: Vec<String> = Vec::new();
        if loop_state != LoopState::Holding {
            tmux_recipients = if let Some(explicit) = &config.explicit_targets {
                explicit.clone()
            } else {
                let panes = match list_tmux_panes() {
                    Ok(value) => value,
                    Err(err) => {
                        let detail = err.to_string();
                        logger.log(LogEvent::error(&config, detail))?;
                        return Err(err);
                    }
                };
                select_targets_for_scope(&config.target_scope, &panes)
            };
            let mut poll_targets = tmux_recipients.clone();
            poll_targets.extend(config.file_sources.iter().map(|path| file_source_key(path)));
            let mut broadcast_plan_keys: HashSet<String> = HashSet::new();

            for target in &poll_targets {
                let output = match capture_source(target, config.capture_window) {
                    Ok(output) => output,
                    Err(err) => {
                        let detail = err.to_string();
                        logger.log(LogEvent::error(&config, detail))?;
                        return Err(err);
                    }
                };
                let output =
                    if config.capture_window.lines() == 1 && config.capture_window.is_tail() {
                        last_non_empty_line(&output)
                    } else {
                        output
                    };
                let hash = hash_output(&output);
                let last_hash = last_hash_by_target.get(target).cloned().unwrap_or_default();
                let hash_changed = !last_hash.is_empty() && hash != last_hash;
                let has_pending_confirm =
                    has_pending_confirm_for_target(&trigger_confirm_pending_since, target);
                if should_skip_scan_by_hash(
                    config.trigger_edge,
                    &hash,
                    &last_hash,
                    has_pending_confirm,
                ) {
                    emit_trigger_debug(
                        &mut logger,
                        &config,
                        target,
                        "hash_skip",
                        &format!(
                            "hash={} last_hash={} pending_confirm={}",
                            short_hash(&hash),
                            short_hash(&last_hash),
                            has_pending_confirm
                        ),
                    )?;
                    continue;
                }

                let active = active_rule_by_target
                    .get(target)
                    .and_then(|value| value.as_deref());
                let rule_matches = evaluate_rules(&config, &mut logger, &output, active)?;

                let matched_edge_keys = rule_matches
                    .iter()
                    .map(|rule_match| trigger_edge_key(target, rule_match))
                    .collect::<HashSet<_>>();
                refresh_trigger_edges_for_target(
                    &mut trigger_edge_active,
                    target,
                    &matched_edge_keys,
                    hash_changed,
                    config.trigger_edge,
                );
                refresh_trigger_confirm_for_target(
                    &mut trigger_confirm_pending_since,
                    target,
                    &matched_edge_keys,
                );

                if rule_matches.is_empty() {
                    emit_trigger_debug(
                        &mut logger,
                        &config,
                        target,
                        "no_match",
                        &format!(
                            "hash_changed={} pending_confirm={}",
                            hash_changed, has_pending_confirm
                        ),
                    )?;
                    continue;
                }

                injection_filter.observe_trigger_target(target);

                matched_sources.insert(target.clone());
                for rule_match in rule_matches {
                    let edge_key = trigger_edge_key(target, &rule_match);
                    if !edge_guard_allows(&trigger_edge_active, &edge_key, config.trigger_edge) {
                        emit_trigger_debug(
                            &mut logger,
                            &config,
                            target,
                            "edge_blocked",
                            &format!(
                                "rule={}",
                                rule_match.rule.id.as_deref().unwrap_or("<unnamed>")
                            ),
                        )?;
                        continue;
                    }
                    if !confirm_window_elapsed(
                        config.trigger_confirm_seconds,
                        rule_match.rule.confirm_seconds,
                        &edge_key,
                        &mut trigger_confirm_pending_since,
                        std::time::Instant::now(),
                    ) {
                        emit_trigger_debug(
                            &mut logger,
                            &config,
                            target,
                            "confirm_pending",
                            &format!(
                                "rule={} confirm_seconds={}",
                                rule_match.rule.id.as_deref().unwrap_or("<unnamed>"),
                                rule_match
                                    .rule
                                    .confirm_seconds
                                    .unwrap_or(config.trigger_confirm_seconds)
                            ),
                        )?;
                        continue;
                    }

                    let (trigger_preview_lines, trigger_preview) =
                        extract_trigger_preview(&output, config.log_preview_lines, log_use_unicode);

                    let action = rule_match
                        .rule
                        .action
                        .as_ref()
                        .unwrap_or(&config.default_action);
                    let prompt = prompt_editor.effective_prompt(&build_prompt(action));
                    if config.fanout == FanoutMode::Broadcast {
                        let key = format!(
                            "{}|{}",
                            rule_match.rule.id.as_deref().unwrap_or("<unnamed>"),
                            prompt
                        );
                        if !broadcast_plan_keys.insert(key) {
                            continue;
                        }
                    }
                    let delay = rule_match.rule.delay.as_ref().or(config.delay.as_ref());
                    let delay_seconds = if let Some(delay) = delay {
                        Some(compute_delay_seconds(
                            delay,
                            &rule_match,
                            &mut backoff_state,
                        )?)
                    } else {
                        None
                    };
                    plans.push(SendPlan {
                        source_target: target.clone(),
                        rule_id: rule_match.rule.id.clone(),
                        rule_index: rule_match.index,
                        next_rule: rule_match.rule.next.clone(),
                        edge_key,
                        prompt,
                        trigger_preview,
                        trigger_preview_lines,
                        stop_after: rule_match.rule.next.as_deref() == Some("stop"),
                        delay_seconds,
                    });
                    emit_trigger_debug(
                        &mut logger,
                        &config,
                        target,
                        "plan_ready",
                        &format!(
                            "rule={} delay={}",
                            rule_match.rule.id.as_deref().unwrap_or("<unnamed>"),
                            delay_seconds.unwrap_or(0)
                        ),
                    )?;
                }
                if config.trigger_edge {
                    last_hash_by_target.insert(target.clone(), hash);
                }

                if matches!(config.rule_eval, RuleEval::MultiMatch) {
                    active_rule_by_target.insert(target.clone(), None);
                }
            }
        }

        if plans.is_empty() {
            if ui_mode == UiMode::Tui {
                loop_state = LoopState::Waiting;
            }
        } else {
            let mut stop_after = false;
            for plan in plans {
                if loop_state == LoopState::Holding {
                    break;
                }

                if let Some(delay_seconds) = plan.delay_seconds
                    && delay_seconds > 0
                {
                    if ui_mode == UiMode::Tui {
                        loop_state = LoopState::Delay;
                    }
                    let detail = format!("delay {}s", delay_seconds);
                    logger.log(LogEvent::delay_scheduled(
                        &config,
                        plan.rule_id.as_deref(),
                        detail,
                    ))?;
                    if let Some(tui_state) = tui.as_mut() {
                        tui_state.push_log(format!(
                            "[{}] delay rule={} detail=\"delay {}s\"",
                            timestamp_now(),
                            plan.rule_id.as_deref().unwrap_or("<unnamed>"),
                            delay_seconds
                        ));
                        tui_state.update(
                            loop_state,
                            &config,
                            send_count,
                            max_sends,
                            plan.rule_id.as_deref(),
                            effective_elapsed(run_started, held_total, hold_started),
                            None,
                        )?;
                    }
                    sleep_with_heartbeat(
                        &fleet_registry,
                        &config.target_label,
                        loop_state,
                        send_count,
                        config.poll,
                        delay_seconds,
                    )?;
                }

                let recipients = match config.fanout {
                    FanoutMode::Matched => {
                        if file_source_path(&plan.source_target).is_some() {
                            tmux_recipients.clone()
                        } else {
                            vec![plan.source_target.clone()]
                        }
                    }
                    FanoutMode::Broadcast => tmux_recipients.clone(),
                };
                let recipients = recipients
                    .into_iter()
                    .filter(|target| injection_filter.is_allowed(target))
                    .collect::<Vec<_>>();
                if recipients.is_empty() {
                    let detail = format!(
                        "suppressed by active list rule={} source={}",
                        plan.rule_id.as_deref().unwrap_or("<unnamed>"),
                        plan.source_target
                    );
                    logger.log(LogEvent::status(&config, detail.clone()))?;
                    if let Some(tui_state) = tui.as_mut() {
                        tui_state.push_log(format!(
                            "[{}] {}",
                            timestamp_now(),
                            truncate_text(&detail, 120, log_use_unicode)
                        ));
                    }
                    continue;
                }

                let mut sent_any_for_plan = false;
                for target in recipients {
                    if config.recheck_before_send {
                        let output = capture_source(&target, config.capture_window)?;
                        let output = if config.capture_window.lines() == 1
                            && config.capture_window.is_tail()
                        {
                            last_non_empty_line(&output)
                        } else {
                            output
                        };
                        let Some(rule) = config.rules.get(plan.rule_index) else {
                            continue;
                        };
                        if !matches_rule(rule, &output)? {
                            let (recheck_preview_lines, recheck_preview) = extract_trigger_preview(
                                &output,
                                config.log_preview_lines,
                                log_use_unicode,
                            );
                            let detail = format!(
                                "suppressed stale trigger target={} rule={} preview={}L {}",
                                target,
                                plan.rule_id.as_deref().unwrap_or("<unnamed>"),
                                recheck_preview_lines,
                                truncate_text(&recheck_preview, 70, log_use_unicode)
                            );
                            logger.log(LogEvent::status(&config, detail.clone()))?;
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] {}",
                                    timestamp_now(),
                                    truncate_text(&detail, 120, log_use_unicode)
                                ));
                            }
                            emit_trigger_debug(
                                &mut logger,
                                &config,
                                &target,
                                "stale_recheck",
                                &format!(
                                    "rule={} preview_lines={}",
                                    plan.rule_id.as_deref().unwrap_or("<unnamed>"),
                                    recheck_preview_lines
                                ),
                            )?;
                            continue;
                        }
                    }
                    if ui_mode == UiMode::Tui {
                        loop_state = LoopState::Sending;
                    }
                    if let Err(err) = send_prompt(&target, &plan.prompt) {
                        let detail = err.to_string();
                        logger.log(LogEvent::error(&config, detail.clone()))?;
                        if ui_mode == UiMode::Tui {
                            loop_state = LoopState::Error;
                            if let Some(tui_state) = tui.as_mut() {
                                tui_state.push_log(format!(
                                    "[{}] error detail=\"{}\"",
                                    timestamp_now(),
                                    truncate_text(&detail, 120, true)
                                ));
                                tui_state.update(
                                    loop_state,
                                    &config,
                                    send_count,
                                    max_sends,
                                    plan.rule_id.as_deref(),
                                    effective_elapsed(run_started, held_total, hold_started),
                                    None,
                                )?;
                            }
                        }
                        return Err(err);
                    }
                    if ui_mode == UiMode::Tui {
                        loop_state = LoopState::Running;
                    }
                    send_count = send_count.saturating_add(1);
                    sent_any_for_plan = true;
                    active_rule = plan.next_rule.clone();
                    active_rule_by_target
                        .insert(plan.source_target.clone(), plan.next_rule.clone());
                    let now = OffsetDateTime::now_utc();
                    let timestamp = now
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_else(|_| "unknown".into());
                    let elapsed = format_std_duration(effective_elapsed(
                        run_started,
                        held_total,
                        hold_started,
                    ));
                    let status = status_line(
                        &config,
                        send_count,
                        max_sends,
                        plan.rule_id.as_deref(),
                        &elapsed,
                    );
                    if ui_mode == UiMode::SingleLine {
                        print!("\r{status}");
                        let _ = std::io::stdout().flush();
                    } else if ui_mode == UiMode::Tui {
                        if let Some(tui_state) = tui.as_mut() {
                            tui_state.push_log(compact_sent_log(
                                &timestamp,
                                target.as_str(),
                                plan.rule_id.as_deref(),
                                &plan.trigger_preview,
                                plan.trigger_preview_lines,
                                log_use_unicode,
                                log_icon_mode,
                            ));
                            tui_state.update(
                                loop_state,
                                &config,
                                send_count,
                                max_sends,
                                plan.rule_id.as_deref(),
                                effective_elapsed(run_started, held_total, hold_started),
                                None,
                            )?;
                        }
                    } else {
                        println!(
                            "[{}/{}] sent target={} via rule {} at {timestamp} (elapsed {elapsed})",
                            send_count,
                            if config.infinite { 0 } else { max_sends },
                            target,
                            plan.rule_id.as_deref().unwrap_or("<unnamed>")
                        );
                        println!("{status}");
                    }
                    logger.log(LogEvent::status(&config, status))?;
                    emit_trigger_debug(
                        &mut logger,
                        &config,
                        &target,
                        "sent",
                        &format!("rule={}", plan.rule_id.as_deref().unwrap_or("<unnamed>")),
                    )?;
                    logger.log(LogEvent::sent(
                        &config,
                        plan.rule_id.as_deref(),
                        timestamp,
                        &redacted_sent_detail(&target, &plan.prompt),
                    ))?;

                    if !config.infinite && send_count >= max_sends {
                        break;
                    }
                }
                if config.trigger_edge && sent_any_for_plan {
                    trigger_edge_active.insert(plan.edge_key.clone());
                }
                if plan.stop_after {
                    stop_after = true;
                }
                if config.once || (!config.infinite && send_count >= max_sends) {
                    break;
                }
            }

            if stop_after {
                if ui_mode == UiMode::Tui
                    && let Some(tui_state) = tui.as_mut()
                {
                    tui_state.push_log(format!("[{}] stopped reason=stop_rule", timestamp_now()));
                    tui_state.update(
                        LoopState::Stopped,
                        &config,
                        send_count,
                        max_sends,
                        active_rule.as_deref(),
                        effective_elapsed(run_started, held_total, hold_started),
                        None,
                    )?;
                }
                if ui_mode == UiMode::Plain {
                    println!("loopmux: stopping due to stop rule");
                }
                logger.log(LogEvent::stopped(&config, "stop rule matched", send_count))?;
                break;
            }
            if config.once {
                if ui_mode == UiMode::Tui
                    && let Some(tui_state) = tui.as_mut()
                {
                    tui_state.push_log(format!("[{}] stopped reason=once", timestamp_now()));
                    tui_state.update(
                        LoopState::Stopped,
                        &config,
                        send_count,
                        max_sends,
                        active_rule.as_deref(),
                        effective_elapsed(run_started, held_total, hold_started),
                        None,
                    )?;
                }
                if ui_mode == UiMode::Plain {
                    println!("loopmux: stopping after single send");
                }
                logger.log(LogEvent::stopped(&config, "once", send_count))?;
                break;
            }
            if ui_mode == UiMode::Tui && matched_sources.is_empty() {
                loop_state = LoopState::Waiting;
            }
        }

        if ui_mode == UiMode::Tui {
            let mut open_fleet_manager = false;
            if let Some(tui_state) = tui.as_mut() {
                sync_tui_overlays(tui_state, &injection_filter, &prompt_editor);
                if let Some(action) =
                    tui_state.poll_input(prompt_editor.open, prompt_editor.confirm.is_some())?
                {
                    if let Some(plan) = plan_hold_action(action, hold_started.is_some(), false) {
                        apply_hold_transition(
                            plan.transition,
                            &mut loop_state,
                            &mut hold_started,
                            &mut held_total,
                        );
                        if plan.force_rescan {
                            force_rescan = true;
                        }
                    } else {
                        match action {
                            TuiAction::Fleet => {
                                open_fleet_manager = true;
                            }
                            TuiAction::Stop => {
                                tui_state.push_log(format!(
                                    "[{}] stopped reason=manual",
                                    timestamp_now()
                                ));
                                tui_state.update(
                                    LoopState::Stopped,
                                    &config,
                                    send_count,
                                    max_sends,
                                    active_rule.as_deref(),
                                    effective_elapsed(run_started, held_total, hold_started),
                                    None,
                                )?;
                                logger.log(LogEvent::stopped(&config, "manual", send_count))?;
                                break;
                            }
                            TuiAction::Next => {
                                if prompt_editor.open && prompt_editor.confirm.is_some() {
                                    prompt_editor.confirm_no();
                                    continue;
                                }
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                loop_state = LoopState::Running;
                                force_rescan = true;
                            }
                            TuiAction::Renew => {
                                send_count = 0;
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                tui_state.push_log(format!(
                                    "[{}] renewed counter reason=manual",
                                    timestamp_now()
                                ));
                            }
                            TuiAction::ActiveListToggle => {
                                if prompt_editor.open {
                                    continue;
                                }
                                if injection_filter.popup_open {
                                    injection_filter.close_popup();
                                } else {
                                    injection_filter.open_popup();
                                }
                            }
                            TuiAction::ActiveListUp => {
                                if prompt_editor.open {
                                    prompt_editor.select_up();
                                } else if injection_filter.popup_open {
                                    injection_filter.move_up();
                                }
                            }
                            TuiAction::ActiveListDown => {
                                if prompt_editor.open {
                                    prompt_editor.select_down();
                                } else if injection_filter.popup_open {
                                    injection_filter.move_down();
                                }
                            }
                            TuiAction::ActiveListLeft => {
                                if injection_filter.popup_open {
                                    injection_filter.move_left();
                                }
                            }
                            TuiAction::ActiveListRight => {
                                if injection_filter.popup_open {
                                    injection_filter.move_right();
                                }
                            }
                            TuiAction::ActiveListToggleSelection => {
                                if prompt_editor.open {
                                    prompt_editor.use_selection();
                                } else if injection_filter.popup_open {
                                    injection_filter.toggle_current_selection();
                                }
                            }
                            TuiAction::ActiveListEnableAll => {
                                if injection_filter.popup_open {
                                    injection_filter.enable_all();
                                }
                            }
                            TuiAction::ActiveListDisableAll => {
                                if injection_filter.popup_open {
                                    injection_filter.disable_all();
                                }
                            }
                            TuiAction::ActiveListClose => {
                                if prompt_editor.open {
                                    prompt_editor.close();
                                } else {
                                    injection_filter.close_popup();
                                }
                            }
                            TuiAction::PromptEditorToggle => {
                                injection_filter.close_popup();
                                prompt_editor.toggle_open();
                            }
                            TuiAction::PromptEditorClearHistory => {
                                if prompt_editor.open {
                                    prompt_editor.request_clear_history();
                                }
                            }
                            TuiAction::PromptEditorUndo => {
                                if prompt_editor.open {
                                    prompt_editor.undo();
                                }
                            }
                            TuiAction::PromptEditorConfirmYes => {
                                if prompt_editor.open {
                                    prompt_editor.confirm_yes();
                                }
                            }
                            TuiAction::PromptEditorConfirmNo => {
                                if prompt_editor.open {
                                    prompt_editor.confirm_no();
                                }
                            }
                            TuiAction::PromptEditorBackspace => {
                                if prompt_editor.open {
                                    prompt_editor.backspace();
                                }
                            }
                            TuiAction::PromptEditorInput(ch) => {
                                if prompt_editor.open && !ch.is_control() {
                                    prompt_editor.input_char(ch);
                                }
                            }
                            TuiAction::ToggleLogView => {
                                tui_state.toggle_log_view();
                            }
                            TuiAction::Pause | TuiAction::Resume | TuiAction::HoldToggle => {}
                            TuiAction::Redraw => {}
                            TuiAction::Quit => {
                                if prompt_editor.open {
                                    prompt_editor.close();
                                    continue;
                                }
                                if injection_filter.popup_open {
                                    injection_filter.close_popup();
                                    continue;
                                }
                                tui_state
                                    .push_log(format!("[{}] stopped reason=quit", timestamp_now()));
                                logger.log(LogEvent::stopped(&config, "quit", send_count))?;
                                break;
                            }
                            TuiAction::PromptEditorDeleteSelected => {
                                if prompt_editor.open {
                                    prompt_editor.request_delete_selected();
                                }
                            }
                        }
                    }
                }
                sync_tui_overlays(tui_state, &injection_filter, &prompt_editor);
                tui_state.update(
                    loop_state,
                    &config,
                    send_count,
                    max_sends,
                    active_rule.as_deref(),
                    effective_elapsed(run_started, held_total, hold_started),
                    None,
                )?;
            }
            if open_fleet_manager {
                if let Err(err) = run_fleet_manager_tui_embedded()
                    && let Some(tui_state) = tui.as_mut()
                {
                    tui_state.push_log(format!(
                        "[{}] fleet manager error=\"{}\"",
                        timestamp_now(),
                        truncate_text(&err.to_string(), 100, true)
                    ));
                }
                if let Some(tui_state) = tui.as_mut() {
                    tui_state
                        .push_log(format!("[{}] returned from fleet manager", timestamp_now()));
                }
                continue;
            }
            if force_rescan {
                continue;
            }
        }

        if ui_mode == UiMode::Tui {
            let wait_seconds = if first_wait_cycle {
                config.initial_poll
            } else {
                config.poll
            };
            let sleep_until =
                std::time::Instant::now() + std::time::Duration::from_secs(wait_seconds);
            let mut should_exit_loop = false;
            while std::time::Instant::now() < sleep_until {
                if let Some(tui_state) = tui.as_mut() {
                    sync_tui_overlays(tui_state, &injection_filter, &prompt_editor);
                    if let Some(action) =
                        tui_state.poll_input(prompt_editor.open, prompt_editor.confirm.is_some())?
                    {
                        if let Some(plan) = plan_hold_action(action, hold_started.is_some(), true) {
                            apply_hold_transition(
                                plan.transition,
                                &mut loop_state,
                                &mut hold_started,
                                &mut held_total,
                            );
                            if plan.force_rescan {
                                force_rescan = true;
                            }
                            if plan.break_wait {
                                break;
                            }
                            continue;
                        }
                        match action {
                            TuiAction::Fleet => {
                                if let Err(err) = run_fleet_manager_tui_embedded() {
                                    tui_state.push_log(format!(
                                        "[{}] fleet manager error=\"{}\"",
                                        timestamp_now(),
                                        truncate_text(&err.to_string(), 100, true)
                                    ));
                                }
                                tui_state.push_log(format!(
                                    "[{}] returned from fleet manager",
                                    timestamp_now()
                                ));
                                force_rescan = true;
                                break;
                            }
                            TuiAction::Next => {
                                if prompt_editor.open && prompt_editor.confirm.is_some() {
                                    prompt_editor.confirm_no();
                                    continue;
                                }
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                loop_state = LoopState::Running;
                                force_rescan = true;
                                break;
                            }
                            TuiAction::Renew => {
                                send_count = 0;
                                last_hash_by_target.clear();
                                trigger_edge_active.clear();
                                trigger_confirm_pending_since.clear();
                                active_rule = None;
                                active_rule_by_target.clear();
                                backoff_state.clear();
                                tui_state.push_log(format!(
                                    "[{}] renewed counter reason=manual",
                                    timestamp_now()
                                ));
                            }
                            TuiAction::ActiveListToggle => {
                                if prompt_editor.open {
                                    continue;
                                }
                                if injection_filter.popup_open {
                                    injection_filter.close_popup();
                                } else {
                                    injection_filter.open_popup();
                                }
                            }
                            TuiAction::ActiveListUp => {
                                if prompt_editor.open {
                                    prompt_editor.select_up();
                                } else if injection_filter.popup_open {
                                    injection_filter.move_up();
                                }
                            }
                            TuiAction::ActiveListDown => {
                                if prompt_editor.open {
                                    prompt_editor.select_down();
                                } else if injection_filter.popup_open {
                                    injection_filter.move_down();
                                }
                            }
                            TuiAction::ActiveListLeft => {
                                if injection_filter.popup_open {
                                    injection_filter.move_left();
                                }
                            }
                            TuiAction::ActiveListRight => {
                                if injection_filter.popup_open {
                                    injection_filter.move_right();
                                }
                            }
                            TuiAction::ActiveListToggleSelection => {
                                if prompt_editor.open {
                                    prompt_editor.use_selection();
                                } else if injection_filter.popup_open {
                                    injection_filter.toggle_current_selection();
                                }
                            }
                            TuiAction::ActiveListEnableAll => {
                                if injection_filter.popup_open {
                                    injection_filter.enable_all();
                                }
                            }
                            TuiAction::ActiveListDisableAll => {
                                if injection_filter.popup_open {
                                    injection_filter.disable_all();
                                }
                            }
                            TuiAction::ActiveListClose => {
                                if prompt_editor.open {
                                    prompt_editor.close();
                                } else {
                                    injection_filter.close_popup();
                                }
                            }
                            TuiAction::PromptEditorToggle => {
                                injection_filter.close_popup();
                                prompt_editor.toggle_open();
                            }
                            TuiAction::PromptEditorClearHistory => {
                                if prompt_editor.open {
                                    prompt_editor.request_clear_history();
                                }
                            }
                            TuiAction::PromptEditorUndo => {
                                if prompt_editor.open {
                                    prompt_editor.undo();
                                }
                            }
                            TuiAction::PromptEditorConfirmYes => {
                                if prompt_editor.open {
                                    prompt_editor.confirm_yes();
                                }
                            }
                            TuiAction::PromptEditorConfirmNo => {
                                if prompt_editor.open {
                                    prompt_editor.confirm_no();
                                }
                            }
                            TuiAction::PromptEditorBackspace => {
                                if prompt_editor.open {
                                    prompt_editor.backspace();
                                }
                            }
                            TuiAction::PromptEditorInput(ch) => {
                                if prompt_editor.open && !ch.is_control() {
                                    prompt_editor.input_char(ch);
                                }
                            }
                            TuiAction::ToggleLogView => {
                                tui_state.toggle_log_view();
                            }
                            TuiAction::Stop => {
                                tui_state.push_log(format!(
                                    "[{}] stopped reason=manual",
                                    timestamp_now()
                                ));
                                logger.log(LogEvent::stopped(&config, "manual", send_count))?;
                                tui_state.update(
                                    LoopState::Stopped,
                                    &config,
                                    send_count,
                                    max_sends,
                                    active_rule.as_deref(),
                                    effective_elapsed(run_started, held_total, hold_started),
                                    None,
                                )?;
                                should_exit_loop = true;
                                break;
                            }
                            TuiAction::Quit => {
                                if prompt_editor.open {
                                    prompt_editor.close();
                                    continue;
                                }
                                if injection_filter.popup_open {
                                    injection_filter.close_popup();
                                    continue;
                                }
                                tui_state
                                    .push_log(format!("[{}] stopped reason=quit", timestamp_now()));
                                logger.log(LogEvent::stopped(&config, "quit", send_count))?;
                                should_exit_loop = true;
                                break;
                            }
                            TuiAction::PromptEditorDeleteSelected => {
                                if prompt_editor.open {
                                    prompt_editor.request_delete_selected();
                                }
                            }
                            TuiAction::Pause | TuiAction::Resume | TuiAction::HoldToggle => {}
                            TuiAction::Redraw => {}
                        }
                    }
                    sync_tui_overlays(tui_state, &injection_filter, &prompt_editor);
                    let next_scan_remaining =
                        sleep_until.saturating_duration_since(std::time::Instant::now());
                    tui_state.update(
                        loop_state,
                        &config,
                        send_count,
                        max_sends,
                        active_rule.as_deref(),
                        effective_elapsed(run_started, held_total, hold_started),
                        Some(next_scan_remaining),
                    )?;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if should_exit_loop {
                break;
            }
            if force_rescan {
                continue;
            }
            first_wait_cycle = false;
        } else {
            let wait_seconds = if first_wait_cycle {
                config.initial_poll
            } else {
                config.poll
            };
            sleep_with_heartbeat(
                &fleet_registry,
                &config.target_label,
                loop_state,
                send_count,
                wait_seconds,
                config.poll,
            )?;
            first_wait_cycle = false;
        }
    }

    if let Some(mut in_flight) = exec_in_flight {
        let _ = in_flight.child.kill();
        let _ = in_flight.child.wait();
    }

    let elapsed = format_std_duration(effective_elapsed(run_started, held_total, hold_started));
    if ui_mode == UiMode::Tui
        && let Some(tui_state) = tui.as_mut()
    {
        tui_state.push_log(format!(
            "[{}] stopped reason=completed sends={} elapsed={}",
            timestamp_now(),
            send_count,
            elapsed
        ));
        tui_state.update(
            LoopState::Stopped,
            &config,
            send_count,
            max_sends,
            active_rule.as_deref(),
            effective_elapsed(run_started, held_total, hold_started),
            None,
        )?;
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    logger.log(LogEvent::stopped(&config, "completed", send_count))?;
    if let Some(mut tui_state) = tui {
        tui_state.shutdown()?;
    }
    if ui_mode == UiMode::SingleLine {
        println!();
    }
    println!("loopmux: stopped after {send_count} sends (elapsed {elapsed})");
    Ok(())
}

pub(crate) fn capture_source(source: &str, window: CaptureWindow) -> Result<String> {
    if let Some(path) = file_source_path(source) {
        return capture_file(path, window);
    }
    capture_pane(source, window)
}

pub(crate) fn capture_file(path: &str, window: CaptureWindow) -> Result<String> {
    let path_buf = PathBuf::from(path);
    let content = std::fs::read_to_string(&path_buf)
        .with_context(|| format!("failed to read file source: {}", path_buf.display()))?;
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(String::new());
    }
    let selected = match window {
        CaptureWindow::Tail(count) => {
            let start = lines.len().saturating_sub(count);
            &lines[start..]
        }
        CaptureWindow::Head(count) => {
            let end = lines.len().min(count);
            &lines[..end]
        }
    };
    Ok(selected.join("\n"))
}

pub(crate) fn capture_pane(target: &str, window: CaptureWindow) -> Result<String> {
    let mut command = std::process::Command::new("tmux");
    command.arg("capture-pane").arg("-p");
    match window {
        CaptureWindow::Tail(lines) => {
            command.arg("-S").arg(format!("-{lines}"));
        }
        CaptureWindow::Head(lines) => {
            let end = lines.saturating_sub(1);
            command.arg("-S").arg("0").arg("-E").arg(end.to_string());
        }
    }
    let output = command
        .args(["-t", target])
        .output()
        .context("failed to capture tmux pane")?;
    if !output.status.success() {
        bail!("tmux capture-pane failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(crate) fn last_non_empty_line(output: &str) -> String {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

pub(crate) fn send_prompt(target: &str, prompt: &str) -> Result<()> {
    let before_submit = capture_pane(target, CaptureWindow::Tail(8)).ok();

    let output = std::process::Command::new("tmux")
        .args(["send-keys", "-t", target, "-l", prompt])
        .output()
        .context("failed to send tmux keys")?;
    if !output.status.success() {
        bail!("tmux send-keys failed");
    }

    send_enter_key(target)?;

    std::thread::sleep(std::time::Duration::from_millis(80));
    let after_submit = capture_pane(target, CaptureWindow::Tail(8)).ok();
    if should_retry_enter_submit(before_submit.as_deref(), after_submit.as_deref(), prompt) {
        send_enter_key(target)?;
    }
    Ok(())
}

pub(crate) fn send_enter_key(target: &str) -> Result<()> {
    let output = std::process::Command::new("tmux")
        .args(["send-keys", "-t", target, "Enter"])
        .output()
        .context("failed to submit tmux keys")?;
    if !output.status.success() {
        bail!("tmux send-keys submit failed");
    }
    Ok(())
}

pub(crate) fn should_retry_enter_submit(
    before: Option<&str>,
    after: Option<&str>,
    prompt: &str,
) -> bool {
    let (Some(before), Some(after)) = (before, after) else {
        return false;
    };
    if hash_output(before) == hash_output(after) {
        return true;
    }
    let before_pending = pane_tail_indicates_pending_submit(before, prompt);
    let after_pending = pane_tail_indicates_pending_submit(after, prompt);
    !before_pending && after_pending
}

pub(crate) fn pane_tail_indicates_pending_submit(capture: &str, prompt: &str) -> bool {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return false;
    }
    let tail = last_non_empty_line(capture);
    let tail = tail.trim();
    tail == prompt || tail.ends_with(prompt)
}

pub(crate) fn hash_output(output: &str) -> String {
    let mut hash: u64 = 14695981039346656037;
    for byte in output.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:x}")
}

pub(crate) fn select_rules<'a>(
    output: &str,
    rules: &'a [Rule],
    rule_eval: &RuleEval,
    active_rule: Option<&str>,
) -> Result<Vec<RuleMatch<'a>>> {
    let mut candidates = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        if let Some(active) = active_rule
            && rule.id.as_deref() != Some(active)
        {
            continue;
        }
        if !matches_rule(rule, output)? {
            continue;
        }
        candidates.push(RuleMatch { rule, index });
        if matches!(rule_eval, RuleEval::FirstMatch) {
            break;
        }
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    match rule_eval {
        RuleEval::FirstMatch => Ok(vec![candidates.remove(0)]),
        RuleEval::MultiMatch => Ok(candidates),
        RuleEval::Priority => {
            let mut best = &candidates[0];
            for candidate in &candidates[1..] {
                let priority = candidate.rule.priority.unwrap_or(0);
                let best_priority = best.rule.priority.unwrap_or(0);
                if priority > best_priority
                    || (priority == best_priority && candidate.index < best.index)
                {
                    best = candidate;
                }
            }
            Ok(vec![RuleMatch {
                rule: best.rule,
                index: best.index,
            }])
        }
    }
}

pub(crate) fn evaluate_rules<'a>(
    config: &'a ResolvedConfig,
    logger: &mut Logger,
    output: &str,
    active_rule: Option<&str>,
) -> Result<Vec<RuleMatch<'a>>> {
    let matches = select_rules(output, &config.rules, &config.rule_eval, active_rule)?;
    for rule_match in &matches {
        logger.log(LogEvent::matched(config, rule_match.rule.id.as_deref()))?;
    }
    Ok(matches)
}

pub(crate) fn trigger_edge_key(target: &str, rule_match: &RuleMatch<'_>) -> String {
    let rule_id = rule_match.rule.id.as_deref().unwrap_or("<unnamed>");
    format!("{target}|{rule_id}|{}", rule_match.index)
}

pub(crate) fn refresh_trigger_edges_for_target(
    active_edges: &mut HashSet<String>,
    target: &str,
    matched_keys: &HashSet<String>,
    hash_changed: bool,
    enabled: bool,
) {
    if !enabled {
        return;
    }
    let prefix = format!("{target}|");
    if hash_changed {
        active_edges.retain(|key| !key.starts_with(&prefix));
        return;
    }
    active_edges.retain(|key| !key.starts_with(&prefix) || matched_keys.contains(key));
}

pub(crate) fn edge_guard_allows(
    active_edges: &HashSet<String>,
    edge_key: &str,
    enabled: bool,
) -> bool {
    !enabled || !active_edges.contains(edge_key)
}

pub(crate) fn refresh_trigger_confirm_for_target(
    pending_since: &mut std::collections::HashMap<String, std::time::Instant>,
    target: &str,
    matched_keys: &HashSet<String>,
) {
    let prefix = format!("{target}|");
    pending_since.retain(|key, _| !key.starts_with(&prefix) || matched_keys.contains(key));
}

pub(crate) fn confirm_window_elapsed(
    global_seconds: u64,
    rule_override_seconds: Option<u64>,
    edge_key: &str,
    pending_since: &mut std::collections::HashMap<String, std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    let seconds = rule_override_seconds.unwrap_or(global_seconds);
    if seconds == 0 {
        pending_since.remove(edge_key);
        return true;
    }

    let wait = std::time::Duration::from_secs(seconds);
    let Some(first_seen) = pending_since.get(edge_key).copied() else {
        pending_since.insert(edge_key.to_string(), now);
        return false;
    };
    if now.duration_since(first_seen) >= wait {
        pending_since.remove(edge_key);
        return true;
    }
    false
}

pub(crate) fn has_pending_confirm_for_target(
    pending_since: &std::collections::HashMap<String, std::time::Instant>,
    target: &str,
) -> bool {
    let prefix = format!("{target}|");
    pending_since.keys().any(|key| key.starts_with(&prefix))
}

pub(crate) fn should_skip_scan_by_hash(
    trigger_edge_enabled: bool,
    hash: &str,
    last_hash: &str,
    has_pending_confirm: bool,
) -> bool {
    trigger_edge_enabled && !last_hash.is_empty() && hash == last_hash && !has_pending_confirm
}

pub(crate) fn short_hash(hash: &str) -> &str {
    if hash.len() > 8 { &hash[..8] } else { hash }
}

pub(crate) fn emit_trigger_debug(
    logger: &mut Logger,
    config: &ResolvedConfig,
    target: &str,
    decision: &str,
    detail: &str,
) -> Result<()> {
    if !config.debug_trigger {
        return Ok(());
    }
    logger.log(LogEvent::status(
        config,
        format!("trigger-debug target={target} decision={decision} {detail}"),
    ))
}

pub(crate) fn extract_trigger_preview(
    output: &str,
    max_lines: usize,
    use_unicode: bool,
) -> (usize, String) {
    let lines = output
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| truncate_text(line, 60, use_unicode))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return (0, "<empty>".to_string());
    }
    let take = max_lines.max(1).min(lines.len());
    let start = lines.len().saturating_sub(take);
    let sep = if use_unicode { " │ " } else { " | " };
    let preview = lines[start..].join(sep);
    (take, preview)
}

pub(crate) fn compact_timestamp(timestamp: &str) -> String {
    let mut parts = timestamp.split('T');
    let _date = parts.next();
    let Some(time_part) = parts.next() else {
        return timestamp.to_string();
    };
    let time = time_part.trim_end_matches('Z');
    let time = time.split('+').next().unwrap_or(time);
    let time = time.split('-').next().unwrap_or(time);
    truncate_text(time, 12, false)
}

pub(crate) fn latest_stop_reason(logs: &[String]) -> Option<String> {
    const MARKER: &str = "stopped reason=";
    logs.iter().rev().find_map(|line| {
        let idx = line.find(MARKER)?;
        let rest = &line[idx + MARKER.len()..];
        let token = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches('"');
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    })
}

pub(crate) fn fleet_heartbeat_interval_seconds(poll_seconds: u64) -> u64 {
    poll_seconds
        .max(1)
        .saturating_mul(FLEET_HEARTBEAT_POLL_MULTIPLIER)
        .clamp(
            FLEET_HEARTBEAT_MIN_INTERVAL_SECONDS,
            FLEET_HEARTBEAT_MAX_INTERVAL_SECONDS,
        )
}

pub(crate) fn should_emit_fleet_heartbeat(
    now: OffsetDateTime,
    last_reported_at: Option<&str>,
    poll_seconds: u64,
) -> bool {
    let Some(last_reported_at) = last_reported_at else {
        return false;
    };
    let Ok(last_reported_at) = OffsetDateTime::parse(
        last_reported_at,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return true;
    };
    let elapsed = (now - last_reported_at).whole_seconds();
    elapsed >= fleet_heartbeat_interval_seconds(poll_seconds) as i64
}

pub(crate) fn fleet_heartbeat_drift_seconds(
    now: OffsetDateTime,
    last_reported_at: Option<&str>,
    interval_seconds: u64,
) -> u64 {
    let Some(last_reported_at) = last_reported_at else {
        return 0;
    };
    let Ok(last_reported_at) = OffsetDateTime::parse(
        last_reported_at,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return 0;
    };
    let elapsed_seconds = (now - last_reported_at).whole_seconds().max(0) as u64;
    elapsed_seconds.saturating_sub(interval_seconds)
}

pub(crate) fn fleet_heartbeat_drift_severity(drift_seconds: u64) -> &'static str {
    if drift_seconds == 0 {
        "ok"
    } else if drift_seconds <= 30 {
        "warn"
    } else {
        "critical"
    }
}

pub(crate) fn format_fleet_heartbeat_metric(
    state: LoopState,
    sends_total: u32,
    sends_delta: u32,
    poll_seconds: u64,
    interval_seconds: u64,
    drift_seconds: u64,
) -> String {
    let activity = if sends_delta > 0 { "active" } else { "idle" };
    let progress = if sends_delta > 0 {
        "progressing"
    } else {
        "stalled"
    };
    format!(
        "fleet-heartbeat state={} activity={} progress={} sends_total={} sends_delta={} poll={}s window={}s drift={}s severity={}",
        fleet_state_label(state),
        activity,
        progress,
        sends_total,
        sends_delta,
        poll_seconds,
        interval_seconds,
        drift_seconds,
        fleet_heartbeat_drift_severity(drift_seconds)
    )
}

pub(crate) fn compact_sent_log(
    timestamp: &str,
    target: &str,
    rule_id: Option<&str>,
    trigger_preview: &str,
    trigger_preview_lines: usize,
    use_unicode: bool,
    icon_mode: IconMode,
) -> String {
    let rule = rule_id.unwrap_or("-");
    let ts = compact_timestamp(timestamp);
    let use_nerd = use_unicode && icon_mode == IconMode::Nerd;
    let send_icon = if use_nerd { "󰐊" } else { ">" };
    let fold_icon = if use_nerd { "" } else { ">" };
    format!(
        "{ts} {send_icon} {target} {rule} {fold_icon} {}L {}",
        trigger_preview_lines,
        truncate_text(trigger_preview, 70, use_unicode)
    )
}

pub(crate) fn matches_rule(rule: &Rule, output: &str) -> Result<bool> {
    let match_defined = rule.match_.as_ref().map(has_match).unwrap_or(false);
    let matches = if match_defined {
        rule.match_
            .as_ref()
            .map(|criteria| matches_criteria(criteria, output))
            .unwrap_or(Ok(false))?
    } else {
        true
    };
    if !matches {
        return Ok(false);
    }
    if let Some(exclude) = &rule.exclude
        && matches_criteria(exclude, output)?
    {
        return Ok(false);
    }
    Ok(true)
}

pub(crate) fn matches_criteria(criteria: &MatchCriteria, output: &str) -> Result<bool> {
    if let Some(trigger_expr) = &criteria.trigger_expr
        && eval_trigger_expr(&parse_trigger_expr(trigger_expr)?, output)
    {
        return Ok(true);
    }
    if let Some(exact_line) = &criteria.exact_line {
        let expected = exact_line.trim();
        if output.lines().any(|line| line.trim() == expected) {
            return Ok(true);
        }
    }
    if let Some(regex) = &criteria.regex {
        let re = Regex::new(regex).context("invalid regex")?;
        if re.is_match(output) {
            return Ok(true);
        }
    }
    if let Some(contains) = &criteria.contains
        && output.contains(contains)
    {
        return Ok(true);
    }
    if let Some(prefix) = &criteria.starts_with
        && output.starts_with(prefix)
    {
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn tokenize_trigger_expr(input: &str) -> Result<Vec<TriggerExprToken>> {
    let mut tokens = Vec::new();
    let mut idx = 0;
    while idx < input.len() {
        let rest = &input[idx..];
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if ch.is_whitespace() {
            idx += ch.len_utf8();
            continue;
        }
        if rest.starts_with("&&") {
            tokens.push(TriggerExprToken::And { pos: idx });
            idx += 2;
            continue;
        }
        if rest.starts_with("||") {
            tokens.push(TriggerExprToken::Or { pos: idx });
            idx += 2;
            continue;
        }
        if ch == '(' {
            tokens.push(TriggerExprToken::LParen { pos: idx });
            idx += 1;
            continue;
        }
        if ch == ')' {
            tokens.push(TriggerExprToken::RParen { pos: idx });
            idx += 1;
            continue;
        }

        let start = idx;
        while idx < input.len() {
            let next = &input[idx..];
            if next.starts_with("&&") || next.starts_with("||") {
                break;
            }
            let Some(next_ch) = next.chars().next() else {
                break;
            };
            if next_ch.is_whitespace() || next_ch == '(' || next_ch == ')' {
                break;
            }
            idx += next_ch.len_utf8();
        }
        let term = input[start..idx].trim();
        if term.is_empty() {
            bail!("invalid trigger expression at pos {start}: unexpected token");
        }
        tokens.push(TriggerExprToken::Term {
            pattern: term.to_string(),
            pos: start,
        });
    }
    Ok(tokens)
}

pub(crate) fn compile_trigger_expr(
    node: TriggerExprRawNode,
    compiled_terms: &mut Vec<Regex>,
) -> Result<TriggerExprNode> {
    match node {
        TriggerExprRawNode::Term { pattern, pos } => {
            let regex = Regex::new(&pattern).map_err(|err| {
                anyhow::anyhow!(
                    "invalid trigger expression at pos {pos}: invalid regex term: {err}"
                )
            })?;
            let idx = compiled_terms.len();
            compiled_terms.push(regex);
            Ok(TriggerExprNode::Term(idx))
        }
        TriggerExprRawNode::And(left, right) => Ok(TriggerExprNode::And(
            Box::new(compile_trigger_expr(*left, compiled_terms)?),
            Box::new(compile_trigger_expr(*right, compiled_terms)?),
        )),
        TriggerExprRawNode::Or(left, right) => Ok(TriggerExprNode::Or(
            Box::new(compile_trigger_expr(*left, compiled_terms)?),
            Box::new(compile_trigger_expr(*right, compiled_terms)?),
        )),
    }
}

pub(crate) fn parse_trigger_expr(input: &str) -> Result<TriggerExpr> {
    let tokens = tokenize_trigger_expr(input)?;
    if tokens.is_empty() {
        bail!("invalid trigger expression at pos 0: expected term");
    }
    let parser = TriggerExprParser {
        tokens: &tokens,
        index: 0,
        source_len: input.len(),
    };
    let raw = parser.parse()?;
    let mut terms = Vec::new();
    let ast = compile_trigger_expr(raw, &mut terms)?;
    Ok(TriggerExpr { ast, terms })
}

pub(crate) fn eval_trigger_expr(expr: &TriggerExpr, output: &str) -> bool {
    fn eval_node(node: &TriggerExprNode, terms: &[Regex], output: &str) -> bool {
        match node {
            TriggerExprNode::Term(idx) => terms[*idx].is_match(output),
            TriggerExprNode::And(left, right) => {
                eval_node(left, terms, output) && eval_node(right, terms, output)
            }
            TriggerExprNode::Or(left, right) => {
                eval_node(left, terms, output) || eval_node(right, terms, output)
            }
        }
    }

    eval_node(&expr.ast, &expr.terms, output)
}

#[cfg(test)]
pub(crate) fn matches_trigger_expr(expr: &str, output: &str) -> Result<bool> {
    let parsed = parse_trigger_expr(expr)?;
    Ok(eval_trigger_expr(&parsed, output))
}
