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
    vec![
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
        DiagnosticCheck::environment(
            true,
            "Safety boundary",
            "Plans are read-only; package execution requires an explicit confirmation boundary.",
        ),
    ]
}
