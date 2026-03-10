use std::collections::HashSet;

use crate::{
    fleet::FleetControlCommand, fleet::FleetListedRun, fleet_command_label, truncate_text,
    FleetSortMode, FleetStateFilter, FleetViewPreset, PendingFleetAction, LOOPMUX_VERSION,
};

pub(crate) struct FleetDetailRenderArgs<'a> {
    pub(crate) selected_run: Option<&'a FleetListedRun>,
    pub(crate) profile_filter: Option<&'a str>,
    pub(crate) show_stale: bool,
    pub(crate) mismatch_only: bool,
    pub(crate) state_filter: FleetStateFilter,
    pub(crate) search_query: &'a str,
    pub(crate) counts: (usize, usize, usize, usize),
    pub(crate) sort_mode: FleetSortMode,
    pub(crate) view_preset: FleetViewPreset,
    pub(crate) marked_count: usize,
    pub(crate) pending_action: Option<&'a PendingFleetAction>,
}

pub(crate) trait FleetPaneRenderer {
    fn render_list_lines(
        &self,
        runs: &[FleetListedRun],
        content_rows: usize,
        selected: usize,
        selected_ids: &HashSet<String>,
    ) -> Vec<String>;

    fn render_detail_lines(&self, args: FleetDetailRenderArgs<'_>) -> Vec<String>;
}

pub(crate) struct LegacyFleetPaneRenderer;

impl FleetPaneRenderer for LegacyFleetPaneRenderer {
    fn render_list_lines(
        &self,
        runs: &[FleetListedRun],
        content_rows: usize,
        selected: usize,
        selected_ids: &HashSet<String>,
    ) -> Vec<String> {
        fleet_run_list_lines(runs, content_rows, selected, selected_ids)
    }

    fn render_detail_lines(&self, args: FleetDetailRenderArgs<'_>) -> Vec<String> {
        fleet_detail_lines(&args)
    }
}

pub(crate) fn fleet_detail_lines(args: &FleetDetailRenderArgs<'_>) -> Vec<String> {
    let selected_run = args.selected_run;
    let profile_filter = args.profile_filter;
    let show_stale = args.show_stale;
    let mismatch_only = args.mismatch_only;
    let state_filter = args.state_filter;
    let search_query = args.search_query;
    let counts = args.counts;
    let sort_mode = args.sort_mode;
    let view_preset = args.view_preset;
    let marked_count = args.marked_count;
    let pending_action = args.pending_action;
    let mut lines = Vec::new();
    lines.push("Details".to_string());
    lines.push(format!(
        "view={} sort={} state={}",
        view_preset.label(),
        sort_mode.label(),
        state_filter.label(),
    ));
    lines.push(format!(
        "filters stale={} mismatch={} search={}",
        if show_stale { "on" } else { "off" },
        if mismatch_only { "on" } else { "off" },
        if search_query.trim().is_empty() {
            "<none>"
        } else {
            search_query.trim()
        }
    ));
    lines.push(format!(
        "scope profile={} marked={}",
        profile_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("<all>"),
        marked_count,
    ));
    lines.push(format!(
        "summary active={} holding={} stale={} mismatch={}",
        counts.0, counts.1, counts.2, counts.3
    ));

    if let Some(action) = pending_action {
        match action {
            PendingFleetAction::SingleStop { run_name, .. } => lines.push(format!(
                "pending: stop {} (press Enter to confirm, c to cancel)",
                run_name
            )),
            PendingFleetAction::Bulk {
                command, run_names, ..
            } => {
                lines.push(format!(
                    "pending: bulk {} for {} run(s)",
                    fleet_command_label(*command),
                    run_names.len()
                ));
                lines.push(format!(
                    "targets: {}",
                    truncate_text(&run_names.join(", "), 70, true)
                ));
                lines.push("press Enter to confirm, c to cancel".to_string());
            }
        }
    }
    lines.push(String::new());

    if let Some(run) = selected_run {
        let version = if run.record.version.is_empty() {
            "unknown"
        } else {
            run.record.version.as_str()
        };
        lines.push(format!("name: {}", run.record.name));
        lines.push(format!("id: {}", run.record.id));
        lines.push(format!("pid: {}", run.record.pid));
        lines.push(format!("host: {}", run.record.host));
        lines.push(format!("state: {}", run.record.state));
        lines.push(format!("target: {}", run.record.target));
        lines.push(format!("sends: {}", run.record.sends));
        lines.push(format!(
            "version: {} ({})",
            version,
            if run.version_mismatch {
                "mismatch"
            } else {
                "match"
            }
        ));
        lines.push(format!(
            "health: {} ({}){}",
            run.health_label,
            run.health_score,
            if run.needs_attention {
                " attention"
            } else {
                ""
            }
        ));
        lines.push(format!("started: {}", run.record.started_at));
        lines.push(format!("last_seen: {}", run.record.last_seen));

        lines.push(String::new());
        lines.push("timeline (latest)".to_string());
        if run.record.events.is_empty() {
            lines.push("- no events yet".to_string());
        } else {
            for event in run.record.events.iter().rev().take(6) {
                lines.push(format!(
                    "- {} {} {}",
                    truncate_text(&event.timestamp, 19, false),
                    event.kind,
                    truncate_text(&event.detail, 48, true)
                ));
            }
        }
    } else {
        lines.push("no run selected".to_string());
    }

    lines.push(String::new());
    lines.push("actions".to_string());
    lines.push("space mark, a clear marks, / search".to_string());
    lines.push("p or 1-4 presets, o sort, x/v/f filters".to_string());
    lines.push("h/r/n/R single control, s safe stop".to_string());
    lines.push("S/H/P/N/U bulk arm, Enter confirm, c cancel".to_string());
    lines.push("i copy id, y copy stop snippet".to_string());
    lines
}

pub(crate) fn fleet_header_line(
    visible_runs: usize,
    total_runs: usize,
    selected_index: usize,
    counts: (usize, usize, usize, usize),
    embedded: bool,
) -> String {
    format!(
        "loopmux v{} fleet manager | visible={}/{} selected={} active={} holding={} stale={} mismatch={} | q/esc {}",
        LOOPMUX_VERSION,
        visible_runs,
        total_runs,
        selected_index,
        counts.0,
        counts.1,
        counts.2,
        counts.3,
        if embedded {
            "return to run"
        } else {
            "quit manager"
        }
    )
}

pub(crate) fn fleet_status_line(
    view_preset: FleetViewPreset,
    sort_mode: FleetSortMode,
    state_filter: FleetStateFilter,
    show_stale: bool,
    mismatch_only: bool,
    search_query: &str,
    profile_filter: Option<&str>,
) -> String {
    format!(
        "view={} sort={} state={} stale={} mismatch={} search={} profile={}",
        view_preset.label(),
        sort_mode.label(),
        state_filter.label(),
        if show_stale { "on" } else { "off" },
        if mismatch_only { "on" } else { "off" },
        if search_query.trim().is_empty() {
            "<none>"
        } else {
            search_query.trim()
        },
        profile_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("<all>")
    )
}

pub(crate) fn fleet_run_list_line(
    run: &FleetListedRun,
    selected: bool,
    marked: bool,
    use_unicode: bool,
) -> String {
    let marker = if selected { ">" } else { " " };
    let selected_mark = if marked { "[x]" } else { "[ ]" };
    let version = if run.record.version.is_empty() {
        "unknown"
    } else {
        run.record.version.as_str()
    };
    let mut tags: Vec<&str> = Vec::new();
    tags.push(if run.stale { "stale" } else { "active" });
    if run.needs_attention {
        tags.push("attention");
    }
    if run.version_mismatch {
        tags.push("mismatch");
    }
    let mut line = format!(
        "{}{} {} [{}] sends={} health={}({}) target={}",
        marker,
        selected_mark,
        run.record.name,
        tags.join(","),
        run.record.sends,
        run.health_label,
        run.health_score,
        truncate_text(&run.record.target, 28, use_unicode)
    );
    if run.version_mismatch {
        line.push_str(&format!(" ver={version}"));
    }
    line.push_str(&format!(" state={}", run.record.state));
    line
}

pub(crate) fn fleet_run_list_lines(
    runs: &[FleetListedRun],
    content_rows: usize,
    selected: usize,
    selected_ids: &HashSet<String>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (idx, run) in runs.iter().take(content_rows).enumerate() {
        let line = fleet_run_list_line(
            run,
            idx == selected,
            selected_ids.contains(&run.record.id),
            true,
        );
        lines.push(line);
    }
    lines
}

pub(crate) fn fleet_bulk_confirmation(command: FleetControlCommand, run_count: usize) -> String {
    format!(
        "confirm bulk {} for {} run(s): press Enter, or c to cancel",
        fleet_command_label(command),
        run_count
    )
}
