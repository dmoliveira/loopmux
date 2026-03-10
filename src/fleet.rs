use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    FleetSortMode, FleetStateFilter, FleetViewPreset, LOOPMUX_VERSION, fleet_control_path,
    fleet_state_dir, resolve_fleet_target, timestamp_now,
};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct FleetRunRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) profile_id: String,
    pub(crate) pid: u32,
    pub(crate) host: String,
    pub(crate) target: String,
    pub(crate) state: String,
    pub(crate) sends: u32,
    pub(crate) poll_seconds: u64,
    pub(crate) started_at: String,
    pub(crate) last_seen: String,
    #[serde(default)]
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) events: Vec<FleetRunEvent>,
    #[serde(default)]
    pub(crate) heartbeat_sends_reported: u32,
    #[serde(default)]
    pub(crate) heartbeat_reported_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct FleetRunEvent {
    pub(crate) timestamp: String,
    pub(crate) kind: String,
    pub(crate) detail: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FleetControlEnvelope {
    pub(crate) token: String,
    pub(crate) command: FleetControlCommand,
    pub(crate) issued_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FleetControlCommand {
    Stop,
    Hold,
    Resume,
    Next,
    Renew,
}

#[derive(Debug, Clone)]
pub(crate) struct FleetListedRun {
    pub(crate) record: FleetRunRecord,
    pub(crate) stale: bool,
    pub(crate) version_mismatch: bool,
    pub(crate) health_score: u8,
    pub(crate) health_label: &'static str,
    pub(crate) needs_attention: bool,
}

pub(crate) struct FleetVisibleArgs<'a> {
    pub(crate) runs: &'a [FleetListedRun],
    pub(crate) profile_filter: Option<&'a str>,
    pub(crate) show_stale: bool,
    pub(crate) mismatch_only: bool,
    pub(crate) state_filter: FleetStateFilter,
    pub(crate) search_query: &'a str,
    pub(crate) sort_mode: FleetSortMode,
    pub(crate) view_preset: FleetViewPreset,
}

pub(crate) fn load_fleet_runs() -> Result<Vec<FleetListedRun>> {
    let dir = fleet_state_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)?;
        let Ok(record) = serde_json::from_str::<FleetRunRecord>(&raw) else {
            continue;
        };
        let stale = is_fleet_record_stale(&record);
        let version_mismatch = is_version_mismatch(&record.version);
        let (health_score, health_label) = fleet_health(&record, stale, version_mismatch);
        let needs_attention = stale
            || version_mismatch
            || health_score < 70
            || record.state == "error"
            || record.state == "stopped";
        runs.push(FleetListedRun {
            stale,
            version_mismatch,
            health_score,
            health_label,
            needs_attention,
            record,
        });
    }
    Ok(runs)
}

pub(crate) fn is_version_mismatch(run_version: &str) -> bool {
    run_version.trim().is_empty() || run_version.trim() != LOOPMUX_VERSION
}

pub(crate) fn fleet_health(
    record: &FleetRunRecord,
    stale: bool,
    version_mismatch: bool,
) -> (u8, &'static str) {
    if stale {
        return (20, "critical");
    }

    let mut score: i32 = 100;
    if version_mismatch {
        score -= 25;
    }
    if record.state == "holding" {
        score -= 8;
    }
    if record.state == "error" {
        score -= 35;
    }

    if let Some(age_seconds) = fleet_last_seen_age_seconds(record) {
        let budget = (record.poll_seconds.max(1) * 3 + 5) as i64;
        if age_seconds > budget {
            score -= 25;
        } else if age_seconds > budget / 2 {
            score -= 10;
        }
    } else {
        score -= 20;
    }

    let score = score.clamp(0, 100) as u8;
    let label = if score >= 85 {
        "healthy"
    } else if score >= 65 {
        "watch"
    } else {
        "critical"
    };
    (score, label)
}

pub(crate) fn fleet_last_seen_age_seconds(record: &FleetRunRecord) -> Option<i64> {
    let last_seen = OffsetDateTime::parse(
        &record.last_seen,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()?;
    Some((OffsetDateTime::now_utc() - last_seen).whole_seconds())
}

pub(crate) fn is_fleet_record_stale(record: &FleetRunRecord) -> bool {
    if !pid_alive(record.pid) {
        return true;
    }
    let Ok(last_seen) = OffsetDateTime::parse(
        &record.last_seen,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return true;
    };
    let now = OffsetDateTime::now_utc();
    let age = now - last_seen;
    let max_age = (record.poll_seconds.max(1) * 3 + 5) as i64;
    age.whole_seconds() > max_age
}

pub(crate) fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn fleet_manager_visible_runs(args: FleetVisibleArgs<'_>) -> Vec<FleetListedRun> {
    let FleetVisibleArgs {
        runs,
        profile_filter,
        show_stale,
        mismatch_only,
        state_filter,
        search_query,
        sort_mode,
        view_preset,
    } = args;
    let search = search_query.trim().to_ascii_lowercase();
    let mut visible: Vec<FleetListedRun> = runs
        .iter()
        .filter(|run| {
            if let Some(profile_filter) = profile_filter {
                run_matches_profile_filter(run, profile_filter)
            } else {
                true
            }
        })
        .filter(|run| show_stale || !run.stale)
        .filter(|run| !mismatch_only || run.version_mismatch)
        .filter(|run| state_filter.allows(run))
        .filter(|run| {
            if view_preset == FleetViewPreset::NeedsAttention {
                run.needs_attention
            } else {
                true
            }
        })
        .filter(|run| search.is_empty() || run_matches_query(run, &search))
        .cloned()
        .collect();

    visible.sort_by(|a, b| match sort_mode {
        FleetSortMode::LastSeen => b.record.last_seen.cmp(&a.record.last_seen),
        FleetSortMode::Sends => b.record.sends.cmp(&a.record.sends),
        FleetSortMode::Health => a.health_score.cmp(&b.health_score),
        FleetSortMode::Name => a.record.name.cmp(&b.record.name),
        FleetSortMode::State => a.record.state.cmp(&b.record.state),
    });
    visible
}

pub(crate) fn run_matches_query(run: &FleetListedRun, query: &str) -> bool {
    let version = if run.record.version.is_empty() {
        "unknown"
    } else {
        run.record.version.as_str()
    };
    [
        run.record.name.as_str(),
        run.record.id.as_str(),
        run.record.profile_id.as_str(),
        run.record.target.as_str(),
        run.record.state.as_str(),
        version,
    ]
    .iter()
    .any(|value| value.to_ascii_lowercase().contains(query))
}

pub(crate) fn run_matches_profile_filter(run: &FleetListedRun, profile_filter: &str) -> bool {
    let needle = profile_filter.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return true;
    }
    run.record.profile_id.to_ascii_lowercase() == needle
        || run.record.name.to_ascii_lowercase() == needle
}

pub(crate) fn fleet_manager_counts(runs: &[FleetListedRun]) -> (usize, usize, usize, usize) {
    let mut active = 0;
    let mut holding = 0;
    let mut stale = 0;
    let mut mismatch = 0;
    for run in runs {
        if run.stale {
            stale += 1;
        } else {
            active += 1;
        }
        if run.record.state == "holding" {
            holding += 1;
        }
        if run.version_mismatch {
            mismatch += 1;
        }
    }
    (active, holding, stale, mismatch)
}

pub(crate) fn send_fleet_command(target: &str, command: FleetControlCommand) -> Result<()> {
    let run = dispatch_fleet_command(target, command)?;
    println!(
        "Sent {} to {} ({})",
        fleet_command_label(command),
        run.record.name,
        run.record.id
    );
    Ok(())
}

pub(crate) fn dispatch_fleet_command(
    target: &str,
    command: FleetControlCommand,
) -> Result<FleetListedRun> {
    let runs = load_fleet_runs()?;
    if runs.is_empty() {
        bail!("no active local loopmux runs found");
    }
    let run = resolve_fleet_target(target, &runs)?;
    let path = fleet_control_path(&run.record.id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let token = format!(
        "{}-{}",
        OffsetDateTime::now_utc().unix_timestamp_nanos(),
        std::process::id()
    );
    let envelope = FleetControlEnvelope {
        token,
        command,
        issued_at: timestamp_now(),
    };
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&envelope)?)?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(run)
}

pub(crate) fn fleet_command_label(command: FleetControlCommand) -> &'static str {
    match command {
        FleetControlCommand::Stop => "stop",
        FleetControlCommand::Hold => "hold",
        FleetControlCommand::Resume => "resume",
        FleetControlCommand::Next => "next",
        FleetControlCommand::Renew => "renew",
    }
}
