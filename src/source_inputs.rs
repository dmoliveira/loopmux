use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::SourceInputs;

pub(crate) fn collect_source_inputs(
    targets: &[String],
    targets_file: &[PathBuf],
    files: &[PathBuf],
    files_file: &[PathBuf],
) -> Result<SourceInputs> {
    let mut merged_targets = targets.to_vec();
    for path in targets_file {
        merged_targets.extend(read_list_file_entries(path)?);
    }

    let mut merged_files = files
        .iter()
        .map(|value| value.display().to_string())
        .collect::<Vec<_>>();
    for path in files_file {
        merged_files.extend(read_list_file_entries(path)?);
    }

    Ok(SourceInputs {
        tmux_targets: dedupe_preserve_order(merged_targets),
        file_paths: dedupe_preserve_order(merged_files),
    })
}

pub(crate) fn dedupe_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value);
        }
    }
    deduped
}

fn read_list_file_entries(path: &PathBuf) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read list file: {}", path.display()))?;
    let mut values = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        values.push(trimmed.to_string());
    }
    Ok(values)
}
