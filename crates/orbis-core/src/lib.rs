#![deny(unsafe_code)]
#![warn(missing_docs)]

//! The provider-neutral, read-only foundation of Orbis.

pub mod diagnostics;
pub mod explain;
pub mod maintenance;
pub mod models;
pub mod privilege;
pub mod process;
pub mod progress;
pub mod providers;
pub mod transaction;

use std::collections::BTreeSet;

use maintenance::{
    MaintenanceAction, MaintenancePlan, MaintenanceProvider, MaintenanceResult,
    ProviderMaintenancePlan, UpdateInventoryReport, WhyReport, aggregate_inventory,
};
use models::{Package, PackageRef, PackageSource, ProviderIssue, SourceInfo};
use providers::{Provider, ProviderError, TransactionProvider};
use transaction::{
    ExecutionSummary, OperationExecutor, OperationPlan, OperationRequest, TransactionError,
    TransactionResult, TransactionStatus, VerificationResult,
};

/// A registry of the providers supported by this Orbis milestone.
pub struct ProviderRegistry {
    providers: Vec<Box<dyn TransactionProvider>>,
    runner: process::SharedRunner,
}

impl ProviderRegistry {
    /// Creates a registry using the real local process runner.
    pub fn system() -> Self {
        let runner = process::RealCommandRunner::new();
        Self::with_runner(runner)
    }

    /// Creates a registry with an injected process runner.
    pub fn with_runner<R>(runner: R) -> Self
    where
        R: process::CommandRunner + 'static,
    {
        let shared = std::sync::Arc::new(runner);
        Self {
            runner: shared.clone(),
            providers: vec![
                Box::new(providers::apt::AptProvider::new(shared.clone())),
                Box::new(providers::flatpak::FlatpakProvider::new(shared.clone())),
                Box::new(providers::snap::SnapProvider::new(shared.clone())),
                Box::new(providers::developer::CargoProvider::new(shared.clone())),
                Box::new(providers::developer::NpmProvider::new(shared.clone())),
                Box::new(providers::developer::PnpmProvider::new(shared.clone())),
                Box::new(providers::developer::UvProvider::new(shared.clone())),
                Box::new(providers::developer::PipxProvider::new(shared)),
            ],
        }
    }

    /// Returns the shared process seam used by providers and the privilege boundary.
    pub fn runner(&self) -> process::SharedRunner {
        self.runner.clone()
    }

    /// Returns a concise snapshot of provider availability.
    pub fn sources(&self) -> Vec<SourceInfo> {
        self.providers.iter().map(|provider| provider.source_info()).collect()
    }

    /// Searches one provider or all available providers, isolating failures.
    pub fn search(&self, query: &str, source: Option<PackageSource>) -> SearchReport {
        let mut results = Vec::new();
        let mut issues = Vec::new();

        let providers = self.selected(source);
        std::thread::scope(|scope| {
            let handles = providers
                .into_iter()
                .map(|provider| scope.spawn(move || (provider.source(), provider.search(query))))
                .collect::<Vec<_>>();
            for handle in handles {
                match handle.join().expect("provider search thread panicked") {
                    (_, Ok(packages)) => results.extend(packages),
                    (source, Err(error)) => issues.push(issue(source, error)),
                }
            }
        });

        results.sort_by(|left, right| {
            (!left.installed.unwrap_or(false), left.name.to_ascii_lowercase(), left.source).cmp(&(
                !right.installed.unwrap_or(false),
                right.name.to_ascii_lowercase(),
                right.source,
            ))
        });
        SearchReport { results, issues }
    }

    /// Resolves a package reference while refusing unsafe ambiguity.
    pub fn resolve(&self, package_ref: &PackageRef) -> ResolveReport {
        let mut matches = Vec::new();
        let mut issues = Vec::new();

        for provider in self.selected(package_ref.source).into_iter().filter(|provider| {
            package_ref.source.is_some() || provider.supports_unqualified_resolution()
        }) {
            match provider.info(&package_ref.query) {
                Ok(package) => matches.push(package),
                Err(ProviderError::NotFound { .. }) => {}
                Err(error) => issues.push(issue(provider.source(), error)),
            }
        }

        if matches.len() == 1 {
            return ResolveReport::Found { package: Box::new(matches.remove(0)), issues };
        }

        if matches.len() > 1 {
            let installed: Vec<_> =
                matches.iter().filter(|package| package.installed == Some(true)).cloned().collect();
            if installed.len() == 1 {
                return ResolveReport::Found {
                    package: Box::new(installed.into_iter().next().expect("length checked")),
                    issues,
                };
            }
            return ResolveReport::Ambiguous { matches, issues };
        }

        ResolveReport::NotFound { issues }
    }

    /// Plans one explicit, single-package transaction using strict resolution.
    pub fn plan_transaction(
        &self,
        request: &OperationRequest,
    ) -> Result<OperationPlan, TransactionError> {
        transaction::validate_query(&request.package.query)?;
        if let Some(channel) = &request.channel {
            validate_channel(channel)?;
        }
        let mut matches = Vec::new();
        let mut issues = Vec::new();
        // An unqualified beginner request may resolve when exactly one safe
        // source can identify it. Ambiguity remains a hard stop below.
        for provider in self.selected(request.package.source).into_iter().filter(|provider| {
            request.package.source.is_some() || provider.supports_unqualified_resolution()
        }) {
            match provider.info(&request.package.query) {
                Ok(package) => matches.push(package),
                Err(ProviderError::NotFound { .. }) => {}
                Err(error) => issues.push(issue(provider.source(), error)),
            }
        }
        if matches.is_empty() {
            if let Some(provider_issue) = issues.into_iter().next() {
                return Err(TransactionError::Planning(provider_issue.message));
            }
            return Err(TransactionError::NotFound { query: request.package.query.clone() });
        }
        if matches.len() != 1 {
            return Err(TransactionError::Ambiguous {
                query: request.package.query.clone(),
                matches,
            });
        }

        let target = matches.pop().expect("length checked");
        let provider =
            self.providers.iter().find(|provider| provider.source() == target.source).ok_or_else(
                || TransactionError::Planning("resolved provider is unavailable".into()),
            )?;
        provider.plan_transaction(request, &target)
    }

    /// Executes a previously displayed and confirmed plan through the typed executor.
    pub fn execute_transaction(
        &self,
        plan: OperationPlan,
        executor: &dyn OperationExecutor,
    ) -> Result<TransactionResult, TransactionError> {
        self.execute_transaction_with_observer(plan, executor, &progress::SilentObserver)
    }

    /// Executes a previously displayed and confirmed plan through the typed executor with progress observation.
    pub fn execute_transaction_with_observer(
        &self,
        plan: OperationPlan,
        executor: &dyn OperationExecutor,
        observer: &dyn progress::ProgressObserver,
    ) -> Result<TransactionResult, TransactionError> {
        if !plan.executable() {
            return Err(TransactionError::Blocked(
                "the plan is incomplete or was classified as blocked".into(),
            ));
        }
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.source() == plan.target.source)
            .ok_or_else(|| TransactionError::Planning("resolved provider is unavailable".into()))?;
        let operation = provider.provider_operation(&plan)?;
        observer
            .on_event(&progress::OperationEvent::ProviderStarted { source: plan.target.source });

        if plan.privilege == transaction::PrivilegeRequirement::Administrator {
            observer.on_event(&progress::OperationEvent::StageChanged {
                stage: progress::ExecutionStage::Authenticating,
            });
        }

        observer.on_event(&progress::OperationEvent::StageChanged {
            stage: progress::ExecutionStage::Executing,
        });

        let output = match executor.execute_with_observer(&operation, plan.privilege, observer) {
            Ok(output) => output,
            Err(error) => {
                observer.on_event(&progress::OperationEvent::ProviderFinished {
                    source: plan.target.source,
                    success: false,
                });
                return Err(error.into());
            }
        };
        let execution = ExecutionSummary {
            exit_status: output.status,
            process_succeeded: output.success(),
            message: (!output.success())
                .then(|| transaction::safe_process_message(&output))
                .flatten(),
        };
        if !output.success() {
            observer.on_event(&progress::OperationEvent::ProviderFinished {
                source: plan.target.source,
                success: false,
            });
            return Ok(TransactionResult {
                plan,
                execution,
                verification: VerificationResult::Failed,
                status: TransactionStatus::Failed,
            });
        }

        observer.on_event(&progress::OperationEvent::StageChanged {
            stage: progress::ExecutionStage::Verifying,
        });

        let verification = match provider.verify_transaction(&plan) {
            Ok(verification) => verification,
            Err(error) => {
                observer.on_event(&progress::OperationEvent::ProviderFinished {
                    source: plan.target.source,
                    success: false,
                });
                return Err(error);
            }
        };
        let status = match verification {
            VerificationResult::Verified => TransactionStatus::Succeeded,
            VerificationResult::PartiallyVerified => TransactionStatus::PartiallyVerified,
            VerificationResult::Failed => TransactionStatus::Failed,
        };
        observer.on_event(&progress::OperationEvent::ProviderFinished {
            source: plan.target.source,
            success: status != TransactionStatus::Failed,
        });
        Ok(TransactionResult { plan, execution, verification, status })
    }

    /// Writes an executing record before invoking a typed mutation, then atomically replaces it
    /// with the verified outcome. An inability to write the start record fails closed.
    pub fn execute_transaction_with_history(
        &self,
        request: &OperationRequest,
        plan: OperationPlan,
        executor: &dyn OperationExecutor,
        history: &transaction::history::HistoryStore,
    ) -> Result<TransactionResult, TransactionError> {
        self.execute_transaction_with_progress(
            request,
            plan,
            executor,
            history,
            &progress::SilentObserver,
        )
    }

    /// Writes an executing record before invoking a typed mutation, emits progress events at each stage,
    /// then atomically replaces it with the verified outcome.
    pub fn execute_transaction_with_progress(
        &self,
        request: &OperationRequest,
        plan: OperationPlan,
        executor: &dyn OperationExecutor,
        history: &transaction::history::HistoryStore,
        observer: &dyn progress::ProgressObserver,
    ) -> Result<TransactionResult, TransactionError> {
        if !plan.executable() {
            let err = TransactionError::Blocked(
                "the plan is incomplete or was classified as blocked".into(),
            );
            observer.on_event(&progress::OperationEvent::Finished {
                stage: progress::ExecutionStage::Failed,
                message: Some(err.to_string()),
                operation_id: plan.operation_id.clone(),
            });
            return Err(err);
        }
        let operation_id = plan.operation_id.clone();

        observer.on_event(&progress::OperationEvent::StageChanged {
            stage: progress::ExecutionStage::Preparing,
        });

        history
            .write(
                &transaction::history::TransactionRecord::execution_started(
                    request.clone(),
                    plan.clone(),
                ),
                &operation_id,
            )
            .map_err(TransactionError::History)?;

        match self.execute_transaction_with_observer(plan.clone(), executor, observer) {
            Ok(result) => {
                observer.on_event(&progress::OperationEvent::StageChanged {
                    stage: progress::ExecutionStage::SavingResult,
                });
                history
                    .write(
                        &transaction::history::TransactionRecord::completed(
                            request.clone(),
                            result.clone(),
                        ),
                        &operation_id,
                    )
                    .map_err(TransactionError::History)?;

                let final_stage = if result.status == TransactionStatus::Failed {
                    progress::ExecutionStage::Failed
                } else {
                    progress::ExecutionStage::Completed
                };
                observer.on_event(&progress::OperationEvent::Finished {
                    stage: final_stage,
                    message: result.execution.message.clone(),
                    operation_id,
                });
                Ok(result)
            }
            Err(error) => {
                observer.on_event(&progress::OperationEvent::StageChanged {
                    stage: progress::ExecutionStage::SavingResult,
                });
                let failed = TransactionResult {
                    plan,
                    execution: ExecutionSummary {
                        exit_status: None,
                        process_succeeded: false,
                        message: Some(error.to_string()),
                    },
                    verification: VerificationResult::Failed,
                    status: TransactionStatus::Failed,
                };
                let _ = history.write(
                    &transaction::history::TransactionRecord::completed(request.clone(), failed),
                    &operation_id,
                );
                observer.on_event(&progress::OperationEvent::Finished {
                    stage: progress::ExecutionStage::Failed,
                    message: Some(error.to_string()),
                    operation_id,
                });
                Err(error)
            }
        }
    }

    /// Runs safe diagnostics for every selected provider.
    pub fn diagnostics(&self, source: Option<PackageSource>) -> diagnostics::DoctorReport {
        let mut checks = Vec::new();
        for provider in self.selected(source) {
            checks.push(provider.diagnostic());
        }
        diagnostics::DoctorReport { checks, read_only: true }
    }

    /// Collects a unified, read-only update inventory from selected providers.
    pub fn updates(&self, source: Option<PackageSource>) -> UpdateInventoryReport {
        self.updates_with_observer(source, &progress::SilentObserver)
    }

    /// Collects the update inventory while reporting read-only provider activity.
    /// These events are presentation-only; the inventory remains the same typed
    /// report returned by [`Self::updates`].
    pub fn updates_with_observer(
        &self,
        source: Option<PackageSource>,
        observer: &dyn progress::ProgressObserver,
    ) -> UpdateInventoryReport {
        let mut inventories = Vec::new();
        let mut issues = Vec::new();
        let providers = self.selected_maintenance(source);
        std::thread::scope(|scope| {
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            for provider in providers {
                let provider_source = provider.source();
                observer.on_event(&progress::OperationEvent::ProviderStarted {
                    source: provider_source,
                });
                let result_tx = result_tx.clone();
                scope.spawn(move || {
                    let result = provider.update_inventory();
                    let _ = result_tx.send((provider_source, result));
                });
            }
            drop(result_tx);
            for (source, result) in result_rx {
                let inventory = match &result {
                    Ok(inventory) => inventory.clone(),
                    Err(_) => maintenance::ProviderUpdateInventory {
                        source,
                        available: false,
                        candidates: Vec::new(),
                        notes: vec![
                            "Provider could not answer this read-only inventory query.".into(),
                        ],
                        metadata_state: None,
                    },
                };
                observer.on_event(&progress::OperationEvent::ProviderInventory { inventory });
                observer.on_event(&progress::OperationEvent::ProviderFinished {
                    source,
                    success: result.is_ok(),
                });
                match result {
                    Ok(inventory) => inventories.push(inventory),
                    Err(error) => {
                        issues.push(issue(source, error));
                        inventories.push(maintenance::ProviderUpdateInventory {
                            source,
                            available: false,
                            candidates: Vec::new(),
                            notes: vec![
                                "Provider could not answer this read-only inventory query.".into(),
                            ],
                            metadata_state: None,
                        });
                    }
                }
            }
        });
        inventories.sort_by_key(|inventory| inventory.source);
        issues.sort_by_key(|provider_issue| provider_issue.source);
        aggregate_inventory(inventories, issues)
    }

    /// Builds a coordinated but non-atomic maintenance plan.
    pub fn maintenance_plan(
        &self,
        action: MaintenanceAction,
        source: Option<PackageSource>,
    ) -> Result<MaintenancePlan, String> {
        let mut providers = Vec::new();
        let mut issues = Vec::new();
        for provider in self.selected_maintenance(source) {
            let result = match action {
                MaintenanceAction::Refresh => provider.refresh_plan(),
                MaintenanceAction::Upgrade => provider.upgrade_plan(),
                MaintenanceAction::Cleanup => provider.cleanup_plan().map(|plan| vec![plan]),
            };
            match result {
                Ok(plans) => providers.extend(plans),
                Err(error) => issues.push(format!("{}: {}", provider.source(), error)),
            }
        }
        if providers.is_empty() && !issues.is_empty() {
            return Err(issues.join("; "));
        }
        let mut plan = MaintenancePlan::new(action, source, providers);
        plan.warnings.extend(issues.into_iter().map(|message| transaction::PlanWarning {
            level: transaction::WarningLevel::Caution,
            message,
        }));
        plan.warnings.push(transaction::PlanWarning {
            level: transaction::WarningLevel::Info,
            message: "Providers are coordinated for reporting but are not one atomic transaction."
                .into(),
        });
        Ok(plan)
    }

    /// Re-checks an upgrade plan immediately before execution.
    pub fn revalidate_upgrade_plan(&self, plan: &MaintenancePlan) -> Result<(), String> {
        if plan.action != MaintenanceAction::Upgrade {
            return Ok(());
        }
        for expected in plan.providers.iter().filter(|provider| provider.executable()) {
            let provider = self
                .providers
                .iter()
                .find(|provider| provider.source() == expected.source)
                .ok_or_else(|| format!("{} is no longer available", expected.source))?;
            let current_plans = provider
                .upgrade_plan()
                .map_err(|error| format!("{} could not be revalidated: {error}", expected.source))?
                .into_iter()
                .collect::<Vec<_>>();
            let current_fingerprint = current_plans
                .iter()
                .filter(|item| item.scope == expected.scope)
                .flat_map(|item| item.candidates.clone())
                .collect::<Vec<_>>();
            let mut expected_fingerprint = expected.candidates.clone();
            expected_fingerprint.sort_by_key(|candidate| candidate.key());
            let mut actual_fingerprint = current_fingerprint;
            actual_fingerprint.sort_by_key(|candidate| candidate.key());
            let expected_keys: Vec<_> = expected_fingerprint
                .iter()
                .map(|candidate| {
                    (
                        candidate.key(),
                        candidate.current_version.clone(),
                        candidate.available_version.clone(),
                        candidate.held,
                    )
                })
                .collect();
            let actual_keys: Vec<_> = actual_fingerprint
                .iter()
                .map(|candidate| {
                    (
                        candidate.key(),
                        candidate.current_version.clone(),
                        candidate.available_version.clone(),
                        candidate.held,
                    )
                })
                .collect();
            if expected_keys != actual_keys {
                return Err(format!(
                    "{} upgrade plan is stale; review a new plan",
                    expected.source
                ));
            }
        }
        Ok(())
    }

    /// Resolves provider-specific reasoning for one already-resolved package.
    pub fn why(&self, package: &Package) -> Result<WhyReport, String> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.source() == package.source)
            .ok_or_else(|| format!("{} is unavailable", package.source))?;
        provider.why(package).map_err(|error| error.to_string())
    }

    /// Executes one provider plan through the existing typed privilege boundary.
    pub fn execute_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
        executor: &dyn OperationExecutor,
    ) -> Result<MaintenanceResult, String> {
        self.execute_maintenance_with_progress(plan, executor, &progress::SilentObserver)
    }

    /// Executes one provider plan through the existing typed privilege boundary with progress observation.
    pub fn execute_maintenance_with_progress(
        &self,
        plan: &ProviderMaintenancePlan,
        executor: &dyn OperationExecutor,
        observer: &dyn progress::ProgressObserver,
    ) -> Result<MaintenanceResult, String> {
        if !plan.executable() {
            return Err(format!("{} maintenance plan is blocked or unsupported", plan.source));
        }
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.source() == plan.source)
            .ok_or_else(|| format!("{} is unavailable", plan.source))?;

        // Some developer ecosystems do not maintain a separate local catalog.
        // Their refresh plan is an explicit, successful no-op so the live view
        // can report that source without inventing a provider command.
        if plan.action == MaintenanceAction::Refresh && !plan.mutates {
            observer.on_event(&progress::OperationEvent::ProviderStarted { source: plan.source });
            observer.on_event(&progress::OperationEvent::StageChanged {
                stage: progress::ExecutionStage::Preparing,
            });
            observer.on_event(&progress::OperationEvent::StageChanged {
                stage: progress::ExecutionStage::Verifying,
            });
            observer.on_event(&progress::OperationEvent::StageChanged {
                stage: progress::ExecutionStage::SavingResult,
            });
            observer.on_event(&progress::OperationEvent::Finished {
                stage: progress::ExecutionStage::Completed,
                message: None,
                operation_id: plan.operation_id.clone(),
            });
            observer.on_event(&progress::OperationEvent::ProviderFinished {
                source: plan.source,
                success: true,
            });
            return Ok(MaintenanceResult {
                operation_id: plan.operation_id.clone(),
                action: plan.action,
                status: maintenance::MaintenanceStatus::Succeeded,
                providers: vec![maintenance::MaintenanceProviderResult {
                    source: plan.source,
                    action: plan.action,
                    status: maintenance::MaintenanceProviderStatus::Succeeded,
                    candidate_count: 0,
                    verification: Some(transaction::VerificationResult::Verified),
                    message: plan.notes.first().cloned(),
                }],
            });
        }
        let operation = provider.maintenance_operation(plan).map_err(|error| error.to_string())?;
        let operation = match operation {
            transaction::ProviderOperation::Maintenance { operation } => operation,
            _ => return Err("provider returned a non-maintenance operation".into()),
        };
        if plan.action == MaintenanceAction::Upgrade
            && matches!(
                &operation,
                transaction::MaintenanceOperation::FlatpakUpgrade { refs, .. }
                    | transaction::MaintenanceOperation::SnapUpgrade { package_ids: refs }
                    | transaction::MaintenanceOperation::NpmUpgrade { package_ids: refs }
                    | transaction::MaintenanceOperation::PnpmUpgrade { package_ids: refs }
                    | transaction::MaintenanceOperation::UvUpgrade { package_ids: refs }
                    | transaction::MaintenanceOperation::PipxUpgrade { package_ids: refs }
                    if refs.is_empty()
            )
        {
            return Err(format!("{} upgrade plan has no reviewed candidates", plan.source));
        }

        observer.on_event(&progress::OperationEvent::ProviderStarted { source: plan.source });

        observer.on_event(&progress::OperationEvent::StageChanged {
            stage: progress::ExecutionStage::Preparing,
        });

        if plan.privilege == transaction::PrivilegeRequirement::Administrator {
            observer.on_event(&progress::OperationEvent::StageChanged {
                stage: progress::ExecutionStage::Authenticating,
            });
        }

        observer.on_event(&progress::OperationEvent::StageChanged {
            stage: progress::ExecutionStage::Executing,
        });

        let output = match executor.execute_with_observer(
            &transaction::ProviderOperation::Maintenance { operation },
            plan.privilege,
            observer,
        ) {
            Ok(output) => output,
            Err(error) => {
                observer.on_event(&progress::OperationEvent::ProviderFinished {
                    source: plan.source,
                    success: false,
                });
                return Err(error.to_string());
            }
        };
        if !output.success() {
            let msg = transaction::safe_process_message(&output);
            observer.on_event(&progress::OperationEvent::StageChanged {
                stage: progress::ExecutionStage::SavingResult,
            });
            observer.on_event(&progress::OperationEvent::Finished {
                stage: progress::ExecutionStage::Failed,
                message: msg.clone(),
                operation_id: plan.operation_id.clone(),
            });
            observer.on_event(&progress::OperationEvent::ProviderFinished {
                source: plan.source,
                success: false,
            });
            return Ok(MaintenanceResult {
                operation_id: plan.operation_id.clone(),
                action: plan.action,
                status: maintenance::MaintenanceStatus::Failed,
                providers: vec![maintenance::MaintenanceProviderResult {
                    source: plan.source,
                    action: plan.action,
                    status: maintenance::MaintenanceProviderStatus::Failed,
                    candidate_count: plan.candidates.len().max(plan.cleanup_candidates.len()),
                    verification: Some(transaction::VerificationResult::Failed),
                    message: msg,
                }],
            });
        }

        observer.on_event(&progress::OperationEvent::StageChanged {
            stage: progress::ExecutionStage::Verifying,
        });

        let verification = match provider.verify_maintenance(plan) {
            Ok(verification) => verification,
            Err(error) => {
                observer.on_event(&progress::OperationEvent::StageChanged {
                    stage: progress::ExecutionStage::SavingResult,
                });
                observer.on_event(&progress::OperationEvent::Finished {
                    stage: progress::ExecutionStage::Failed,
                    message: Some(error.to_string()),
                    operation_id: plan.operation_id.clone(),
                });
                observer.on_event(&progress::OperationEvent::ProviderFinished {
                    source: plan.source,
                    success: false,
                });
                return Err(error.to_string());
            }
        };
        let provider_status = match verification {
            transaction::VerificationResult::Verified => {
                maintenance::MaintenanceProviderStatus::Succeeded
            }
            transaction::VerificationResult::PartiallyVerified => {
                maintenance::MaintenanceProviderStatus::PartiallySucceeded
            }
            transaction::VerificationResult::Failed => {
                maintenance::MaintenanceProviderStatus::Failed
            }
        };
        let status = match provider_status {
            maintenance::MaintenanceProviderStatus::Succeeded => {
                maintenance::MaintenanceStatus::Succeeded
            }
            maintenance::MaintenanceProviderStatus::PartiallySucceeded => {
                maintenance::MaintenanceStatus::PartiallySucceeded
            }
            _ => maintenance::MaintenanceStatus::Failed,
        };

        observer.on_event(&progress::OperationEvent::StageChanged {
            stage: progress::ExecutionStage::SavingResult,
        });

        let final_stage = if status == maintenance::MaintenanceStatus::Failed {
            progress::ExecutionStage::Failed
        } else {
            progress::ExecutionStage::Completed
        };
        observer.on_event(&progress::OperationEvent::Finished {
            stage: final_stage,
            message: None,
            operation_id: plan.operation_id.clone(),
        });
        observer.on_event(&progress::OperationEvent::ProviderFinished {
            source: plan.source,
            success: status != maintenance::MaintenanceStatus::Failed,
        });

        Ok(MaintenanceResult {
            operation_id: plan.operation_id.clone(),
            action: plan.action,
            status,
            providers: vec![maintenance::MaintenanceProviderResult {
                source: plan.source,
                action: plan.action,
                status: provider_status,
                candidate_count: plan.candidates.len().max(plan.cleanup_candidates.len()),
                verification: Some(verification),
                message: None,
            }],
        })
    }

    fn selected(&self, source: Option<PackageSource>) -> Vec<&dyn Provider> {
        self.providers
            .iter()
            .filter(|provider| source.is_none_or(|wanted| provider.source() == wanted))
            .map(|provider| {
                let provider: &dyn Provider = provider.as_ref();
                provider
            })
            .collect()
    }

    fn selected_maintenance(&self, source: Option<PackageSource>) -> Vec<&dyn MaintenanceProvider> {
        self.providers
            .iter()
            .filter(|provider| source.is_none_or(|wanted| provider.source() == wanted))
            .map(|provider| {
                let provider: &dyn MaintenanceProvider = provider.as_ref();
                provider
            })
            .collect()
    }
}

fn validate_channel(channel: &str) -> Result<(), TransactionError> {
    if channel.is_empty()
        || channel.len() > 128
        || channel.starts_with('-')
        || channel.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, ';' | '&' | '|' | '$' | '`' | '\'' | '"')
        })
    {
        return Err(TransactionError::InvalidRequest(
            "Snap channels must be concise names without whitespace or shell-like characters"
                .into(),
        ));
    }
    Ok(())
}

/// Search results and provider-local warnings.
#[derive(Debug, serde::Serialize)]
pub struct SearchReport {
    /// Normalized package results.
    pub results: Vec<Package>,
    /// Provider failures that did not prevent other providers from answering.
    pub issues: Vec<ProviderIssue>,
}

/// The result of resolving a package reference.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResolveReport {
    /// One safe package choice.
    Found {
        /// The resolved package.
        package: Box<Package>,
        /// Non-fatal provider issues.
        issues: Vec<ProviderIssue>,
    },
    /// More than one provider matched and no installed result made the choice obvious.
    Ambiguous {
        /// The possible packages.
        matches: Vec<Package>,
        /// Non-fatal provider issues.
        issues: Vec<ProviderIssue>,
    },
    /// No provider returned a package.
    NotFound {
        /// Non-fatal provider issues.
        issues: Vec<ProviderIssue>,
    },
}

fn issue(source: PackageSource, error: ProviderError) -> ProviderIssue {
    ProviderIssue { source, message: error.to_string(), technical: error.technical_message() }
}

/// Parses a concise provider-qualified package reference such as `apt:libssl-dev`.
pub fn parse_package_ref(input: &str, source: Option<PackageSource>) -> PackageRef {
    let (prefix, query) = input.split_once(':').unwrap_or(("", input));
    let qualified = PackageSource::parse(prefix);
    PackageRef {
        source: source.or(qualified),
        query: if qualified.is_some() { query.to_owned() } else { input.to_owned() },
    }
}

/// De-duplicates provider sources while preserving their canonical display order.
pub fn source_set(sources: impl IntoIterator<Item = PackageSource>) -> BTreeSet<PackageSource> {
    sources.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PackageSource;
    use crate::process::{CommandOutput, CommandRunner, CommandSpec, ProcessError};
    use crate::transaction::{
        OperationAction, OperationExecutor, OperationRequest, PackageRefJson,
    };
    use std::{
        collections::BTreeSet,
        fs,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct FakeRunner {
        available: BTreeSet<String>,
    }

    impl FakeRunner {
        fn all() -> Self {
            Self {
                available: ["apt-cache", "dpkg-query", "flatpak", "snap"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            }
        }

        fn apt_only() -> Self {
            Self { available: ["apt-cache", "dpkg-query"].into_iter().map(str::to_owned).collect() }
        }
    }

    impl CommandRunner for FakeRunner {
        fn is_available(&self, program: &str) -> bool {
            self.available.contains(program)
        }

        fn run(&self, command: &CommandSpec) -> Result<CommandOutput, ProcessError> {
            let output = match command.program.as_str() {
                "apt-cache" if command.args.iter().any(|arg| arg == "search") => {
                    "btop - Modern terminal monitor\n"
                }
                "apt-cache" if command.args.iter().any(|arg| arg == "stats") => {
                    "Total package names: 1\n"
                }
                "apt-cache" => {
                    "Package: btop\nVersion: 1.0\nArchitecture: amd64\nDescription-en: Modern terminal monitor\n details\n\n"
                }
                "dpkg-query" => "btop\tinstall ok installed\t1.0\n",
                "flatpak" if command.args.first().is_some_and(|arg| arg == "list") => "",
                "flatpak" if command.args.first().is_some_and(|arg| arg == "remotes") => {
                    "flathub\n"
                }
                "flatpak" if command.args.first().is_some_and(|arg| arg == "search") => {
                    "org.example.Monitor\tExample Monitor\tGraphical monitor\t2.0\tstable\tflathub\n"
                }
                "flatpak" => {
                    "org.example.Monitor\tExample Monitor\tGraphical monitor\t2.0\tstable\tflathub\tx86_64\thttps://example.test\t1000\n"
                }
                "snap" if command.args.first().is_some_and(|arg| arg == "list") => {
                    "Name Version Rev Tracking Publisher Notes\nbtop 1.1 1 latest/stable example -\n"
                }
                "snap" if command.args.first().is_some_and(|arg| arg == "find") => {
                    "Name Version Publisher Notes Summary\nbtop 1.1 example - Terminal monitor\n"
                }
                "snap" if command.args.first().is_some_and(|arg| arg == "version") => {
                    "snap 1.0\nsnapd 1.0\n"
                }
                "snap" => {
                    "name: btop\nsummary: Terminal monitor\ndescription: |\n  Watches resources.\npublisher: example\ninstalled: 1.1 (1) 1MB -\n"
                }
                program => panic!("unexpected fake command: {program} {:?}", command.args),
            };
            Ok(CommandOutput { stdout: output.into(), stderr: String::new(), status: Some(0) })
        }
    }

    struct TransactionFakeRunner {
        installed: Mutex<bool>,
    }

    impl TransactionFakeRunner {
        fn new() -> Self {
            Self { installed: Mutex::new(false) }
        }
    }

    impl CommandRunner for TransactionFakeRunner {
        fn is_available(&self, program: &str) -> bool {
            matches!(program, "apt-cache" | "apt-get" | "dpkg-query")
        }

        fn run(&self, command: &CommandSpec) -> Result<CommandOutput, ProcessError> {
            let installed = *self.installed.lock().expect("state lock");
            let stdout = match command.program.as_str() {
                "apt-cache" => {
                    "Package: btop\nVersion: 1.0\nArchitecture: amd64\nDescription-en: Modern terminal monitor\n\n"
                }
                "dpkg-query" if installed => "btop\tinstall ok installed\t1.0\n",
                "dpkg-query" => "",
                "apt-get" => {
                    "NOTE: This is only a simulation!\nInst btop (1.0 test [amd64])\nConf btop (1.0 test [amd64])\n"
                }
                program => panic!("unexpected transaction command: {program} {:?}", command.args),
            };
            Ok(CommandOutput { stdout: stdout.into(), stderr: String::new(), status: Some(0) })
        }
    }

    struct FakeOperationExecutor {
        runner: Arc<TransactionFakeRunner>,
        start_record: Option<std::path::PathBuf>,
        fail: bool,
    }

    impl OperationExecutor for FakeOperationExecutor {
        fn execute(
            &self,
            operation: &crate::transaction::ProviderOperation,
            _requirement: crate::transaction::PrivilegeRequirement,
        ) -> Result<CommandOutput, crate::privilege::PrivilegeError> {
            if let Some(path) = &self.start_record {
                let body = fs::read_to_string(path).expect("start record exists before executor");
                let record: crate::transaction::history::TransactionRecord =
                    serde_json::from_str(&body).expect("start record parses");
                assert_eq!(
                    record.lifecycle,
                    crate::transaction::history::TransactionLifecycle::Executing
                );
                assert!(record.result.is_none());
            }
            if self.fail {
                return Err(crate::privilege::PrivilegeError::Execution(
                    PackageSource::Apt,
                    "simulated provider failure".into(),
                ));
            }
            if let crate::transaction::ProviderOperation::Apt { action, .. } = operation {
                *self.runner.installed.lock().expect("state lock") =
                    *action == OperationAction::Install;
            }
            Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: Some(0) })
        }
    }

    #[test]
    fn parses_qualified_and_unqualified_references() {
        assert_eq!(
            parse_package_ref("apt:libssl-dev", None),
            PackageRef { source: Some(PackageSource::Apt), query: "libssl-dev".into() }
        );
        assert_eq!(
            parse_package_ref("org.example.App", Some(PackageSource::Flatpak)),
            PackageRef { source: Some(PackageSource::Flatpak), query: "org.example.App".into() }
        );
        assert_eq!(
            parse_package_ref("libssl:amd64", None),
            PackageRef { source: None, query: "libssl:amd64".into() }
        );
    }

    #[test]
    fn unavailable_providers_are_isolated_from_search() {
        let registry = ProviderRegistry::with_runner(FakeRunner::apt_only());
        let report = registry.search("btop", None);
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].source, PackageSource::Apt);
        assert_eq!(report.issues.len(), 5);
        assert!(
            registry
                .sources()
                .iter()
                .any(|source| source.source == PackageSource::Flatpak && !source.available)
        );
    }

    #[test]
    fn search_aggregates_normalized_results_and_preserves_sources() {
        let registry = ProviderRegistry::with_runner(FakeRunner::all());
        let report = registry.search("btop", None);
        assert_eq!(report.results.len(), 3);
        assert_eq!(source_set(report.results.iter().map(|package| package.source)).len(), 3);
        assert!(report.results.iter().any(|package| package.source == PackageSource::Flatpak
            && package.origin.as_deref() == Some("flathub")));
    }

    #[test]
    fn normalized_models_serialize_without_ansi_or_missing_truth() {
        let registry = ProviderRegistry::with_runner(FakeRunner::all());
        let package = match registry
            .resolve(&PackageRef { source: Some(PackageSource::Apt), query: "btop".into() })
        {
            ResolveReport::Found { package, .. } => package,
            other => panic!("expected package, got {other:?}"),
        };
        let json = serde_json::to_string(&package).expect("package serializes");
        assert!(json.contains("\"source\":\"apt\""));
        assert!(!json.contains("\\u001b"));
    }

    #[test]
    fn transaction_plan_and_verification_use_fake_execution_only() {
        let runner = Arc::new(TransactionFakeRunner::new());
        let registry = ProviderRegistry::with_runner(runner.clone());
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "btop".into() },
            scope: None,
            channel: None,
        };
        let plan = registry.plan_transaction(&request).expect("plan");
        assert!(plan.authoritative_simulation);
        let result = registry
            .execute_transaction(
                plan,
                &FakeOperationExecutor { runner, start_record: None, fail: false },
            )
            .expect("execution");
        assert_eq!(result.status, crate::transaction::TransactionStatus::Succeeded);
        assert_eq!(result.verification, crate::transaction::VerificationResult::Verified);
    }

    #[test]
    fn history_starts_before_successful_execution_and_is_replaced() {
        let runner = Arc::new(TransactionFakeRunner::new());
        let registry = ProviderRegistry::with_runner(runner.clone());
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "btop".into() },
            scope: None,
            channel: None,
        };
        let plan = registry.plan_transaction(&request).expect("plan");
        let directory = temporary_test_directory("success");
        let history = crate::transaction::history::HistoryStore::at(&directory);
        let start_record = history.record_path(&plan.operation_id);
        let result = registry
            .execute_transaction_with_history(
                &request,
                plan,
                &FakeOperationExecutor {
                    runner,
                    start_record: Some(start_record.clone()),
                    fail: false,
                },
                &history,
            )
            .expect("execution");
        assert_eq!(result.status, crate::transaction::TransactionStatus::Succeeded);
        let body = fs::read_to_string(start_record).expect("final record exists");
        let record: crate::transaction::history::TransactionRecord =
            serde_json::from_str(&body).expect("final record parses");
        assert_eq!(record.lifecycle, crate::transaction::history::TransactionLifecycle::Succeeded);
        assert!(record.result.is_some());
        assert!(record.result.as_ref().is_some_and(|result| result.execution.message.is_none()));
        assert!(!body.contains("simulated"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn history_records_failed_execution_after_the_start_record() {
        let runner = Arc::new(TransactionFakeRunner::new());
        let registry = ProviderRegistry::with_runner(runner.clone());
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "btop".into() },
            scope: None,
            channel: None,
        };
        let plan = registry.plan_transaction(&request).expect("plan");
        let directory = temporary_test_directory("failure");
        let history = crate::transaction::history::HistoryStore::at(&directory);
        let path = history.record_path(&plan.operation_id);
        let result = registry.execute_transaction_with_history(
            &request,
            plan,
            &FakeOperationExecutor { runner, start_record: Some(path.clone()), fail: true },
            &history,
        );
        assert!(matches!(result, Err(crate::transaction::TransactionError::Execution(_))));
        let body = fs::read_to_string(path).expect("failed record exists");
        let record: crate::transaction::history::TransactionRecord =
            serde_json::from_str(&body).expect("failed record parses");
        assert_eq!(record.lifecycle, crate::transaction::history::TransactionLifecycle::Failed);
        assert!(record.result.is_some());
        assert!(record.result.as_ref().is_some_and(|result| result.execution.message.is_none()));
        let _ = fs::remove_dir_all(directory);
    }

    fn temporary_test_directory(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        std::env::temp_dir().join(format!("orbis-transaction-{label}-{unique}"))
    }

    #[test]
    fn transaction_resolution_refuses_cross_provider_ambiguity() {
        let registry = ProviderRegistry::with_runner(FakeRunner::all());
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: None, query: "btop".into() },
            scope: None,
            channel: None,
        };
        assert!(matches!(
            registry.plan_transaction(&request),
            Err(crate::transaction::TransactionError::Ambiguous { .. })
        ));
    }

    struct StageRecorder {
        stages: std::sync::Mutex<Vec<crate::progress::ExecutionStage>>,
        providers: std::sync::Mutex<Vec<(PackageSource, bool)>>,
    }

    impl crate::progress::ProgressObserver for StageRecorder {
        fn on_event(&self, event: &crate::progress::OperationEvent) {
            match event {
                crate::progress::OperationEvent::StageChanged { stage } => {
                    self.stages.lock().unwrap().push(*stage);
                }
                crate::progress::OperationEvent::Finished { stage, .. } => {
                    self.stages.lock().unwrap().push(*stage);
                }
                crate::progress::OperationEvent::ProviderStarted { source } => {
                    self.providers.lock().unwrap().push((*source, false));
                }
                crate::progress::OperationEvent::ProviderFinished { source, success } => {
                    self.providers.lock().unwrap().push((*source, *success));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn transaction_progress_stages_are_strictly_monotonic() {
        let runner = Arc::new(TransactionFakeRunner::new());
        let registry = ProviderRegistry::with_runner(runner.clone());
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "btop".into() },
            scope: None,
            channel: None,
        };
        let plan = registry.plan_transaction(&request).expect("plan");
        let directory = temporary_test_directory("monotonic-progress");
        let history = crate::transaction::history::HistoryStore::at(&directory);
        let start_record = history.record_path(&plan.operation_id);

        let recorder = Arc::new(StageRecorder {
            stages: std::sync::Mutex::new(Vec::new()),
            providers: std::sync::Mutex::new(Vec::new()),
        });

        let result = registry
            .execute_transaction_with_progress(
                &request,
                plan,
                &FakeOperationExecutor { runner, start_record: Some(start_record), fail: false },
                &history,
                recorder.as_ref(),
            )
            .expect("execution");
        assert_eq!(result.status, crate::transaction::TransactionStatus::Succeeded);
        assert_eq!(
            *recorder.providers.lock().unwrap(),
            vec![(PackageSource::Apt, false), (PackageSource::Apt, true)]
        );

        let recorded = recorder.stages.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                crate::progress::ExecutionStage::Preparing,
                crate::progress::ExecutionStage::Authenticating,
                crate::progress::ExecutionStage::Executing,
                crate::progress::ExecutionStage::Verifying,
                crate::progress::ExecutionStage::SavingResult,
                crate::progress::ExecutionStage::Completed,
            ]
        );

        for window in recorded.windows(2) {
            assert!(
                window[0] <= window[1],
                "stages must move forward monotonically: {:?} followed by {:?}",
                window[0],
                window[1]
            );
        }

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn update_inventory_reports_each_provider_as_it_finishes() {
        struct EmptyRunner;
        impl CommandRunner for EmptyRunner {
            fn is_available(&self, _program: &str) -> bool {
                true
            }

            fn run(&self, _command: &CommandSpec) -> Result<CommandOutput, ProcessError> {
                Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: Some(0) })
            }
        }

        struct EventRecorder(std::sync::Mutex<Vec<crate::progress::OperationEvent>>);
        impl crate::progress::ProgressObserver for EventRecorder {
            fn on_event(&self, event: &crate::progress::OperationEvent) {
                self.0.lock().unwrap().push(event.clone());
            }
        }

        let registry = ProviderRegistry::with_runner(EmptyRunner);
        let recorder = EventRecorder(std::sync::Mutex::new(Vec::new()));
        let _ = registry.updates_with_observer(None, &recorder);
        let events = recorder.0.lock().unwrap();
        let started = events
            .iter()
            .filter_map(|event| match event {
                crate::progress::OperationEvent::ProviderStarted { source } => Some(*source),
                _ => None,
            })
            .collect::<Vec<_>>();
        let finished = events
            .iter()
            .filter_map(|event| match event {
                crate::progress::OperationEvent::ProviderFinished { source, .. } => Some(*source),
                _ => None,
            })
            .collect::<Vec<_>>();
        let inventories = events
            .iter()
            .filter_map(|event| match event {
                crate::progress::OperationEvent::ProviderInventory { inventory } => {
                    Some(inventory.source)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!started.is_empty());
        assert_eq!(source_set(started.clone()), source_set(finished));
        assert_eq!(source_set(started), source_set(inventories));
    }

    #[test]
    fn metadata_only_refresh_completes_without_inventing_a_command() {
        struct AvailableRunner;
        impl CommandRunner for AvailableRunner {
            fn is_available(&self, _program: &str) -> bool {
                true
            }

            fn run(&self, _command: &CommandSpec) -> Result<CommandOutput, ProcessError> {
                Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: Some(0) })
            }
        }

        let registry = ProviderRegistry::with_runner(AvailableRunner);
        let plan = registry
            .maintenance_plan(MaintenanceAction::Refresh, Some(PackageSource::Cargo))
            .expect("developer refresh plan");
        let recorder = StageRecorder {
            stages: std::sync::Mutex::new(Vec::new()),
            providers: std::sync::Mutex::new(Vec::new()),
        };
        let result = registry
            .execute_maintenance_with_progress(
                &plan.providers[0],
                &FakeOperationExecutor {
                    runner: Arc::new(TransactionFakeRunner::new()),
                    start_record: None,
                    fail: false,
                },
                &recorder,
            )
            .expect("metadata-only refresh succeeds");

        assert_eq!(result.status, maintenance::MaintenanceStatus::Succeeded);
        assert_eq!(result.providers[0].verification, Some(VerificationResult::Verified));
        assert_eq!(
            *recorder.providers.lock().unwrap(),
            vec![(PackageSource::Cargo, false), (PackageSource::Cargo, true)]
        );
        assert_eq!(
            *recorder.stages.lock().unwrap(),
            vec![
                progress::ExecutionStage::Preparing,
                progress::ExecutionStage::Verifying,
                progress::ExecutionStage::SavingResult,
                progress::ExecutionStage::Completed,
            ]
        );
    }

    #[test]
    fn transaction_failure_progress_stages_are_strictly_monotonic() {
        let runner = Arc::new(TransactionFakeRunner::new());
        let registry = ProviderRegistry::with_runner(runner.clone());
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "btop".into() },
            scope: None,
            channel: None,
        };
        let plan = registry.plan_transaction(&request).expect("plan");
        let directory = temporary_test_directory("failure-progress");
        let history = crate::transaction::history::HistoryStore::at(&directory);
        let path = history.record_path(&plan.operation_id);

        let recorder = Arc::new(StageRecorder {
            stages: std::sync::Mutex::new(Vec::new()),
            providers: std::sync::Mutex::new(Vec::new()),
        });

        let result = registry.execute_transaction_with_progress(
            &request,
            plan,
            &FakeOperationExecutor { runner, start_record: Some(path), fail: true },
            &history,
            recorder.as_ref(),
        );
        assert!(result.is_err());
        assert_eq!(
            *recorder.providers.lock().unwrap(),
            vec![(PackageSource::Apt, false), (PackageSource::Apt, false)]
        );

        let recorded = recorder.stages.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                crate::progress::ExecutionStage::Preparing,
                crate::progress::ExecutionStage::Authenticating,
                crate::progress::ExecutionStage::Executing,
                crate::progress::ExecutionStage::SavingResult,
                crate::progress::ExecutionStage::Failed,
            ]
        );

        for window in recorded.windows(2) {
            assert!(
                window[0] <= window[1],
                "failure stages must move forward monotonically: {:?} followed by {:?}",
                window[0],
                window[1]
            );
        }

        let _ = fs::remove_dir_all(directory);
    }
}
