//! Post-execution observation that turns process results into verified outcomes.

use std::sync::Arc;

use crate::apt_ops::{
    AptOutcomeProbe, diagnose_apt_failure, transaction_changes, upgrade_changes,
    verify_package_state,
};
use crate::facts::{ChangeKind, FailureCause, FailureDiagnosis, PackageChange, RawCommandRef};
use crate::maintenance::{MaintenanceAction, ProviderMaintenancePlan};
use crate::models::PackageSource;
use crate::operation::OperationStatus;
use crate::process::{CommandOutput, CommandRunner};
use crate::transaction::{OperationAction, OperationPlan, ProviderOperation, VerificationResult};

/// Rich outcome gathered after a mutation, used to build the operation journal.
#[derive(Clone, Debug)]
pub struct ObservedOutcome {
    /// Verification result after observation.
    pub verification: VerificationResult,
    /// Observed package changes.
    pub changes: Vec<PackageChange>,
    /// Warnings (reboot, residual upgrades, …).
    pub warnings: Vec<String>,
    /// Failure diagnosis when the operation failed.
    pub diagnosis: Option<FailureDiagnosis>,
    /// What was checked.
    pub checks: Vec<String>,
    /// Expandable command detail.
    pub raw_commands: Vec<RawCommandRef>,
}

impl Default for ObservedOutcome {
    fn default() -> Self {
        Self {
            verification: VerificationResult::Failed,
            changes: Vec::new(),
            warnings: Vec::new(),
            diagnosis: None,
            checks: Vec::new(),
            raw_commands: Vec::new(),
        }
    }
}

impl ObservedOutcome {
    /// Maps verification into journal status. Failed verification never becomes Succeeded.
    pub fn operation_status(&self, process_succeeded: bool) -> OperationStatus {
        if !process_succeeded || self.diagnosis.is_some() {
            return OperationStatus::Failed;
        }
        match self.verification {
            VerificationResult::Verified => OperationStatus::Succeeded,
            VerificationResult::PartiallyVerified => OperationStatus::PartiallyVerified,
            VerificationResult::Failed => OperationStatus::Failed,
        }
    }
}

/// Observe a single-package transaction after the provider command finished.
pub fn observe_transaction(
    runner: Arc<dyn CommandRunner>,
    plan: &OperationPlan,
    output: &CommandOutput,
    operation: &ProviderOperation,
) -> ObservedOutcome {
    let mut outcome = ObservedOutcome {
        raw_commands: vec![raw_command_for(operation, output.status)],
        ..ObservedOutcome::default()
    };

    if !output.success() {
        outcome.verification = VerificationResult::Failed;
        if plan.target.source == PackageSource::Apt {
            outcome.diagnosis =
                Some(diagnose_apt_failure(&output.stdout, &output.stderr, output.status));
        } else {
            outcome.diagnosis = Some(FailureDiagnosis {
                cause: FailureCause::Unknown,
                summary: "Provider command failed.".into(),
                hint: crate::transaction::safe_process_message(output),
            });
        }
        return outcome;
    }

    if plan.target.source != PackageSource::Apt {
        // Non-APT providers keep their existing installed-state verification elsewhere.
        outcome.verification = VerificationResult::Verified;
        outcome.checks.push("Provider reported success; detailed APT observation skipped".into());
        return outcome;
    }

    let probe = AptOutcomeProbe::new(runner);
    let package_id = plan.target.provider_id.clone();
    let after = probe.versions(std::slice::from_ref(&package_id));
    let after_version = after.get(&package_id).cloned();
    let action_install = plan.action == OperationAction::Install;
    let (verification, checks) = verify_package_state(
        action_install,
        plan.target.version.as_deref(),
        after_version.as_deref(),
    );
    outcome.verification = verification;
    outcome.checks = checks;
    outcome.changes = transaction_changes(
        &package_id,
        &plan.target.name,
        action_install,
        None,
        after_version.as_deref(),
    );
    if !action_install && after_version.is_none() {
        outcome.changes = vec![PackageChange {
            package_id,
            name: Some(plan.target.name.clone()),
            kind: ChangeKind::Removed,
            from_version: plan.target.version.clone(),
            to_version: None,
        }];
    }
    if let Some(warning) = probe.reboot_required().warning_line() {
        outcome.warnings.push(warning);
    }
    let health = probe.dependency_health();
    if health.broken {
        outcome.verification = VerificationResult::Failed;
        outcome.diagnosis = Some(FailureDiagnosis {
            cause: FailureCause::BrokenDependencies,
            summary: health
                .summary
                .unwrap_or_else(|| "Package dependencies are broken after the operation.".into()),
            hint: Some("Inspect package state before retrying.".into()),
        });
        outcome.checks.push("Dependency health check failed".into());
    }
    outcome
}

/// Observe an APT maintenance plan after execution.
pub fn observe_apt_maintenance(
    runner: Arc<dyn CommandRunner>,
    plan: &ProviderMaintenancePlan,
    output: &CommandOutput,
    remaining_candidates: usize,
) -> ObservedOutcome {
    let operation = ProviderOperation::Maintenance {
        operation: match plan.action {
            MaintenanceAction::Refresh => crate::transaction::MaintenanceOperation::AptRefresh,
            MaintenanceAction::Upgrade => crate::transaction::MaintenanceOperation::AptUpgrade,
            MaintenanceAction::Cleanup => crate::transaction::MaintenanceOperation::AptAutoremove,
        },
    };
    let mut outcome = ObservedOutcome {
        raw_commands: vec![raw_command_for(&operation, output.status)],
        ..ObservedOutcome::default()
    };

    if !output.success() {
        outcome.verification = VerificationResult::Failed;
        outcome.diagnosis =
            Some(diagnose_apt_failure(&output.stdout, &output.stderr, output.status));
        return outcome;
    }

    let probe = AptOutcomeProbe::new(runner);
    match plan.action {
        MaintenanceAction::Refresh => {
            outcome.verification = VerificationResult::Verified;
            outcome.checks.push("APT index refresh completed".into());
        }
        MaintenanceAction::Upgrade => {
            let planned: Vec<_> = plan
                .candidates
                .iter()
                .map(|candidate| {
                    (
                        candidate.provider_id.clone(),
                        candidate.current_version.clone(),
                        candidate.available_version.clone(),
                    )
                })
                .collect();
            let names: Vec<_> = planned.iter().map(|(id, _, _)| id.clone()).collect();
            let after = probe.versions(&names);
            outcome.changes = upgrade_changes(&planned, &after);
            outcome.checks.push(format!(
                "Observed {} package version change{}",
                outcome.changes.len(),
                if outcome.changes.len() == 1 { "" } else { "s" }
            ));
            if remaining_candidates == 0 {
                outcome.verification = VerificationResult::Verified;
                outcome.checks.push("No remaining ordinary APT upgrades".into());
            } else {
                outcome.verification = VerificationResult::PartiallyVerified;
                outcome.warnings.push(format!(
                    "{remaining_candidates} package{} still pending upgrade",
                    if remaining_candidates == 1 { "" } else { "s" }
                ));
                outcome.checks.push("Remaining upgrade candidates after execution".into());
            }
            // Planned candidates that did not move when an available version was known.
            let stalled = planned
                .iter()
                .filter(|(id, from, expected)| {
                    expected.is_some()
                        && after
                            .get(id)
                            .and_then(|to| from.as_ref().map(|f| f == to))
                            .unwrap_or(false)
                })
                .count();
            if stalled > 0 && remaining_candidates > 0 {
                outcome.warnings.push(format!(
                    "{stalled} planned package{} did not change version",
                    if stalled == 1 { "" } else { "s" }
                ));
            }
        }
        MaintenanceAction::Cleanup => {
            outcome.verification = if remaining_candidates == 0 {
                VerificationResult::Verified
            } else {
                VerificationResult::PartiallyVerified
            };
            outcome.checks.push("Re-checked APT autoremove candidates".into());
        }
    }

    if let Some(warning) = probe.reboot_required().warning_line() {
        outcome.warnings.push(warning);
    }
    let health = probe.dependency_health();
    if health.broken {
        outcome.verification = VerificationResult::Failed;
        outcome.diagnosis = Some(FailureDiagnosis {
            cause: FailureCause::BrokenDependencies,
            summary: health
                .summary
                .unwrap_or_else(|| "Broken dependencies detected after maintenance.".into()),
            hint: Some("Resolve dependency problems before further package changes.".into()),
        });
    }
    outcome
}

fn raw_command_for(operation: &ProviderOperation, exit_status: Option<i32>) -> RawCommandRef {
    match operation {
        ProviderOperation::Apt { action, package_id } => RawCommandRef {
            program: "apt-get".into(),
            args: vec![
                match action {
                    OperationAction::Install => "install",
                    OperationAction::Remove => "remove",
                }
                .into(),
                "--".into(),
                package_id.clone(),
            ],
            exit_status,
        },
        ProviderOperation::Maintenance { operation } => {
            let (program, args) = match operation {
                crate::transaction::MaintenanceOperation::AptRefresh => {
                    ("apt-get", vec!["update".into()])
                }
                crate::transaction::MaintenanceOperation::AptUpgrade => {
                    ("apt-get", vec!["upgrade".into(), "--assume-yes".into(), "--no-remove".into()])
                }
                crate::transaction::MaintenanceOperation::AptAutoremove => {
                    ("apt-get", vec!["autoremove".into(), "--assume-yes".into()])
                }
                other => ("maintenance", vec![format!("{other:?}")]),
            };
            RawCommandRef { program: program.into(), args, exit_status }
        }
        other => RawCommandRef {
            program: format!("{:?}", other.source()).to_ascii_lowercase(),
            args: vec![format!("{:?}", other.action()).to_ascii_lowercase()],
            exit_status,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Package, PackageKind};
    use crate::process::{CommandOutput, CommandSpec, ProcessError};
    use crate::transaction::{
        InstallScope, OperationAction, PackageState, PlanCompleteness, PlanConfidence,
        PrivilegeRequirement, RiskLevel,
    };
    use std::sync::Mutex;

    struct ScriptedRunner {
        responses: Mutex<Vec<(String, CommandOutput)>>,
    }

    impl CommandRunner for ScriptedRunner {
        fn is_available(&self, program: &str) -> bool {
            matches!(program, "dpkg-query" | "apt-get" | "apt-cache" | "apt-mark")
        }

        fn run(&self, command: &CommandSpec) -> Result<CommandOutput, ProcessError> {
            let key = format!("{} {}", command.program, command.args.join(" "));
            let mut responses = self.responses.lock().unwrap();
            if let Some(index) = responses.iter().position(|(pattern, _)| key.contains(pattern)) {
                return Ok(responses.remove(index).1);
            }
            Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: Some(0) })
        }
    }

    fn plan(action: OperationAction) -> OperationPlan {
        OperationPlan {
            operation_id: "tx-obs-1".into(),
            action,
            target: Package {
                source: PackageSource::Apt,
                name: "curl".into(),
                provider_id: "curl".into(),
                version: Some("8.5.0-1".into()),
                summary: None,
                description: None,
                installed: Some(false),
                kind: Some(PackageKind::CliTool),
                origin: None,
                architecture: None,
                homepage: None,
                license: None,
                size_bytes: None,
                metadata: Default::default(),
            },
            scope: InstallScope::System,
            current_state: PackageState::NotInstalled,
            requested_state: PackageState::Installed,
            changes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: true,
            risk: RiskLevel::Normal,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn failed_apt_install_is_diagnosed_not_completed() {
        let runner = Arc::new(ScriptedRunner { responses: Mutex::new(Vec::new()) });
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "E: Could not get lock /var/lib/dpkg/lock-frontend".into(),
            status: Some(100),
        };
        let operation =
            ProviderOperation::Apt { action: OperationAction::Install, package_id: "curl".into() };
        let outcome =
            observe_transaction(runner, &plan(OperationAction::Install), &output, &operation);
        assert_eq!(outcome.operation_status(false), OperationStatus::Failed);
        assert_eq!(outcome.diagnosis.as_ref().map(|d| d.cause), Some(FailureCause::AptLock));
        assert_eq!(outcome.verification, VerificationResult::Failed);
    }

    #[test]
    fn successful_install_records_version_and_verifies() {
        let runner = Arc::new(ScriptedRunner {
            responses: Mutex::new(vec![
                (
                    "dpkg-query".into(),
                    CommandOutput {
                        stdout: "curl\tinstall ok installed\t8.5.0-1\n".into(),
                        stderr: String::new(),
                        status: Some(0),
                    },
                ),
                (
                    "apt-get check".into(),
                    CommandOutput { stdout: String::new(), stderr: String::new(), status: Some(0) },
                ),
            ]),
        });
        let output = CommandOutput {
            stdout: "Setting up curl".into(),
            stderr: String::new(),
            status: Some(0),
        };
        let operation =
            ProviderOperation::Apt { action: OperationAction::Install, package_id: "curl".into() };
        let outcome =
            observe_transaction(runner, &plan(OperationAction::Install), &output, &operation);
        assert_eq!(outcome.verification, VerificationResult::Verified);
        assert_eq!(outcome.operation_status(true), OperationStatus::Succeeded);
        assert_eq!(outcome.changes.len(), 1);
        assert_eq!(outcome.changes[0].to_version.as_deref(), Some("8.5.0-1"));
    }
}
