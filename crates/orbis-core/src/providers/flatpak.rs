//! Flatpak provider using documented column-based output.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    diagnostics::DiagnosticCheck,
    maintenance::{
        MaintenanceAction, MaintenanceProvider, ProviderMaintenancePlan, ProviderUpdateInventory,
        UpdateCandidate, WhyConsumer, WhyReport, maintenance_operation_for,
    },
    models::{Package, PackageKind, PackageSource, ProviderCapabilities, SourceInfo},
    process::{CommandRunner, CommandSpec, SharedRunner},
    providers::{
        Provider, ProviderError, TransactionProvider, execute, expect_success, short_timeout,
    },
    transaction::{
        ChangeKind, InstallScope, OperationAction, OperationPlan, OperationRequest,
        PlanCompleteness, PlanConfidence, PrivilegeRequirement, ProviderOperation, RiskLevel,
        TransactionError, VerificationResult, WarningLevel, base_risk, target_change,
    },
};

/// Flatpak provider.
pub struct FlatpakProvider {
    runner: SharedRunner,
}

impl FlatpakProvider {
    /// Creates a Flatpak provider around an injected command runner.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn available(&self) -> bool {
        self.runner.is_available("flatpak")
    }

    fn installed(&self) -> Result<BTreeMap<String, String>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read installed Flatpak state",
            CommandSpec::new(
                "flatpak",
                [
                    "list",
                    "--app",
                    "--columns=application,version,branch,origin,arch,name,description",
                ],
            )
            .with_timeout(short_timeout()),
        )?;
        let output =
            expect_success(PackageSource::Flatpak, "read installed Flatpak state", output)?;
        Ok(output
            .stdout
            .lines()
            .filter_map(parse_columns)
            .filter_map(|fields| {
                let id = fields.first()?.to_owned();
                let version = fields.get(1).cloned().unwrap_or_default();
                Some((id, version))
            })
            .collect())
    }

    fn remotes(&self) -> Result<Vec<String>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read Flatpak remotes",
            CommandSpec::new("flatpak", ["remotes", "--columns=name"])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Flatpak, "read Flatpak remotes", output)?;
        Ok(output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn info_columns(&self, package_id: &str) -> Result<Package, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read Flatpak application information",
            CommandSpec::new(
                "flatpak",
                ["info", "--columns=application,name,description,version,branch,origin,arch,homepage,installed-size", package_id],
            )
            .with_timeout(short_timeout()),
        )?;
        let output =
            expect_success(PackageSource::Flatpak, "read Flatpak application information", output)?;
        let fields =
            parse_columns(output.stdout.lines().next().unwrap_or_default()).ok_or_else(|| {
                ProviderError::NotFound {
                    package_source: PackageSource::Flatpak,
                    query: package_id.into(),
                }
            })?;
        package_from_fields(fields, None)
    }

    fn installed_in_scope(
        &self,
        scope: InstallScope,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let scope_flag = match scope {
            InstallScope::System => "--system",
            InstallScope::User => "--user",
        };
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read scoped Flatpak state",
            CommandSpec::new(
                "flatpak",
                [
                    scope_flag,
                    "list",
                    "--app",
                    "--columns=application,version,branch,origin,arch,name,description",
                ],
            )
            .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Flatpak, "read scoped Flatpak state", output)?;
        Ok(output
            .stdout
            .lines()
            .filter_map(parse_columns)
            .filter_map(|fields| {
                let id = fields.first()?.to_owned();
                Some((id, fields.get(1).cloned().unwrap_or_default()))
            })
            .collect())
    }

    fn installed_scopes(&self, package_id: &str) -> Result<Vec<InstallScope>, ProviderError> {
        let mut scopes = Vec::new();
        for scope in [InstallScope::System, InstallScope::User] {
            if self.installed_in_scope(scope)?.contains_key(package_id) {
                scopes.push(scope);
            }
        }
        Ok(scopes)
    }

    fn remote_details(
        &self,
        package_id: &str,
        scope: InstallScope,
        preferred_remote: Option<&str>,
    ) -> Result<Option<RemoteDetails>, ProviderError> {
        let remotes = if let Some(remote) = preferred_remote {
            vec![remote.to_owned()]
        } else {
            self.remotes()?
        };
        for remote in remotes {
            let scope_flag = match scope {
                InstallScope::System => "--system",
                InstallScope::User => "--user",
            };
            let output = execute(
                &self.runner,
                PackageSource::Flatpak,
                "read Flatpak remote package information",
                CommandSpec::new(
                    "flatpak",
                    [scope_flag, "remote-info", "--show-details", remote.as_str(), package_id],
                )
                .with_timeout(short_timeout()),
            )?;
            if !output.success() {
                continue;
            }
            let mut details = parse_remote_details(&output.stdout);
            details.remote = Some(remote);
            return Ok(Some(details));
        }
        Ok(None)
    }

    fn update_inventory_scope(
        &self,
        scope: InstallScope,
    ) -> Result<Vec<UpdateCandidate>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "inspect available Flatpak updates",
            CommandSpec::new(
                "flatpak",
                [
                    scope_flag(scope),
                    "remote-ls",
                    "--updates",
                    "--columns=ref,application,name,version,arch,branch,origin,download-size,installed-size",
                ],
            )
            .with_timeout(short_timeout()),
        )?;
        let output =
            expect_success(PackageSource::Flatpak, "inspect available Flatpak updates", output)?;
        let installed = self.installed_records(scope)?;
        Ok(output
            .stdout
            .lines()
            .filter_map(parse_columns)
            .filter_map(|fields| update_candidate_from_fields(fields, scope, &installed))
            .collect())
    }

    fn installed_records(
        &self,
        scope: InstallScope,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read scoped Flatpak versions",
            CommandSpec::new(
                "flatpak",
                [
                    scope_flag(scope),
                    "list",
                    "--columns=ref,application,version,arch,branch,origin,name,runtime",
                ],
            )
            .with_timeout(short_timeout()),
        )?;
        let output =
            expect_success(PackageSource::Flatpak, "read scoped Flatpak versions", output)?;
        Ok(output
            .stdout
            .lines()
            .filter_map(parse_columns)
            .filter_map(|fields| {
                let id = fields.get(1).cloned().or_else(|| fields.first().cloned())?;
                Some((id, fields.get(2).cloned().unwrap_or_default()))
            })
            .collect())
    }
}

impl Provider for FlatpakProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Flatpak
    }

    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let notes = if !available {
            vec!["Install Flatpak to discover applications from configured remotes.".into()]
        } else {
            match self.remotes() {
                Ok(remotes) if remotes.is_empty() => {
                    vec!["No Flatpak remotes are configured.".into()]
                }
                Ok(remotes) => vec![format!("Remotes: {}", remotes.join(", "))],
                Err(error) => vec![format!("Remotes could not be inspected: {error}")],
            }
        };
        SourceInfo {
            source: PackageSource::Flatpak,
            available,
            state: if available { "ready".into() } else { "unavailable".into() },
            backend: Some("Flatpak CLI".into()),
            capabilities: capabilities(),
            notes,
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        let installed = self.installed().unwrap_or_default();
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "search Flatpak applications",
            CommandSpec::new(
                "flatpak",
                ["search", "--columns=application,name,description,version,branch,remotes", query],
            )
            .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Flatpak, "search Flatpak applications", output)?;
        Ok(output
            .stdout
            .lines()
            .filter_map(parse_columns)
            .filter_map(|fields| package_from_fields(fields, Some(&installed)).ok())
            .take(60)
            .collect())
    }

    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        match self.info_columns(package_id) {
            Ok(mut package) => {
                let installed = self.installed().unwrap_or_default();
                package.installed = Some(installed.contains_key(&package.provider_id));
                Ok(package)
            }
            Err(ProviderError::Command { .. }) | Err(ProviderError::NotFound { .. }) => {
                let candidates = self.search(package_id)?;
                let candidate = candidates
                    .into_iter()
                    .find(|package| {
                        package.provider_id == package_id
                            || package.name.eq_ignore_ascii_case(package_id)
                    })
                    .ok_or_else(|| ProviderError::NotFound {
                        package_source: PackageSource::Flatpak,
                        query: package_id.into(),
                    })?;
                Ok(candidate)
            }
            Err(error) => Err(error),
        }
    }

    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Flatpak,
                "Flatpak CLI",
                "Install Flatpak to enable Flatpak discovery.",
            );
        }
        match self.remotes() {
            Ok(_) => {
                DiagnosticCheck::passed(PackageSource::Flatpak, "Flatpak CLI and remotes respond")
            }
            Err(error) => DiagnosticCheck::failed(
                PackageSource::Flatpak,
                "Flatpak remotes",
                &error.to_string(),
            ),
        }
    }
}

impl TransactionProvider for FlatpakProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        if request.channel.is_some() {
            return Err(TransactionError::InvalidRequest(
                "`--channel` is only supported for Snap operations".into(),
            ));
        }
        validate_package_id(&target.provider_id)?;
        let observed_scopes = self.installed_scopes(&target.provider_id)?;
        let scope = match request.action {
            OperationAction::Install => request.scope.unwrap_or(InstallScope::System),
            OperationAction::Remove => match request.scope {
                Some(scope) => scope,
                None => match observed_scopes.as_slice() {
                    [scope] => *scope,
                    [] => {
                        return Err(TransactionError::Planning(format!(
                            "Flatpak application `{}` is not installed",
                            target.provider_id
                        )));
                    }
                    _ => {
                        return Err(TransactionError::Planning(format!(
                            "Flatpak application `{}` is installed in multiple scopes; specify `--scope system` or `--scope user`",
                            target.provider_id
                        )));
                    }
                },
            },
        };
        let installed_in_scope = observed_scopes.contains(&scope);
        match (request.action, installed_in_scope) {
            (OperationAction::Install, true) => {
                return Err(TransactionError::Planning(format!(
                    "Flatpak application `{}` is already installed in the {} scope",
                    target.provider_id,
                    scope.label()
                )));
            }
            (OperationAction::Remove, false) => {
                return Err(TransactionError::Planning(format!(
                    "Flatpak application `{}` is not installed in the {} scope",
                    target.provider_id,
                    scope.label()
                )));
            }
            _ => {}
        }

        let mut resolved_target = target.clone();
        let mut plan = OperationPlan::new(request.action, resolved_target.clone(), scope);
        plan.privilege = if scope == InstallScope::User {
            PrivilegeRequirement::None
        } else {
            PrivilegeRequirement::Administrator
        };
        plan.changes.push(target_change(&plan));
        plan.completeness = if request.action == OperationAction::Install {
            PlanCompleteness::Partial
        } else {
            PlanCompleteness::Complete
        };
        plan.confidence = if request.action == OperationAction::Install {
            PlanConfidence::Medium
        } else {
            PlanConfidence::High
        };
        plan.authoritative_simulation = false;
        plan.risk = base_risk(request.action, target.kind);

        if request.action == OperationAction::Install {
            let details =
                self.remote_details(&target.provider_id, scope, target.origin.as_deref())?;
            if let Some(details) = details {
                resolved_target.version = details.version.or(resolved_target.version);
                resolved_target.origin = details.remote.or(resolved_target.origin);
                resolved_target.architecture =
                    details.architecture.or(resolved_target.architecture);
                resolved_target.size_bytes = details.download_size.or(resolved_target.size_bytes);
                if let Some(runtime) = details.runtime {
                    let runtime_id = runtime.split('/').next().unwrap_or(runtime.as_str());
                    if !runtime_id.is_empty()
                        && !self.installed_in_scope(scope)?.contains_key(runtime_id)
                    {
                        plan.changes.push(crate::transaction::PlannedChange {
                            kind: ChangeKind::Install,
                            package_id: runtime_id.to_owned(),
                            name: None,
                            version: None,
                            reason: Some("required runtime reported by Flatpak metadata".into()),
                        });
                    }
                }
                plan.download_size_bytes = details.download_size;
                plan.disk_delta_bytes = details.installed_size.map(|size| size as i64);
            }
            if resolved_target.origin.is_none() {
                plan.completeness = PlanCompleteness::Unknown;
                plan.confidence = PlanConfidence::Low;
                plan.risk = RiskLevel::Blocked;
                plan.warnings.push(crate::transaction::PlanWarning {
                    level: WarningLevel::Blocked,
                    message:
                        "Flatpak did not identify an exact configured remote for this application."
                            .into(),
                });
            }
            plan.warnings.push(crate::transaction::PlanWarning {
                level: WarningLevel::Caution,
                message: "Flatpak resolves runtimes and extensions at commit time; this plan is not a dependency simulation.".into(),
            });
        } else {
            plan.warnings.push(crate::transaction::PlanWarning {
                level: WarningLevel::Info,
                message: "Flatpak data is retained because Orbis does not request `--delete-data`."
                    .into(),
            });
        }
        plan.target = resolved_target;
        Ok(plan)
    }

    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_package_id(&plan.target.provider_id)?;
        let remote =
            (plan.action == OperationAction::Install).then(|| plan.target.origin.clone()).flatten();
        if let Some(remote) = &remote {
            validate_remote(remote)?;
        }
        Ok(ProviderOperation::Flatpak {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
            scope: plan.scope,
            remote,
        })
    }

    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        let installed = self.installed_scopes(&plan.target.provider_id)?.contains(&plan.scope);
        let expected = plan.action == OperationAction::Install;
        Ok(if installed == expected {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        })
    }
}

impl MaintenanceProvider for FlatpakProvider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        let mut candidates = Vec::new();
        for scope in [InstallScope::System, InstallScope::User] {
            candidates.extend(self.update_inventory_scope(scope)?);
        }
        Ok(ProviderUpdateInventory {
            source: PackageSource::Flatpak,
            available: true,
            candidates,
            notes: vec![
                "Read-only inventory uses scoped `flatpak remote-ls --updates` metadata.".into(),
                "System and user installations remain separate candidates.".into(),
            ],
            metadata_state: Some("local_flatpak_remote_metadata".into()),
        })
    }

    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        Ok([InstallScope::System, InstallScope::User]
            .into_iter()
            .map(|scope| ProviderMaintenancePlan {
                operation_id: flatpak_maintenance_id(MaintenanceAction::Refresh, scope),
                source: PackageSource::Flatpak,
                action: MaintenanceAction::Refresh,
                scope: Some(scope),
                candidates: Vec::new(),
                cleanup_candidates: Vec::new(),
                privilege: if scope == InstallScope::System {
                    PrivilegeRequirement::Administrator
                } else {
                    PrivilegeRequirement::None
                },
                completeness: PlanCompleteness::Complete,
                confidence: PlanConfidence::High,
                authoritative_simulation: false,
                risk: RiskLevel::Normal,
                supported: true,
                mutates: true,
                warnings: Vec::new(),
                notes: vec!["Only Flatpak AppStream metadata is refreshed; installed refs are not upgraded.".into()],
                download_size_bytes: None,
                disk_delta_bytes: None,
            })
            .collect())
    }

    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        let mut plans = Vec::new();
        for scope in [InstallScope::System, InstallScope::User] {
            let inventory = self.update_inventory_scope(scope)?;
            let download_size_bytes = if inventory.is_empty() {
                None
            } else {
                Some(
                    inventory
                        .iter()
                        .filter_map(|candidate| {
                            candidate
                                .metadata
                                .get("download_size_bytes")
                                .and_then(|value| value.parse::<u64>().ok())
                        })
                        .sum(),
                )
            };
            plans.push(ProviderMaintenancePlan {
                operation_id: flatpak_maintenance_id(MaintenanceAction::Upgrade, scope),
                source: PackageSource::Flatpak,
                action: MaintenanceAction::Upgrade,
                scope: Some(scope),
                candidates: inventory,
                cleanup_candidates: Vec::new(),
                privilege: if scope == InstallScope::System {
                    PrivilegeRequirement::Administrator
                } else {
                    PrivilegeRequirement::None
                },
                completeness: PlanCompleteness::Partial,
                confidence: PlanConfidence::Medium,
                authoritative_simulation: false,
                risk: RiskLevel::Caution,
                // Flatpak documents that update may also offer unused EOL runtime removal.
                // Without a no-action interface that proves the complete commit, Orbis does
                // not run this provider automatically in the unified `upgrade` path.
                supported: false,
                mutates: true,
                warnings: vec![crate::transaction::PlanWarning {
                    level: WarningLevel::Blocked,
                    message: "Flatpak update may offer unused end-of-life runtime removal; exact non-mutating impact is not available, so automatic unified execution is disabled.".into(),
                }],
                notes: vec!["Review and run Flatpak's own update flow separately when this limitation is acceptable.".into()],
                download_size_bytes,
                disk_delta_bytes: None,
            });
        }
        Ok(plans)
    }

    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        Ok(ProviderMaintenancePlan::blocked(
            PackageSource::Flatpak,
            MaintenanceAction::Cleanup,
            "Exact unused Flatpak refs cannot be enumerated through the currently supported non-mutating planning path; Orbis will not run `flatpak uninstall --unused`.",
        ))
    }

    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        let mut used_by = Vec::new();
        let mut is_installed_app = false;
        for scope in [InstallScope::System, InstallScope::User] {
            let output = execute(
                &self.runner,
                PackageSource::Flatpak,
                "explain Flatpak runtime use",
                CommandSpec::new(
                    "flatpak",
                    [scope_flag(scope), "list", "--app", "--columns=application,name,runtime"],
                )
                .with_timeout(short_timeout()),
            )?;
            let output =
                expect_success(PackageSource::Flatpak, "explain Flatpak runtime use", output)?;
            for fields in output.stdout.lines().filter_map(parse_columns) {
                let Some(application) = fields.first() else { continue };
                if application == &package.provider_id {
                    is_installed_app = true;
                }
                if fields.get(2).is_some_and(|runtime| runtime.contains(&package.provider_id)) {
                    used_by.push(WhyConsumer {
                        source: PackageSource::Flatpak,
                        provider_id: application.clone(),
                        name: fields.get(1).cloned().unwrap_or_else(|| application.clone()),
                        relationship: "uses this Flatpak runtime".into(),
                        scope: Some(scope),
                    });
                }
            }
        }
        let runtime_like = package.provider_id.contains(".Platform")
            || package.provider_id.contains(".Sdk")
            || package.provider_id.contains(".Locale")
            || !is_installed_app;
        Ok(WhyReport {
            package: package.clone(),
            installed_as: if runtime_like {
                "Shared runtime or extension".into()
            } else {
                "Installed application".into()
            },
            used_by,
            evidence: vec!["Scoped Flatpak app listings and their declared runtime fields.".into()],
            removal_advice: if runtime_like {
                "Keep this runtime while installed applications use it; Flatpak's own unused-ref logic is stronger evidence than name-based guesses.".into()
            } else {
                "This is an application ref; removal affects the selected Flatpak installation scope.".into()
            },
            orbis_history: Vec::new(),
            notes: Vec::new(),
        })
    }

    fn maintenance_operation(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError> {
        let operation = maintenance_operation_for(plan).ok_or_else(|| ProviderError::Parse {
            package_source: PackageSource::Flatpak,
            operation: "build maintenance operation".into(),
            technical: "Flatpak maintenance plan has an unsupported shape".into(),
        })?;
        Ok(ProviderOperation::Maintenance { operation })
    }

    fn verify_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError> {
        match plan.action {
            MaintenanceAction::Refresh => Ok(VerificationResult::Verified),
            MaintenanceAction::Upgrade => Ok(VerificationResult::PartiallyVerified),
            MaintenanceAction::Cleanup => Ok(VerificationResult::Failed),
        }
    }
}

#[derive(Default)]
struct RemoteDetails {
    remote: Option<String>,
    version: Option<String>,
    architecture: Option<String>,
    runtime: Option<String>,
    download_size: Option<u64>,
    installed_size: Option<u64>,
}

fn parse_remote_details(output: &str) -> RemoteDetails {
    let mut details = RemoteDetails::default();
    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "version" => details.version = (!value.is_empty()).then(|| value.to_owned()),
            "arch" | "architecture" => {
                details.architecture = (!value.is_empty()).then(|| value.to_owned())
            }
            "runtime" => details.runtime = (!value.is_empty()).then(|| value.to_owned()),
            "download size" => details.download_size = crate::transaction::parse_human_size(value),
            "installed size" => {
                details.installed_size = crate::transaction::parse_human_size(value)
            }
            _ => {}
        }
    }
    details
}

fn validate_package_id(package_id: &str) -> Result<(), TransactionError> {
    if package_id.is_empty()
        || package_id.starts_with('-')
        || package_id.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, ';' | '&' | '|')
        })
    {
        return Err(TransactionError::InvalidRequest(
            "Flatpak IDs must be exact application names without whitespace or option characters"
                .into(),
        ));
    }
    Ok(())
}

fn validate_remote(remote: &str) -> Result<(), TransactionError> {
    if remote.is_empty()
        || remote.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, ';' | '&' | '|')
        })
    {
        return Err(TransactionError::Planning(
            "Flatpak returned an invalid remote name; the operation is blocked".into(),
        ));
    }
    Ok(())
}

fn capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        search: true,
        info: true,
        installed_state: true,
        installed_list: true,
        mutations: true,
    }
}

fn parse_columns(line: &str) -> Option<Vec<String>> {
    let fields: Vec<String> = if line.contains('\t') {
        line.split('\t').map(|field| field.trim().to_owned()).collect()
    } else {
        line.split("  ")
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(str::to_owned)
            .collect()
    };
    (!fields.is_empty() && fields.first().is_some_and(|field| !field.is_empty())).then_some(fields)
}

fn scope_flag(scope: InstallScope) -> &'static str {
    match scope {
        InstallScope::System => "--system",
        InstallScope::User => "--user",
    }
}

fn flatpak_maintenance_id(action: MaintenanceAction, scope: InstallScope) -> String {
    format!(
        "maint-flatpak-{}-{}-{}",
        action.label().to_ascii_lowercase(),
        scope.label(),
        std::process::id()
    )
}

fn update_candidate_from_fields(
    fields: Vec<String>,
    scope: InstallScope,
    installed: &BTreeMap<String, String>,
) -> Option<UpdateCandidate> {
    let reference = fields.first()?.clone();
    let provider_id = fields
        .get(1)
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| reference.split('/').nth(1).map(str::to_owned))?;
    if provider_id.is_empty() {
        return None;
    }
    let available_version = fields.get(3).cloned().filter(|value| !value.is_empty());
    let mut metadata = BTreeMap::new();
    if let Some(reference) = (!reference.is_empty()).then_some(reference) {
        metadata.insert("ref".into(), reference);
    }
    if let Some(size) = fields.get(7).and_then(|value| crate::transaction::parse_human_size(value))
    {
        metadata.insert("download_size_bytes".into(), size.to_string());
    }
    if let Some(size) = fields.get(8).and_then(|value| crate::transaction::parse_human_size(value))
    {
        metadata.insert("installed_size_bytes".into(), size.to_string());
    }
    Some(UpdateCandidate {
        source: PackageSource::Flatpak,
        provider_id: provider_id.clone(),
        name: fields
            .get(2)
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| provider_id.clone()),
        current_version: installed.get(&provider_id).cloned().filter(|value| !value.is_empty()),
        available_version,
        architecture: fields.get(4).cloned().filter(|value| !value.is_empty()),
        scope: Some(scope),
        channel: fields.get(5).cloned().filter(|value| !value.is_empty()),
        held: None,
        security_relevance: None,
        notes: vec![
            "Flatpak update metadata does not provide an APT-style security classification.".into(),
        ],
        metadata,
    })
}

fn package_from_fields(
    fields: Vec<String>,
    installed: Option<&BTreeMap<String, String>>,
) -> Result<Package, ProviderError> {
    let provider_id = fields.first().cloned().unwrap_or_default();
    if provider_id.is_empty() {
        return Err(ProviderError::Parse {
            package_source: PackageSource::Flatpak,
            operation: "parse Flatpak metadata".into(),
            technical: format!("missing application ID in fields: {fields:?}"),
        });
    }
    let name = fields
        .get(1)
        .cloned()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| provider_id.clone());
    let summary = fields.get(2).cloned().filter(|value| !value.is_empty());
    let version = fields.get(3).cloned().filter(|value| !value.is_empty());
    let branch = fields.get(4).cloned().filter(|value| !value.is_empty());
    let origin = fields.get(5).cloned().filter(|value| !value.is_empty());
    let architecture = fields.get(6).cloned().filter(|value| !value.is_empty());
    let homepage = fields.get(7).cloned().filter(|value| !value.is_empty());
    let size_bytes = fields.get(8).and_then(|value| value.parse().ok());
    let installed_state = installed.map(|packages| packages.contains_key(&provider_id));
    let mut metadata = BTreeMap::new();
    if let Some(branch) = branch {
        metadata.insert("branch".into(), branch);
    }
    Ok(Package {
        source: PackageSource::Flatpak,
        provider_id,
        name,
        version,
        summary: summary.clone(),
        description: summary.clone(),
        installed: installed_state,
        kind: Some(PackageKind::Application),
        origin,
        architecture,
        homepage,
        license: None,
        size_bytes,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tabular_flatpak_columns() {
        let fields =
            parse_columns("org.example.App\tExample App\tA useful app\t1.2\tstable\tflathub")
                .expect("fields");
        let package = package_from_fields(fields, None).expect("package");
        assert_eq!(package.provider_id, "org.example.App");
        assert_eq!(package.origin.as_deref(), Some("flathub"));
        assert_eq!(package.kind, Some(PackageKind::Application));
    }

    #[test]
    fn normalizes_update_scope_and_versions_separately() {
        let installed = BTreeMap::from([("org.example.App".into(), "1.0".into())]);
        let system = update_candidate_from_fields(
            parse_columns("app/org.example.App/x86_64/stable\torg.example.App\tExample App\t2.0\tx86_64\tstable\tflathub\t4.0 MB\t10.0 MB").expect("fields"),
            InstallScope::System,
            &installed,
        )
        .expect("system update");
        let user = update_candidate_from_fields(
            parse_columns("app/org.example.App/x86_64/stable\torg.example.App\tExample App\t2.1\tx86_64\tstable\tflathub\t5.0 MB\t11.0 MB").expect("fields"),
            InstallScope::User,
            &installed,
        )
        .expect("user update");
        assert_eq!(system.scope, Some(InstallScope::System));
        assert_eq!(user.scope, Some(InstallScope::User));
        assert_eq!(system.current_version.as_deref(), Some("1.0"));
        assert_eq!(user.available_version.as_deref(), Some("2.1"));
    }

    #[test]
    fn flatpak_cleanup_is_explicitly_blocked() {
        let plan = ProviderMaintenancePlan::blocked(
            PackageSource::Flatpak,
            MaintenanceAction::Cleanup,
            "exact planning unavailable",
        );
        assert!(!plan.executable());
        assert_eq!(plan.risk, RiskLevel::Blocked);
    }
}
