//! Durable, sanitized transaction records under the XDG state directory.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    OperationPlan, OperationRequest, TransactionResult, TransactionStatus, VerificationResult,
};
use crate::maintenance::{MaintenancePlan, MaintenanceResult};
use crate::models::PackageSource;

/// Current on-disk transaction record schema.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Durable lifecycle state for one operation ID.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionLifecycle {
    /// A plan exists but execution has not crossed the mutation boundary.
    #[default]
    Planned,
    /// The pre-execution record was written and the provider operation was invoked.
    Executing,
    /// Execution completed and the requested state was verified.
    Succeeded,
    /// Execution completed but verification was incomplete.
    PartiallyVerified,
    /// Execution or verification failed.
    Failed,
}

/// A persisted record for an attempted operation, including failed attempts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionRecord {
    /// On-disk schema version.
    #[serde(default = "legacy_schema_version")]
    pub schema_version: u32,
    /// Unix timestamp in milliseconds when the record was first written.
    pub recorded_at_unix_ms: u64,
    /// Unix timestamp in milliseconds when the record was last replaced.
    #[serde(default)]
    pub updated_at_unix_ms: u64,
    /// The request before provider resolution.
    pub request: OperationRequest,
    /// Current transaction lifecycle.
    #[serde(default)]
    pub lifecycle: TransactionLifecycle,
    /// The resolved plan, retained even if the process stops during execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<OperationPlan>,
    /// Final execution result, absent while lifecycle is executing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TransactionResult>,
}

/// A durable record for one coordinated maintenance run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceRecord {
    /// On-disk schema version.
    #[serde(default = "legacy_schema_version")]
    pub schema_version: u32,
    /// Unix timestamp in milliseconds when the record was first written.
    pub recorded_at_unix_ms: u64,
    /// Unix timestamp in milliseconds when the record was last replaced.
    #[serde(default)]
    pub updated_at_unix_ms: u64,
    /// Coordinated plan shown to the user.
    pub plan: MaintenancePlan,
    /// Final result, absent while the run is executing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<MaintenanceResult>,
}

impl MaintenanceRecord {
    /// Creates the pre-execution record.
    pub fn execution_started(plan: MaintenancePlan) -> Self {
        Self::with_state(plan, None)
    }

    /// Creates the final record.
    pub fn completed(plan: MaintenancePlan, result: MaintenanceResult) -> Self {
        let mut sanitized = result;
        for provider in &mut sanitized.providers {
            provider.message = provider.message.take().map(sanitize_message);
        }
        Self::with_state(plan, Some(sanitized))
    }

    fn with_state(plan: MaintenancePlan, result: Option<MaintenanceResult>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64);
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            recorded_at_unix_ms: now,
            updated_at_unix_ms: now,
            plan,
            result,
        }
    }
}

/// A compact, provider-neutral history row suitable for terminal and JSON output.
#[derive(Clone, Debug, Serialize)]
pub struct HistoryEntry {
    /// transaction or maintenance.
    pub kind: String,
    /// Stable operation or maintenance run ID.
    pub operation_id: String,
    /// Creation timestamp in Unix milliseconds.
    pub recorded_at_unix_ms: u64,
    /// Human-readable action.
    pub action: String,
    /// Provider, when one provider owns the row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PackageSource>,
    /// Package name for a legacy/single-package operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Flatpak or system scope where known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Lifecycle or final maintenance status.
    pub status: String,
    /// Verification state for single-package records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationResult>,
    /// Highest planned risk, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<crate::transaction::RiskLevel>,
    /// Sanitized failure or limitation summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl TransactionRecord {
    /// Creates a planned record without writing it to disk.
    pub fn planned(request: OperationRequest, plan: OperationPlan) -> Self {
        Self::with_state(request, TransactionLifecycle::Planned, Some(plan), None)
    }

    /// Creates the durable record written immediately before provider execution.
    pub fn execution_started(request: OperationRequest, plan: OperationPlan) -> Self {
        Self::with_state(request, TransactionLifecycle::Executing, Some(plan), None)
    }

    /// Creates the final record and removes process output from the persisted payload.
    pub fn completed(request: OperationRequest, result: TransactionResult) -> Self {
        let lifecycle = match result.status {
            TransactionStatus::Succeeded => TransactionLifecycle::Succeeded,
            TransactionStatus::PartiallyVerified => TransactionLifecycle::PartiallyVerified,
            TransactionStatus::Failed => TransactionLifecycle::Failed,
        };
        let mut sanitized = result;
        sanitized.execution.message = None;
        Self::with_state(request, lifecycle, Some(sanitized.plan.clone()), Some(sanitized))
    }

    /// Compatibility constructor for callers that previously wrote only a finished result.
    pub fn new(request: OperationRequest, result: TransactionResult) -> Self {
        Self::completed(request, result)
    }

    fn with_state(
        request: OperationRequest,
        lifecycle: TransactionLifecycle,
        plan: Option<OperationPlan>,
        result: Option<TransactionResult>,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64);
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            recorded_at_unix_ms: now,
            updated_at_unix_ms: now,
            request,
            lifecycle,
            plan,
            result,
        }
    }

    /// Returns the plan for both current records and legacy Milestone 2 records.
    pub fn resolved_plan(&self) -> Option<&OperationPlan> {
        self.plan.as_ref().or_else(|| self.result.as_ref().map(|result| &result.plan))
    }

    /// Returns the lifecycle for current records and infers a final state for legacy records.
    pub fn effective_lifecycle(&self) -> TransactionLifecycle {
        if self.schema_version < CURRENT_SCHEMA_VERSION
            && self.lifecycle == TransactionLifecycle::Planned
        {
            return self.result.as_ref().map_or(
                TransactionLifecycle::Planned,
                |result| match result.status {
                    TransactionStatus::Succeeded => TransactionLifecycle::Succeeded,
                    TransactionStatus::PartiallyVerified => TransactionLifecycle::PartiallyVerified,
                    TransactionStatus::Failed => TransactionLifecycle::Failed,
                },
            );
        }
        self.lifecycle
    }
}

fn legacy_schema_version() -> u32 {
    1
}

/// Filesystem-backed transaction history writer.
pub struct HistoryStore {
    directory: PathBuf,
}

impl HistoryStore {
    /// Uses `$XDG_STATE_HOME/orbis/transactions`, falling back to `$HOME/.local/state/orbis`.
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
        Ok(Self::at(base.join("orbis").join("transactions")))
    }

    /// Creates a store at an explicit directory, useful for tests and controlled deployments.
    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into() }
    }

    /// Returns the record directory.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns the stable path used for one operation ID.
    pub fn record_path(&self, operation_id: &str) -> PathBuf {
        if is_safe_operation_id(operation_id) {
            self.directory.join(format!("{operation_id}.json"))
        } else {
            self.directory.join(".invalid-operation-id.json")
        }
    }

    /// Atomically writes one JSON record without command output or credentials.
    pub fn write(&self, record: &TransactionRecord, operation_id: &str) -> Result<PathBuf, String> {
        validate_operation_id(operation_id)?;
        self.write_json(record, operation_id)
    }

    /// Atomically writes a coordinated maintenance record.
    pub fn write_maintenance(
        &self,
        record: &MaintenanceRecord,
        operation_id: &str,
    ) -> Result<PathBuf, String> {
        validate_operation_id(operation_id)?;
        self.write_json(record, operation_id)
    }

    fn write_json<T: Serialize>(&self, record: &T, operation_id: &str) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let final_path = self.record_path(operation_id);
        let temporary_path = self.directory.join(format!(".{operation_id}.tmp"));
        let body = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
        fs::write(&temporary_path, body).map_err(|error| error.to_string())?;
        fs::rename(&temporary_path, &final_path).map_err(|error| error.to_string())?;
        Ok(final_path)
    }

    /// Reads and summarizes all current and legacy records, newest first.
    pub fn entries(&self) -> Result<Vec<HistoryEntry>, String> {
        if !self.directory.exists() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.directory).map_err(|error| error.to_string())? {
            let path = item.map_err(|error| error.to_string())?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let body = fs::read_to_string(&path).map_err(|error| error.to_string())?;
            if let Ok(record) = serde_json::from_str::<MaintenanceRecord>(&body)
                && record.plan.operation_id.starts_with("maint-")
            {
                let status = record.result.as_ref().map_or_else(
                    || "executing".into(),
                    |result| format!("{:?}", result.status).to_ascii_lowercase(),
                );
                entries.push(HistoryEntry {
                    kind: "maintenance".into(),
                    operation_id: record.plan.operation_id.clone(),
                    recorded_at_unix_ms: record.recorded_at_unix_ms,
                    action: record.plan.action.label().into(),
                    source: record.plan.source,
                    package: None,
                    scope: None,
                    status,
                    verification: None,
                    risk: Some(record.plan.risk),
                    message: record.result.as_ref().and_then(|result| {
                        result.providers.iter().find_map(|provider| provider.message.clone())
                    }),
                });
                continue;
            }
            if let Ok(record) = serde_json::from_str::<TransactionRecord>(&body) {
                let Some(plan) = record.resolved_plan() else { continue };
                let operation_id = plan.operation_id.clone();
                let (status, verification, message) = match &record.result {
                    Some(result) => (
                        format!("{:?}", record.effective_lifecycle()).to_ascii_lowercase(),
                        Some(result.verification),
                        result.execution.message.clone(),
                    ),
                    None => (
                        format!("{:?}", record.effective_lifecycle()).to_ascii_lowercase(),
                        None,
                        None,
                    ),
                };
                entries.push(HistoryEntry {
                    kind: "transaction".into(),
                    operation_id,
                    recorded_at_unix_ms: record.recorded_at_unix_ms,
                    action: plan.action.label().into(),
                    source: Some(plan.target.source),
                    package: Some(plan.target.name.clone()),
                    scope: Some(plan.scope.label().into()),
                    status,
                    verification,
                    risk: Some(plan.risk),
                    message,
                });
            }
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.recorded_at_unix_ms));
        Ok(entries)
    }

    /// Reads a single sanitized record by its operation ID.
    pub fn entry(&self, operation_id: &str) -> Result<Option<serde_json::Value>, String> {
        validate_operation_id(operation_id)?;
        let path = self.record_path(operation_id);
        if !path.is_file() {
            return Ok(None);
        }
        let body = fs::read_to_string(path).map_err(|error| error.to_string())?;
        serde_json::from_str(&body).map(Some).map_err(|error| error.to_string())
    }
}

fn is_safe_operation_id(operation_id: &str) -> bool {
    !operation_id.is_empty()
        && operation_id.len() <= 160
        && operation_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn validate_operation_id(operation_id: &str) -> Result<(), String> {
    is_safe_operation_id(operation_id)
        .then_some(())
        .ok_or_else(|| "history operation ID contains unsafe path characters".into())
}

fn sanitize_message(message: String) -> String {
    message.lines().next().unwrap_or_default().chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{Package, PackageSource},
        transaction::{
            InstallScope, OperationAction, OperationPlan, OperationRequest, PackageRefJson,
            PlanCompleteness,
        },
    };
    use std::{
        collections::BTreeMap,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn writes_structured_record_atomically_without_command_output() {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let directory = std::env::temp_dir().join(format!("orbis-history-test-{unique}"));
        let package = Package {
            source: PackageSource::Apt,
            provider_id: "btop".into(),
            name: "btop".into(),
            version: Some("1.0".into()),
            summary: None,
            description: None,
            installed: Some(false),
            kind: None,
            origin: None,
            architecture: None,
            homepage: None,
            license: None,
            size_bytes: None,
            metadata: BTreeMap::new(),
        };
        let mut plan = OperationPlan::new(OperationAction::Install, package, InstallScope::System);
        plan.completeness = PlanCompleteness::Complete;
        plan.risk = crate::transaction::RiskLevel::Normal;
        let result = crate::transaction::TransactionResult {
            plan,
            execution: crate::transaction::ExecutionSummary {
                exit_status: Some(0),
                process_succeeded: true,
                message: None,
            },
            verification: crate::transaction::VerificationResult::Verified,
            status: crate::transaction::TransactionStatus::Succeeded,
        };
        let record = TransactionRecord::new(
            OperationRequest {
                action: OperationAction::Install,
                package: PackageRefJson { source: Some(PackageSource::Apt), query: "btop".into() },
                scope: Some(InstallScope::System),
                channel: None,
            },
            result,
        );
        let store = HistoryStore::at(&directory);
        let path = store.write(&record, "tx-test").expect("record writes");
        let body = fs::read_to_string(path).expect("record readable");
        assert!(body.contains("\"operation_id\""));
        assert!(!body.contains("shell"));
        assert!(!directory.join(".tx-test.tmp").exists());
        let mut legacy = serde_json::to_value(&record).expect("record serializes");
        let object = legacy.as_object_mut().expect("record object");
        object.remove("schema_version");
        object.remove("updated_at_unix_ms");
        object.remove("lifecycle");
        object.remove("plan");
        let legacy_record: TransactionRecord =
            serde_json::from_value(legacy).expect("legacy record remains readable");
        assert_eq!(legacy_record.schema_version, 1);
        assert_eq!(legacy_record.effective_lifecycle(), TransactionLifecycle::Succeeded);
        assert!(legacy_record.resolved_plan().is_some());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rejects_history_path_traversal() {
        let store = HistoryStore::at("/tmp/orbis-history-safety-test");
        assert!(store.entry("../outside").is_err());
        assert!(!store.record_path("../outside").starts_with("/tmp/outside"));
    }
}
