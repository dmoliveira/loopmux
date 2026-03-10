use std::collections::HashSet;

use crate::{Action, PromptBlock, Rule, TemplateVars};

pub(crate) fn collect_template_placeholders(
    default_action: &Action,
    rules: &Option<Vec<Rule>>,
) -> Vec<String> {
    let mut vars = HashSet::new();
    collect_action_placeholders(default_action, &mut vars);
    if let Some(rules) = rules {
        for rule in rules {
            if let Some(action) = &rule.action {
                collect_action_placeholders(action, &mut vars);
            }
        }
    }
    let mut values: Vec<String> = vars.into_iter().collect();
    values.sort();
    values
}

pub(crate) fn find_missing_vars(required: &[String], available: &TemplateVars) -> Vec<String> {
    let mut missing = Vec::new();
    for key in required {
        if !available.contains_key(key) {
            missing.push(key.clone());
        }
    }
    missing
}

pub(crate) fn default_template() -> String {
    let template = r#"target: "ai:5.0"
iterations: 10
poll: 5
initial_poll: 5
trigger_confirm_seconds: 5
log_preview_lines: 3
trigger_edge: true
recheck_before_send: true
duration: 2h

template_vars:
  project: loopmux

rule_eval: first_match

default_action:
  pre: "Keep context on UX simplification."
  prompt: "Do the next iteration."
  post: "Run lint/tests; fix failures."

delay:
  mode: range
  min: 5
  max: 120

rules:
  - id: continue-loop
    match:
      exact_line: "<CONTINUE-LOOP>"
    action:
      prompt: "Continue iteration in the wt flow e2e, remember to commit small advances."
    next: success-path

  - id: success-path
    match:
      regex: "(All tests passed|LGTM)"
    action:
      prompt: "Continue with next iteration."
    next: review-path

  - id: review-path
    match:
      regex: "(Ready for review|PR created)"
    confirm_seconds: 3
    delay:
      mode: fixed
      value: 300
    action:
      prompt: "Audit UX for simplification."
    next: success-path

  - id: failure-path
    match:
      regex: "(FAIL|Error|Exception)"
    action:
      pre: "Fix the errors before proceeding."
      prompt: "Repair and re-run tests."
      post: "Summarize fixes."
    next: success-path

logging:
  path: "loopmux.log"
  format: "jsonl"
"#;
    template.to_string()
}

fn collect_action_placeholders(action: &Action, vars: &mut HashSet<String>) {
    collect_prompt_block_placeholders(action.pre.as_ref(), vars);
    collect_prompt_block_placeholders(action.prompt.as_ref(), vars);
    collect_prompt_block_placeholders(action.post.as_ref(), vars);
}

fn collect_prompt_block_placeholders(block: Option<&PromptBlock>, vars: &mut HashSet<String>) {
    let Some(block) = block else {
        return;
    };
    match block {
        PromptBlock::Single(text) => extract_placeholders(text, vars),
        PromptBlock::Multi(items) => {
            for item in items {
                extract_placeholders(item, vars);
            }
        }
    }
}

fn extract_placeholders(text: &str, vars: &mut HashSet<String>) {
    let mut remaining = text;
    while let Some(start) = remaining.find("{{") {
        if let Some(end) = remaining[start + 2..].find("}}") {
            let raw = &remaining[start + 2..start + 2 + end];
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                vars.insert(trimmed.to_string());
            }
            remaining = &remaining[start + 2 + end + 2..];
        } else {
            break;
        }
    }
}
