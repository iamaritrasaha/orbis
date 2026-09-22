//! Shared outcome facts used by operations, APT observation, and maintenance results.
//!
//! Kept free of journal/maintenance imports so transaction and maintenance records
//! can carry verified changes without module cycles.

use serde::{Deserialize, Serialize};

use crate::transaction::VerificationResult;

/// How a package changed as a result of an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Package was newly installed.
    Installed,
    /// Package was removed.
    Removed,
    /// Package version changed.
    Upgraded,
    /// Package remained present without a recorded version change.
    Unchanged,
}

/// One observed package change after execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageChange {
    /// Provider package identifier.
    pub package_id: String,
    /// Friendly name when distinct from the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Kind of change observed.
    pub kind: ChangeKind,
    /// Version before the operation, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    /// Version after the operation, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_version: Option<String>,
}

impl PackageChange {
    /// Compact `old → new` label for activity rendering.
    pub fn version_span(&self) -> Option<String> {
        match (&self.from_version, &self.to_version) {
            (Some(from), Some(to)) if from != to => Some(format!("{from} → {to}")),
            (None, Some(to)) => Some(to.clone()),
            (Some(from), None) => Some(format!("{from} → (removed)")),
            _ => None,
        }
    }
}

/// Structured diagnosis when a package operation fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCause {
    /// apt/dpkg lock held by another process.
    AptLock,
    /// Broken package dependencies or half-configured packages.
    BrokenDependencies,
    /// Repository / mirror / index error.
    RepositoryError,
    /// Network unreachable or download failure.
    NetworkFailure,
    /// Held package blocked the change.
    HeldPackage,
    /// Insufficient privileges.
    PermissionFailure,
    /// Package name or candidate does not exist.
    InvalidPackage,
    /// dpkg was interrupted and needs recovery.
    InterruptedDpkg,
    /// Cause could not be classified; keep the summary.
    Unknown,
}

impl FailureCause {
    /// Short label for UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::AptLock => "package manager busy",
            Self::BrokenDependencies => "broken dependencies",
            Self::RepositoryError => "repository error",
            Self::NetworkFailure => "network failure",
            Self::HeldPackage => "held package",
            Self::PermissionFailure => "permission denied",
            Self::InvalidPackage => "invalid package",
            Self::InterruptedDpkg => "interrupted dpkg state",
            Self::Unknown => "unclassified failure",
        }
    }
}

/// Human-usable failure diagnosis (not a stderr dump).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FailureDiagnosis {
    /// Classified cause.
    pub cause: FailureCause,
    /// One-line summary for the user.
    pub summary: String,
    /// Optional next step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Verification details recorded with an operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerificationReport {
    /// Overall verification outcome.
    pub result: VerificationResult,
    /// Whether Orbis considers the user-facing goal achieved.
    pub verified: bool,
    /// Human explanation of what was checked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
}

/// Expandable technical detail: the typed command that ran.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawCommandRef {
    /// Program basename (never a shell string).
    pub program: String,
    /// Argument vector.
    pub args: Vec<String>,
    /// Exit status when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
}
