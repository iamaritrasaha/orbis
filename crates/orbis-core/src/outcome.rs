//! Post-execution observation that turns process results into verified outcomes.

use std::sync::Arc;

use crate::apt_ops::{
    AptOutcomeProbe, DependencyHealthStatus, InstalledObservation, diagnose_apt_failure,
    transaction_changes, upgrade_changes, verify_package_state,
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
    let lookup = after.lookup(&package_id);
    let after_version = match lookup {
        crate::apt_ops::PackageLookup::Installed(version) => Some(version.to_owned()),
        _ => None,
    };
    let action_install = plan.action == OperationAction::Install;
    let (verification, checks) =
        verify_package_state(action_install, plan.target.version.as_deref(), lookup);
    outcome.verification = verification;
    outcome.checks = checks;
    if matches!(after, InstalledObservation::Observed(_)) {
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
    }
    if let Some(warning) = probe.reboot_required().warning_line() {
        outcome.warnings.push(warning);
    }
    apply_dependency_health(&mut outcome, probe.dependency_health());
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
            match after.versions() {
                Some(versions) => {
                    outcome.changes = upgrade_changes(&planned, versions);
                    outcome.checks.push("dpkg package state observed".into());
                    outcome.checks.push(format!(
                        "Observed {} package version change{}",
                        outcome.changes.len(),
                        if outcome.changes.len() == 1 { "" } else { "s" }
                    ));
                    if remaining_candidates == 0 {
                        outcome.verification = VerificationResult::Verified;
                        outcome.checks.push("no remaining APT upgrades".into());
                    } else {
                        outcome.verification = VerificationResult::PartiallyVerified;
                        outcome.warnings.push(format!(
                            "{remaining_candidates} package{} still pending upgrade",
                            if remaining_candidates == 1 { "" } else { "s" }
                        ));
                        outcome.checks.push("Remaining upgrade candidates after execution".into());
                    }
                    let stalled = planned
                        .iter()
                        .filter(|(id, from, expected)| {
                            expected.is_some()
                                && versions
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
                None => {
                    let reason = match after {
                        InstalledObservation::Unavailable { reason } => reason.as_str(),
                        InstalledObservation::Observed(_) => "unknown",
                    };
                    outcome.checks.push(format!(
                        "planned candidate version observation unavailable ({reason})"
                    ));
                    outcome.warnings.push("Package-level verification incomplete".into());
                    outcome.verification = VerificationResult::PartiallyVerified;
                }
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
    apply_dependency_health(&mut outcome, probe.dependency_health());
    outcome
}

fn apply_dependency_health(
    outcome: &mut ObservedOutcome,
    health: crate::apt_ops::DependencyHealth,
) {
    match health.status {
        DependencyHealthStatus::Healthy => {
            outcome.checks.push("apt-get check passed".into());
        }
        DependencyHealthStatus::Broken => {
            outcome.verification = VerificationResult::Failed;
            outcome.diagnosis = Some(FailureDiagnosis {
                cause: FailureCause::BrokenDependencies,
                summary: health.summary.unwrap_or_else(|| {
                    "Package dependencies are broken after the operation.".into()
                }),
                hint: Some("Inspect package state before retrying.".into()),
            });
            outcome.checks.push("Dependency health check failed".into());
        }
        DependencyHealthStatus::Unknown => {
            outcome.checks.push("dependency health could not be observed".into());
        }
    }
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
        assert!(outcome.checks.iter().any(|c| c.contains("installed version observed")));
    }

    fn dpkg_query_failure() -> CommandOutput {
        CommandOutput {
            stdout: String::new(),
            stderr: "dpkg-query: error: failed to open package info file `/var/lib/dpkg/status'"
                .into(),
            status: Some(1),
        }
    }

    fn check_ok() -> (String, CommandOutput) {
        (
            "apt-get check".into(),
            CommandOutput { stdout: String::new(), stderr: String::new(), status: Some(0) },
        )
    }

    #[test]
    fn remove_with_failed_dpkg_query_is_never_verified() {
        let runner = Arc::new(ScriptedRunner {
            responses: Mutex::new(vec![("dpkg-query".into(), dpkg_query_failure()), check_ok()]),
        });
        let output = CommandOutput {
            stdout: "Removing curl".into(),
            stderr: String::new(),
            status: Some(0),
        };
        let operation =
            ProviderOperation::Apt { action: OperationAction::Remove, package_id: "curl".into() };
        let outcome =
            observe_transaction(runner, &plan(OperationAction::Remove), &output, &operation);
        assert_ne!(outcome.verification, VerificationResult::Verified);
        assert_eq!(outcome.verification, VerificationResult::Failed);
    }

    #[test]
    fn install_with_failed_dpkg_query_is_never_verified() {
        let runner = Arc::new(ScriptedRunner {
            responses: Mutex::new(vec![("dpkg-query".into(), dpkg_query_failure()), check_ok()]),
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
        assert_ne!(outcome.verification, VerificationResult::Verified);
        assert_eq!(outcome.verification, VerificationResult::Failed);
    }

    fn upgrade_plan() -> crate::maintenance::ProviderMaintenancePlan {
        crate::maintenance::ProviderMaintenancePlan {
            operation_id: "mnt-obs-1".into(),
            source: PackageSource::Apt,
            action: crate::maintenance::MaintenanceAction::Upgrade,
            scope: Some(InstallScope::System),
            candidates: vec![crate::maintenance::UpdateCandidate {
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
            }],
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

    #[test]
    fn upgrade_with_failed_version_observation_is_never_fully_verified() {
        let runner = Arc::new(ScriptedRunner {
            responses: Mutex::new(vec![("dpkg-query".into(), dpkg_query_failure()), check_ok()]),
        });
        let output = CommandOutput {
            stdout: "Setting up openssl".into(),
            stderr: String::new(),
            status: Some(0),
        };
        let outcome = observe_apt_maintenance(runner, &upgrade_plan(), &output, 0);
        assert_ne!(outcome.verification, VerificationResult::Verified);
        assert_eq!(outcome.verification, VerificationResult::PartiallyVerified);
    }
}
