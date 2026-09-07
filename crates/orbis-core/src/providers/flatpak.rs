//! Flatpak provider using documented column-based output.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    diagnostics::DiagnosticCheck,
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
}
