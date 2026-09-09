//! Small, scrollback-preserving terminal interactions for the default CLI.
//!
//! This module deliberately has no Ratatui control flow. It owns only a short
//! reveal and a bounded launcher; command execution remains in commands and the
//! persistent UI remains an explicit mode.

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crossterm::{event, terminal};

use super::{
    brand::{BrandFrame, IdentityMode},
    region::TransientRegion,
    theme::{Theme, Token},
};
use crate::cli::Command;

const LAUNCHER_OPTIONS: [&str; 8] = [
    "Find software",
    "Show software",
    "Check updates",
    "Refresh information",
    "Clean up",
    "Health",
    "History",
    "Full interface",
];
/// Returns the compact command identity used by the transient reveal.
pub(crate) fn command_label(command: Option<&Command>) -> Option<&'static str> {
    Some(match command? {
        Command::Dashboard => "UI",
        Command::Sources => "SOURCES",
        Command::Find { .. } => "FIND",
        Command::Show { .. } => "SHOW",
        Command::Info { .. } => "INFO",
        Command::Explain { .. } => "EXPLAIN",
        Command::Health => "HEALTH",
        Command::SelfUpdate { .. } => "SELF UPDATE",
        Command::Install { .. } => "INSTALL",
        Command::Remove { .. } => "REMOVE",
        Command::Update { apply, .. } => {
            if *apply {
                "UPDATE"
            } else {
                "UPDATES"
            }
        }
        Command::Refresh { .. } => "REFRESH",
        Command::Upgrade { .. } => "UPGRADE",
        Command::Clean { .. } => "CLEAN",
        Command::History { .. } => "HISTORY",
        Command::Why { .. } => "WHY",
    })
}

/// Shows a bounded identity reveal in one terminal area.
pub(crate) fn reveal(theme: Theme, label: &str) {
    if !theme.color
        || !theme.unicode
        || !io::stdin().is_terminal()
        || !io::stdout().is_terminal()
        || std::env::var_os("NO_COLOR").is_some()
        || std::env::var_os("REDUCE_MOTION").is_some()
        || std::env::var("TERM").is_ok_and(|term| term == "dumb")
    {
        return;
    }

    let mut stdout = io::stdout();
    let region = TransientRegion::new(4);
    if region.reserve(&mut stdout).is_err() {
        return;
    }
    let initial = BrandFrame::animation(theme, 0);
    if region.render(&mut stdout, initial.lines()).is_err() {
        return;
    }
    let _ = stdout.flush();

    let raw_guard = RawModeGuard::new();
    let mut skipped = false;
    for step in 1..=6 {
        let frame = BrandFrame::animation(theme, step);
        let _ = region.render(&mut stdout, frame.lines());
        let _ = stdout.flush();
        let wait = [45, 55, 55, 65, 65, 75, 0][step];
        if wait > 0 && event::poll(Duration::from_millis(wait)).unwrap_or(false) {
            let _ = event::read();
            skipped = true;
            break;
        }
    }
    if skipped {
        let frame = BrandFrame::animation(theme, usize::MAX);
        let _ = region.render(&mut stdout, frame.lines());
        let _ = stdout.flush();
    }
    let collapse = BrandFrame::settled(theme, IdentityMode::Command(label));
    let _ = region.render(&mut stdout, collapse.lines());
    let _ = stdout.flush();
    drop(raw_guard);
    let _ = region.clear_and_release_at_anchor(&mut stdout);
    let _ = stdout.flush();
}

struct RawModeGuard(bool);

impl RawModeGuard {
    fn new() -> Self {
        Self(terminal::enable_raw_mode().is_ok())
    }

    fn try_new() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|error| error.to_string())?;
        Ok(Self(true))
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = terminal::disable_raw_mode();
        }
    }
}

/// Runs the bare-command launcher. None means cancel or a non-interactive
/// caller; the caller can then return without entering a persistent screen.
pub(crate) fn launcher(theme: Theme) -> Result<Option<Command>, String> {
    if !terminal_capable() {
        print!("{}", launcher_text_compact(theme, 0));
        return Ok(None);
    }

    let mut selected = 0usize;
    let region = TransientRegion::new(launcher_rows(theme, selected).len());
    let mut stdout = io::stdout();
    region.reserve(&mut stdout).map_err(|error| error.to_string())?;
    region
        .render(&mut stdout, &launcher_rows(theme, selected))
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())?;
    let raw_guard = RawModeGuard::try_new()?;
    let result: Result<Option<usize>, String> = (|| loop {
        if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
            continue;
        }
        let event = event::read().map_err(|error| error.to_string())?;
        let event::Event::Key(key) = event else { continue };
        let mut changed = false;
        match key.code {
            event::KeyCode::Up | event::KeyCode::Char('k') => {
                selected = (selected + LAUNCHER_OPTIONS.len() - 1) % LAUNCHER_OPTIONS.len();
                changed = true;
            }
            event::KeyCode::Down | event::KeyCode::Char('j') => {
                selected = (selected + 1) % LAUNCHER_OPTIONS.len();
                changed = true;
            }
            event::KeyCode::Enter => break Ok(Some(selected)),
            event::KeyCode::Esc | event::KeyCode::Char('q' | 'Q') => break Ok(None),
            _ => {}
        }
        if changed {
            redraw_launcher(&region, theme, selected)?;
        }
    })();
    drop(raw_guard);
    region.clear_and_finish(&mut stdout).map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())?;
    let result = result?;

    let Some(selected) = result else { return Ok(None) };
    Ok(Some(command_for_selection(selected)?))
}

fn command_for_selection(selected: usize) -> Result<Command, String> {
    match selected {
        0 => Ok(Command::Find { query: prompt("Find software")?, source: None }),
        1 => Ok(Command::Show { package: prompt("Show software")?, source: None }),
        2 => Ok(Command::Update { source: None, plan: false, apply: false, yes: false }),
        3 => Ok(Command::Refresh { source: None, plan: false, yes: false }),
        4 => Ok(Command::Clean { source: None, plan: false, yes: false }),
        5 => Ok(Command::Health),
        6 => Ok(Command::History { operation_id: None, limit: 20, source: None }),
        7 => Ok(Command::Dashboard),
        _ => Err("invalid launcher selection".into()),
    }
}

fn prompt(label: &str) -> Result<String, String> {
    print!("  {label}: ");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut value = String::new();
    io::stdin().read_line(&mut value).map_err(|error| error.to_string())?;
    let value = value.trim().to_owned();
    if value.is_empty() { Err("a software name is required".into()) } else { Ok(value) }
}

fn redraw_launcher(region: &TransientRegion, theme: Theme, selected: usize) -> Result<(), String> {
    let mut stdout = io::stdout();
    region
        .render(&mut stdout, &launcher_rows(theme, selected))
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn launcher_text(theme: Theme, selected: usize) -> String {
    let mut output = launcher_rows(theme, selected).join("\n");
    output.push('\n');
    output
}

fn launcher_rows(theme: Theme, selected: usize) -> Vec<String> {
    let mut rows = BrandFrame::settled(theme, IdentityMode::Launcher).lines().to_vec();
    rows.push(String::new());
    for (index, option) in LAUNCHER_OPTIONS.iter().enumerate() {
        let marker = if index == selected { if theme.unicode { "▸" } else { ">" } } else { " " };
        let token = if index == selected { Token::Selected } else { Token::Foreground };
        rows.push(format!(
            "  {} {}",
            theme.paint(marker, Token::Primary),
            theme.paint(option, token)
        ));
    }
    let controls = if theme.unicode {
        "↑↓ move  Enter choose  q cancel"
    } else {
        "j/k move  Enter choose  q cancel"
    };
    rows.push(String::new());
    rows.push(format!("  {controls}"));
    rows
}

fn launcher_text_compact(theme: Theme, selected: usize) -> String {
    let mut output = format!(
        "{}\n  {}\n\n",
        theme.paint(theme.brand_compact(), Token::Primary),
        theme.paint(Theme::brand_tagline(), Token::Muted)
    );
    for (index, option) in LAUNCHER_OPTIONS.iter().enumerate() {
        let marker = if index == selected { if theme.unicode { "▸" } else { ">" } } else { " " };
        let token = if index == selected { Token::Selected } else { Token::Foreground };
        output.push_str(&format!(
            "  {} {}\n",
            theme.paint(marker, Token::Primary),
            theme.paint(option, token)
        ));
    }
    output.push_str("\n  j/k move  Enter choose  q cancel\n");
    output
}

fn terminal_capable() -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_is_task_first_and_has_no_boxed_ui() {
        let text = launcher_text(Theme::test(80), 0);
        assert!(text.contains("╭──╮") || text.contains("+--+"));
        assert!(text.contains("HRIK"));
        assert!(text.contains("Find software"));
        assert!(text.contains("Show software"));
        assert!(text.contains("Full interface"));
        assert!(!text.contains("╭────"));
    }

    #[test]
    fn command_labels_are_compact_and_explicit() {
        assert_eq!(command_label(Some(&Command::Health)), Some("HEALTH"));
        assert_eq!(command_label(Some(&Command::Dashboard)), Some("UI"));
    }

    #[test]
    fn launcher_navigation_reuses_one_fixed_frame() {
        let theme = Theme::test(80);
        let initial = launcher_rows(theme, 0);
        assert_eq!(initial.len(), 17);

        for selected in 0..=4 {
            let rows = launcher_rows(theme, selected);
            assert_eq!(rows.len(), initial.len());
            assert_eq!(rows.iter().filter(|row| row.contains("Find software")).count(), 1);
            assert_eq!(rows.iter().filter(|row| row.contains("Clean up")).count(), 1);
        }

        let clean_up = launcher_rows(theme, 4);
        let clean_up_index = initial.iter().position(|row| row.contains("Clean up")).unwrap();
        for (index, (before, after)) in initial.iter().zip(clean_up.iter()).enumerate() {
            assert_eq!(
                before.replace("> ", "  "),
                after.replace("> ", "  "),
                "unexpected launcher row change at {index}"
            );
        }
        assert!(initial.iter().any(|row| row.contains("> Find software")));
        assert!(clean_up[clean_up_index].contains("> Clean up"));
    }
}
