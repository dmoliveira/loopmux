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

pub(crate) use crate::run_view_render::*;
