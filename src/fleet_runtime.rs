use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::QueueableCommand;
use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::terminal::{Clear, ClearType};

use crate::{FleetControlCommand, FleetVisibleArgs};
use crate::{
    FleetDetailRenderArgs, FleetListedRun, FleetPaneRenderer, FleetSortMode, FleetStateFilter,
    FleetViewPreset, LegacyFleetPaneRenderer, PendingFleetAction, RawModeGuard,
    dispatch_fleet_command, fit_line, fleet_bulk_confirmation, fleet_command_label,
    fleet_header_line, fleet_manager_counts, fleet_manager_visible_runs, fleet_status_line,
    jump_to_tmux_target, load_fleet_runs, pad_to_width, render_with_retry, truncate_text,
};

pub(crate) fn run_fleet_manager_tui(profile_filter: Option<&str>) -> Result<()> {
    let _raw_mode_guard = RawModeGuard::acquire("failed to enable raw mode for fleet manager")?;
    run_fleet_manager_tui_inner(false, profile_filter)
}

pub(crate) fn run_fleet_manager_tui_embedded() -> Result<()> {
    run_fleet_manager_tui_inner(true, None)
}

#[derive(Clone, Copy)]
pub(crate) enum FleetLoopPhase {
    Tick,
    Render,
    Input,
}

pub(crate) fn fleet_phase_label(phase: FleetLoopPhase) -> &'static str {
    match phase {
        FleetLoopPhase::Tick => "tick",
        FleetLoopPhase::Render => "render",
        FleetLoopPhase::Input => "input",
    }
}

pub(crate) fn fleet_mark_resize(force_full_redraw: &mut bool, needs_refresh: &mut bool) {
    *force_full_redraw = true;
    *needs_refresh = true;
}

pub(crate) fn fleet_step_selection_left(selected: usize, runs_len: usize) -> usize {
    if runs_len == 0 {
        0
    } else if selected == 0 {
        runs_len - 1
    } else {
        selected - 1
    }
}

pub(crate) fn fleet_step_selection_right(selected: usize, runs_len: usize) -> usize {
    if runs_len == 0 {
        0
    } else {
        (selected + 1) % runs_len
    }
}

pub(crate) fn run_fleet_manager_tui_inner(
    embedded: bool,
    profile_filter: Option<&str>,
) -> Result<()> {
    let mut selected: usize = 0;
    let mut selected_run_id: Option<String> = None;
    let mut message = String::from("fleet manager ready");
    let mut show_stale = false;
    let mut mismatch_only = false;
    let mut state_filter = FleetStateFilter::All;
    let mut sort_mode = FleetSortMode::LastSeen;
    let mut view_preset = FleetViewPreset::Default;
    let mut search_query = String::new();
    let mut search_mode = false;
    let mut selected_ids: HashSet<String> = HashSet::new();
    let mut pending_action: Option<PendingFleetAction> = None;
    let mut last_lines: Vec<String> = Vec::new();
    let mut force_full_redraw = true;
    let mut last_refresh = std::time::Instant::now() - Duration::from_secs(1);
    let refresh_interval = Duration::from_millis(450);
    let mut needs_refresh = true;

    let mut all_runs: Vec<FleetListedRun> = Vec::new();
    let mut runs: Vec<FleetListedRun> = Vec::new();
    let mut counts = (0, 0, 0, 0);
    let pane_renderer = LegacyFleetPaneRenderer;

    loop {
        let mut phase = FleetLoopPhase::Tick;
        if needs_refresh || last_refresh.elapsed() >= refresh_interval {
            all_runs = load_fleet_runs().with_context(|| {
                format!(
                    "fleet manager refresh failed in {} phase",
                    fleet_phase_label(phase)
                )
            })?;
            runs = fleet_manager_visible_runs(FleetVisibleArgs {
                runs: &all_runs,
                profile_filter,
                show_stale,
                mismatch_only,
                state_filter,
                search_query: &search_query,
                sort_mode,
                view_preset,
            });
            counts = fleet_manager_counts(&all_runs);
            last_refresh = std::time::Instant::now();
            needs_refresh = false;

            let run_ids: HashSet<&str> =
                all_runs.iter().map(|run| run.record.id.as_str()).collect();
            selected_ids.retain(|id| run_ids.contains(id.as_str()));

            if runs.is_empty() {
                selected = 0;
                selected_run_id = None;
                pending_action = None;
            } else if let Some(id) = selected_run_id.as_deref() {
                if let Some(pos) = runs.iter().position(|run| run.record.id == id) {
                    selected = pos;
                } else {
                    selected = selected.min(runs.len() - 1);
                    selected_run_id = Some(runs[selected].record.id.clone());
                }
            } else {
                selected = selected.min(runs.len() - 1);
                selected_run_id = Some(runs[selected].record.id.clone());
            }
        }

        phase = FleetLoopPhase::Render;
        let (width, height) = crossterm::terminal::size().unwrap_or((120, 30));
        let header = fleet_header_line(
            runs.len(),
            all_runs.len(),
            if runs.is_empty() { 0 } else { selected + 1 },
            counts,
            embedded,
        );
        let status = fleet_status_line(
            view_preset,
            sort_mode,
            state_filter,
            show_stale,
            mismatch_only,
            &search_query,
            profile_filter,
        );

        let content_rows = height.saturating_sub(3) as usize;
        let lines = pane_renderer.render_list_lines(&runs, content_rows, selected, &selected_ids);

        let selected_run = runs.get(selected);
        let details = pane_renderer.render_detail_lines(FleetDetailRenderArgs {
            selected_run,
            profile_filter,
            show_stale,
            mismatch_only,
            state_filter,
            search_query: &search_query,
            counts,
            sort_mode,
            view_preset,
            marked_count: selected_ids.len(),
            pending_action: pending_action.as_ref(),
        });

        let footer = format!(
            "nav <-/-> · mark space · clear a · presets p/1-4 · sort o · filters x/v/f · search / · single h/r/n/R/s · bulk S/H/P/N/U · enter confirm · c cancel · i id · y stop-cmd · q/esc {} · {}",
            if embedded {
                "return to run"
            } else {
                "quit manager"
            },
            truncate_text(&message, width.saturating_sub(130) as usize, true)
        );

        let split_mode = width >= 120;
        let left_width = ((width as usize) * 54 / 100)
            .max(46)
            .min((width as usize).saturating_sub(24));
        let right_width = (width as usize).saturating_sub(left_width + 3);
        let mut screen_lines = vec![String::new(); height as usize];
        if !screen_lines.is_empty() {
            screen_lines[0] = fit_line(&header, width as usize, true);
        }
        if screen_lines.len() > 1 {
            screen_lines[1] = fit_line(&status, width as usize, true);
        }
        for idx in 0..content_rows {
            let row = idx + 2;
            if row >= screen_lines.len().saturating_sub(1) {
                break;
            }
            if split_mode {
                let left = lines.get(idx).map(|value| value.as_str()).unwrap_or("");
                let right = details.get(idx).map(|value| value.as_str()).unwrap_or("");
                screen_lines[row] = fit_line(
                    &format!(
                        "{} | {}",
                        pad_to_width(&fit_line(left, left_width, true), left_width),
                        fit_line(right, right_width, true)
                    ),
                    width as usize,
                    true,
                );
            } else {
                let line = lines.get(idx).map(|value| value.as_str()).unwrap_or("");
                screen_lines[row] = fit_line(line, width as usize, true);
            }
        }
        if height > 0 {
            let footer_row = height.saturating_sub(1) as usize;
            screen_lines[footer_row] = fit_line(&footer, width as usize, true);
        }

        if force_full_redraw || screen_lines != last_lines {
            let render_context = format!("fleet-manager-{}", fleet_phase_label(phase));
            render_with_retry(&render_context, || {
                let mut out = std::io::stdout();
                if force_full_redraw {
                    out.queue(MoveTo(0, 0))?;
                    out.queue(Clear(ClearType::All))?;
                }
                for (row, line) in screen_lines.iter().enumerate() {
                    if force_full_redraw || last_lines.get(row) != Some(line) {
                        out.queue(MoveTo(0, row as u16))?;
                        out.queue(Clear(ClearType::CurrentLine))?;
                        write!(out, "{}", line)?;
                    }
                }
                out.flush()?;
                Ok(())
            })?;
            last_lines = screen_lines;
            force_full_redraw = false;
        }

        phase = FleetLoopPhase::Input;
        if event::poll(Duration::from_millis(80)).with_context(|| {
            format!(
                "fleet manager poll failed in {} phase",
                fleet_phase_label(phase)
            )
        })? {
            match event::read().with_context(|| {
                format!(
                    "fleet manager read failed in {} phase",
                    fleet_phase_label(phase)
                )
            })? {
                Event::Resize(_, _) => {
                    fleet_mark_resize(&mut force_full_redraw, &mut needs_refresh);
                }
                Event::Key(KeyEvent { code, .. }) => {
                    if search_mode {
                        match code {
                            KeyCode::Esc => {
                                search_mode = false;
                                message = "search cancelled".to_string();
                            }
                            KeyCode::Enter => {
                                search_mode = false;
                                message = if search_query.is_empty() {
                                    "search cleared".to_string()
                                } else {
                                    format!("search applied: {}", search_query)
                                };
                            }
                            KeyCode::Backspace => {
                                search_query.pop();
                                selected = 0;
                                selected_run_id = runs.first().map(|run| run.record.id.clone());
                                pending_action = None;
                                message = format!("search: {}", search_query);
                            }
                            KeyCode::Char(c) => {
                                search_query.push(c);
                                selected = 0;
                                selected_run_id = runs.first().map(|run| run.record.id.clone());
                                pending_action = None;
                                message = format!("search: {}", search_query);
                            }
                            _ => {}
                        }
                        needs_refresh = true;
                        continue;
                    }

                    if let Some(control_key) = fleet_control_key(&code) {
                        match control_key {
                            FleetControlKey::Quit => break,
                            FleetControlKey::MoveLeft => {
                                if !runs.is_empty() {
                                    selected = fleet_step_selection_left(selected, runs.len());
                                    selected_run_id = Some(runs[selected].record.id.clone());
                                }
                                pending_action = None;
                                needs_refresh = true;
                                continue;
                            }
                            FleetControlKey::MoveRight => {
                                if !runs.is_empty() {
                                    selected = fleet_step_selection_right(selected, runs.len());
                                    selected_run_id = Some(runs[selected].record.id.clone());
                                }
                                pending_action = None;
                                needs_refresh = true;
                                continue;
                            }
                        }
                    }

                    match code {
                        KeyCode::Enter => {
                            if let Some(action) = pending_action.take() {
                                message = apply_pending_fleet_action(&action);
                            } else {
                                message = apply_selected_fleet_jump(&runs, selected);
                            }
                        }
                        KeyCode::Char(' ') => {
                            if let Some(run) = runs.get(selected) {
                                if !selected_ids.insert(run.record.id.clone()) {
                                    selected_ids.remove(&run.record.id);
                                }
                                message = format!("marked runs={}", selected_ids.len());
                            } else {
                                message = "no run selected".to_string();
                            }
                            pending_action = None;
                        }
                        KeyCode::Char('a') => {
                            selected_ids.clear();
                            pending_action = None;
                            message = "cleared marked runs".to_string();
                        }
                        KeyCode::Char('x') => {
                            show_stale = !show_stale;
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = if show_stale {
                                "showing stale + active runs".to_string()
                            } else {
                                "showing active runs only".to_string()
                            };
                        }
                        KeyCode::Char('v') => {
                            mismatch_only = !mismatch_only;
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = if mismatch_only {
                                "showing version mismatches only".to_string()
                            } else {
                                "showing all version states".to_string()
                            };
                        }
                        KeyCode::Char('f') => {
                            state_filter = state_filter.next();
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("state filter={}", state_filter.label());
                        }
                        KeyCode::Char('o') => {
                            sort_mode = sort_mode.next();
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("sort={}", sort_mode.label());
                        }
                        KeyCode::Char('p') => {
                            view_preset = view_preset.next();
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('1') => {
                            view_preset = FleetViewPreset::Default;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('2') => {
                            view_preset = FleetViewPreset::NeedsAttention;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('3') => {
                            view_preset = FleetViewPreset::MismatchOnly;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('4') => {
                            view_preset = FleetViewPreset::Holding;
                            apply_view_preset(
                                view_preset,
                                &mut show_stale,
                                &mut mismatch_only,
                                &mut state_filter,
                                &mut sort_mode,
                            );
                            selected = 0;
                            selected_run_id = None;
                            pending_action = None;
                            message = format!("preset={}", view_preset.label());
                        }
                        KeyCode::Char('/') => {
                            search_mode = true;
                            pending_action = None;
                            message = format!("search: {}", search_query);
                        }
                        KeyCode::Char('s') => {
                            if let Some(run) = runs.get(selected) {
                                pending_action = Some(PendingFleetAction::SingleStop {
                                    run_id: run.record.id.clone(),
                                    run_name: run.record.name.clone(),
                                });
                                message = fleet_single_stop_confirmation(&run.record.name);
                            } else {
                                message = "no run selected".to_string();
                            }
                        }
                        KeyCode::Char('S') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Stop,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('H') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Hold,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('P') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Resume,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('N') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Next,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('U') => {
                            pending_action = arm_bulk_action(
                                FleetControlCommand::Renew,
                                &selected_ids,
                                &runs,
                                selected,
                                &mut message,
                            );
                        }
                        KeyCode::Char('c') => {
                            pending_action = None;
                            message = fleet_pending_action_cleared_message().to_string();
                        }
                        KeyCode::Char('i') => {
                            pending_action = None;
                            message = copy_selected_run_id(&runs, selected);
                        }
                        KeyCode::Char('y') => {
                            pending_action = None;
                            message = copy_selected_run_command(&runs, selected);
                        }
                        KeyCode::Char('h') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Hold,
                            );
                        }
                        KeyCode::Char('r') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Resume,
                            );
                        }
                        KeyCode::Char('n') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Next,
                            );
                        }
                        KeyCode::Char('R') => {
                            pending_action = None;
                            message = apply_selected_fleet_command(
                                &runs,
                                selected,
                                FleetControlCommand::Renew,
                            );
                        }
                        _ => {}
                    }
                    needs_refresh = true;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_view_preset(
    preset: FleetViewPreset,
    show_stale: &mut bool,
    mismatch_only: &mut bool,
    state_filter: &mut FleetStateFilter,
    sort_mode: &mut FleetSortMode,
) {
    match preset {
        FleetViewPreset::Default => {
            *show_stale = false;
            *mismatch_only = false;
            *state_filter = FleetStateFilter::All;
            *sort_mode = FleetSortMode::LastSeen;
        }
        FleetViewPreset::NeedsAttention => {
            *show_stale = true;
            *mismatch_only = false;
            *state_filter = FleetStateFilter::All;
            *sort_mode = FleetSortMode::Health;
        }
        FleetViewPreset::MismatchOnly => {
            *show_stale = true;
            *mismatch_only = true;
            *state_filter = FleetStateFilter::All;
            *sort_mode = FleetSortMode::LastSeen;
        }
        FleetViewPreset::Holding => {
            *show_stale = true;
            *mismatch_only = false;
            *state_filter = FleetStateFilter::Holding;
            *sort_mode = FleetSortMode::Sends;
        }
    }
}

pub(crate) fn fleet_single_stop_confirmation(run_name: &str) -> String {
    format!("confirm stop {}: press Enter, or c to cancel", run_name)
}

pub(crate) fn fleet_pending_action_cleared_message() -> &'static str {
    "pending action cleared"
}

pub(crate) fn arm_bulk_action(
    command: FleetControlCommand,
    selected_ids: &HashSet<String>,
    runs: &[FleetListedRun],
    selected: usize,
    message: &mut String,
) -> Option<PendingFleetAction> {
    let mut targets: Vec<&FleetListedRun> = if selected_ids.is_empty() {
        runs.get(selected).map(|run| vec![run]).unwrap_or_default()
    } else {
        runs.iter()
            .filter(|run| selected_ids.contains(&run.record.id))
            .collect()
    };
    if targets.is_empty() {
        *message = "no runs selected for bulk action".to_string();
        return None;
    }
    targets.sort_by(|a, b| a.record.name.cmp(&b.record.name));
    let run_ids = targets.iter().map(|run| run.record.id.clone()).collect();
    let run_names: Vec<String> = targets.iter().map(|run| run.record.name.clone()).collect();
    *message = fleet_bulk_confirmation(command, run_names.len());
    Some(PendingFleetAction::Bulk {
        command,
        run_ids,
        run_names,
    })
}

pub(crate) fn apply_pending_fleet_action(action: &PendingFleetAction) -> String {
    match action {
        PendingFleetAction::SingleStop { run_id, run_name } => {
            match dispatch_fleet_command(run_id, FleetControlCommand::Stop) {
                Ok(_) => format!("sent stop to {}", run_name),
                Err(err) => format!("stop failed: {err}"),
            }
        }
        PendingFleetAction::Bulk {
            command,
            run_ids,
            run_names,
        } => {
            let mut ok = 0usize;
            let mut errors = Vec::new();
            for run_id in run_ids {
                match dispatch_fleet_command(run_id, *command) {
                    Ok(_) => ok += 1,
                    Err(err) => errors.push(format!("{}: {}", run_id, err)),
                }
            }
            if errors.is_empty() {
                format!(
                    "sent {} to {} run(s): {}",
                    fleet_command_label(*command),
                    ok,
                    truncate_text(&run_names.join(", "), 100, true)
                )
            } else {
                format!(
                    "{} sent to {} run(s), {} failed ({})",
                    fleet_command_label(*command),
                    ok,
                    errors.len(),
                    truncate_text(&errors.join("; "), 100, true)
                )
            }
        }
    }
}

pub(crate) fn apply_selected_fleet_command(
    runs: &[FleetListedRun],
    selected: usize,
    command: FleetControlCommand,
) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    match dispatch_fleet_command(&run.record.id, command) {
        Ok(_) => format!(
            "sent {} to {}",
            fleet_command_label(command),
            run.record.name
        ),
        Err(err) => format!("command failed: {err}"),
    }
}

pub(crate) fn apply_selected_fleet_jump(runs: &[FleetListedRun], selected: usize) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    match jump_to_tmux_target(&run.record.target) {
        Ok(()) => format!("jumped to {} ({})", run.record.target, run.record.name),
        Err(err) => format!("jump failed: {err}"),
    }
}

pub(crate) fn copy_selected_run_id(runs: &[FleetListedRun], selected: usize) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    match crate::copy_to_clipboard(&run.record.id) {
        Ok(()) => format!("copied run id: {}", run.record.id),
        Err(err) => format!("copy failed: {err}"),
    }
}

pub(crate) fn copy_selected_run_command(runs: &[FleetListedRun], selected: usize) -> String {
    let Some(run) = runs.get(selected) else {
        return "no run selected".to_string();
    };
    let snippet = crate::fleet_stop_snippet(&run.record.id);
    match crate::copy_to_clipboard(&snippet) {
        Ok(()) => format!("copied snippet: {}", snippet),
        Err(err) => format!("copy failed: {err}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FleetControlKey {
    Quit,
    MoveLeft,
    MoveRight,
}

pub(crate) fn fleet_control_key(code: &KeyCode) -> Option<FleetControlKey> {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => Some(FleetControlKey::Quit),
        KeyCode::Char('<') | KeyCode::Left => Some(FleetControlKey::MoveLeft),
        KeyCode::Char('>') | KeyCode::Right => Some(FleetControlKey::MoveRight),
        _ => None,
    }
}
