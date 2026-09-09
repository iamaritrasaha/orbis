use super::theme::{Theme, Token};
use unicode_width::UnicodeWidthStr;

/// The two-dimensional identity states used by the short interactive reveal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdentityMode<'a> {
    Launcher,
    Command(&'a str),
}

/// A prepared identity frame. Rendering stays separate from terminal I/O so
/// the animation can be tested as ordinary deterministic strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BrandFrame {
    lines: Vec<String>,
}

impl BrandFrame {
    pub(crate) fn animation(theme: Theme, step: usize) -> Self {
        let lines = match step {
            0 => vec![theme.paint("·", Token::Primary)],
            1 => vec![theme.paint(if theme.unicode { "◇" } else { "+" }, Token::Primary)],
            2 => vec![theme.paint(theme.brand_compact(), Token::Primary)],
            3 => wordmark(theme, 1),
            4 => wordmark(theme, 3),
            5 => wordmark(theme, 4),
            _ => wordmark(theme, 5),
        };
        Self { lines }
    }

    pub(crate) fn launcher(theme: Theme) -> Self {
        let mut lines = wordmark(theme, 5);
        lines.push(String::new());
        lines.push(centered(theme, Theme::brand_tagline(), wordmark_width(theme), Token::Muted));
        Self { lines }
    }

    pub(crate) fn settled(theme: Theme, mode: IdentityMode<'_>) -> Self {
        match mode {
            IdentityMode::Launcher => Self::launcher(theme),
            IdentityMode::Command(label) => Self::command(theme, label),
        }
    }

    pub(crate) fn command(theme: Theme, label: &str) -> Self {
        Self { lines: vec![compact_identity(theme, label)] }
    }

    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        text.push('\n');
        text
    }
}

pub(crate) fn compact_identity(theme: Theme, label: &str) -> String {
    format!(
        "{} // {}",
        theme.paint(theme.brand_compact(), Token::Primary),
        theme.paint(label, Token::Primary)
    )
}

fn wordmark(theme: Theme, letters: usize) -> Vec<String> {
    let rows = if theme.unicode { unicode_rows() } else { ascii_rows() };
    let visible = letters.min(5);
    let mut output = rows
        .iter()
        .map(|row| {
            let prefix = row.chars().take(visible_width(row, visible)).collect::<String>();
            theme.paint(&prefix, Token::Primary)
        })
        .collect::<Vec<_>>();
    if visible == 5 {
        output.push(signature(theme));
    }
    output
}

fn unicode_rows() -> [&'static str; 3] {
    [
        "╭──╮  ╭──╮  ╭──╮    ╷    ╭──╮",
        "│  │  ├──╯  ├──┤    │    ╰──╮",
        "╰──╯  ╵  ╲  ╰──╯    ╵    ╰──╯",
    ]
}

fn ascii_rows() -> [&'static str; 3] {
    [
        "+--+  +--+  +--+    |    +--+",
        "|  |  +--/  +--+    |    +--+",
        "+--+  |  \\  +--+    |    \\--+",
    ]
}

fn visible_width(row: &str, letters: usize) -> usize {
    match letters {
        0 => 0,
        1 => 4,
        2 => 10,
        3 => 16,
        4 => 23,
        _ => row.chars().count(),
    }
}

fn signature(theme: Theme) -> String {
    centered(theme, "HRIK", wordmark_width(theme), Token::Muted)
}

fn centered(theme: Theme, value: &str, width: usize, token: Token) -> String {
    let left = width.saturating_sub(value.width()) / 2;
    theme.paint(&format!("{:left$}{value}", ""), token)
}

fn wordmark_width(theme: Theme) -> usize {
    if theme.unicode { unicode_rows()[0].width() } else { ascii_rows()[0].width() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_wordmark_is_compact_and_readable() {
        let mut theme = Theme::test(80);
        theme.unicode = true;
        let frame = BrandFrame::animation(theme, 6);
        let text = frame.text();
        assert!(text.contains("╭──╮"));
        assert!(text.contains("├──╯"));
        assert!(text.contains("HRIK"));
        assert_eq!(frame.lines().len(), 4);
        assert!(text.lines().all(|line| line.chars().count() < 80));
    }

    #[test]
    fn ascii_wordmark_is_intentional_and_has_no_unicode_art() {
        let mut theme = Theme::test(80);
        theme.unicode = false;
        let text = BrandFrame::animation(theme, 6).text();
        assert!(text.contains("+--+"));
        assert!(text.contains("|  |"));
        assert!(!text.contains('╭'));
        assert!(text.contains("HRIK"));
    }

    #[test]
    fn animation_assembles_left_to_right() {
        let theme = Theme::test(80);
        let short = BrandFrame::animation(theme, 3).text();
        let longer = BrandFrame::animation(theme, 5).text();
        assert!(short.lines().next().unwrap().len() < longer.lines().next().unwrap().len());
    }

    #[test]
    fn identity_modes_are_explicit() {
        assert_eq!(IdentityMode::Launcher, IdentityMode::Launcher);
        assert_eq!(IdentityMode::Command("HEALTH"), IdentityMode::Command("HEALTH"));
    }

    #[test]
    fn signature_uses_the_wordmarks_display_width() {
        let mut theme = Theme::test(80);
        theme.unicode = true;
        let frame = BrandFrame::animation(theme, 6);
        let rows = frame.lines();
        assert_eq!(rows[0].width(), rows[2].width());
        assert_eq!(rows[3].trim(), "HRIK");
        assert_eq!(rows[3].find('H'), Some((rows[0].width() - "HRIK".width()) / 2));
    }
}
