use crate::{IconMode, LayoutMode, StyleConfig, UiMode};

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
