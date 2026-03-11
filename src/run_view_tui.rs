use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutMode {
    Compact,
    Standard,
    Wide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconMode {
    Nerd,
    Ascii,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StyleConfig {
    pub(crate) use_color: bool,
    pub(crate) use_bg: bool,
    pub(crate) use_unicode_ellipsis: bool,
    pub(crate) dim_logs: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TuiAction {
    Pause,
    Resume,
    HoldToggle,
    Fleet,
    Stop,
    Next,
    Renew,
    ActiveListToggle,
    ActiveListUp,
    ActiveListDown,
    ActiveListLeft,
    ActiveListRight,
    ActiveListToggleSelection,
    ActiveListEnableAll,
    ActiveListDisableAll,
    ActiveListClose,
    PromptEditorToggle,
    PromptEditorDeleteSelected,
    PromptEditorClearHistory,
    PromptEditorUndo,
    PromptEditorConfirmYes,
    PromptEditorConfirmNo,
    PromptEditorBackspace,
    PromptEditorInput(char),
    ToggleLogView,
    Redraw,
    Quit,
}

pub(crate) fn map_run_tui_key_action(
    code: KeyCode,
    modifiers: KeyModifiers,
    prompt_editor_open: bool,
    prompt_confirm_open: bool,
) -> Option<TuiAction> {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        return Some(TuiAction::Stop);
    }
    if prompt_editor_open {
        return match code {
            KeyCode::Char('e') => Some(TuiAction::PromptEditorToggle),
            KeyCode::Up => Some(TuiAction::ActiveListUp),
            KeyCode::Down => Some(TuiAction::ActiveListDown),
            KeyCode::Enter => Some(TuiAction::ActiveListToggleSelection),
            KeyCode::Char(' ') => Some(TuiAction::ActiveListToggleSelection),
            KeyCode::Char('d') => Some(TuiAction::PromptEditorDeleteSelected),
            KeyCode::Char('c') => Some(TuiAction::PromptEditorClearHistory),
            KeyCode::Char('u') => Some(TuiAction::PromptEditorUndo),
            KeyCode::Char('y') if prompt_confirm_open => Some(TuiAction::PromptEditorConfirmYes),
            KeyCode::Char('n') | KeyCode::Char('N') if prompt_confirm_open => {
                Some(TuiAction::PromptEditorConfirmNo)
            }
            KeyCode::Backspace => Some(TuiAction::PromptEditorBackspace),
            KeyCode::Esc => Some(TuiAction::ActiveListClose),
            KeyCode::Char(ch) if !ch.is_control() => Some(TuiAction::PromptEditorInput(ch)),
            _ => None,
        };
    }
    match code {
        KeyCode::Char('p') => Some(TuiAction::Pause),
        KeyCode::Char('r') => Some(TuiAction::Resume),
        KeyCode::Char('h') => Some(TuiAction::HoldToggle),
        KeyCode::Char('f') => Some(TuiAction::Fleet),
        KeyCode::Char('e') => Some(TuiAction::PromptEditorToggle),
        KeyCode::Char('l') => Some(TuiAction::ActiveListToggle),
        KeyCode::Up => Some(TuiAction::ActiveListUp),
        KeyCode::Down => Some(TuiAction::ActiveListDown),
        KeyCode::Left => Some(TuiAction::ActiveListLeft),
        KeyCode::Right => Some(TuiAction::ActiveListRight),
        KeyCode::Enter => Some(TuiAction::ActiveListToggleSelection),
        KeyCode::Char(' ') => Some(TuiAction::ActiveListToggleSelection),
        KeyCode::Char('a') => Some(TuiAction::ActiveListEnableAll),
        KeyCode::Char('d') => Some(TuiAction::ActiveListDisableAll),
        KeyCode::Char('g') => Some(TuiAction::ToggleLogView),
        KeyCode::Esc => Some(TuiAction::ActiveListClose),
        KeyCode::Char('R') => Some(TuiAction::Renew),
        KeyCode::Char('s') => Some(TuiAction::Stop),
        KeyCode::Char('n') => Some(TuiAction::Next),
        KeyCode::Char('q') => Some(TuiAction::Quit),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogViewMode {
    Chronological,
    GroupedByPane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveListColumn {
    Session,
    Window,
    Pane,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveListCursor {
    pub(crate) column: ActiveListColumn,
    pub(crate) session_idx: usize,
    pub(crate) window_idx: usize,
    pub(crate) pane_idx: usize,
}

impl Default for ActiveListCursor {
    fn default() -> Self {
        Self {
            column: ActiveListColumn::Session,
            session_idx: 0,
            window_idx: 0,
            pane_idx: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct KnownPaneTarget {
    pub(crate) target: String,
    pub(crate) session: String,
    pub(crate) window: String,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct InjectionFilterState {
    pub(crate) known_targets: BTreeMap<String, KnownPaneTarget>,
    pub(crate) disabled_targets: HashSet<String>,
    pub(crate) popup_open: bool,
    pub(crate) cursor: ActiveListCursor,
}

impl InjectionFilterState {
    pub(crate) fn observe_trigger_target(&mut self, target: &str) {
        let Ok((session, window, _pane)) = parse_target(target) else {
            return;
        };
        self.known_targets
            .entry(target.to_string())
            .or_insert_with(|| KnownPaneTarget {
                target: target.to_string(),
                session: session.to_string(),
                window: window.to_string(),
            });
    }

    pub(crate) fn is_allowed(&self, target: &str) -> bool {
        !self.disabled_targets.contains(target)
    }

    pub(crate) fn active_counts(&self) -> (usize, usize) {
        let total = self.known_targets.len();
        let disabled = self
            .known_targets
            .keys()
            .filter(|target| self.disabled_targets.contains(*target))
            .count();
        (total.saturating_sub(disabled), total)
    }

    pub(crate) fn open_popup(&mut self) {
        self.popup_open = true;
        self.normalize_cursor();
    }

    pub(crate) fn close_popup(&mut self) {
        self.popup_open = false;
    }

    pub(crate) fn enable_all(&mut self) {
        self.disabled_targets.clear();
    }

    pub(crate) fn disable_all(&mut self) {
        self.disabled_targets = self.known_targets.keys().cloned().collect();
    }

    pub(crate) fn normalize_cursor(&mut self) {
        let sessions = self.sessions();
        if sessions.is_empty() {
            self.cursor = ActiveListCursor::default();
            return;
        }
        self.cursor.session_idx = self
            .cursor
            .session_idx
            .min(sessions.len().saturating_sub(1));
        let Some(session) = sessions.get(self.cursor.session_idx) else {
            return;
        };
        let windows = self.windows_for(session);
        self.cursor.window_idx = self.cursor.window_idx.min(windows.len().saturating_sub(1));
        if let Some(window) = windows.get(self.cursor.window_idx) {
            let panes = self.panes_for(session, window);
            self.cursor.pane_idx = self.cursor.pane_idx.min(panes.len().saturating_sub(1));
        } else {
            self.cursor.pane_idx = 0;
        }
    }

    pub(crate) fn sessions(&self) -> Vec<String> {
        self.known_targets
            .values()
            .map(|item| item.session.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn windows_for(&self, session: &str) -> Vec<String> {
        self.known_targets
            .values()
            .filter(|item| item.session == session)
            .map(|item| item.window.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn panes_for(&self, session: &str, window: &str) -> Vec<String> {
        self.known_targets
            .values()
            .filter(|item| item.session == session && item.window == window)
            .map(|item| item.target.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn selected_session(&self) -> Option<String> {
        self.sessions().get(self.cursor.session_idx).cloned()
    }

    pub(crate) fn selected_window(&self, session: &str) -> Option<String> {
        self.windows_for(session)
            .get(self.cursor.window_idx)
            .cloned()
    }

    pub(crate) fn move_up(&mut self) {
        match self.cursor.column {
            ActiveListColumn::Session => {
                self.cursor.session_idx = self.cursor.session_idx.saturating_sub(1)
            }
            ActiveListColumn::Window => {
                self.cursor.window_idx = self.cursor.window_idx.saturating_sub(1)
            }
            ActiveListColumn::Pane => self.cursor.pane_idx = self.cursor.pane_idx.saturating_sub(1),
        }
        self.normalize_cursor();
    }

    pub(crate) fn move_down(&mut self) {
        match self.cursor.column {
            ActiveListColumn::Session => {
                self.cursor.session_idx = self.cursor.session_idx.saturating_add(1)
            }
            ActiveListColumn::Window => {
                self.cursor.window_idx = self.cursor.window_idx.saturating_add(1)
            }
            ActiveListColumn::Pane => self.cursor.pane_idx = self.cursor.pane_idx.saturating_add(1),
        }
        self.normalize_cursor();
    }

    pub(crate) fn move_left(&mut self) {
        self.cursor.column = match self.cursor.column {
            ActiveListColumn::Session => ActiveListColumn::Session,
            ActiveListColumn::Window => ActiveListColumn::Session,
            ActiveListColumn::Pane => ActiveListColumn::Window,
        };
        self.normalize_cursor();
    }

    pub(crate) fn move_right(&mut self) {
        self.cursor.column = match self.cursor.column {
            ActiveListColumn::Session => ActiveListColumn::Window,
            ActiveListColumn::Window => ActiveListColumn::Pane,
            ActiveListColumn::Pane => ActiveListColumn::Pane,
        };
        self.normalize_cursor();
    }

    pub(crate) fn toggle_current_selection(&mut self) {
        self.normalize_cursor();
        let Some(session) = self.selected_session() else {
            return;
        };
        match self.cursor.column {
            ActiveListColumn::Session => {
                let targets = self
                    .known_targets
                    .values()
                    .filter(|item| item.session == session)
                    .map(|item| item.target.clone())
                    .collect::<Vec<_>>();
                self.toggle_targets(&targets);
            }
            ActiveListColumn::Window => {
                let Some(window) = self.selected_window(&session) else {
                    return;
                };
                let targets = self
                    .known_targets
                    .values()
                    .filter(|item| item.session == session && item.window == window)
                    .map(|item| item.target.clone())
                    .collect::<Vec<_>>();
                self.toggle_targets(&targets);
            }
            ActiveListColumn::Pane => {
                let Some(window) = self.selected_window(&session) else {
                    return;
                };
                let panes = self.panes_for(&session, &window);
                let Some(target) = panes.get(self.cursor.pane_idx) else {
                    return;
                };
                self.toggle_targets(std::slice::from_ref(target));
            }
        }
    }

    pub(crate) fn toggle_targets(&mut self, targets: &[String]) {
        if targets.is_empty() {
            return;
        }
        let all_enabled = targets.iter().all(|target| self.is_allowed(target));
        if all_enabled {
            for target in targets {
                self.disabled_targets.insert(target.clone());
            }
        } else {
            for target in targets {
                self.disabled_targets.remove(target);
            }
        }
    }
}

pub(crate) struct TuiState {
    pub(crate) raw_mode_guard: RawModeGuard,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) icon_mode: IconMode,
    pub(crate) style: StyleConfig,
    pub(crate) logs: Vec<String>,
    pub(crate) max_logs: usize,
    pub(crate) overlay_lines: Option<Vec<String>>,
    pub(crate) overlay_help: Option<String>,
    pub(crate) footer_note: Option<String>,
    pub(crate) status_bar_renderer: Box<dyn StatusBarRenderer>,
    pub(crate) footer_renderer: Box<dyn FooterRenderer>,
    pub(crate) process_usage_provider: Box<dyn ProcessUsageProvider>,
    pub(crate) usage_sample: Option<ProcessUsageSample>,
    pub(crate) log_view: LogViewMode,
    pub(crate) last_frame_signature: Option<u64>,
    pub(crate) last_frame_at: Option<Instant>,
    pub(crate) skipped_redraws: u64,
}

pub(crate) struct ProcessUsageSample {
    pub(crate) captured_at: Instant,
    pub(crate) summary: String,
}

pub(crate) trait ProcessUsageProvider {
    fn sample(&self, pid: u32) -> Option<String>;
}

pub(crate) struct StatusBarRenderArgs<'a> {
    pub(crate) state: LoopState,
    pub(crate) layout: LayoutMode,
    pub(crate) icon_mode: IconMode,
    pub(crate) style: StyleConfig,
    pub(crate) width: u16,
    pub(crate) config: &'a ResolvedConfig,
    pub(crate) current: u32,
    pub(crate) total: u32,
    pub(crate) rule_id: Option<&'a str>,
    pub(crate) elapsed: &'a str,
    pub(crate) remaining_duration: Option<&'a str>,
    pub(crate) next_scan_remaining: Option<&'a str>,
    pub(crate) process_usage: Option<&'a str>,
}

pub(crate) trait StatusBarRenderer {
    fn render(&self, args: StatusBarRenderArgs<'_>) -> String;
}

pub(crate) struct FooterRenderArgs<'a> {
    pub(crate) style: StyleConfig,
    pub(crate) width: u16,
    pub(crate) summary: Option<&'a str>,
    pub(crate) note: Option<&'a str>,
    pub(crate) overlay_help: Option<&'a str>,
}

pub(crate) trait FooterRenderer {
    fn render(&self, args: FooterRenderArgs<'_>) -> String;
}

pub(crate) struct LegacyStatusBarRenderer;
pub(crate) struct LegacyFooterRenderer;

impl StatusBarRenderer for LegacyStatusBarRenderer {
    fn render(&self, args: StatusBarRenderArgs<'_>) -> String {
        render_status_bar(&args)
    }
}

impl FooterRenderer for LegacyFooterRenderer {
    fn render(&self, args: FooterRenderArgs<'_>) -> String {
        render_footer(
            args.style,
            args.width,
            args.summary,
            args.note,
            args.overlay_help,
        )
    }
}

pub(crate) struct SystemProcessUsageProvider;

impl ProcessUsageProvider for SystemProcessUsageProvider {
    fn sample(&self, pid: u32) -> Option<String> {
        let output = std::process::Command::new("ps")
            .args(["-o", "%cpu=", "-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_process_usage_summary(&stdout)
    }
}

impl TuiState {
    pub(crate) fn new(_config: &ResolvedConfig) -> Result<Self> {
        let raw_mode_guard = RawModeGuard::acquire("failed to enable raw mode")?;
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        let style = detect_style();
        Ok(Self {
            raw_mode_guard,
            width,
            height,
            icon_mode: detect_icon_mode(),
            style,
            logs: Vec::new(),
            max_logs: height.saturating_sub(3) as usize,
            overlay_lines: None,
            overlay_help: None,
            footer_note: None,
            status_bar_renderer: Box::new(LegacyStatusBarRenderer),
            footer_renderer: Box::new(LegacyFooterRenderer),
            process_usage_provider: Box::new(SystemProcessUsageProvider),
            usage_sample: None,
            log_view: LogViewMode::Chronological,
            last_frame_signature: None,
            last_frame_at: None,
            skipped_redraws: 0,
        })
    }

    pub(crate) fn toggle_log_view(&mut self) {
        self.log_view = match self.log_view {
            LogViewMode::Chronological => LogViewMode::GroupedByPane,
            LogViewMode::GroupedByPane => LogViewMode::Chronological,
        };
    }

    pub(crate) fn process_usage_summary(&mut self) -> Option<String> {
        self.usage_sample
            .as_ref()
            .map(|sample| sample.summary.clone())
    }

    pub(crate) fn refresh_process_usage_summary(&mut self) {
        if let Some(sample) = self.usage_sample.as_ref()
            && sample.captured_at.elapsed() < Duration::from_secs(1)
        {
            return;
        }

        let summary = match self.process_usage_provider.sample(std::process::id()) {
            Some(summary) => summary,
            None => {
                self.usage_sample = None;
                return;
            }
        };
        self.usage_sample = Some(ProcessUsageSample {
            captured_at: Instant::now(),
            summary,
        });
    }

    pub(crate) fn set_overlay_lines(&mut self, lines: Option<Vec<String>>) {
        self.overlay_lines = lines;
    }

    pub(crate) fn set_overlay_help(&mut self, help: Option<String>) {
        self.overlay_help = help;
    }

    pub(crate) fn set_footer_note(&mut self, note: Option<String>) {
        self.footer_note = note;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        state: LoopState,
        config: &ResolvedConfig,
        current: u32,
        total: u32,
        rule_id: Option<&str>,
        active_elapsed: std::time::Duration,
        next_scan_remaining: Option<std::time::Duration>,
    ) -> Result<()> {
        let elapsed = format_std_duration(active_elapsed);
        let remaining_duration = config
            .duration
            .map(|limit| format_std_duration(limit.saturating_sub(active_elapsed)));
        let next_scan_remaining = next_scan_remaining.map(format_clock_countdown);
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        self.width = width;
        self.height = height;
        self.max_logs = height.saturating_sub(3) as usize;

        let layout = layout_mode(width);
        let process_usage = self.process_usage_summary();
        let bar = self.status_bar_renderer.render(StatusBarRenderArgs {
            state,
            layout,
            icon_mode: self.icon_mode,
            style: self.style,
            width,
            config,
            current,
            total,
            rule_id,
            elapsed: &elapsed,
            remaining_duration: remaining_duration.as_deref(),
            next_scan_remaining: next_scan_remaining.as_deref(),
            process_usage: process_usage.as_deref(),
        });

        let log_height = if width < 60 { 0 } else { self.max_logs };
        let overlay_lines = self.overlay_lines.as_ref();
        let display_lines = if let Some(lines) = overlay_lines {
            lines.clone()
        } else if matches!(self.log_view, LogViewMode::GroupedByPane) {
            build_grouped_log_lines(&self.logs, log_height, self.style.use_unicode_ellipsis)
        } else {
            self.logs
                .iter()
                .rev()
                .take(log_height)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
        };

        let footer_row = self.height.saturating_sub(1);
        let footer_summary = if state == LoopState::Stopped {
            Some(render_footer_summary(config, current, total, &elapsed))
        } else {
            None
        };
        let view_note = match self.log_view {
            LogViewMode::Chronological => "view chrono",
            LogViewMode::GroupedByPane => "view grouped",
        };
        let footer_note_owned = if let Some(note) = self.footer_note.as_ref() {
            format!("{note} {view_note}")
        } else if state == LoopState::Stopped {
            if let Some(reason) = latest_stop_reason(&self.logs) {
                format!("stop {reason} {view_note}")
            } else {
                view_note.to_string()
            }
        } else {
            view_note.to_string()
        };
        let footer_note_owned = if self.skipped_redraws > 0 {
            format!("{footer_note_owned} skip {}", self.skipped_redraws)
        } else {
            footer_note_owned
        };
        let footer = self.footer_renderer.render(FooterRenderArgs {
            style: self.style,
            width,
            summary: footer_summary.as_deref(),
            note: Some(footer_note_owned.as_str()),
            overlay_help: self.overlay_help.as_deref(),
        });
        let frame_signature = tui_frame_signature(
            width,
            height,
            &bar,
            &display_lines,
            &footer,
            overlay_lines.is_some(),
        );
        if self.last_frame_signature == Some(frame_signature)
            && let Some(last_frame_at) = self.last_frame_at
            && last_frame_at.elapsed() < Duration::from_millis(250)
        {
            self.skipped_redraws = self.skipped_redraws.saturating_add(1);
            return Ok(());
        }
        render_with_retry("run-view", || {
            let mut out = std::io::stdout();
            out.queue(MoveTo(0, 0))?;
            out.queue(Clear(ClearType::All))?;
            write!(out, "{bar}")?;

            for idx in 0..log_height {
                let raw_line = display_lines.get(idx).cloned().unwrap_or_default();
                let mut line = fit_line(&raw_line, width as usize, self.style.use_unicode_ellipsis);
                if self.style.use_color && self.style.dim_logs && !line.is_empty() {
                    let log_prefix = style_prefix(Some(log_line_color(&raw_line)), None, false);
                    line = format!("{log_prefix}{line}\x1B[0m");
                }
                out.queue(MoveTo(0, (idx + 1) as u16))?;
                out.queue(Clear(ClearType::CurrentLine))?;
                write!(out, "{line}")?;
            }

            out.queue(MoveTo(0, footer_row))?;
            out.queue(Clear(ClearType::CurrentLine))?;
            write!(out, "{footer}")?;
            out.flush()?;
            Ok(())
        })?;
        self.last_frame_signature = Some(frame_signature);
        self.last_frame_at = Some(Instant::now());
        Ok(())
    }

    pub(crate) fn push_log(&mut self, line: String) {
        self.logs.push(sanitize_tui_log_line(&line));
        if self.logs.len() > 500 {
            self.logs.drain(0..self.logs.len().saturating_sub(500));
        }
    }

    pub(crate) fn poll_input(
        &self,
        prompt_editor_open: bool,
        prompt_confirm_open: bool,
    ) -> Result<Option<TuiAction>> {
        if event::poll(Duration::from_millis(10)).context("poll input failed")? {
            let ev = event::read()?;
            return Ok(match ev {
                Event::Resize(_, _) => Some(TuiAction::Redraw),
                Event::Key(KeyEvent {
                    code, modifiers, ..
                }) => {
                    map_run_tui_key_action(code, modifiers, prompt_editor_open, prompt_confirm_open)
                }
                _ => None,
            });
        }
        Ok(None)
    }

    pub(crate) fn shutdown(&mut self) -> Result<()> {
        self.raw_mode_guard.release()?;
        Ok(())
    }
}

pub(crate) fn active_list_note(filter: &InjectionFilterState) -> Option<String> {
    let (active, total) = filter.active_counts();
    if total == 0 {
        None
    } else {
        Some(format!("active {active}/{total}"))
    }
}

pub(crate) fn sync_tui_overlays(
    tui_state: &mut TuiState,
    filter: &InjectionFilterState,
    prompt_editor: &PromptEditorState,
) {
    let mut note = active_list_note(filter).unwrap_or_default();
    if prompt_editor.open {
        let editor_note = format!(
            "prompt {} / {} chars",
            prompt_editor.current.chars().count(),
            prompt_editor.max_chars
        );
        if note.is_empty() {
            note = editor_note;
        } else {
            note = format!("{note} {editor_note}");
        }
        tui_state.set_overlay_lines(Some(render_prompt_editor_popup(
            prompt_editor,
            tui_state.width as usize,
            tui_state.max_logs,
            tui_state.style.use_unicode_ellipsis,
        )));
        let sep = if tui_state.style.use_unicode_ellipsis {
            " · "
        } else {
            " . "
        };
        tui_state.set_overlay_help(Some(format!(
            "prompt editor{sep}↑/↓ select{sep}enter apply{sep}type/backspace edit{sep}d delete{sep}c clear{sep}u undo{sep}y/n confirm{sep}e/esc close"
        )));
    } else if filter.popup_open {
        tui_state.set_overlay_lines(Some(render_active_list_popup(
            filter,
            tui_state.width as usize,
            tui_state.max_logs,
            tui_state.style.use_unicode_ellipsis,
        )));
        let sep = if tui_state.style.use_unicode_ellipsis {
            " · "
        } else {
            " . "
        };
        tui_state.set_overlay_help(Some(format!(
            "active list{sep}<-/-> cols{sep}↑/↓ rows{sep}space/enter toggle{sep}a all on{sep}d all off{sep}q/esc close"
        )));
    } else {
        tui_state.set_overlay_lines(None);
        tui_state.set_overlay_help(None);
    }
    if note.is_empty() {
        tui_state.set_footer_note(None);
    } else {
        tui_state.set_footer_note(Some(note));
    }
}

pub(crate) fn render_prompt_editor_popup(
    editor: &PromptEditorState,
    width: usize,
    max_lines: usize,
    use_unicode: bool,
) -> Vec<String> {
    let on = if use_unicode { "●" } else { "*" };
    let off = if use_unicode { "○" } else { "-" };
    let pointer = if use_unicode { "▶▶" } else { ">>" };
    let col_width = width.saturating_sub(2).max(20);
    let mut lines = Vec::new();
    lines.push("Prompt Editor - edit or choose history".to_string());
    lines.push(format!(
        "current ({} / {}): {}",
        editor.current.chars().count(),
        editor.max_chars,
        fit_line(&editor.current, col_width.saturating_sub(24), use_unicode)
    ));
    lines.push("choose source (original first):".to_string());

    let mut entries = Vec::new();
    entries.push((
        "original".to_string(),
        editor.original.clone(),
        "-".to_string(),
    ));
    for item in &editor.history {
        entries.push((
            "history".to_string(),
            item.text.clone(),
            compact_timestamp(&item.created_at),
        ));
    }

    let rows = max_lines.saturating_sub(lines.len()).max(1);
    for idx in 0..rows {
        let Some((kind, text, when)) = entries.get(idx) else {
            lines.push(String::new());
            continue;
        };
        let marker = if idx == editor.selected_idx {
            pointer
        } else {
            "  "
        };
        let enabled = if idx == editor.selected_idx { on } else { off };
        lines.push(format!(
            "{marker} {enabled} {kind:<8} [{}] {}",
            when,
            fit_line(text, col_width.saturating_sub(24), use_unicode)
        ));
    }

    if let Some(confirm) = editor.confirm {
        let msg = match confirm {
            PromptEditorConfirm::DeleteSelected => "confirm delete selected history? y yes / n no",
            PromptEditorConfirm::ClearAll => "confirm clear all history? y yes / n no",
        };
        lines.push(msg.to_string());
    }
    lines
}

pub(crate) fn render_active_list_popup(
    filter: &InjectionFilterState,
    width: usize,
    max_lines: usize,
    use_unicode: bool,
) -> Vec<String> {
    let mark_on = if use_unicode { "●" } else { "*" };
    let mark_off = if use_unicode { "○" } else { "-" };
    let mark_partial = if use_unicode { "◐" } else { "+" };
    let focus_selected = if use_unicode { "▶▶ " } else { ">>>" };
    let focus_column = if use_unicode { "▸  " } else { "-->" };
    let focus_none = "   ";
    let spacer = if use_unicode { " │ " } else { " | " };
    let col_width = (width.saturating_sub(8) / 3).max(18);
    let active_column = filter.cursor.column;

    let sessions = filter.sessions();
    let selected_session = sessions.get(filter.cursor.session_idx).cloned();
    let windows = selected_session
        .as_deref()
        .map(|session| filter.windows_for(session))
        .unwrap_or_default();
    let selected_window = selected_session
        .as_deref()
        .and_then(|session| {
            windows
                .get(filter.cursor.window_idx)
                .map(|window| (session, window))
        })
        .map(|(session, window)| (session.to_string(), window.to_string()));
    let panes = selected_window
        .as_ref()
        .map(|(session, window)| filter.panes_for(session, window))
        .unwrap_or_default();

    let mut lines = Vec::new();
    lines.push("Active List - injection filter".to_string());
    let session_header = if matches!(active_column, ActiveListColumn::Session) {
        "» SESSIONS «"
    } else {
        "sessions"
    };
    let window_header = if matches!(active_column, ActiveListColumn::Window) {
        "» WINDOWS «"
    } else {
        "windows"
    };
    let pane_header = if matches!(active_column, ActiveListColumn::Pane) {
        "» PANES «"
    } else {
        "panes"
    };
    lines.push(format!(
        "{}{}{}{}{}",
        pad_to_width(session_header, col_width),
        spacer,
        pad_to_width(window_header, col_width),
        spacer,
        pad_to_width(pane_header, col_width)
    ));

    let rows = max_lines.saturating_sub(lines.len()).max(1);
    for idx in 0..rows {
        let session_cell = if let Some(session) = sessions.get(idx) {
            let targets = filter
                .known_targets
                .values()
                .filter(|item| item.session == *session)
                .map(|item| item.target.clone())
                .collect::<Vec<_>>();
            let enabled = targets
                .iter()
                .filter(|target| filter.is_allowed(target))
                .count();
            let mark = if enabled == 0 {
                mark_off
            } else if enabled == targets.len() {
                mark_on
            } else {
                mark_partial
            };
            let pointer = if matches!(active_column, ActiveListColumn::Session)
                && idx == filter.cursor.session_idx
            {
                focus_selected
            } else if matches!(active_column, ActiveListColumn::Session) {
                focus_column
            } else {
                focus_none
            };
            format!("{pointer}{mark} {session} ({enabled}/{})", targets.len())
        } else {
            String::new()
        };

        let window_cell = if let Some(session) = selected_session.as_ref() {
            if let Some(window) = windows.get(idx) {
                let targets = filter
                    .known_targets
                    .values()
                    .filter(|item| item.session == *session && item.window == *window)
                    .map(|item| item.target.clone())
                    .collect::<Vec<_>>();
                let enabled = targets
                    .iter()
                    .filter(|target| filter.is_allowed(target))
                    .count();
                let mark = if enabled == 0 {
                    mark_off
                } else if enabled == targets.len() {
                    mark_on
                } else {
                    mark_partial
                };
                let pointer = if matches!(active_column, ActiveListColumn::Window)
                    && idx == filter.cursor.window_idx
                {
                    focus_selected
                } else if matches!(active_column, ActiveListColumn::Window) {
                    focus_column
                } else {
                    focus_none
                };
                format!(
                    "{pointer}{mark} {session}:{window} ({enabled}/{})",
                    targets.len()
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let pane_cell = if let Some(target) = panes.get(idx) {
            let mark = if filter.is_allowed(target) {
                mark_on
            } else {
                mark_off
            };
            let pointer = if matches!(active_column, ActiveListColumn::Pane)
                && idx == filter.cursor.pane_idx
            {
                focus_selected
            } else if matches!(active_column, ActiveListColumn::Pane) {
                focus_column
            } else {
                focus_none
            };
            format!("{pointer}{mark} {target}")
        } else {
            String::new()
        };

        lines.push(format!(
            "{}{}{}{}{}",
            pad_to_width(&fit_line(&session_cell, col_width, use_unicode), col_width),
            spacer,
            pad_to_width(&fit_line(&window_cell, col_width, use_unicode), col_width),
            spacer,
            fit_line(&pane_cell, col_width, use_unicode)
        ));
    }

    lines
}

pub(crate) fn render_status_bar(args: &StatusBarRenderArgs<'_>) -> String {
    let state = args.state;
    let layout = args.layout;
    let icon_mode = args.icon_mode;
    let style = args.style;
    let width = args.width;
    let config = args.config;
    let current = args.current;
    let total = args.total;
    let rule_id = args.rule_id;
    let elapsed = args.elapsed;
    let remaining_duration = args.remaining_duration;
    let process_usage = args.process_usage;
    let (icon, label) = state_label(state, icon_mode);
    let progress = if config.infinite {
        "inf".to_string()
    } else {
        format!("{}/{}", current, total)
    };
    let percent = if config.infinite || total == 0 {
        "--".to_string()
    } else {
        format!("{}%", (current * 100 / total))
    };
    let bar = render_progress_bar(current, total, layout, style.use_unicode_ellipsis);
    let trigger = rule_id.unwrap_or("-");
    let profile = config.profile_id.as_deref().unwrap_or("-");

    let icon_glyph = if style.use_unicode_ellipsis {
        icon
    } else {
        ascii_icon(icon)
    };
    let state_text = format!("{icon_glyph} {label}");
    let iter_text = if config.infinite {
        "iter ∞".to_string()
    } else {
        format!("iter {progress}")
    };
    let trigger_text = truncate_text(
        trigger,
        match layout {
            LayoutMode::Compact => 16,
            LayoutMode::Standard => 28,
            LayoutMode::Wide => 44,
        },
        style.use_unicode_ellipsis,
    );
    let trigger_prefix = if config.exec_command.is_some() {
        "evt"
    } else {
        "trg"
    };

    let sep_text = if style.use_unicode_ellipsis {
        " · "
    } else {
        " . "
    };

    let mut left_parts = Vec::new();
    left_parts.push(state_text.clone());
    left_parts.push(iter_text);
    left_parts.push(format!("{bar} {percent}"));

    let mut right_parts = Vec::new();
    match layout {
        LayoutMode::Compact => {
            if let Some(next_scan) = args.next_scan_remaining {
                right_parts.push(format!("next {next_scan}"));
            }
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("run {profile}"));
            if let Some(usage) = process_usage {
                right_parts.push(usage.to_string());
            }
            right_parts.push(format!("{trigger_prefix} {trigger_text}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(config.target_label.clone());
        }
        LayoutMode::Standard => {
            if let Some(next_scan) = args.next_scan_remaining {
                right_parts.push(format!("next {next_scan}"));
            }
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("run {profile}"));
            if let Some(usage) = process_usage {
                right_parts.push(usage.to_string());
            }
            right_parts.push(format!("{trigger_prefix} {trigger_text}"));
            right_parts.push(format!("last {elapsed}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(config.target_label.clone());
        }
        LayoutMode::Wide => {
            if let Some(next_scan) = args.next_scan_remaining {
                right_parts.push(format!("next {next_scan}"));
            }
            if let Some(remaining) = remaining_duration {
                right_parts.push(format!("rem {remaining}"));
            }
            right_parts.push(format!("run {profile}"));
            if let Some(usage) = process_usage {
                right_parts.push(usage.to_string());
            }
            right_parts.push(format!("{trigger_prefix} {trigger_text}"));
            right_parts.push(format!("last {elapsed}"));
            right_parts.push(format!("v{}", LOOPMUX_VERSION));
            right_parts.push(format!("target {}", config.target_label));
        }
    }

    let left_sep_text = if matches!(layout, LayoutMode::Compact) {
        " "
    } else {
        sep_text
    };
    let left_text = left_parts.join(left_sep_text);
    let right_sep_text = if matches!(layout, LayoutMode::Compact) {
        " "
    } else {
        sep_text
    };
    let mut right_text = right_parts.join(right_sep_text);
    let mut line = if right_text.is_empty() {
        left_text.clone()
    } else {
        let width_usize = width as usize;
        let left_len = left_text.chars().count();
        let right_len = right_text.chars().count();
        let gap = 1;
        if left_len + gap + right_len > width_usize {
            let available = width_usize.saturating_sub(left_len + gap);
            if available > 0 {
                right_text = truncate_text(&right_text, available, style.use_unicode_ellipsis);
                format!("{left_text}{}{}", " ".repeat(gap), right_text)
            } else {
                left_text.clone()
            }
        } else {
            let padding = width_usize.saturating_sub(left_len + gap + right_len);
            format!(
                "{left_text}{}{}{}",
                " ".repeat(gap),
                " ".repeat(padding),
                right_text
            )
        }
    };
    line = pad_to_width(&line, width as usize);

    if style.use_color {
        let label_color = state_color(state);
        let base_prefix = style_prefix(Some(248), style.use_bg.then_some(236), false);
        let state_prefix = format!("\x1B[38;5;{label_color}m");
        let sep_prefix = style_prefix(Some(240), style.use_bg.then_some(236), false);
        let colored_state = format!("{state_prefix}{state_text}{base_prefix}");
        let mut colored_line = line.replacen(&state_text, &colored_state, 1);
        colored_line =
            colored_line.replace(sep_text, &format!("{sep_prefix}{sep_text}{base_prefix}"));
        format!("{base_prefix}{colored_line}\x1B[0m")
    } else {
        line
    }
}

pub(crate) fn parse_process_usage_summary(ps_stdout: &str) -> Option<String> {
    let line = ps_stdout
        .lines()
        .find(|line| !line.trim().is_empty())?
        .trim();
    let mut fields = line.split_whitespace();
    let cpu = fields.next()?.parse::<f64>().ok()?;
    let rss_kb = fields.next()?.parse::<u64>().ok()?;
    let mem_mb = rss_kb as f64 / 1024.0;
    Some(format!("cpu {:.1}% mem {:.1}mb", cpu, mem_mb))
}

pub(crate) fn state_label(state: LoopState, icon_mode: IconMode) -> (&'static str, &'static str) {
    match (state, icon_mode) {
        (LoopState::Running, IconMode::Nerd) => ("󰐊", "RUN"),
        (LoopState::Holding, IconMode::Nerd) => ("󰏤", "HOLD"),
        (LoopState::Delay, IconMode::Nerd) => ("󰔟", "DELAY"),
        (LoopState::Error, IconMode::Nerd) => ("󰅚", "ERROR"),
        (LoopState::Stopped, IconMode::Nerd) => ("󰩈", "STOP"),
        (LoopState::Waiting, IconMode::Nerd) => ("󰔟", "WAIT"),
        (LoopState::Sending, IconMode::Nerd) => ("󰐊", "SEND"),
        (LoopState::Running, IconMode::Ascii) => (">", "RUN"),
        (LoopState::Holding, IconMode::Ascii) => ("||", "HOLD"),
        (LoopState::Delay, IconMode::Ascii) => ("...", "DELAY"),
        (LoopState::Error, IconMode::Ascii) => ("!", "ERROR"),
        (LoopState::Stopped, IconMode::Ascii) => ("x", "STOP"),
        (LoopState::Waiting, IconMode::Ascii) => ("...", "WAIT"),
        (LoopState::Sending, IconMode::Ascii) => (">", "SEND"),
    }
}

pub(crate) fn render_progress_bar(
    current: u32,
    total: u32,
    layout: LayoutMode,
    unicode: bool,
) -> String {
    let width = match layout {
        LayoutMode::Compact => 6,
        LayoutMode::Standard => 10,
        LayoutMode::Wide => 14,
    };
    if total == 0 {
        return if unicode {
            "░".repeat(width)
        } else {
            ".".repeat(width)
        };
    }
    let filled = ((current as f64 / total as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    let filled_char = if unicode { "▰" } else { "=" };
    let empty_char = if unicode { "▱" } else { "." };
    format!(
        "{}{}",
        filled_char.repeat(filled),
        empty_char.repeat(width - filled)
    )
}

pub(crate) fn truncate_text(text: &str, max: usize, use_unicode: bool) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let suffix = if use_unicode { "…" } else { "..." };
    let suffix_len = suffix.chars().count();
    if max <= suffix_len {
        return text.chars().take(max).collect();
    }
    let mut s = text
        .chars()
        .take(max.saturating_sub(suffix_len))
        .collect::<String>();
    s.push_str(suffix);
    s
}

pub(crate) fn pad_to_width(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.chars().take(width).collect();
    }
    let padding = width - len;
    format!("{text}{}", " ".repeat(padding))
}

pub(crate) fn ascii_icon(icon: &str) -> &str {
    match icon {
        "󰐊" => ">",
        "󰏤" => "||",
        "󰔟" => "...",
        "󰅚" => "!",
        "󰩈" => "x",
        _ => ">",
    }
}

pub(crate) fn state_color(state: LoopState) -> u8 {
    match state {
        LoopState::Running => 71,
        LoopState::Holding => 179,
        LoopState::Waiting | LoopState::Delay => 109,
        LoopState::Error => 166,
        LoopState::Stopped => 246,
        LoopState::Sending => 109,
    }
}

pub(crate) fn style_prefix(fg: Option<u8>, bg: Option<u8>, bold: bool) -> String {
    let mut prefix = String::new();
    if bold {
        prefix.push_str("\x1B[1m");
    }
    if let Some(fg) = fg {
        prefix.push_str(&format!("\x1B[38;5;{fg}m"));
    }
    if let Some(bg) = bg {
        prefix.push_str(&format!("\x1B[48;5;{bg}m"));
    }
    prefix
}

pub(crate) fn fit_line(text: &str, width: usize, use_unicode: bool) -> String {
    if text.chars().count() <= width {
        return pad_to_width(text, width);
    }
    truncate_text(text, width, use_unicode)
}

pub(crate) fn timestamp_now() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

pub(crate) fn timestamp_local_now() -> String {
    OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

pub(crate) fn log_line_color(line: &str) -> u8 {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    log_line_color_at(line, now)
}

pub(crate) fn log_line_color_at(line: &str, now: OffsetDateTime) -> u8 {
    if let Some(timestamp) = parse_log_timestamp(line) {
        let local_timestamp = timestamp.to_offset(now.offset());
        if local_timestamp.date() == now.date() {
            return 251;
        }
        return 244;
    }
    if looks_like_compact_time_prefix(line) {
        return 249;
    }
    245
}

#[cfg(test)]
pub(crate) fn log_line_date(line: &str) -> Option<&str> {
    if !line.starts_with('[') {
        return None;
    }
    let close = line.find(']')?;
    let ts = line.get(1..close)?;
    let date = ts.split('T').next()?;
    if date.len() == 10 { Some(date) } else { None }
}

pub(crate) fn parse_log_timestamp(line: &str) -> Option<OffsetDateTime> {
    if !line.starts_with('[') {
        return None;
    }
    let close = line.find(']')?;
    let ts = line.get(1..close)?;
    OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok()
}

pub(crate) fn looks_like_compact_time_prefix(line: &str) -> bool {
    let mut parts = line.split(':');
    let (Some(h), Some(m), Some(s)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    h.len() == 2
        && m.len() == 2
        && s.len() >= 2
        && h.chars().all(|ch| ch.is_ascii_digit())
        && m.chars().all(|ch| ch.is_ascii_digit())
        && s.chars().take(2).all(|ch| ch.is_ascii_digit())
}
