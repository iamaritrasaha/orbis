//! Durable, sanitized transaction records under the XDG state directory.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{OperationRequest, TransactionResult};

/// A persisted record for an attempted operation, including failed attempts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionRecord {
    /// Unix timestamp in milliseconds when the record was written.
    pub recorded_at_unix_ms: u64,
    /// The request before provider resolution.
    pub request: OperationRequest,
    /// The resolved execution result, when planning reached that stage.
    pub result: TransactionResult,
}

impl TransactionRecord {
    /// Creates a record with the current Unix timestamp.
    pub fn new(request: OperationRequest, result: TransactionResult) -> Self {
        let recorded_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64);
        Self { recorded_at_unix_ms, request, result }
    }
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

    /// Atomically writes one JSON record without command output or credentials.
    pub fn write(&self, record: &TransactionRecord, operation_id: &str) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let final_path = self.directory.join(format!("{operation_id}.json"));
        let temporary_path = self.directory.join(format!(".{operation_id}.tmp"));
        let body = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
        fs::write(&temporary_path, body).map_err(|error| error.to_string())?;
        fs::rename(&temporary_path, &final_path).map_err(|error| error.to_string())?;
        Ok(final_path)
    }
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
        let _ = fs::remove_dir_all(directory);
    }
}
