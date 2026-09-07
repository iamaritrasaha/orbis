use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Token {
    Primary,
    Foreground,
    Secondary,
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
}

fn basic_code(token: Token) -> u16 {
    match token {
        Token::Primary | Token::Section => 36,
        Token::Foreground | Token::Selected | Token::Surface => 37,
        Token::Secondary | Token::Caution | Token::Provider => 33,
        Token::Muted | Token::Divider | Token::Unavailable => 90,
        Token::Positive => 32,
        Token::Destructive => 31,
    }
}

fn basic_color(token: Token) -> Color {
    match token {
        Token::Primary | Token::Section => Color::Cyan,
        Token::Foreground | Token::Selected | Token::Surface => Color::White,
        Token::Secondary | Token::Caution | Token::Provider => Color::Yellow,
        Token::Muted | Token::Divider | Token::Unavailable => Color::DarkGray,
        Token::Positive => Color::Green,
        Token::Destructive => Color::Red,
    }
}

fn ansi256_code(token: Token) -> u8 {
    match token {
        Token::Primary | Token::Section => 81,
        Token::Foreground | Token::Selected | Token::Surface => 255,
        Token::Secondary | Token::Caution | Token::Provider => 221,
        Token::Muted | Token::Divider | Token::Unavailable => 245,
        Token::Positive => 78,
        Token::Destructive => 203,
    }
}

fn rgb(token: Token) -> (u8, u8, u8) {
    match token {
        Token::Primary | Token::Section => (94, 201, 213),
        Token::Foreground | Token::Selected | Token::Surface => (232, 238, 244),
        Token::Secondary => (236, 196, 118),
        Token::Muted | Token::Divider | Token::Unavailable => (122, 136, 151),
        Token::Positive => (109, 201, 151),
        Token::Caution | Token::Provider => (236, 196, 118),
        Token::Destructive => (235, 117, 117),
    }
}
