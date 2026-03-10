use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use crate::{IconMode, LayoutMode, ResolvedConfig, StyleConfig, UiMode};

pub(crate) fn resolve_ui_mode(
    tui_requested: bool,
    single_line_requested: bool,
    stdout_is_terminal: bool,
) -> UiMode {
    if tui_requested && stdout_is_terminal {
        UiMode::Tui
    } else if single_line_requested {
        UiMode::SingleLine
    } else {
        UiMode::Plain
    }
}

pub(crate) fn layout_mode(width: u16) -> LayoutMode {
    if width <= 80 {
        LayoutMode::Compact
    } else if width <= 120 {
        LayoutMode::Standard
    } else {
        LayoutMode::Wide
    }
}

pub(crate) fn detect_icon_mode() -> IconMode {
    if std::env::var("LOOPMUX_NO_NERD_FONT").is_ok() {
        return IconMode::Ascii;
    }
    IconMode::Nerd
}

pub(crate) fn detect_style() -> StyleConfig {
    let no_color = std::env::var("NO_COLOR").is_ok();
    let term = std::env::var("TERM").unwrap_or_default();
    let color_term = std::env::var("COLORTERM").unwrap_or_default();
    let use_color = !no_color && term != "dumb";
    let use_bg = use_color && (term.contains("256color") || !color_term.is_empty());
    let use_unicode_ellipsis = supports_unicode();
    let dim_logs = std::env::var("LOOPMUX_TUI_BRIGHT_LOGS").is_err();
    StyleConfig {
        use_color,
        use_bg,
        use_unicode_ellipsis,
        dim_logs,
    }
}

pub(crate) fn supports_unicode() -> bool {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_CTYPE"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let locale = locale.to_lowercase();
    locale.contains("utf-8") || locale.contains("utf8")
}

pub(crate) fn tui_frame_signature(
    width: u16,
    height: u16,
    bar: &str,
    display_lines: &[String],
    footer: &str,
    overlay_open: bool,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    overlay_open.hash(&mut hasher);
    bar.hash(&mut hasher);
    display_lines.hash(&mut hasher);
    footer.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn render_footer(
    style: StyleConfig,
    width: u16,
    summary: Option<&str>,
    note: Option<&str>,
    overlay_help: Option<&str>,
) -> String {
    let sep_text = if style.use_unicode_ellipsis {
        " . ".replace('.', "·")
    } else {
        " . ".to_string()
    };
    let text = if let Some(summary) = summary {
        format!("stopped{sep_text}{summary}{sep_text}q quit")
    } else if let Some(help) = overlay_help {
        help.to_string()
    } else {
        format!(
            "h hold/resume (p/r){sep_text}l active-list{sep_text}g log-view{sep_text}f fleet{sep_text}R renew{sep_text}n next{sep_text}s/^C stop{sep_text}q quit"
        )
    };
    let text = if let Some(note) = note {
        format!("{text}{sep_text}{note}")
    } else {
        text
    };
    let line = pad_to_width(&text, width as usize);
    if style.use_color {
        let prefix = style_prefix(Some(240), style.use_bg.then_some(235), false);
        format!("{prefix}{line}\x1B[0m")
    } else {
        line
    }
}

pub(crate) fn render_footer_summary(
    config: &ResolvedConfig,
    current: u32,
    total: u32,
    elapsed: &str,
) -> String {
    if config.infinite || total == 0 || total == u32::MAX {
        format!("sends {current} elapsed {elapsed}")
    } else {
        format!("iter {current}/{total} elapsed {elapsed}")
    }
}

pub(crate) fn sanitize_tui_log_line(line: &str) -> String {
    let mut cleaned = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                let _ = chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '\u{fffd}' {
            continue;
        }
        if ch.is_control() {
            cleaned.push(' ');
            continue;
        }
        cleaned.push(ch);
    }

    let mut collapsed = String::new();
    let mut space_run = 0usize;
    for ch in cleaned.chars() {
        if ch.is_whitespace() {
            space_run += 1;
            if space_run <= 2 {
                collapsed.push(' ');
            }
        } else {
            space_run = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

pub(crate) fn build_grouped_log_lines(
    logs: &[String],
    max_lines: usize,
    use_unicode: bool,
) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }

    let mut groups: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    let mut misc: Option<(usize, usize, String)> = None;

    for (idx, line) in logs.iter().enumerate() {
        let key = extract_log_target(line);
        if let Some(target) = key {
            let entry = groups
                .entry(target)
                .or_insert_with(|| (0usize, idx, String::new()));
            entry.0 = entry.0.saturating_add(1);
            entry.1 = idx;
            entry.2 = line.clone();
        } else {
            match misc.as_mut() {
                Some(entry) => {
                    entry.0 = entry.0.saturating_add(1);
                    entry.1 = idx;
                    entry.2 = line.clone();
                }
                None => misc = Some((1usize, idx, line.clone())),
            }
        }
    }

    let mut items = groups
        .into_iter()
        .map(|(target, (count, last_idx, last_line))| (target, count, last_idx, last_line))
        .collect::<Vec<_>>();
    if let Some((count, last_idx, last_line)) = misc {
        items.push(("misc".to_string(), count, last_idx, last_line));
    }

    items.sort_by(|a, b| b.2.cmp(&a.2));
    if items.is_empty() {
        return vec!["grouped view: no logs yet".to_string()];
    }

    items
        .into_iter()
        .take(max_lines)
        .map(|(target, count, _idx, last_line)| {
            let preview = truncate_text(&last_line, 74, use_unicode);
            format!("{target} x{count} {preview}")
        })
        .collect()
}

pub(crate) fn extract_log_target(line: &str) -> Option<String> {
    if let Some(pos) = line.find("target=") {
        let value = &line[pos + 7..];
        let token = value
            .chars()
            .take_while(|ch| !ch.is_whitespace() && *ch != '"' && *ch != ',' && *ch != ';')
            .collect::<String>();
        if looks_like_pane_target(&token) {
            return Some(token);
        }
    }

    for token in line.split_whitespace() {
        let cleaned = token
            .trim_matches(|ch: char| {
                ch == '"' || ch == '\'' || ch == '[' || ch == ']' || ch == ',' || ch == ';'
            })
            .to_string();
        if looks_like_pane_target(&cleaned) {
            return Some(cleaned);
        }
    }
    None
}

fn looks_like_pane_target(token: &str) -> bool {
    let Some(colon) = token.find(':') else {
        return false;
    };
    let Some(dot_rel) = token[colon + 1..].find('.') else {
        return false;
    };
    let dot = colon + 1 + dot_rel;
    let session = &token[..colon];
    let window = &token[colon + 1..dot];
    let pane = &token[dot + 1..];
    !session.is_empty() && !window.is_empty() && !pane.is_empty()
}

fn pad_to_width(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.chars().take(width).collect();
    }
    let padding = width - len;
    format!("{text}{}", " ".repeat(padding))
}

fn truncate_text(text: &str, max: usize, use_unicode: bool) -> String {
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

fn style_prefix(fg: Option<u8>, bg: Option<u8>, bold: bool) -> String {
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
