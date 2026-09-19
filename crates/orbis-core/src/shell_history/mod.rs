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

use std::path::{Path, PathBuf};

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
///
/// Deliberately contains no filesystem paths: structured output exposes
/// sanitized, normalized command insights only, never the local history
/// location, the user's home, or raw entries.
#[derive(Clone, Debug, Serialize)]
pub struct ShellHistoryReport {
    /// Shell the entries came from, e.g. `bash`.
    pub shell: String,
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
    /// Reads entries from a bounded tail of the history file, for
    /// latency-sensitive callers such as the bare launcher. The result covers
    /// only the most recent history — reports built from it must be labeled
    /// as recent, never presented as all-time frequency.
    fn read_recent_entries(&self, max_bytes: u64) -> Result<Vec<String>, ShellHistoryError>;
}

/// Byte budget the launcher reads from the tail of the history file. Large
/// enough for thousands of recent commands, small enough that pathological
/// history sizes cannot stall startup.
pub const LAUNCHER_HISTORY_TAIL_BYTES: u64 = 256 * 1024;

/// Bash history source.
///
/// Resolution contract (see [`resolve_bash_histfile_from`]):
///
/// 1. `ORBIS_BASH_HISTFILE` when it names an existing regular file. This is
///    the explicit Orbis-level override and may deliberately live outside
///    the user's home directory.
/// 2. `$HISTFILE` only when it names an existing regular file **inside the
///    user's home directory** (after resolving symlinks) *and* there is
///    Bash-specific evidence: the file name itself looks like Bash history,
///    or the login shell is Bash. A name that itself identifies zsh or fish
///    history is rejected outright, regardless of other signals.
/// 3. `~/.bash_history` when it is an existing regular file.
/// 4. Otherwise no source: the caller reports the shell as unavailable
///    honestly rather than parsing another shell's history as Bash.
pub struct BashHistorySource {
    histfile: Option<PathBuf>,
}

impl BashHistorySource {
    /// Creates a Bash source using the documented resolution order.
    pub fn from_environment() -> Self {
        Self { histfile: resolve_bash_histfile() }
    }

    /// Creates a Bash source pinned to one file. Intended for tests; this
    /// bypasses environment resolution entirely.
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
        let path = self
            .histfile
            .as_ref()
            .ok_or_else(|| ShellHistoryError::NotFound { shell: self.shell_name().to_owned() })?;
        let bytes = std::fs::read(path).map_err(|error| ShellHistoryError::Io {
            shell: self.shell_name().to_owned(),
            message: error.to_string(),
        })?;
        Ok(parse_bash_history(&String::from_utf8_lossy(&bytes)))
    }

    fn read_recent_entries(&self, max_bytes: u64) -> Result<Vec<String>, ShellHistoryError> {
        use std::io::{Read, Seek, SeekFrom};
        let path = self
            .histfile
            .as_ref()
            .ok_or_else(|| ShellHistoryError::NotFound { shell: self.shell_name().to_owned() })?;
        let mut file = std::fs::File::open(path).map_err(|error| ShellHistoryError::Io {
            shell: self.shell_name().to_owned(),
            message: error.to_string(),
        })?;
        let length = file.metadata().map_err(|error| ShellHistoryError::Io {
            shell: self.shell_name().to_owned(),
            message: error.to_string(),
        })?
        .len();
        let start = length.saturating_sub(max_bytes);
        file.seek(SeekFrom::Start(start)).map_err(|error| ShellHistoryError::Io {
            shell: self.shell_name().to_owned(),
            message: error.to_string(),
        })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| ShellHistoryError::Io {
            shell: self.shell_name().to_owned(),
            message: error.to_string(),
        })?;
        let text = String::from_utf8_lossy(&bytes);
        // Skip the first, likely truncated line unless the tail starts at the
        // beginning of the file, so parsing always begins at a line boundary.
        let complete = if start > 0 {
            text.split_once('\n').map(|(_, rest)| rest).unwrap_or_default()
        } else {
            text.as_ref()
        };
        Ok(parse_bash_history(complete))
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
    let mut insights: Vec<CommandInsight> =
        counts.into_iter().map(|(signature, count)| CommandInsight { signature, count }).collect();
    insights.sort_by(|left, right| {
        right.count.cmp(&left.count).then_with(|| left.signature.cmp(&right.signature))
    });
    ShellHistoryReport {
        shell: "bash".into(),
        entries_scanned: entries.len() as u64,
        commands_analyzed: analyzed,
        insights: insights.into_iter().take(limit).collect(),
    }
}

/// Produces a complete report for one source. The resolved path stays
/// internal; reports never carry filesystem locations.
pub fn analyze_source(
    source: &dyn ShellHistorySource,
    limit: usize,
) -> Result<ShellHistoryReport, ShellHistoryError> {
    let entries = source.read_entries()?;
    let mut report = analyze(&entries, limit);
    report.shell = source.shell_name().to_owned();
    Ok(report)
}

/// Produces a report from a bounded recent tail of one source's history.
///
/// The counts describe only the sampled recent window (at most `max_bytes`
/// of the history file), so callers must present them as recent activity,
/// not all-time frequency. [`analyze_source`] remains the complete scan.
pub fn analyze_recent(
    source: &dyn ShellHistorySource,
    max_bytes: u64,
    limit: usize,
) -> Result<ShellHistoryReport, ShellHistoryError> {
    let entries = source.read_recent_entries(max_bytes)?;
    let mut report = analyze(&entries, limit);
    report.shell = source.shell_name().to_owned();
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
    resolve_bash_histfile_from(&BashHistfileEnvironment::from_process())
}

/// Environment inputs to Bash history resolution, so the contract can be
/// tested without mutating process-wide variables.
#[derive(Clone, Debug, Default)]
struct BashHistfileEnvironment {
    /// Explicit Orbis override (`ORBIS_BASH_HISTFILE`).
    orbis_override: Option<PathBuf>,
    /// The invoking shell's `$HISTFILE`.
    histfile: Option<PathBuf>,
    /// The user's home directory (`$HOME`).
    home: Option<PathBuf>,
    /// The login shell (`$SHELL`).
    login_shell: Option<PathBuf>,
}

impl BashHistfileEnvironment {
    fn from_process() -> Self {
        Self {
            orbis_override: std::env::var_os("ORBIS_BASH_HISTFILE").map(PathBuf::from),
            histfile: std::env::var_os("HISTFILE").map(PathBuf::from),
            home: std::env::var_os("HOME").map(PathBuf::from),
            login_shell: std::env::var_os("SHELL").map(PathBuf::from),
        }
    }
}

/// Resolves the Bash history file for one environment. `$HISTFILE` belongs to
/// whichever shell invoked Orbis and may point at zsh or fish history, so it
/// is trusted only with Bash evidence and only under the user's home; the
/// explicit Orbis override is the one deliberately unconstrained path.
fn resolve_bash_histfile_from(env: &BashHistfileEnvironment) -> Option<PathBuf> {
    if let Some(path) = env.orbis_override.as_ref().filter(|path| is_regular_file(path)) {
        return Some(path.clone());
    }
    if let Some(path) = env.histfile.as_ref().filter(|path| {
        is_regular_file(path)
            && is_under_home(path, env.home.as_deref())
            && !looks_like_foreign_shell_history(path)
            && (looks_like_bash_history(path) || login_shell_is_bash(env.login_shell.as_deref()))
    }) {
        return Some(path.clone());
    }
    let home = env.home.as_ref()?;
    let default = home.join(".bash_history");
    is_regular_file(&default).then_some(default)
}

/// Existing regular file; symlinks count only when they resolve to a
/// regular file, and broken symlinks count as nothing.
fn is_regular_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Whether the path stays under the given home directory once symlinks are
/// resolved, so an in-home symlink to an arbitrary location is not trusted.
fn is_under_home(path: &Path, home: Option<&Path>) -> bool {
    let Some(home) = home else { return false };
    let (Ok(home), Ok(canonical)) = (home.canonicalize(), path.canonicalize()) else {
        return false;
    };
    canonical.starts_with(home)
}

/// Whether a file name itself identifies another shell's history. Such a name
/// is definitive evidence against Bash, so no other signal can override it.
fn looks_like_foreign_shell_history(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            lower.contains("zsh") || lower.contains("fish")
        })
}

/// Whether a file name is itself Bash-history evidence. Neutral names carry
/// no evidence either way and rely on the login shell instead.
fn looks_like_bash_history(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else { return false };
    let lower = name.to_ascii_lowercase();
    lower == ".bash_history" || lower.contains("bash")
}

/// Whether the login shell identifies itself as Bash by its executable name.
fn login_shell_is_bash(login_shell: Option<&Path>) -> bool {
    login_shell
        .and_then(|shell| shell.file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("bash"))
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

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("orbis-bash-histfile-{label}-{unique}"))
    }

    fn environment(
        home: &std::path::Path,
        histfile: Option<std::path::PathBuf>,
        login_shell: Option<&str>,
    ) -> BashHistfileEnvironment {
        BashHistfileEnvironment {
            orbis_override: None,
            histfile,
            home: Some(home.to_path_buf()),
            login_shell: login_shell.map(PathBuf::from),
        }
    }

    #[test]
    fn histfile_is_used_only_with_bash_evidence_under_home() {
        let home = unique_dir("home");
        std::fs::create_dir_all(&home).expect("home directory");
        let default = home.join(".bash_history");
        std::fs::write(&default, "ls\n").expect("default history");
        let bash_histfile = home.join("bash_history");
        std::fs::write(&bash_histfile, "git status\n").expect("named history");

        // Plain Bash history file name: accepted without shell evidence.
        let resolved = resolve_bash_histfile_from(&environment(&home, Some(bash_histfile.clone()), None));
        assert_eq!(resolved, Some(bash_histfile.clone()));

        // Bash login shell vouches for an unnamed in-home history file.
        let unnamed = home.join(".histfile");
        std::fs::write(&unnamed, "cargo build\n").expect("unnamed history");
        let resolved = resolve_bash_histfile_from(&environment(
            &home,
            Some(unnamed.clone()),
            Some("/bin/bash"),
        ));
        assert_eq!(resolved, Some(unnamed));

        // No Bash evidence at all: the default is used instead.
        let resolved = resolve_bash_histfile_from(&environment(
            &home,
            Some(home.join(".histfile")),
            Some("/usr/bin/zsh"),
        ));
        assert_eq!(resolved, Some(default.clone()));

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn zsh_and_fish_histfiles_are_rejected_even_with_bash_login_shell() {
        let home = unique_dir("home");
        std::fs::create_dir_all(&home).expect("home directory");
        let default = home.join(".bash_history");
        std::fs::write(&default, "ls\n").expect("default history");
        for foreign in [".zsh_history", "zsh-history", ".fish_history"] {
            let path = home.join(foreign);
            std::fs::write(&path, ": 1750000000:0;ls\n").expect("foreign history");
            let resolved = resolve_bash_histfile_from(&environment(
                &home,
                Some(path),
                Some("/bin/bash"),
            ));
            assert_eq!(resolved, Some(default.clone()), "{foreign} must not be read as Bash");
        }
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn histfile_outside_home_or_behind_a_symlink_is_rejected() {
        let home = unique_dir("home");
        let outside = unique_dir("outside");
        std::fs::create_dir_all(&home).expect("home directory");
        std::fs::create_dir_all(&outside).expect("outside directory");
        let default = home.join(".bash_history");
        std::fs::write(&default, "ls\n").expect("default history");

        // Bash-looking name, but outside HOME: not trusted.
        let external = outside.join(".bash_history");
        std::fs::write(&external, "ls\n").expect("external history");
        let resolved = resolve_bash_histfile_from(&environment(&home, Some(external), Some("/bin/bash")));
        assert_eq!(resolved, Some(default.clone()));

        // In-home symlink that resolves outside HOME: not trusted either.
        let target = outside.join("real-history");
        std::fs::write(&target, "ls\n").expect("symlink target");
        let linked = home.join("linked-bash-history");
        std::os::unix::fs::symlink(&target, &linked).expect("symlink");
        let resolved = resolve_bash_histfile_from(&environment(&home, Some(linked), Some("/bin/bash")));
        assert_eq!(resolved, Some(default));

        let _ = std::fs::remove_dir_all(home);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn explicit_orbis_override_wins_and_may_live_outside_home() {
        let home = unique_dir("home");
        let outside = unique_dir("outside");
        std::fs::create_dir_all(&home).expect("home directory");
        std::fs::create_dir_all(&outside).expect("outside directory");
        let default = home.join(".bash_history");
        std::fs::write(&default, "ls\n").expect("default history");

        let override_path = outside.join("deliberate-history");
        std::fs::write(&override_path, "git status\n").expect("override history");
        let env = BashHistfileEnvironment {
            orbis_override: Some(override_path.clone()),
            ..environment(&home, None, None)
        };
        assert_eq!(resolve_bash_histfile_from(&env), Some(override_path));

        // A nonexistent override is ignored, not invented.
        let env = BashHistfileEnvironment {
            orbis_override: Some(outside.join("missing")),
            ..environment(&home, None, None)
        };
        assert_eq!(resolve_bash_histfile_from(&env), Some(default));

        let _ = std::fs::remove_dir_all(home);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn nonexistent_histfile_and_plain_fallbacks() {
        let home = unique_dir("home");
        std::fs::create_dir_all(&home).expect("home directory");

        // No history anywhere: honestly unavailable.
        assert_eq!(resolve_bash_histfile_from(&environment(&home, None, None)), None);
        assert_eq!(
            resolve_bash_histfile_from(&environment(&home, Some(home.join(".bash_history")), None)),
            None
        );

        // Default ~/.bash_history appears once it exists.
        let default = home.join(".bash_history");
        std::fs::write(&default, "ls\n").expect("default history");
        assert_eq!(resolve_bash_histfile_from(&environment(&home, None, None)), Some(default));

        // No HOME at all: no resolution.
        let env = BashHistfileEnvironment::default();
        assert_eq!(resolve_bash_histfile_from(&env), None);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn serialized_report_exposes_no_local_paths() {
        let home = unique_dir("json-privacy");
        std::fs::create_dir_all(&home).expect("home directory");
        let history = home.join(".bash_history");
        std::fs::write(&history, "git status\nls\n").expect("history file");

        let source = BashHistorySource::at(history.clone());
        let report = analyze_source(&source, 10).expect("analysis");
        let json = serde_json::to_string(&report).expect("report serializes");

        // No history path, no home path, and no path field at all.
        assert!(!json.contains("histfile"), "histfile must not appear in JSON");
        assert!(!json.contains(history.to_str().expect("utf-8 temp path")));
        assert!(!json.contains(home.to_str().expect("utf-8 temp path")));
        assert!(!json.contains("/tmp/"), "no filesystem location may appear");

        // Only the sanitized aggregate shape is exposed.
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let mut keys =
            value.as_object().expect("object").keys().cloned().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, vec!["commands_analyzed", "entries_scanned", "insights", "shell"]);
        for insight in value["insights"].as_array().expect("insights array") {
            let mut insight_keys =
                insight.as_object().expect("insight object").keys().cloned().collect::<Vec<_>>();
            insight_keys.sort();
            assert_eq!(insight_keys, vec!["count", "signature"]);
        }

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn recent_tail_read_is_bounded_and_starts_at_a_line_boundary() {
        let home = unique_dir("tail");
        std::fs::create_dir_all(&home).expect("home directory");
        let history = home.join(".bash_history");
        // A large file whose interesting commands live only at the very end.
        let filler = "echo filler\n".repeat(20_000);
        let tail = "cargo test --release\ngit status\n";
        std::fs::write(&history, format!("echo early-marker\n{filler}{tail}"))
            .expect("large history");

        let source = BashHistorySource::at(history.clone());
        let budget = 64; // far below the file size, spanning only a few lines
        let entries = source.read_recent_entries(budget).expect("tail read");
        // Only a handful of lines fit in the window: the read is bounded.
        assert!(entries.len() <= 6, "tail read must respect the byte budget");
        assert!(
            !entries.iter().any(|entry| entry.contains("early-marker")),
            "entries far outside the window must never appear"
        );
        assert_eq!(
            entries.last().map(String::as_str),
            Some("git status"),
            "the newest entry is always included"
        );
        assert!(entries.iter().any(|entry| entry == "cargo test --release"));

        // A budget of the whole file behaves like a complete read.
        let full = source.read_recent_entries(u64::MAX).expect("full read");
        assert_eq!(full.len(), 20_003);
        assert_eq!(full.last().map(String::as_str), Some("git status"));

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn recent_tail_drops_the_truncated_first_line() {
        let home = unique_dir("tail-partial");
        std::fs::create_dir_all(&home).expect("home directory");
        let history = home.join(".bash_history");
        // "git stat" is split by the budget; the fragment must never surface.
        std::fs::write(&history, "ls -la\ncargo build\ngit status").expect("history file");
        let body = std::fs::read_to_string(&history).expect("content");
        let cut = body.find("git status").expect("marker");
        let budget = (body.len() - cut + 2) as u64; // starts mid-"git stat…"
        assert!(budget < body.len() as u64);

        let source = BashHistorySource::at(history.clone());
        let entries = source.read_recent_entries(budget).expect("tail read");
        assert_eq!(entries, vec!["git status"]);
        assert!(entries.iter().all(|entry| !entry.contains("stat\ncargo")));

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn analyze_recent_counts_only_the_sampled_window() {
        let home = unique_dir("recent");
        std::fs::create_dir_all(&home).expect("home directory");
        let history = home.join(".bash_history");
        // Filler lines are far longer than the budget, so a 64-byte window
        // can only reach the recent section at the end.
        let old = format!("{}\n", "echo old-command-".repeat(12)).repeat(5_000);
        let recent = "git status\n#1712345678\ngit status\n";
        std::fs::write(&history, format!("{old}{recent}")).expect("history file");

        let source = BashHistorySource::at(history);
        let report = analyze_recent(&source, 64, 10).expect("recent analysis");
        assert_eq!(report.shell, "bash");
        assert_eq!(report.entries_scanned, 2);
        assert_eq!(report.insights[0].signature, "git status");
        assert_eq!(report.insights[0].count, 2);
        assert!(!report.insights.iter().any(|insight| insight.signature == "echo"));

        // The complete scan still sees everything, unchanged.
        let complete = analyze_source(&source, 10).expect("complete analysis");
        assert_eq!(complete.entries_scanned, 5_002);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn bash_history_name_evidence_is_conservative() {
        assert!(looks_like_bash_history(Path::new("/home/u/.bash_history")));
        assert!(looks_like_bash_history(Path::new("/home/u/bash_history")));
        assert!(!looks_like_bash_history(Path::new("/home/u/.zsh_history")));
        assert!(!looks_like_bash_history(Path::new("/home/u/.fish-history")));
        assert!(!looks_like_bash_history(Path::new("/home/u/.histfile")));
        assert!(!looks_like_bash_history(Path::new("/home/u/")));
    }
}
