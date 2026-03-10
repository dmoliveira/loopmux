use std::io::Write;

use anyhow::{Context, Result};
use serde_json::json;
use time::OffsetDateTime;

use crate::{LogEvent, LogFormatResolved, LoggingConfigResolved};

pub(crate) struct Logger {
    config: LoggingConfigResolved,
    file: Option<std::fs::File>,
}

impl Logger {
    pub(crate) fn new(config: LoggingConfigResolved) -> Result<Self> {
        let file = if let Some(path) = &config.path {
            Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("failed to open log file {}", path.display()))?,
            )
        } else {
            None
        };
        Ok(Self { config, file })
    }

    pub(crate) fn log(&mut self, mut event: LogEvent) -> Result<()> {
        let timestamp = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into());
        if event.timestamp.is_empty() {
            event.timestamp = timestamp;
        }
        match self.config.format {
            LogFormatResolved::Text => self.log_text(&event),
            LogFormatResolved::Jsonl => self.log_json(&event),
        }
    }

    fn log_text(&mut self, event: &LogEvent) -> Result<()> {
        let mut line = format!(
            "[{}] {} target={}",
            event.timestamp, event.event, event.target
        );
        if let Some(rule_id) = event.rule_id.as_ref() {
            line.push_str(&format!(" rule={rule_id}"));
        }
        if let Some(detail) = event.detail.as_ref() {
            line.push_str(&format!(" detail=\"{}\"", sanitize_detail(detail)));
        }
        if let Some(sends) = event.sends {
            line.push_str(&format!(" sends={sends}"));
        }
        line.push('\n');
        self.write_line(&line)
    }

    fn log_json(&mut self, event: &LogEvent) -> Result<()> {
        let value = json!({
            "event": event.event,
            "timestamp": event.timestamp,
            "target": event.target,
            "rule_id": event.rule_id,
            "detail": event.detail.as_ref().map(|value| sanitize_detail(value)),
            "sends": event.sends,
        });
        let mut line = serde_json::to_string(&value).context("failed to serialize log JSON")?;
        line.push('\n');
        self.write_line(&line)
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        if let Some(file) = &mut self.file {
            file.write_all(line.as_bytes())?;
        } else {
            print!("{line}");
        }
        Ok(())
    }
}

pub(crate) fn redacted_sent_detail(target: &str, prompt: &str) -> String {
    let char_count = prompt.chars().count();
    format!("target={target} prompt_redacted chars={char_count}")
}

fn sanitize_detail(detail: &str) -> String {
    detail
        .replace('"', "'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
