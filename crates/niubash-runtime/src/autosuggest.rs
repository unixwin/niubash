//! Native Niubash history autosuggestions.

use nu_ansi_term::{Color, Style};
use reedline::{Hinter, History, SearchQuery};

use crate::config::AutosuggestConfig;

/// Reedline hinter that suggests the latest history entry matching the prefix.
pub struct HistoryAutosuggestHinter {
    style: Style,
    current_hint: String,
    buffer_max_size: Option<usize>,
}

impl HistoryAutosuggestHinter {
    pub fn new(config: &AutosuggestConfig) -> Self {
        Self {
            style: parse_highlight_style(&config.highlight_style),
            current_hint: String::new(),
            buffer_max_size: config.buffer_max_size,
        }
    }
}

impl Hinter for HistoryAutosuggestHinter {
    fn handle(
        &mut self,
        line: &str,
        _pos: usize,
        history: &dyn History,
        use_ansi_coloring: bool,
        _cwd: &str,
    ) -> String {
        self.current_hint.clear();

        if line.is_empty() || buffer_too_large(line, self.buffer_max_size) {
            return String::new();
        }

        if let Ok(entries) = history.search(SearchQuery::last_with_prefix(
            line.to_string(),
            history.session(),
        )) {
            if let Some(entry) = entries.first() {
                self.current_hint = entry
                    .command_line
                    .get(line.len()..)
                    .unwrap_or_default()
                    .to_string();
            }
        }

        if use_ansi_coloring && !self.current_hint.is_empty() {
            self.style.paint(&self.current_hint).to_string()
        } else {
            self.current_hint.clone()
        }
    }

    fn complete_hint(&self) -> String {
        self.current_hint.clone()
    }

    fn next_hint_token(&self) -> String {
        first_hint_token(&self.current_hint)
    }
}

fn buffer_too_large(line: &str, max_size: Option<usize>) -> bool {
    max_size
        .map(|max| line.chars().count() > max)
        .unwrap_or(false)
}

fn first_hint_token(hint: &str) -> String {
    let mut reached_content = false;
    let mut end = 0;

    for (idx, ch) in hint.char_indices() {
        if reached_content && ch.is_whitespace() {
            break;
        }
        if !ch.is_whitespace() {
            reached_content = true;
        }
        end = idx + ch.len_utf8();
    }

    hint.get(..end).unwrap_or_default().to_string()
}

pub fn parse_highlight_style(value: &str) -> Style {
    parse_style(value, Style::new().fg(Color::Fixed(8)))
}

pub fn parse_style(value: &str, default_style: Style) -> Style {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Style::new();
    }

    let mut style = default_style;

    for part in value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if let Some(color) = part.strip_prefix("fg=").and_then(parse_color) {
            style = style.fg(color);
        } else if let Some(color) = part.strip_prefix("bg=").and_then(parse_color) {
            style = style.on(color);
        } else {
            style = match part {
                "bold" => style.bold(),
                "underline" => style.underline(),
                "italic" => style.italic(),
                "standout" | "reverse" => style.reverse(),
                _ => style,
            };
        }
    }

    style
}

fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim().to_ascii_lowercase();

    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex_color(hex);
    }

    if let Ok(number) = value.parse::<u8>() {
        return Some(Color::Fixed(number));
    }

    match value.as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "purple" => Some(Color::Purple),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "grey" | "gray" | "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "lightgray" | "lightgrey" => Some(Color::LightGray),
        "default" => Some(Color::Default),
        _ => None,
    }
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::LiveFileBackedHistory;
    use reedline::{FileBackedHistory, HistoryItem};

    #[test]
    fn returns_history_suffix_for_prefix() {
        let mut history = FileBackedHistory::default();
        history
            .save(HistoryItem::from_command_line("git status --short"))
            .unwrap();

        let config = AutosuggestConfig::default();
        let mut hinter = HistoryAutosuggestHinter::new(&config);

        assert_eq!(
            hinter.handle("git", 3, &history, false, ""),
            " status --short"
        );
        assert_eq!(hinter.complete_hint(), " status --short");
        assert_eq!(hinter.next_hint_token(), " status");
    }

    #[test]
    fn suppresses_large_buffers() {
        let mut history = FileBackedHistory::default();
        history
            .save(HistoryItem::from_command_line("git status --short"))
            .unwrap();

        let config = AutosuggestConfig {
            buffer_max_size: Some(2),
            ..Default::default()
        };
        let mut hinter = HistoryAutosuggestHinter::new(&config);

        assert_eq!(hinter.handle("git", 3, &history, false, ""), "");
        assert_eq!(hinter.complete_hint(), "");
    }

    #[test]
    fn parses_highlight_style_subset() {
        let style = parse_highlight_style("fg=#ff00ff,bg=cyan,bold,underline");

        assert_eq!(style.foreground, Some(Color::Rgb(255, 0, 255)));
        assert_eq!(style.background, Some(Color::Cyan));
        assert!(style.is_bold);
        assert!(style.is_underline);
    }

    #[test]
    fn none_style_returns_plain_style() {
        assert!(parse_style("none", Style::new().fg(Color::Red)).is_plain());
    }

    #[test]
    fn autosuggest_sees_history_saved_by_another_session() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history");
        let mut writer = LiveFileBackedHistory::with_file(100, path.clone()).unwrap();
        let reader = LiveFileBackedHistory::with_file(100, path).unwrap();

        writer
            .save(HistoryItem::from_command_line("cd D:/projects"))
            .unwrap();

        let mut hinter = HistoryAutosuggestHinter::new(&AutosuggestConfig::default());
        assert_eq!(hinter.handle("cd", 2, &reader, false, ""), " D:/projects");
    }
}
