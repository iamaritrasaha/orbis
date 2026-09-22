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
    MaintenanceAction, MaintenancePlan, MaintenanceProviderResult, MaintenanceProviderStatus,
    MaintenanceResult, MaintenanceStatus,
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

/// Start and completion timestamps for a journaled operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationTiming {
    /// When the mutating work began (unix ms).
    pub started_at_unix_ms: u64,
    /// When observation finished (unix ms).
    pub completed_at_unix_ms: u64,
}

impl OperationTiming {
    /// Duration in milliseconds.
    pub fn duration_ms(self) -> u64 {
        self.completed_at_unix_ms.saturating_sub(self.started_at_unix_ms)
    }
}

/// Unix epoch milliseconds. Shared so start and finish records use one clock.
pub fn unix_now_ms() -> u64 {
    now_unix_ms()
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Builds an operation record from a completed single-package transaction.
pub fn from_transaction(
    request: &OperationRequest,
    result: &TransactionResult,
    timing: OperationTiming,
    changes: Vec<PackageChange>,
    warnings: Vec<String>,
    diagnosis: Option<FailureDiagnosis>,
    raw_commands: Vec<RawCommandRef>,
) -> OperationRecord {
    let completed = timing.completed_at_unix_ms;
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
    OperationRecord {
        schema_version: SCHEMA_VERSION,
        id: result.plan.operation_id.clone(),
        operation_type,
        intent,
        status,
        started_at_unix_ms: timing.started_at_unix_ms,
        completed_at_unix_ms: Some(completed),
        duration_ms: Some(timing.duration_ms()),
        source: Some(result.plan.target.source),
        summary,
        changes,
        verification: Some(VerificationReport {
            result: result.verification,
            verified,
            checks: result.checks.clone(),
        }),
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
    timing: OperationTiming,
    changes: Vec<PackageChange>,
    warnings: Vec<String>,
    diagnosis: Option<FailureDiagnosis>,
    raw_commands: Vec<RawCommandRef>,
) -> OperationRecord {
    let completed = timing.completed_at_unix_ms;
    let operation_type = match plan.action {
        MaintenanceAction::Refresh => OperationType::Refresh,
        MaintenanceAction::Upgrade => OperationType::SystemUpdate,
        MaintenanceAction::Cleanup => OperationType::Cleanup,
    };
    let mut status = match result.status {
        MaintenanceStatus::Succeeded => OperationStatus::Succeeded,
        MaintenanceStatus::PartiallySucceeded => OperationStatus::PartiallyVerified,
        MaintenanceStatus::Failed | MaintenanceStatus::Cancelled | MaintenanceStatus::Blocked => {
            OperationStatus::Failed
        }
    };
    let upgraded = changes.iter().filter(|c| c.kind == ChangeKind::Upgraded).count();
    let planned_candidates: usize =
        plan.providers.iter().map(|provider| provider.candidates.len()).sum();
    let observation_incomplete = result.providers.iter().any(|provider| {
        provider.checks.iter().any(|check| {
            check.contains("observation unavailable")
                || check.contains("verification incomplete")
                || check.contains("Package-level verification incomplete")
        }) || matches!(provider.verification, Some(VerificationResult::PartiallyVerified))
            && provider.checks.iter().any(|check| check.contains("unavailable"))
    });
    let upgrade_outcome_downgraded = plan.action == MaintenanceAction::Upgrade
        && planned_candidates > 0
        && (observation_incomplete || upgraded == 0);
    if upgrade_outcome_downgraded && status == OperationStatus::Succeeded {
        status = OperationStatus::PartiallyVerified;
    }
    // Aggregate verification over executed providers only. Skipped/blocked
    // providers were never executed, so they contribute neither evidence nor
    // doubt; a coverage limitation must not read as failed verification.
    let verification_result = match aggregate_provider_verification(&result.providers) {
        Some(VerificationResult::Verified) if upgrade_outcome_downgraded => {
            Some(VerificationResult::PartiallyVerified)
        }
        other => other,
    };
    let verified = matches!(verification_result, Some(VerificationResult::Verified));
    let executed_failed = result
        .providers
        .iter()
        .any(|provider| provider.status == MaintenanceProviderStatus::Failed);
    let coverage_incomplete = result.providers.iter().any(|provider| {
        matches!(
            provider.status,
            MaintenanceProviderStatus::Skipped | MaintenanceProviderStatus::Blocked
        )
    });
    let intent: String = match plan.action {
        MaintenanceAction::Refresh => "Refresh package metadata".into(),
        MaintenanceAction::Upgrade => "Update the system".into(),
        MaintenanceAction::Cleanup => "Remove unused packages".into(),
    };
    let summary = match (plan.action, status) {
        (MaintenanceAction::Upgrade, OperationStatus::Succeeded) if planned_candidates == 0 => {
            "System already up to date".into()
        }
        (MaintenanceAction::Upgrade, OperationStatus::Succeeded) if upgraded > 0 => {
            format!(
                "{upgraded} package{} upgraded and verified",
                if upgraded == 1 { "" } else { "s" }
            )
        }
        (MaintenanceAction::Upgrade, OperationStatus::PartiallyVerified)
            if planned_candidates > 0 =>
        {
            if executed_failed {
                "System update partially failed".into()
            } else if matches!(verification_result, Some(VerificationResult::PartiallyVerified)) {
                "System update completed; package-level verification incomplete".into()
            } else {
                // Executed providers verified; the partial status is a coverage limitation.
                format!(
                    "{upgraded} package{} upgraded and verified; some providers were not covered",
                    if upgraded == 1 { "" } else { "s" }
                )
            }
        }
        (MaintenanceAction::Upgrade, OperationStatus::PartiallyVerified) if executed_failed => {
            "System update partially failed".into()
        }
        (MaintenanceAction::Upgrade, OperationStatus::PartiallyVerified) if coverage_incomplete => {
            "System already up to date; some providers were not covered".into()
        }
        (MaintenanceAction::Upgrade, OperationStatus::Succeeded) => {
            "System already up to date".into()
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
    let checks: Vec<String> =
        result.providers.iter().flat_map(|provider| provider.checks.iter().cloned()).collect();
    let verification = verification_result.map(|aggregate| VerificationReport {
        result: aggregate,
        verified,
        checks,
    });
    OperationRecord {
        schema_version: SCHEMA_VERSION,
        id: result.operation_id.clone(),
        operation_type,
        intent,
        status,
        started_at_unix_ms: timing.started_at_unix_ms,
        completed_at_unix_ms: Some(completed),
        duration_ms: Some(timing.duration_ms()),
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

/// Aggregates per-provider verification into one report result.
///
/// Only executed providers (succeeded, partially succeeded, or failed) count.
/// Skipped/blocked providers were not executed and can neither verify nor
/// invalidate anything. Failure dominates, then partial verification; executed
/// providers without verification evidence keep the aggregate incomplete.
fn aggregate_provider_verification(
    providers: &[MaintenanceProviderResult],
) -> Option<VerificationResult> {
    let executed: Vec<&MaintenanceProviderResult> = providers
        .iter()
        .filter(|provider| {
            matches!(
                provider.status,
                MaintenanceProviderStatus::Succeeded
                    | MaintenanceProviderStatus::PartiallySucceeded
                    | MaintenanceProviderStatus::Failed
            )
        })
        .collect();
    if executed.is_empty() {
        return None;
    }
    if executed.iter().any(|provider| {
        provider.status == MaintenanceProviderStatus::Failed
            || provider.verification == Some(VerificationResult::Failed)
    }) {
        return Some(VerificationResult::Failed);
    }
    if executed
        .iter()
        .any(|provider| provider.verification == Some(VerificationResult::PartiallyVerified))
    {
        return Some(VerificationResult::PartiallyVerified);
    }
    if executed.iter().all(|provider| provider.verification == Some(VerificationResult::Verified)) {
        return Some(VerificationResult::Verified);
    }
    Some(VerificationResult::PartiallyVerified)
}

/// Running marker written before mutation.
pub fn running(
    id: impl Into<String>,
    operation_type: OperationType,
    intent: impl Into<String>,
    source: Option<PackageSource>,
) -> OperationRecord {
    running_at(id, operation_type, intent, source, now_unix_ms())
}

/// Running marker with an explicit start timestamp that later records must reuse.
pub fn running_at(
    id: impl Into<String>,
    operation_type: OperationType,
    intent: impl Into<String>,
    source: Option<PackageSource>,
    started_at_unix_ms: u64,
) -> OperationRecord {
    OperationRecord {
        schema_version: SCHEMA_VERSION,
        id: id.into(),
        operation_type,
        intent: intent.into(),
        status: OperationStatus::Running,
        started_at_unix_ms,
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

/// Fail-closed write of a Running journal record. Mutation must not begin if this fails.
pub fn begin_running(
    journal: &OperationJournal,
    id: impl Into<String>,
    operation_type: OperationType,
    intent: impl Into<String>,
    source: Option<PackageSource>,
    started_at_unix_ms: u64,
) -> Result<u64, String> {
    let id = id.into();
    journal.write(&running_at(id, operation_type, intent, source, started_at_unix_ms))?;
    Ok(started_at_unix_ms)
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
            checks: Vec::new(),
        };
        let record = from_transaction(
            &request,
            &result,
            OperationTiming { started_at_unix_ms: 1, completed_at_unix_ms: 2 },
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
            checks: vec!["dpkg package state observed".into()],
        };
        let record = from_transaction(
            &request,
            &result,
            OperationTiming { started_at_unix_ms: 10, completed_at_unix_ms: 20 },
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

    fn apt_upgrade_provider_plan(
        candidates: Vec<crate::maintenance::UpdateCandidate>,
    ) -> crate::maintenance::ProviderMaintenancePlan {
        crate::maintenance::ProviderMaintenancePlan {
            operation_id: "mnt-test-1".into(),
            source: PackageSource::Apt,
            action: crate::maintenance::MaintenanceAction::Upgrade,
            scope: Some(InstallScope::System),
            candidates,
            cleanup_candidates: Vec::new(),
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: true,
            risk: RiskLevel::Normal,
            supported: true,
            mutates: true,
            warnings: Vec::new(),
            notes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
        }
    }

    fn openssl_candidate() -> crate::maintenance::UpdateCandidate {
        crate::maintenance::UpdateCandidate {
            source: PackageSource::Apt,
            provider_id: "openssl".into(),
            name: "openssl".into(),
            current_version: Some("1.0".into()),
            available_version: Some("3.0".into()),
            architecture: None,
            scope: Some(InstallScope::System),
            channel: None,
            held: Some(false),
            security_relevance: None,
            notes: Vec::new(),
            metadata: Default::default(),
        }
    }

    fn maintenance_result(
        status: crate::maintenance::MaintenanceStatus,
        provider_status: crate::maintenance::MaintenanceProviderStatus,
        verification: Option<VerificationResult>,
        checks: Vec<String>,
        source: PackageSource,
    ) -> crate::maintenance::MaintenanceResult {
        crate::maintenance::MaintenanceResult {
            operation_id: "mnt-test-1".into(),
            action: crate::maintenance::MaintenanceAction::Upgrade,
            status,
            providers: vec![crate::maintenance::MaintenanceProviderResult {
                source,
                action: crate::maintenance::MaintenanceAction::Upgrade,
                status: provider_status,
                candidate_count: 1,
                verification,
                message: None,
                changes: Vec::new(),
                warnings: Vec::new(),
                diagnosis: None,
                raw_commands: Vec::new(),
                checks,
            }],
        }
    }

    #[test]
    fn preexisting_zero_candidate_plan_is_already_up_to_date() {
        let provider = apt_upgrade_provider_plan(Vec::new());
        let mut plan = crate::maintenance::MaintenancePlan::new(
            crate::maintenance::MaintenanceAction::Upgrade,
            Some(PackageSource::Apt),
            vec![provider],
        );
        plan.operation_id = "mnt-test-1".into();
        let result = maintenance_result(
            crate::maintenance::MaintenanceStatus::Succeeded,
            crate::maintenance::MaintenanceProviderStatus::Succeeded,
            Some(VerificationResult::Verified),
            vec!["no remaining APT upgrades".into()],
            PackageSource::Apt,
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 1000, completed_at_unix_ms: 1500 },
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.summary, "System already up to date");
        assert_eq!(record.status, OperationStatus::Succeeded);
        assert_eq!(record.duration_ms, Some(500));
        assert_eq!(record.started_at_unix_ms, 1000);
    }

    #[test]
    fn candidate_bearing_upgrade_with_unknown_changes_is_not_already_up_to_date() {
        let provider = apt_upgrade_provider_plan(vec![openssl_candidate()]);
        let mut plan = crate::maintenance::MaintenancePlan::new(
            crate::maintenance::MaintenanceAction::Upgrade,
            Some(PackageSource::Apt),
            vec![provider],
        );
        plan.operation_id = "mnt-test-1".into();
        let result = maintenance_result(
            crate::maintenance::MaintenanceStatus::Succeeded,
            crate::maintenance::MaintenanceProviderStatus::Succeeded,
            Some(VerificationResult::Verified),
            vec!["planned candidate version observation unavailable (dpkg-query failed)".into()],
            PackageSource::Apt,
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 1000, completed_at_unix_ms: 1400 },
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_ne!(record.summary, "System already up to date");
        assert!(record.summary.contains("package-level verification incomplete"));
        assert_eq!(record.status, OperationStatus::PartiallyVerified);
        assert_eq!(record.duration_ms, Some(400));
    }

    #[test]
    fn running_journal_is_written_before_executor_and_interrupted_stays_running() {
        let dir = std::env::temp_dir().join(format!(
            "orbis-journal-running-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        let journal = OperationJournal::at(&dir);
        let started = begin_running(
            &journal,
            "mnt-run-1",
            OperationType::SystemUpdate,
            "Update the system",
            Some(PackageSource::Apt),
            42,
        )
        .expect("running write");
        assert_eq!(started, 42);
        let recorded = journal.get("mnt-run-1").expect("read").expect("present");
        assert_eq!(recorded.status, OperationStatus::Running);
        assert_eq!(recorded.started_at_unix_ms, 42);
        assert!(recorded.completed_at_unix_ms.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duration_uses_actual_operation_start() {
        let provider = apt_upgrade_provider_plan(Vec::new());
        let mut plan = crate::maintenance::MaintenancePlan::new(
            crate::maintenance::MaintenanceAction::Upgrade,
            Some(PackageSource::Apt),
            vec![provider],
        );
        plan.operation_id = "mnt-test-1".into();
        let result = maintenance_result(
            crate::maintenance::MaintenanceStatus::Succeeded,
            crate::maintenance::MaintenanceProviderStatus::Succeeded,
            Some(VerificationResult::Verified),
            Vec::new(),
            PackageSource::Apt,
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 10_000, completed_at_unix_ms: 12_250 },
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.started_at_unix_ms, 10_000);
        assert_eq!(record.completed_at_unix_ms, Some(12_250));
        assert_eq!(record.duration_ms, Some(2_250));
    }

    fn provider_result(
        source: PackageSource,
        status: crate::maintenance::MaintenanceProviderStatus,
        verification: Option<VerificationResult>,
        checks: Vec<String>,
    ) -> crate::maintenance::MaintenanceProviderResult {
        crate::maintenance::MaintenanceProviderResult {
            source,
            action: crate::maintenance::MaintenanceAction::Upgrade,
            status,
            candidate_count: 0,
            verification,
            message: None,
            changes: Vec::new(),
            warnings: Vec::new(),
            diagnosis: None,
            raw_commands: Vec::new(),
            checks,
        }
    }

    fn upgrade_result(
        status: crate::maintenance::MaintenanceStatus,
        providers: Vec<crate::maintenance::MaintenanceProviderResult>,
    ) -> crate::maintenance::MaintenanceResult {
        crate::maintenance::MaintenanceResult {
            operation_id: "mnt-test-1".into(),
            action: crate::maintenance::MaintenanceAction::Upgrade,
            status,
            providers,
        }
    }

    /// APT plan with one candidate plus a blocked-by-design optional provider.
    fn apt_plus_optional_plan(
        optional_source: PackageSource,
    ) -> crate::maintenance::MaintenancePlan {
        let apt = apt_upgrade_provider_plan(vec![openssl_candidate()]);
        let mut optional = apt.clone();
        optional.source = optional_source;
        optional.candidates = Vec::new();
        let mut plan = crate::maintenance::MaintenancePlan::new(
            crate::maintenance::MaintenanceAction::Upgrade,
            Some(PackageSource::Apt),
            vec![apt, optional],
        );
        plan.operation_id = "mnt-test-1".into();
        plan
    }

    fn upgraded_change() -> PackageChange {
        PackageChange {
            package_id: "openssl".into(),
            name: Some("openssl".into()),
            kind: ChangeKind::Upgraded,
            from_version: Some("1.0".into()),
            to_version: Some("3.0".into()),
        }
    }

    fn report_of(record: &OperationRecord) -> crate::facts::VerificationReport {
        record.verification.clone().expect("verification report present")
    }

    #[test]
    fn apt_verified_with_blocked_provider_reports_coverage_not_verification_gap() {
        let plan = apt_plus_optional_plan(PackageSource::Cargo);
        let result = upgrade_result(
            crate::maintenance::MaintenanceStatus::PartiallySucceeded,
            vec![
                provider_result(
                    PackageSource::Apt,
                    crate::maintenance::MaintenanceProviderStatus::Succeeded,
                    Some(VerificationResult::Verified),
                    vec!["dpkg package state observed".into(), "no remaining APT upgrades".into()],
                ),
                provider_result(
                    PackageSource::Cargo,
                    crate::maintenance::MaintenanceProviderStatus::Skipped,
                    None,
                    Vec::new(),
                ),
            ],
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 1000, completed_at_unix_ms: 1100 },
            vec![upgraded_change()],
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.status, OperationStatus::PartiallyVerified);
        let report = report_of(&record);
        assert_eq!(report.result, VerificationResult::Verified);
        assert!(report.verified);
        assert_eq!(
            record.summary,
            "1 package upgraded and verified; some providers were not covered"
        );
        // Internal consistency: result and verified never contradict each other.
        assert!(!(report.result == VerificationResult::Verified && !report.verified));
    }

    #[test]
    fn apt_partially_verified_reports_package_level_incompleteness() {
        let plan = apt_plus_optional_plan(PackageSource::Cargo);
        let result = upgrade_result(
            crate::maintenance::MaintenanceStatus::PartiallySucceeded,
            vec![
                provider_result(
                    PackageSource::Apt,
                    crate::maintenance::MaintenanceProviderStatus::PartiallySucceeded,
                    Some(VerificationResult::PartiallyVerified),
                    vec![
                        "planned candidate version observation unavailable (dpkg-query failed)"
                            .into(),
                    ],
                ),
                provider_result(
                    PackageSource::Cargo,
                    crate::maintenance::MaintenanceProviderStatus::Skipped,
                    None,
                    Vec::new(),
                ),
            ],
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 1000, completed_at_unix_ms: 1100 },
            vec![upgraded_change()],
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.status, OperationStatus::PartiallyVerified);
        let report = report_of(&record);
        assert_eq!(report.result, VerificationResult::PartiallyVerified);
        assert!(!report.verified);
        assert_eq!(
            record.summary,
            "System update completed; package-level verification incomplete"
        );
    }

    #[test]
    fn failed_provider_makes_aggregate_verification_failed() {
        let plan = apt_plus_optional_plan(PackageSource::Snap);
        let result = upgrade_result(
            crate::maintenance::MaintenanceStatus::PartiallySucceeded,
            vec![
                provider_result(
                    PackageSource::Apt,
                    crate::maintenance::MaintenanceProviderStatus::Succeeded,
                    Some(VerificationResult::Verified),
                    vec!["no remaining APT upgrades".into()],
                ),
                provider_result(
                    PackageSource::Snap,
                    crate::maintenance::MaintenanceProviderStatus::Failed,
                    None,
                    Vec::new(),
                ),
            ],
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 1000, completed_at_unix_ms: 1100 },
            vec![upgraded_change()],
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.status, OperationStatus::PartiallyVerified);
        let report = report_of(&record);
        assert_eq!(report.result, VerificationResult::Failed);
        assert!(!report.verified);
        assert_eq!(record.summary, "System update partially failed");
    }

    #[test]
    fn single_source_apt_verified_upgrade_stays_fully_verified() {
        let provider = apt_upgrade_provider_plan(vec![openssl_candidate()]);
        let mut plan = crate::maintenance::MaintenancePlan::new(
            crate::maintenance::MaintenanceAction::Upgrade,
            Some(PackageSource::Apt),
            vec![provider],
        );
        plan.operation_id = "mnt-test-1".into();
        let result = maintenance_result(
            crate::maintenance::MaintenanceStatus::Succeeded,
            crate::maintenance::MaintenanceProviderStatus::Succeeded,
            Some(VerificationResult::Verified),
            vec!["dpkg package state observed".into(), "no remaining APT upgrades".into()],
            PackageSource::Apt,
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 1000, completed_at_unix_ms: 1100 },
            vec![upgraded_change()],
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.status, OperationStatus::Succeeded);
        let report = report_of(&record);
        assert_eq!(report.result, VerificationResult::Verified);
        assert!(report.verified);
        assert_eq!(record.summary, "1 package upgraded and verified");
    }

    #[test]
    fn zero_candidate_upgrade_with_skipped_provider_keeps_coverage_wording() {
        let apt = apt_upgrade_provider_plan(Vec::new());
        let mut cargo = apt.clone();
        cargo.source = PackageSource::Cargo;
        let mut plan = crate::maintenance::MaintenancePlan::new(
            crate::maintenance::MaintenanceAction::Upgrade,
            Some(PackageSource::Apt),
            vec![apt, cargo],
        );
        plan.operation_id = "mnt-test-1".into();
        let result = upgrade_result(
            crate::maintenance::MaintenanceStatus::PartiallySucceeded,
            vec![
                provider_result(
                    PackageSource::Apt,
                    crate::maintenance::MaintenanceProviderStatus::Succeeded,
                    Some(VerificationResult::Verified),
                    Vec::new(),
                ),
                provider_result(
                    PackageSource::Cargo,
                    crate::maintenance::MaintenanceProviderStatus::Skipped,
                    None,
                    Vec::new(),
                ),
            ],
        );
        let record = from_maintenance(
            &plan,
            &result,
            OperationTiming { started_at_unix_ms: 1000, completed_at_unix_ms: 1100 },
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        );
        assert_eq!(record.status, OperationStatus::PartiallyVerified);
        let report = report_of(&record);
        assert_eq!(report.result, VerificationResult::Verified);
        assert!(report.verified);
        assert_eq!(record.summary, "System already up to date; some providers were not covered");
    }

    #[test]
    fn non_apt_verification_text_never_claims_dpkg() {
        let plan = sample_plan(OperationAction::Install);
        let mut npm_plan = plan.clone();
        npm_plan.target.source = PackageSource::Npm;
        npm_plan.target.name = "left-pad".into();
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Npm), query: "left-pad".into() },
            scope: Some(InstallScope::User),
            channel: None,
        };
        let result = TransactionResult {
            plan: npm_plan,
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
            checks: vec!["Provider installed-state verification".into()],
        };
        let record = from_transaction(
            &request,
            &result,
            OperationTiming { started_at_unix_ms: 1, completed_at_unix_ms: 2 },
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        );
        let checks = record.verification.unwrap().checks.join(" ");
        assert!(!checks.to_ascii_lowercase().contains("dpkg"));

        for source in [PackageSource::Snap, PackageSource::Flatpak, PackageSource::Npm] {
            let provider = apt_upgrade_provider_plan(Vec::new());
            let mut snap_plan = crate::maintenance::MaintenancePlan::new(
                crate::maintenance::MaintenanceAction::Upgrade,
                Some(source),
                vec![provider],
            );
            snap_plan.operation_id = "mnt-test-1".into();
            snap_plan.source = Some(source);
            let result = maintenance_result(
                crate::maintenance::MaintenanceStatus::Succeeded,
                crate::maintenance::MaintenanceProviderStatus::Succeeded,
                Some(VerificationResult::Verified),
                vec!["Provider maintenance verification".into()],
                source,
            );
            let record = from_maintenance(
                &snap_plan,
                &result,
                OperationTiming { started_at_unix_ms: 1, completed_at_unix_ms: 2 },
                Vec::new(),
                Vec::new(),
                None,
                Vec::new(),
            );
            let text = record
                .verification
                .as_ref()
                .map(|report| report.checks.join(" "))
                .unwrap_or_default();
            assert!(!text.to_ascii_lowercase().contains("dpkg"), "{source:?}");
        }
    }
}
