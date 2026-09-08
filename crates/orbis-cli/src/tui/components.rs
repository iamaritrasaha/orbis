//! Shared presentation primitives for the interactive terminal UI.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::render::theme::{Theme, Token};

/// Horizontal breathing room used by all secondary pages.
pub(crate) fn inset(area: Rect) -> Rect {
    let margin = if area.width < 90 { 2 } else { 3 };
    Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin * 2),
        height: area.height,
    }
}

/// Reserves the page header, flexible body, and persistent footer rows.
pub(crate) fn page_chunks(area: Rect) -> [Rect; 3] {
    let inner = inset(area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1), Constraint::Length(2)])
        .split(inner);
    [chunks[0], chunks[1], chunks[2]]
}

pub(crate) fn page_header(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: Theme,
    title: &str,
    subtitle: &str,
) {
    let lines = vec![
        Line::from(vec![
            Span::styled(
                theme.brand_compact(),
                theme.style(Token::Primary).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  /  ", theme.style(Token::Divider)),
            Span::styled(title.to_owned(), theme.style(Token::Foreground)),
        ]),
        Line::from(Span::styled(subtitle.to_owned(), theme.style(Token::Muted))),
    ];
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

pub(crate) fn page_footer(frame: &mut Frame<'_>, area: Rect, theme: Theme, actions: &str) {
    let block = Block::default().borders(Borders::TOP).border_style(theme.style(Token::Divider));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(actions.to_owned(), theme.style(Token::Muted)))),
        inner,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn page(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: Theme,
    title: &str,
    subtitle: &str,
    body: Vec<Line<'static>>,
    scroll: u16,
    actions: &str,
) {
    let [header, content, footer] = page_chunks(area);
    page_header(frame, header, theme, title, subtitle);
    frame.render_widget(
        Paragraph::new(Text::from(body)).scroll((scroll, 0)).wrap(Wrap { trim: false }),
        content,
    );
    page_footer(frame, footer, theme, actions);
}

pub(crate) fn panel<'a>(theme: Theme, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.style(Token::Divider))
        .title(Span::styled(title.to_owned(), theme.style(Token::Section)))
}

pub(crate) fn detail_panel<'a>(theme: Theme, title: &'a str) -> Block<'a> {
    panel(theme, title)
}

pub(crate) fn section_title(theme: Theme, title: &str) -> Line<'static> {
    Line::from(Span::styled(title.to_owned(), theme.style(Token::Section)))
}

#[allow(dead_code)]
pub(crate) fn divider(theme: Theme, width: u16) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width as usize), theme.style(Token::Divider)))
}

pub(crate) fn info_row(theme: Theme, label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<15}"), theme.style(Token::Muted)),
        Span::styled(value.into(), theme.style(Token::Foreground)),
    ])
}

pub(crate) fn status_chip(theme: Theme, marker: &str, label: &str, token: Token) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{marker} "), theme.style(token)),
        Span::styled(label.to_owned(), theme.style(token).add_modifier(Modifier::BOLD)),
    ])
}

pub(crate) fn result_summary(
    theme: Theme,
    marker: &str,
    label: &str,
    token: Token,
) -> Line<'static> {
    status_chip(theme, marker, label, token)
}

pub(crate) fn progress_stage(
    theme: Theme,
    marker: &str,
    label: &str,
    token: Token,
) -> Line<'static> {
    status_chip(theme, marker, label, token)
}

pub(crate) fn provider_row(
    theme: Theme,
    marker: &str,
    provider: &str,
    status: &str,
    token: Token,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{marker}  "), theme.style(token)),
        Span::styled(format!("{provider:<20}"), theme.style(Token::Foreground)),
        Span::styled(status.to_owned(), theme.style(token)),
    ])
}

pub(crate) fn selectable_row(
    theme: Theme,
    selected: bool,
    title: &str,
    detail: &str,
) -> Vec<Line<'static>> {
    let title_style = if selected {
        theme.style(Token::Selected).add_modifier(Modifier::BOLD)
    } else {
        theme.style(Token::Foreground)
    };
    vec![
        Line::from(vec![
            Span::styled(if selected { "> " } else { "  " }, title_style),
            Span::styled(title.to_owned(), title_style),
        ]),
        Line::from(Span::styled(format!("    {detail}"), theme.style(Token::Muted))),
    ]
}

pub(crate) fn loading(theme: Theme, message: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(theme.mark(Token::Caution).to_owned() + " ", theme.style(Token::Caution)),
        Span::styled(message.to_owned(), theme.style(Token::Muted)),
    ])
}

pub(crate) fn empty(theme: Theme, message: &str) -> Line<'static> {
    Line::from(Span::styled(message.to_owned(), theme.style(Token::Muted)))
}

pub(crate) fn warning(theme: Theme, message: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", theme.mark(Token::Caution)), theme.style(Token::Caution)),
        Span::styled(message.to_owned(), theme.style(Token::Caution)),
    ])
}

pub(crate) fn error(theme: Theme, message: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} ", theme.mark(Token::Destructive)),
            theme.style(Token::Destructive),
        ),
        Span::styled(message.to_owned(), theme.style(Token::Destructive)),
    ])
}

pub(crate) fn action_hint(theme: Theme, actions: &str) -> Line<'static> {
    Line::from(Span::styled(actions.to_owned(), theme.style(Token::Muted)))
}
