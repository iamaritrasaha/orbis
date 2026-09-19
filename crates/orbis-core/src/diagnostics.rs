//! Read-only health checks and environment diagnostics.

use serde::Serialize;

use crate::models::PackageSource;

/// The outcome of one diagnostic check.
#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticCheck {
    /// Provider or environment area checked.
    pub area: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Short check label.
    pub title: String,
    /// User-facing result or suggestion.
    pub message: String,
}

impl DiagnosticCheck {
    /// Creates a successful provider check.
    pub fn passed(source: PackageSource, message: &str) -> Self {
        Self {
            area: source.label().into(),
            passed: true,
            title: "Ready".into(),
            message: message.into(),
        }
    }

    /// Creates a failed provider check with a suggested action.
    pub fn failed(source: PackageSource, title: &str, message: &str) -> Self {
        Self {
            area: source.label().into(),
            passed: false,
            title: title.into(),
            message: message.into(),
        }
    }

    /// Creates an environment check.
    pub fn environment(passed: bool, title: &str, message: &str) -> Self {
        Self { area: "Environment".into(), passed, title: title.into(), message: message.into() }
    }
}

/// The aggregate output of `orbis doctor`.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    /// Provider checks.
    pub checks: Vec<DiagnosticCheck>,
    /// Always true because diagnostics never execute package mutations.
    pub read_only: bool,
}

impl DoctorReport {
    /// Adds safe environment checks to a provider report.
    pub fn with_environment(mut self) -> Self {
        self.checks.extend(environment_checks());
        self
    }

    /// Whether every diagnostic passed.
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }
}

/// Checks local constraints without changing the machine.
pub fn environment_checks() -> Vec<DiagnosticCheck> {
    let path_ok = std::env::var_os("PATH").is_some_and(|path| !path.is_empty());
    let term = std::env::var("TERM").unwrap_or_else(|_| "unknown".into());
    let terminal_message = if term == "dumb" {
        "TERM=dumb; Orbis will keep output plain and avoid decoration.".to_owned()
    } else {
        format!("TERM={term}; readable terminal output is available.")
    };
    let mut checks = vec![
        DiagnosticCheck::environment(
            path_ok,
            "PATH",
            if path_ok {
                "Executable search path is available."
            } else {
                "PATH is empty; provider discovery may be unavailable."
            },
        ),
        DiagnosticCheck::environment(true, "Terminal", &terminal_message),
    ];
    checks.push(sudo_check());
    checks.push(history_directory_check());
    checks.push(shell_history_check());
    checks.push(self_update_check());
    checks.push(DiagnosticCheck::environment(
        true,
        "Safety boundary",
        "Plans are read-only; package execution requires an explicit confirmation boundary.",
    ));
    checks
}

/// Whether administrator authorization can be requested at all. Absence of
/// `sudo` is informational: user-scoped work never requires it.
fn sudo_check() -> DiagnosticCheck {
    if crate::process::program_on_path("sudo") {
        DiagnosticCheck::environment(
            true,
            "Sudo",
            "sudo is available for operations that require administrator authorization.",
        )
    } else {
        DiagnosticCheck::environment(
            true,
            "Sudo",
            "sudo is not installed; user-scoped work continues without it and system operations are refused.",
        )
    }
}

/// Reports where durable Orbis records live and whether they are writable.
fn history_directory_check() -> DiagnosticCheck {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute() && !path.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".local").join("state"))
        });
    let Some(base) = base else {
        return DiagnosticCheck::environment(
            false,
            "History",
            "Neither XDG_STATE_HOME nor HOME is set; durable history cannot be stored.",
        );
    };
    let directory = base.join("orbis").join("transactions");
    if directory.is_dir() {
        return DiagnosticCheck::environment(
            true,
            "History",
            &format!("Durable records are stored under {}.", directory.display()),
        );
    }
    DiagnosticCheck::environment(
        true,
        "History",
        &format!(
            "Durable records will be created under {} on the first mutation.",
            directory.display()
        ),
    )
}

/// Reports the private shell-history insight feature state.
fn shell_history_check() -> DiagnosticCheck {
    use crate::shell_history::ShellHistorySource;
    if !crate::shell_history::insights_enabled() {
        return DiagnosticCheck::environment(
            true,
            "Shell history",
            "Insights are disabled through ORBIS_HISTORY_INSIGHTS; nothing reads shell history.",
        );
    }
    let source = crate::shell_history::BashHistorySource::from_environment();
    match source.histfile() {
        Some(path) => DiagnosticCheck::environment(
            true,
            "Shell history",
            &format!(
                "Local insights read {} on demand; signatures only, nothing leaves this machine.",
                path.display()
            ),
        ),
        None => DiagnosticCheck::environment(
            true,
            "Shell history",
            "No Bash history file was found; the commands view stays empty.",
        ),
    }
}

/// Reports whether this build may replace itself through the release updater.
fn self_update_check() -> DiagnosticCheck {
    let version = env!("CARGO_PKG_VERSION");
    if version.contains(".dev.") {
        DiagnosticCheck::environment(
            true,
            "Self-update",
            "This is a development build; it never replaces itself through the release channel.",
        )
    } else {
        DiagnosticCheck::environment(
            true,
            "Self-update",
            "Release builds can check and install official Orbis releases with `orbis self-update`.",
        )
    }
}
