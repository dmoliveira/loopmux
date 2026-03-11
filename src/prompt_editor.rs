use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::run_loop::load_run_history_from_path;
use crate::{
    DEFAULT_HISTORY_LIMIT, history_path, prompt_editor_history_path, timestamp_now, truncate_text,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PromptHistoryItem {
    pub(crate) created_at: String,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptEditorConfirm {
    DeleteSelected,
    ClearAll,
}

#[derive(Debug, Clone)]
pub(crate) struct PromptEditorState {
    pub(crate) open: bool,
    pub(crate) max_chars: usize,
    pub(crate) original: String,
    pub(crate) current: String,
    pub(crate) history: Vec<PromptHistoryItem>,
    pub(crate) selected_idx: usize,
    pub(crate) confirm: Option<PromptEditorConfirm>,
    pub(crate) undo_stack: Vec<Vec<PromptHistoryItem>>,
}

impl PromptEditorState {
    pub(crate) fn new(original: String, max_chars: usize) -> Self {
        let mut history = load_prompt_history_items(DEFAULT_HISTORY_LIMIT).unwrap_or_default();
        history.retain(|item| item.text != original);
        Self {
            open: false,
            max_chars: max_chars.max(1),
            original: original.clone(),
            current: truncate_text(&original, max_chars.max(1), true),
            history,
            selected_idx: 0,
            confirm: None,
            undo_stack: Vec::new(),
        }
    }

    pub(crate) fn selected_prompt(&self) -> String {
        if self.selected_idx == 0 {
            self.original.clone()
        } else {
            self.history
                .get(self.selected_idx.saturating_sub(1))
                .map(|item| item.text.clone())
                .unwrap_or_else(|| self.current.clone())
        }
    }

    pub(crate) fn toggle_open(&mut self) {
        self.open = !self.open;
        self.confirm = None;
        if !self.open {
            self.persist_current_to_history();
        }
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.confirm = None;
        self.persist_current_to_history();
    }

    pub(crate) fn normalize_selection(&mut self) {
        let max = self.history.len();
        self.selected_idx = self.selected_idx.min(max);
    }

    pub(crate) fn select_up(&mut self) {
        self.selected_idx = self.selected_idx.saturating_sub(1);
    }

    pub(crate) fn select_down(&mut self) {
        self.selected_idx = self.selected_idx.saturating_add(1);
        self.normalize_selection();
    }

    pub(crate) fn use_selection(&mut self) {
        self.current = self.selected_prompt();
    }

    pub(crate) fn input_char(&mut self, ch: char) {
        if self.current.chars().count() >= self.max_chars {
            return;
        }
        self.current.push(ch);
        self.selected_idx = 0;
    }

    pub(crate) fn backspace(&mut self) {
        let _ = self.current.pop();
        self.selected_idx = 0;
    }

    pub(crate) fn request_delete_selected(&mut self) {
        if self.selected_idx == 0 {
            return;
        }
        self.confirm = Some(PromptEditorConfirm::DeleteSelected);
    }

    pub(crate) fn request_clear_history(&mut self) {
        if self.history.is_empty() {
            return;
        }
        self.confirm = Some(PromptEditorConfirm::ClearAll);
    }

    pub(crate) fn confirm_yes(&mut self) {
        match self.confirm.take() {
            Some(PromptEditorConfirm::DeleteSelected) => {
                if self.selected_idx > 0 {
                    self.undo_stack.push(self.history.clone());
                    let idx = self.selected_idx - 1;
                    if idx < self.history.len() {
                        self.history.remove(idx);
                    }
                    self.normalize_selection();
                }
                self.save_history();
            }
            Some(PromptEditorConfirm::ClearAll) => {
                self.undo_stack.push(self.history.clone());
                self.history.clear();
                self.selected_idx = 0;
                self.save_history();
            }
            None => {}
        }
    }

    pub(crate) fn confirm_no(&mut self) {
        self.confirm = None;
    }

    pub(crate) fn undo(&mut self) {
        if let Some(previous) = self.undo_stack.pop() {
            self.history = previous;
            self.normalize_selection();
            self.save_history();
        }
    }

    pub(crate) fn effective_prompt(&self, fallback: &str) -> String {
        if self.current.trim().is_empty() {
            fallback.to_string()
        } else {
            self.current.clone()
        }
    }

    fn persist_current_to_history(&mut self) {
        let text = self.current.trim().to_string();
        if text.is_empty() || text == self.original {
            return;
        }
        self.history.retain(|entry| entry.text != text);
        self.history.insert(
            0,
            PromptHistoryItem {
                created_at: timestamp_now(),
                text,
            },
        );
        if self.history.len() > DEFAULT_HISTORY_LIMIT {
            self.history.truncate(DEFAULT_HISTORY_LIMIT);
        }
        self.save_history();
    }

    fn save_history(&self) {
        let _ = save_prompt_history_items(&self.history, DEFAULT_HISTORY_LIMIT);
    }
}

pub(crate) fn load_prompt_history_items(limit: usize) -> Result<Vec<PromptHistoryItem>> {
    let persisted_path = prompt_editor_history_path()?;
    let run_history_path = history_path()?;
    load_prompt_history_items_from_paths(limit, &persisted_path, &run_history_path)
}

pub(crate) fn load_prompt_history_items_from_paths(
    limit: usize,
    persisted_path: &Path,
    run_history_path: &Path,
) -> Result<Vec<PromptHistoryItem>> {
    let limit = limit.max(1);
    let mut seen = HashSet::new();
    let mut items = Vec::new();

    if persisted_path.exists()
        && let Ok(content) = std::fs::read_to_string(persisted_path)
        && let Ok(persisted) = serde_json::from_str::<Vec<PromptHistoryItem>>(&content)
    {
        for item in persisted {
            let text = item.text.trim().to_string();
            if text.is_empty() || !seen.insert(text.clone()) {
                continue;
            }
            items.push(PromptHistoryItem {
                created_at: item.created_at,
                text,
            });
            if items.len() >= limit {
                return Ok(items);
            }
        }
    }

    let history = load_run_history_from_path(run_history_path)?;
    for entry in history.entries {
        let text = entry.prompt.trim().to_string();
        if text.is_empty() || !seen.insert(text.clone()) {
            continue;
        }
        items.push(PromptHistoryItem {
            created_at: entry.last_run,
            text,
        });
        if items.len() >= limit {
            break;
        }
    }
    Ok(items)
}

pub(crate) fn save_prompt_history_items(items: &[PromptHistoryItem], limit: usize) -> Result<()> {
    let path = prompt_editor_history_path()?;
    save_prompt_history_items_to_path(items, limit, &path)
}

pub(crate) fn save_prompt_history_items_to_path(
    items: &[PromptHistoryItem],
    limit: usize,
    path: &Path,
) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create history dir: {}", dir.display()))?;
    }
    let content =
        serde_json::to_string_pretty(&items.iter().take(limit.max(1)).cloned().collect::<Vec<_>>())
            .context("failed to serialize prompt editor history")?;
    std::fs::write(path, content).with_context(|| {
        format!(
            "failed to write prompt editor history file: {}",
            path.display()
        )
    })?;
    Ok(())
}
