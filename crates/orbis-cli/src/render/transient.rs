//! Small, scrollback-preserving terminal interactions for the default CLI.
//!
//! This module deliberately has no Ratatui control flow. It owns only the
//! compact bare-command summary; command execution remains in commands and
//! the persistent UI remains an explicit mode.

use super::theme::{Theme, Token};
use crate::cli::Command;

/// Prints the bare-command summary. Bare `orbis` is intentionally informational;
/// explicit commands remain the fast path for execution.
pub(crate) fn launcher(theme: Theme) -> Result<Option<Command>, String> {
    // Attention + recent activity come from the Orbis operation journal and a
    // cheap reboot-required flag check. Provider inventories never start here.
    let activity = recent_activity();
    let attention = orbis_core::system::launcher_insight(&activity).attention;
    print!("{}", launcher_text_compact(theme, &attention, &activity));
    Ok(None)
}

/// Recent Orbis operations for the launcher footer. Never shell-history noise.
pub(crate) fn recent_activity() -> Vec<orbis_core::operation::OperationRecord> {
    orbis_core::system::recent_activity(3)
}

#[cfg(test)]
pub(crate) fn launcher_text(theme: Theme) -> String {
    launcher_text_compact(theme, &[], &[])
}

fn launcher_text_compact(
    theme: Theme,
    attention: &[orbis_core::system::AttentionItem],
    activity: &[orbis_core::operation::OperationRecord],
) -> String {
    let mut output = format!("{}\n\n", theme.paint(theme.brand_compact(), Token::Primary));
    if !attention.is_empty() {
        for item in attention.iter().take(2) {
            output.push_str(&format!("{} {}\n", theme.mark(Token::Caution), item.title));
        }
    }
    if !activity.is_empty() {
        output.push_str(&format!("{}\n", theme.paint("Recent", Token::Muted)));
        for record in activity.iter().take(2) {
            output.push_str(&format!(
                "{} {}  {}\n",
                activity_mark(record, theme),
                record.activity_title(),
                relative_time(record, current_time_ms())
            ));
        }
    }
    output.push_str("\nfind · install · update · activity · health\n");
    output
}

fn activity_token(record: &orbis_core::operation::OperationRecord) -> Token {
    match record.status {
        orbis_core::operation::OperationStatus::Succeeded => Token::Positive,
        orbis_core::operation::OperationStatus::PartiallyVerified => Token::Caution,
        orbis_core::operation::OperationStatus::Failed => Token::Destructive,
        orbis_core::operation::OperationStatus::Running => Token::Muted,
    }
}

fn activity_mark(record: &orbis_core::operation::OperationRecord, theme: Theme) -> &'static str {
    theme.mark(activity_token(record))
}

fn current_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis() as u64)
}

fn relative_time(record: &orbis_core::operation::OperationRecord, now: u64) -> String {
    let timestamp = record.completed_at_unix_ms.unwrap_or(record.started_at_unix_ms);
    let seconds = now.saturating_sub(timestamp) / 1_000;
    if seconds < 60 {
        "just now".into()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_is_compact_and_has_no_boxed_ui() {
        let text = launcher_text(Theme::test(80));
        assert!(text.contains("* Orbis"));
        assert!(text.contains("find · install · update · activity · health"));
        assert!(!text.contains("+--+"));
        assert!(!text.contains("move"));
    }

    #[test]
    fn recent_activity_section_is_outcome_oriented() {
        use orbis_core::operation::{OperationStatus, OperationType, running};

        let theme = Theme::test(80);
        let plain = launcher_text_compact(theme, &[], &[]);
        assert!(!plain.contains("Recent"));
        assert!(!plain.contains("Recent commands"));

        let mut record = running("tx-demo", OperationType::SystemUpdate, "Update the system", None);
        record.status = OperationStatus::Succeeded;
        record.summary = "18 packages upgraded".into();
        record.changes = vec![orbis_core::facts::PackageChange {
            package_id: "openssl".into(),
            name: Some("openssl".into()),
            kind: orbis_core::facts::ChangeKind::Upgraded,
            from_version: Some("1".into()),
            to_version: Some("3".into()),
        }];
        let text = launcher_text_compact(theme, &[], &[record]);
        assert!(text.contains("Recent"));
        assert!(text.contains("System updated"));
        assert!(!text.contains("Recent commands"));
    }
}
