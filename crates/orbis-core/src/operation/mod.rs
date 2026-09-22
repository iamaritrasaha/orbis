//! Orbis-owned operation journal: meaningful system outcomes, not shell commands.
//!
//! An operation is one user intent → planned actions → execution → observation →
//! verification → recorded outcome. Raw provider commands belong only in technical
//! detail fields, never as the primary history model.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::maintenance::{
    MaintenanceAction, MaintenancePlan, MaintenanceProviderStatus, MaintenanceResult,
    MaintenanceStatus,
};
use crate::models::PackageSource;
use crate::transaction::{
    OperationAction, OperationPlan, OperationRequest, TransactionResult, TransactionStatus,
    VerificationResult,
};

/// On-disk schema for operation journal records.
pub const SCHEMA_VERSION: u32 = 1;

/// High-level kind of system operation Orbis performed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    /// Install one package.
    Install,
    /// Remove one package.
    Remove,
    /// Refresh package metadata / indexes.
    Refresh,
    /// Apply pending upgrades.
    SystemUpdate,
    /// Remove unused packages safely.
    Cleanup,
    /// Other coordinated maintenance.
    Maintenance,
}

impl OperationType {
    /// Short human title used in activity lists.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Install => "Package installed",
            Self::Remove => "Package removed",
            Self::Refresh => "Metadata refreshed",
            Self::SystemUpdate => "System updated",
            Self::Cleanup => "Unused packages cleaned",
            Self::Maintenance => "Maintenance completed",
        }
    }

    /// Title when the operation failed.
    pub const fn failed_title(self) -> &'static str {
        match self {
            Self::Install => "Install failed",
            Self::Remove => "Remove failed",
            Self::Refresh => "Refresh failed",
            Self::SystemUpdate => "System update failed",
            Self::Cleanup => "Cleanup failed",
            Self::Maintenance => "Maintenance failed",
        }
    }
}

/// Final status of one journaled operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Running; mutation boundary crossed but not finished.
    Running,
    /// Process and verification both succeeded.
    Succeeded,
    /// Process succeeded; verification incomplete or residual work remains.
    PartiallyVerified,
    /// Process or verification failed. Never report as completed.
    Failed,
}

pub use crate::facts::{
    ChangeKind, FailureCause, FailureDiagnosis, PackageChange, RawCommandRef, VerificationReport,
};

/// One durable Orbis operation outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationRecord {
    /// Schema version.
    pub schema_version: u32,
    /// Stable operation id (shared with transaction/maintenance ids when applicable).
    pub id: String,
    /// Operation kind.
    pub operation_type: OperationType,
    /// What the user asked for, in plain language.
    pub intent: String,
    /// Final status. Failed never masquerades as completed.
    pub status: OperationStatus,
    /// Start time (unix ms).
    pub started_at_unix_ms: u64,
    /// Completion time (unix ms), absent while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix_ms: Option<u64>,
    /// Duration in milliseconds when completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Primary provider source when one owns the operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PackageSource>,
    /// Short summary of what Orbis did.
    pub summary: String,
    /// Observed package changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<PackageChange>,
    /// Verification report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationReport>,
    /// Warnings (reboot required, kept-back packages, residual candidates, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Structured failure diagnosis when status is Failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<FailureDiagnosis>,
    /// Raw typed commands for expandable technical detail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_commands: Vec<RawCommandRef>,
    /// Optional truncated provider message (never a full dump).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl OperationRecord {
    /// Compact activity line for launcher / Recent Activity.
    pub fn activity_title(&self) -> String {
        match self.status {
            OperationStatus::Succeeded | OperationStatus::PartiallyVerified => {
                match self.operation_type {
                    OperationType::Install => self
                        .changes
                        .iter()
                        .find(|change| change.kind == ChangeKind::Installed)
                        .map(|change| {
                            change.name.clone().unwrap_or_else(|| change.package_id.clone())
                        })
                        .map(|name| format!("{name} installed"))
                        .unwrap_or_else(|| self.operation_type.title().into()),
                    OperationType::Remove => self
                        .changes
                        .iter()
                        .find(|change| change.kind == ChangeKind::Removed)
                        .map(|change| {
                            change.name.clone().unwrap_or_else(|| change.package_id.clone())
                        })
                        .map(|name| format!("{name} removed"))
                        .unwrap_or_else(|| self.operation_type.title().into()),
                    other => other.title().into(),
                }
            }
            OperationStatus::Failed => self.operation_type.failed_title().into(),
            OperationStatus::Running => format!("{}…", self.intent),
        }
    }

    /// Supporting detail lines for activity rendering (changes, warnings, verification).
    pub fn activity_details(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let upgraded = self.changes.iter().filter(|c| c.kind == ChangeKind::Upgraded).count();
        let installed = self.changes.iter().filter(|c| c.kind == ChangeKind::Installed).count();
        let removed = self.changes.iter().filter(|c| c.kind == ChangeKind::Removed).count();
        if upgraded > 0 {
            lines.push(format!(
                "{upgraded} package{} upgraded",
                if upgraded == 1 { "" } else { "s" }
            ));
        }
        if installed > 0 && self.operation_type != OperationType::Install {
            lines.push(format!(
                "{installed} package{} added",
                if installed == 1 { "" } else { "s" }
            ));
        }
        if removed > 0 && self.operation_type != OperationType::Remove {
            lines.push(format!("{removed} package{} removed", if removed == 1 { "" } else { "s" }));
        }
        // Show a few notable version spans (kernels, security-critical names first by prefix).
        for change in self.changes.iter().take(4) {
            if let Some(span) = change.version_span() {
                let name = change.name.as_deref().unwrap_or(change.package_id.as_str());
                lines.push(format!("{name}: {span}"));
            }
        }
        if let Some(verification) = &self.verification {
            lines.push(if verification.verified {
                "Verified: yes".into()
            } else {
                "Verified: no".into()
            });
        }
        for warning in self.warnings.iter().take(3) {
            lines.push(warning.clone());
        }
        if let Some(diagnosis) = &self.diagnosis {
            lines.push(diagnosis.summary.clone());
        } else if let Some(message) = &self.message
            && self.status == OperationStatus::Failed
        {
            lines.push(message.clone());
        }
        if let Some(duration_ms) = self.duration_ms {
            lines.push(format_duration(duration_ms));
        }
        lines
    }

    /// True when this operation needs user attention (failure or reboot warning).
    pub fn needs_attention(&self) -> bool {
        self.status == OperationStatus::Failed
            || self.warnings.iter().any(|w| w.to_ascii_lowercase().contains("reboot"))
    }
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1000 {
        format!("{duration_ms} ms")
    } else if duration_ms < 60_000 {
        format!("{:.1} s", duration_ms as f64 / 1000.0)
    } else {
        let minutes = duration_ms / 60_000;
        let seconds = (duration_ms % 60_000) / 1000;
        format!("{minutes}m {seconds}s")
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Builds an operation record from a completed single-package transaction.
pub fn from_transaction(
    request: &OperationRequest,
    result: &TransactionResult,
    started_at_unix_ms: u64,
    changes: Vec<PackageChange>,
    warnings: Vec<String>,
    diagnosis: Option<FailureDiagnosis>,
    raw_commands: Vec<RawCommandRef>,
) -> OperationRecord {
    let completed = now_unix_ms();
    let operation_type = match request.action {
        OperationAction::Install => OperationType::Install,
        OperationAction::Remove => OperationType::Remove,
    };
    let status = match result.status {
        TransactionStatus::Succeeded => OperationStatus::Succeeded,
        TransactionStatus::PartiallyVerified => OperationStatus::PartiallyVerified,
        TransactionStatus::Failed => OperationStatus::Failed,
    };
    let verified = matches!(result.verification, VerificationResult::Verified)
        && result.status == TransactionStatus::Succeeded;
    let intent = match request.action {
        OperationAction::Install => format!("Install {}", request.package.query),
        OperationAction::Remove => format!("Remove {}", request.package.query),
    };
    let summary = match status {
        OperationStatus::Succeeded => {
            let version = changes.iter().find_map(|c| c.to_version.as_deref()).or(result
                .plan
                .target
                .version
                .as_deref());
            match (request.action, version) {
                (OperationAction::Install, Some(version)) => {
                    format!("{} {} installed and verified", result.plan.target.name, version)
                }
                (OperationAction::Install, None) => {
                    format!("{} installed and verified", result.plan.target.name)
                }
                (OperationAction::Remove, _) => {
                    format!("{} removed and verified", result.plan.target.name)
                }
            }
        }
        OperationStatus::PartiallyVerified => {
            format!("{} completed with limited verification", result.plan.target.name)
        }
        OperationStatus::Failed => diagnosis
            .as_ref()
            .map(|d| d.summary.clone())
            .or_else(|| result.execution.message.clone())
            .unwrap_or_else(|| {
                format!("{} did not complete successfully", result.plan.target.name)
            }),
        OperationStatus::Running => intent.clone(),
    };
    let checks = match request.action {
        OperationAction::Install => vec![
            "Confirmed package is installed via dpkg".into(),
            "Recorded installed version when available".into(),
        ],
        OperationAction::Remove => {
            vec!["Confirmed package is no longer installed via dpkg".into()]
        }
    };
    OperationRecord {
        schema_version: SCHEMA_VERSION,
        id: result.plan.operation_id.clone(),
        operation_type,
        intent,
        status,
        started_at_unix_ms,
        completed_at_unix_ms: Some(completed),
        duration_ms: Some(completed.saturating_sub(started_at_unix_ms)),
        source: Some(result.plan.target.source),
        summary,
        changes,
        verification: Some(VerificationReport { result: result.verification, verified, checks }),
        warnings,
        diagnosis,
        raw_commands,
        message: result.execution.message.clone(),
    }
}

/// Builds an operation record from a coordinated maintenance run.
pub fn from_maintenance(
    plan: &MaintenancePlan,
    result: &MaintenanceResult,
    started_at_unix_ms: u64,
    changes: Vec<PackageChange>,
    warnings: Vec<String>,
    diagnosis: Option<FailureDiagnosis>,
    raw_commands: Vec<RawCommandRef>,
) -> OperationRecord {
    let completed = now_unix_ms();
    let operation_type = match plan.action {
        MaintenanceAction::Refresh => OperationType::Refresh,
        MaintenanceAction::Upgrade => OperationType::SystemUpdate,
        MaintenanceAction::Cleanup => OperationType::Cleanup,
    };
    let status = match result.status {
        MaintenanceStatus::Succeeded => OperationStatus::Succeeded,
        MaintenanceStatus::PartiallySucceeded => OperationStatus::PartiallyVerified,
        MaintenanceStatus::Failed | MaintenanceStatus::Cancelled | MaintenanceStatus::Blocked => {
            OperationStatus::Failed
        }
    };
    let verified = result.providers.iter().all(|provider| {
        matches!(
            provider.status,
            MaintenanceProviderStatus::Succeeded
                | MaintenanceProviderStatus::Skipped
                | MaintenanceProviderStatus::Blocked
        ) && provider.verification.is_none_or(|v| matches!(v, VerificationResult::Verified))
    }) && status == OperationStatus::Succeeded;
    let intent: String = match plan.action {
        MaintenanceAction::Refresh => "Refresh package metadata".into(),
        MaintenanceAction::Upgrade => "Update the system".into(),
        MaintenanceAction::Cleanup => "Remove unused packages".into(),
    };
    let upgraded = changes.iter().filter(|c| c.kind == ChangeKind::Upgraded).count();
    let summary = match (plan.action, status) {
        (MaintenanceAction::Upgrade, OperationStatus::Succeeded) if upgraded > 0 => {
            format!(
                "{upgraded} package{} upgraded and verified",
                if upgraded == 1 { "" } else { "s" }
            )
        }
        (MaintenanceAction::Upgrade, OperationStatus::Succeeded) => {
            "System already up to date".into()
        }
        (MaintenanceAction::Upgrade, OperationStatus::PartiallyVerified) => {
            format!(
                "Upgrade finished with {} package change{}; verification incomplete",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" }
            )
        }
        (MaintenanceAction::Refresh, OperationStatus::Succeeded) => {
            "Package metadata refreshed".into()
        }
        (MaintenanceAction::Cleanup, OperationStatus::Succeeded) => {
            format!(
                "{} unused package{} removed",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" }
            )
        }
        (_, OperationStatus::Failed) => diagnosis
            .as_ref()
            .map(|d| d.summary.clone())
            .or_else(|| result.providers.iter().find_map(|provider| provider.message.clone()))
            .unwrap_or_else(|| format!("{} failed", operation_type.failed_title())),
        _ => intent.clone(),
    };
    let verification =
        result.providers.iter().find_map(|provider| provider.verification).map(|v| {
            VerificationReport {
                result: v,
                verified,
                checks: vec![
                    "Re-checked package state after the operation".into(),
                    "Inspected reboot-required and dependency health when available".into(),
                ],
            }
        });
    OperationRecord {
        schema_version: SCHEMA_VERSION,
        id: result.operation_id.clone(),
        operation_type,
        intent,
        status,
        started_at_unix_ms,
        completed_at_unix_ms: Some(completed),
        duration_ms: Some(completed.saturating_sub(started_at_unix_ms)),
        source: plan.source,
        summary,
        changes,
        verification,
        warnings,
        diagnosis,
        raw_commands,
        message: result.providers.iter().find_map(|provider| provider.message.clone()),
    }
}

/// Running marker written before mutation.
pub fn running(
    id: impl Into<String>,
    operation_type: OperationType,
    intent: impl Into<String>,
    source: Option<PackageSource>,
) -> OperationRecord {
    OperationRecord {
        schema_version: SCHEMA_VERSION,
        id: id.into(),
        operation_type,
        intent: intent.into(),
        status: OperationStatus::Running,
        started_at_unix_ms: now_unix_ms(),
        completed_at_unix_ms: None,
        duration_ms: None,
        source,
        summary: "In progress".into(),
        changes: Vec::new(),
        verification: None,
        warnings: Vec::new(),
        diagnosis: None,
        raw_commands: Vec::new(),
        message: None,
    }
}

/// Filesystem-backed operation journal under XDG state.
pub struct OperationJournal {
    directory: PathBuf,
}

impl OperationJournal {
    /// `$XDG_STATE_HOME/orbis/operations` or `$HOME/.local/state/orbis/operations`.
    pub fn default_location() -> Result<Self, String> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && !path.as_os_str().is_empty())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local").join("state"))
            })
            .ok_or_else(|| "neither XDG_STATE_HOME nor HOME is available".to_owned())?;
        Ok(Self::at(base.join("orbis").join("operations")))
    }

    /// Explicit directory (tests).
    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into() }
    }

    /// Journal directory.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Atomically write one operation record.
    pub fn write(&self, record: &OperationRecord) -> Result<PathBuf, String> {
        validate_id(&record.id)?;
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let final_path = self.directory.join(format!("{}.json", record.id));
        let temporary_path = self.directory.join(format!(".{}.tmp", record.id));
        let body = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
        fs::write(&temporary_path, body).map_err(|error| error.to_string())?;
        fs::rename(&temporary_path, &final_path).map_err(|error| error.to_string())?;
        Ok(final_path)
    }

    /// Read one record by id.
    pub fn get(&self, id: &str) -> Result<Option<OperationRecord>, String> {
        validate_id(id)?;
        let path = self.directory.join(format!("{id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let body = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let record = serde_json::from_str(&body).map_err(|error| error.to_string())?;
        Ok(Some(record))
    }

    /// Recent operations, newest first. Skips corrupt files.
    pub fn recent(&self, limit: usize) -> Result<(Vec<OperationRecord>, usize), String> {
        if !self.directory.exists() {
            return Ok((Vec::new(), 0));
        }
        let mut records = Vec::new();
        let mut skipped = 0usize;
        for item in fs::read_dir(&self.directory).map_err(|error| error.to_string())? {
            let path = match item {
                Ok(item) => item.path(),
                Err(_) => continue,
            };
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(body) = fs::read_to_string(&path) else {
                skipped += 1;
                continue;
            };
            match serde_json::from_str::<OperationRecord>(&body) {
                Ok(record) => records.push(record),
                Err(_) => skipped += 1,
            }
        }
        records.sort_by(|left, right| {
            right
                .completed_at_unix_ms
                .or(Some(right.started_at_unix_ms))
                .cmp(&left.completed_at_unix_ms.or(Some(left.started_at_unix_ms)))
                .then_with(|| right.id.cmp(&left.id))
        });
        if records.len() > limit {
            records.truncate(limit);
        }
        Ok((records, skipped))
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 200
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.chars().any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err("invalid operation id".into());
    }
    Ok(())
}

/// Helper used by planning code when only an [`OperationPlan`] is known.
pub fn type_for_plan(plan: &OperationPlan) -> OperationType {
    match plan.action {
        OperationAction::Install => OperationType::Install,
        OperationAction::Remove => OperationType::Remove,
    }
}

/// Helper for maintenance action mapping.
pub fn type_for_maintenance(action: MaintenanceAction) -> OperationType {
    match action {
        MaintenanceAction::Refresh => OperationType::Refresh,
        MaintenanceAction::Upgrade => OperationType::SystemUpdate,
        MaintenanceAction::Cleanup => OperationType::Cleanup,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Package, PackageKind, PackageSource};
    use crate::transaction::PackageRefJson;
    use crate::transaction::{
        ExecutionSummary, InstallScope, OperationAction, PackageState, PlanCompleteness,
        PlanConfidence, PrivilegeRequirement, RiskLevel,
    };

    fn sample_plan(action: OperationAction) -> OperationPlan {
        OperationPlan {
            operation_id: "tx-test-1".into(),
            action,
            target: Package {
                source: PackageSource::Apt,
                name: "ripgrep".into(),
                provider_id: "ripgrep".into(),
                version: Some("14.1.0-1".into()),
                summary: None,
                description: None,
                installed: Some(action == OperationAction::Remove),
                kind: Some(PackageKind::Application),
                origin: None,
                architecture: None,
                homepage: None,
                license: None,
                size_bytes: None,
                metadata: Default::default(),
            },
            scope: InstallScope::System,
            current_state: if action == OperationAction::Install {
                PackageState::NotInstalled
            } else {
                PackageState::Installed
            },
            requested_state: if action == OperationAction::Install {
                PackageState::Installed
            } else {
                PackageState::NotInstalled
            },
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: true,
            risk: RiskLevel::Normal,
            changes: Vec::new(),
            warnings: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
        }
    }

    #[test]
    fn journal_round_trips_and_lists_newest_first() {
        let dir = std::env::temp_dir().join(format!(
            "orbis-journal-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        let journal = OperationJournal::at(&dir);
        let mut older =
            running("tx-older", OperationType::Install, "Install a", Some(PackageSource::Apt));
        older.started_at_unix_ms = 100;
        older.status = OperationStatus::Succeeded;
        older.completed_at_unix_ms = Some(150);
        older.summary = "a installed".into();
        journal.write(&older).expect("write older");

        let mut newer =
            running("tx-newer", OperationType::SystemUpdate, "Update", Some(PackageSource::Apt));
        newer.started_at_unix_ms = 200;
        newer.status = OperationStatus::Succeeded;
        newer.completed_at_unix_ms = Some(260);
        newer.summary = "updated".into();
        newer.changes = vec![PackageChange {
            package_id: "openssl".into(),
            name: Some("openssl".into()),
            kind: ChangeKind::Upgraded,
            from_version: Some("1".into()),
            to_version: Some("2".into()),
        }];
        journal.write(&newer).expect("write newer");

        let (recent, skipped) = journal.recent(10).expect("list");
        assert_eq!(skipped, 0);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "tx-newer");
        assert_eq!(recent[0].activity_title(), "System updated");
        assert!(recent[0].activity_details().iter().any(|line| line.contains("openssl")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn transaction_outcome_never_marks_failed_as_verified() {
        let plan = sample_plan(OperationAction::Install);
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "ripgrep".into() },
            scope: Some(InstallScope::System),
            channel: None,
        };
        let result = TransactionResult {
            plan,
            execution: ExecutionSummary {
                exit_status: Some(100),
                process_succeeded: false,
                message: Some("Could not get lock".into()),
            },
            verification: VerificationResult::Failed,
            status: TransactionStatus::Failed,
            changes: Vec::new(),
            warnings: Vec::new(),
            diagnosis: None,
            raw_commands: Vec::new(),
        };
        let record = from_transaction(
            &request,
            &result,
            1,
            Vec::new(),
            Vec::new(),
            Some(FailureDiagnosis {
                cause: FailureCause::AptLock,
                summary: "Another package manager holds the APT lock.".into(),
                hint: Some("Wait for the other process to finish, then retry.".into()),
            }),
            Vec::new(),
        );
        assert_eq!(record.status, OperationStatus::Failed);
        assert_eq!(record.activity_title(), "Install failed");
        assert!(!record.verification.as_ref().unwrap().verified);
        assert_eq!(record.diagnosis.as_ref().unwrap().cause, FailureCause::AptLock);
    }

    #[test]
    fn install_activity_names_the_package() {
        let plan = sample_plan(OperationAction::Install);
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "ripgrep".into() },
            scope: Some(InstallScope::System),
            channel: None,
        };
        let result = TransactionResult {
            plan,
            execution: ExecutionSummary {
                exit_status: Some(0),
                process_succeeded: true,
                message: None,
            },
            verification: VerificationResult::Verified,
            status: TransactionStatus::Succeeded,
            changes: Vec::new(),
            warnings: Vec::new(),
            diagnosis: None,
            raw_commands: Vec::new(),
        };
        let record = from_transaction(
            &request,
            &result,
            10,
            vec![PackageChange {
                package_id: "ripgrep".into(),
                name: Some("ripgrep".into()),
                kind: ChangeKind::Installed,
                from_version: None,
                to_version: Some("14.1.0-1".into()),
            }],
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.activity_title(), "ripgrep installed");
        assert!(record.summary.contains("14.1.0-1"));
        assert!(record.verification.unwrap().verified);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let journal = OperationJournal::at("/tmp/orbis-journal-invalid");
        let mut record = running("../etc", OperationType::Install, "bad", None);
        record.id = "../etc".into();
        assert!(journal.write(&record).is_err());
    }
}
