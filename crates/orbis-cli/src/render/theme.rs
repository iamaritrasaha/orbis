use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Token {
    Primary,
    Foreground,
    Muted,
    Positive,
    Caution,
    Destructive,
    Unavailable,
    Selected,
    Surface,
    Divider,
    Section,
    Provider,
}

/// Visual state of a stage in the progress indicator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StageState {
    /// The stage has completed.
    Done,
    /// The stage is currently active.
    Active,
    /// The stage has not started yet.
    Pending,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Theme {
    pub(crate) color: bool,
    pub(crate) unicode: bool,
    pub(crate) width: usize,
    mode: ColorMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ColorMode {
    None,
    Basic,
    Ansi256,
    TrueColor,
}

impl Theme {
    #[cfg(test)]
    pub(crate) fn test(width: usize) -> Self {
        Self { color: false, unicode: false, width, mode: ColorMode::None }
    }

    pub(crate) fn detect(color: bool) -> Self {
        let unicode = std::env::var("TERM").map(|term| term != "dumb").unwrap_or(true);
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(88)
            .clamp(40, 160);
        let mode = if !color {
            ColorMode::None
        } else if std::env::var("COLORTERM").is_ok_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) || std::env::var("TERM").is_ok_and(|value| value.contains("direct"))
        {
            ColorMode::TrueColor
        } else if std::env::var("TERM").is_ok_and(|value| value.contains("256color")) {
            ColorMode::Ansi256
        } else {
            ColorMode::Basic
        };
        Self { color, unicode, width, mode }
    }

    pub(crate) fn paint(self, value: &str, token: Token) -> String {
        if !self.color {
            return value.into();
        }
        let code = match self.mode {
            ColorMode::Basic => format!("\x1b[{}m", basic_code(token)),
            ColorMode::Ansi256 => format!("\x1b[38;5;{}m", ansi256_code(token)),
            ColorMode::TrueColor => {
                let (red, green, blue) = rgb(token);
                format!("\x1b[38;2;{red};{green};{blue}m")
            }
            ColorMode::None => String::new(),
        };
        format!("{code}{value}\x1b[0m")
    }

    pub(crate) fn style(self, token: Token) -> Style {
        if !self.color {
            return Style::default();
        }
        let color = match self.mode {
            ColorMode::Basic => basic_color(token),
            ColorMode::Ansi256 => Color::Indexed(ansi256_code(token)),
            ColorMode::TrueColor => {
                let (red, green, blue) = rgb(token);
                Color::Rgb(red, green, blue)
            }
            ColorMode::None => Color::Reset,
        };
        let mut style = Style::default().fg(color);
        if matches!(token, Token::Primary | Token::Section) {
            style = style.add_modifier(Modifier::BOLD);
        }
        style
    }

    pub(crate) fn mark(self, token: Token) -> &'static str {
        if self.unicode {
            match token {
                Token::Primary => "◈",
                Token::Positive => "●",
                Token::Unavailable => "○",
                Token::Caution => "◐",
                _ => "·",
            }
        } else {
            match token {
                Token::Primary => "@",
                Token::Positive => "*",
                Token::Unavailable => "o",
                Token::Caution => "!",
                _ => "-",
            }
        }
    }

    /// Returns the multi-line Orbis wordmark for dashboard and home screens.
    pub(crate) fn brand_full(self) -> &'static [&'static str] {
        if self.unicode {
            &[
                " ███  ████  ████  █████  ████ ",
                "█   █ █   █ █   █    █   █    ",
                "█   █ ████  ████     █    ███ ",
                "█   █ █ █   █   █     █      █",
                " ███  █  ██ ████   █████ ████ ",
            ]
        } else {
            &[" @  ORBIS"]
        }
    }

    /// Returns a single-line compact header mark.
    pub(crate) fn brand_compact(self) -> &'static str {
        if self.unicode { "◈ ORBIS" } else { "@ ORBIS" }
    }

    /// Returns the Orbis tagline.
    pub(crate) fn brand_tagline() -> &'static str {
        "Your Linux software, in one place."
    }

    /// Stage progress markers for operation views.
    pub(crate) fn stage_mark(self, state: StageState) -> &'static str {
        if self.unicode {
            match state {
                StageState::Done => "●",
                StageState::Active => "◐",
                StageState::Pending => "○",
            }
        } else {
            match state {
                StageState::Done => "*",
                StageState::Active => ">",
                StageState::Pending => ".",
            }
        }
    }
}

fn basic_code(token: Token) -> u16 {
    match token {
        Token::Primary | Token::Section => 36,
        Token::Foreground | Token::Selected | Token::Surface => 37,
        Token::Caution => 33,
        Token::Provider => 37,
        Token::Muted | Token::Divider | Token::Unavailable => 90,
        Token::Positive => 32,
        Token::Destructive => 31,
    }
}

fn basic_color(token: Token) -> Color {
    match token {
        Token::Primary | Token::Section => Color::Cyan,
        Token::Foreground | Token::Selected | Token::Surface => Color::White,
        Token::Caution => Color::Yellow,
        Token::Provider => Color::White,
        Token::Muted | Token::Divider | Token::Unavailable => Color::DarkGray,
        Token::Positive => Color::Green,
        Token::Destructive => Color::Red,
    }
}

fn ansi256_code(token: Token) -> u8 {
    match token {
        Token::Primary | Token::Section => 81,
        Token::Foreground | Token::Selected | Token::Surface => 255,
        Token::Caution => 221,
        Token::Provider => 255,
        Token::Muted | Token::Divider | Token::Unavailable => 245,
        Token::Positive => 78,
        Token::Destructive => 203,
    }
}

fn rgb(token: Token) -> (u8, u8, u8) {
    match token {
        Token::Primary | Token::Section => (94, 201, 213),
        Token::Foreground | Token::Selected | Token::Surface => (232, 238, 244),
        Token::Muted | Token::Divider | Token::Unavailable => (122, 136, 151),
        Token::Positive => (109, 201, 151),
        Token::Caution => (236, 196, 118),
        Token::Provider => (232, 238, 244),
        Token::Destructive => (235, 117, 117),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_theme_provides_the_intended_legible_orbis_wordmark() {
        let theme = Theme { color: true, unicode: true, width: 80, mode: ColorMode::TrueColor };
        let brand = theme.brand_full();
        assert_eq!(
            brand,
            &[
                " ███  ████  ████  █████  ████ ",
                "█   █ █   █ █   █    █   █    ",
                "█   █ ████  ████     █    ███ ",
                "█   █ █ █   █   █     █      █",
                " ███  █  ██ ████   █████ ████ ",
            ]
        );
        assert_eq!(brand.len(), 5);
        assert!(brand.iter().all(|line| line.chars().count() == 30));
        assert_eq!(theme.brand_compact(), "◈ ORBIS");
        assert_eq!(Theme::brand_tagline(), "Your Linux software, in one place.");
        assert_eq!(theme.stage_mark(StageState::Done), "●");
        assert_eq!(theme.stage_mark(StageState::Active), "◐");
        assert_eq!(theme.stage_mark(StageState::Pending), "○");
    }

    #[test]
    fn ascii_theme_provides_safe_ascii_fallback() {
        let theme = Theme { color: false, unicode: false, width: 80, mode: ColorMode::None };
        let brand = theme.brand_full();
        assert_eq!(brand.len(), 1);
        assert!(brand[0].contains("@  ORBIS"));
        assert_eq!(theme.brand_compact(), "@ ORBIS");
        assert_eq!(theme.stage_mark(StageState::Done), "*");
        assert_eq!(theme.stage_mark(StageState::Active), ">");
        assert_eq!(theme.stage_mark(StageState::Pending), ".");
    }

    #[test]
    fn provider_token_is_neutral_foreground() {
        let theme = Theme { color: true, unicode: true, width: 80, mode: ColorMode::TrueColor };
        assert_eq!(theme.style(Token::Provider), theme.style(Token::Foreground));
    }
}
