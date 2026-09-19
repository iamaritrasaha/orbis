//! Private, local shell-history insights.
//!
//! This subsystem answers one question: which commands does this user tend to
//! run? It is deliberately read-only, purely local, and never persists or
//! transmits anything. Raw history arguments are never surfaced; only
//! sanitized command signatures survive (see [`sanitize`]).
//!
//! The subsystem is organized around a [`ShellHistorySource`] so zsh and fish
//! can be added later without changing the analysis or presentation layers.

pub mod sanitize;

use std::path::PathBuf;

use serde::Serialize;

/// One normalized, counted command signature.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommandInsight {
    /// Sanitized signature such as `git status`; never raw arguments.
    pub signature: String,
    /// How many history entries produced this signature.
    pub count: u64,
}

/// Aggregate result of a shell-history analysis.
#[derive(Clone, Debug, Serialize)]
pub struct ShellHistoryReport {
    /// Shell the entries came from, e.g. `bash`.
    pub shell: String,
    /// Source file that was read, when one was readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub histfile: Option<String>,
    /// Physical command lines scanned (timestamp records excluded).
    pub entries_scanned: u64,
    /// Entries that produced a sanitized signature.
    pub commands_analyzed: u64,
    /// Top signatures, sorted by count descending then signature ascending.
    pub insights: Vec<CommandInsight>,
}

/// Why a shell-history source could not answer.
#[derive(Debug, thiserror::Error)]
pub enum ShellHistoryError {
    /// No history file was found for this shell.
    #[error("no {shell} history file was found")]
    NotFound {
        /// Shell name, e.g. `bash`.
        shell: String,
    },
    /// The history file exists but could not be read.
    #[error("could not read the {shell} history file: {message}")]
    Io {
        /// Shell name, e.g. `bash`.
        shell: String,
        /// Underlying error detail.
        message: String,
    },
}

/// A read-only provider of raw shell-history entries.
pub trait ShellHistorySource: Send + Sync {
    /// Canonical shell name, e.g. `bash`.
    fn shell_name(&self) -> &'static str;
    /// Returns the resolved history file path, when one exists.
    fn histfile(&self) -> Option<PathBuf>;
    /// Reads the raw history entries. Timestamp records and shell
    /// continuation artifacts are handled by the implementation; entries are
    /// always returned in file order.
    fn read_entries(&self) -> Result<Vec<String>, ShellHistoryError>;
}

/// Bash history source.
///
/// Resolution order: `$HISTFILE` when explicitly set and pointing at an
/// existing file, otherwise `~/.bash_history`.
pub struct BashHistorySource {
    histfile: Option<PathBuf>,
}

impl BashHistorySource {
    /// Creates a Bash source using the documented resolution order.
    pub fn from_environment() -> Self {
        Self { histfile: resolve_bash_histfile() }
    }

    /// Creates a Bash source pinned to one file, for tests.
    pub fn at(path: PathBuf) -> Self {
        Self { histfile: Some(path) }
    }
}

impl ShellHistorySource for BashHistorySource {
    fn shell_name(&self) -> &'static str {
        "bash"
    }

    fn histfile(&self) -> Option<PathBuf> {
        self.histfile.clone()
    }

    fn read_entries(&self) -> Result<Vec<String>, ShellHistoryError> {
        let path = self.histfile.as_ref().ok_or_else(|| ShellHistoryError::NotFound {
            shell: self.shell_name().to_owned(),
        })?;
        let bytes = std::fs::read(path).map_err(|error| ShellHistoryError::Io {
            shell: self.shell_name().to_owned(),
            message: error.to_string(),
        })?;
        Ok(parse_bash_history(&String::from_utf8_lossy(&bytes)))
    }
}

/// Parses raw Bash history text into logical command entries.
///
/// Bash writes extended history timestamps as `#<seconds>` lines; they are
/// metadata, never commands, and are dropped. A physical line ending in `\`
/// is joined with the following line so a wrapped single command stays one
/// entry. Commands the user entered with embedded newlines were written as
/// separate physical lines by Bash itself; each is analyzed independently,
/// which is safe because entries are never executed.
pub fn parse_bash_history(text: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut continued: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if continued.is_some() {
            let mut joined = continued.take().expect("continuation checked");
            joined.pop();
            joined.push_str(trimmed.trim_start());
            if joined.ends_with('\\') && !joined.ends_with("\\\\") {
                continued = Some(joined);
            } else if !joined.trim().is_empty() {
                entries.push(joined);
            }
            continue;
        }
        if is_bash_timestamp(trimmed) {
            continue;
        }
        if trimmed.ends_with('\\') && !trimmed.ends_with("\\\\") && trimmed.len() > 1 {
            continued = Some(trimmed.to_owned());
            continue;
        }
        if !trimmed.trim().is_empty() {
            entries.push(trimmed.to_owned());
        }
    }
    if let Some(joined) = continued
        && !joined.trim().is_empty()
    {
        entries.push(joined);
    }
    entries
}

fn is_bash_timestamp(line: &str) -> bool {
    line.strip_prefix('#')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

/// Analyzes sanitized history entries and produces the top insights.
///
/// The `limit` caps the returned insights; scanning always covers every
/// entry so the summary counts are complete.
pub fn analyze(entries: &[String], limit: usize) -> ShellHistoryReport {
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut analyzed = 0_u64;
    for entry in entries {
        if let Some(signature) = sanitize::sanitize_command(entry) {
            analyzed += 1;
            *counts.entry(signature).or_insert(0) += 1;
        }
    }
    let mut insights: Vec<CommandInsight> = counts
        .into_iter()
        .map(|(signature, count)| CommandInsight { signature, count })
        .collect();
    insights.sort_by(|left, right| {
        right.count.cmp(&left.count).then_with(|| left.signature.cmp(&right.signature))
    });
    ShellHistoryReport {
        shell: "bash".into(),
        histfile: None,
        entries_scanned: entries.len() as u64,
        commands_analyzed: analyzed,
        insights: insights.into_iter().take(limit).collect(),
    }
}

/// Produces a complete report for one source, including the resolved path.
pub fn analyze_source(source: &dyn ShellHistorySource, limit: usize) -> Result<ShellHistoryReport, ShellHistoryError> {
    let entries = source.read_entries()?;
    let mut report = analyze(&entries, limit);
    report.shell = source.shell_name().to_owned();
    report.histfile = source.histfile().map(|path| path.display().to_string());
    Ok(report)
}

/// Whether shell-history insights are enabled for this run.
///
/// Insights are local-only and private by default; the user disables them by
/// setting `ORBIS_HISTORY_INSIGHTS` to `0`, `false`, or `off`.
pub fn insights_enabled() -> bool {
    insights_enabled_with(std::env::var("ORBIS_HISTORY_INSIGHTS").ok().as_deref())
}

/// Pure decision used by [`insights_enabled`]; exposed for tests.
fn insights_enabled_with(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "off" | "no" | "disable" | "disabled")
        }
        None => true,
    }
}

fn resolve_bash_histfile() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("HISTFILE") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let default = home.join(".bash_history");
    default.is_file().then_some(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_lines_are_metadata_not_commands() {
        let entries = parse_bash_history("#1712345678\ngit status\n#not-a-timestamp\nls\n");
        assert_eq!(entries, vec!["git status", "#not-a-timestamp", "ls"]);
    }

    #[test]
    fn backslash_continuations_join_into_one_entry() {
        let entries = parse_bash_history("echo one \\\n two\nls\n");
        assert_eq!(entries, vec!["echo one two", "ls"]);
    }

    #[test]
    fn escaped_trailing_backslash_stays_one_line() {
        let entries = parse_bash_history("echo \\\\\nls\n");
        assert_eq!(entries, vec!["echo \\\\", "ls"]);
    }

    #[test]
    fn analyze_counts_and_ranks_sanitized_signatures() {
        let entries = vec![
            "git status".to_owned(),
            "git status".to_owned(),
            "git pull".to_owned(),
            "cargo test --release".to_owned(),
            "export OPENAI_API_KEY=sk-something".to_owned(),
            "curl https://example.invalid/secret".to_owned(),
        ];
        let report = analyze(&entries, 10);
        assert_eq!(report.entries_scanned, 6);
        assert_eq!(report.commands_analyzed, 6);
        assert_eq!(report.insights[0].signature, "git status");
        assert_eq!(report.insights[0].count, 2);
        assert!(report.insights.iter().all(|insight| !insight.signature.contains("sk-")));
        assert!(report.insights.iter().all(|insight| !insight.signature.contains("secret")));
        assert!(report.insights.iter().any(|insight| insight.signature == "curl"));
        assert!(report.insights.iter().any(|insight| insight.signature == "export"));
    }

    #[test]
    fn documented_off_values_disable_insights() {
        for value in ["0", "false", "off", "no", "disable", "disabled", "", "  OFF "] {
            assert!(!insights_enabled_with(Some(value)), "{value:?} must disable insights");
        }
        assert!(insights_enabled_with(None));
        assert!(insights_enabled_with(Some("1")));
        assert!(insights_enabled_with(Some("on")));
    }
}
