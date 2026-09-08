//! APT metadata provider using non-mutating `apt-cache` and `dpkg-query`.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use crate::{
    diagnostics::DiagnosticCheck,
    maintenance::{
        CleanupCandidate, MaintenanceAction, MaintenanceProvider, ProviderMaintenancePlan,
        ProviderUpdateInventory, UpdateCandidate, WhyConsumer, WhyReport,
        maintenance_operation_for,
    },
    models::{Package, PackageKind, PackageSource, ProviderCapabilities, SourceInfo},
    process::{CommandRunner, CommandSpec, SharedRunner},
    providers::{
        Provider, ProviderError, TransactionProvider, execute, expect_success, short_timeout,
    },
    transaction::{
        ChangeKind, InstallScope, OperationAction, OperationPlan, OperationRequest, PackageState,
        PlanCompleteness, PlanConfidence, PrivilegeRequirement, ProviderOperation, RiskLevel,
        TransactionError, VerificationResult, WarningLevel, base_risk, target_change,
    },
};

/// APT provider. Nala is detected as an optional frontend, but APT remains the source identity.
pub struct AptProvider {
    runner: SharedRunner,
}

impl AptProvider {
    /// Creates an APT provider around an injected command runner.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn available(&self) -> bool {
        self.runner.is_available("apt-cache")
    }

    fn installed_versions(&self, names: &[String]) -> BTreeMap<String, String> {
        if names.is_empty() || !self.runner.is_available("dpkg-query") {
            return BTreeMap::new();
        }
        let mut args =
            vec!["-W".to_owned(), "-f=${binary:Package}\t${Status}\t${Version}\n".to_owned()];
        args.extend(names.iter().cloned());
        let Ok(output) = execute(
            &self.runner,
            PackageSource::Apt,
            "read installed package state",
            CommandSpec::new("dpkg-query", args).with_timeout(short_timeout()),
        ) else {
            return BTreeMap::new();
        };
        if !output.success() {
            return BTreeMap::new();
        }
        let mut installed = BTreeMap::new();
        for line in output.stdout.lines() {
            let mut fields = line.split('\t');
            let Some(name) = fields.next().map(str::to_owned) else { continue };
            let Some(status) = fields.next() else { continue };
            let Some(version) = fields.next().map(str::to_owned) else { continue };
            if status.contains("install ok installed") {
                installed.insert(name.clone(), version.clone());
                if let Some(base_name) = name.split_once(':').map(|(base, _)| base) {
                    installed.entry(base_name.to_owned()).or_insert(version);
                }
            }
        }
        installed
    }

    fn package_from_record(&self, record: &BTreeMap<String, String>) -> Package {
        let provider_id = value(record, "Package").unwrap_or_default();
        let summary = value(record, "Description-en")
            .or_else(|| value(record, "Description"))
            .and_then(|text| {
                text.lines()
                    .next()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
            });
        let description = value(record, "Description-en").or_else(|| value(record, "Description"));
        let kind = classify(&provider_id, summary.as_deref(), description.as_deref());
        let mut metadata = BTreeMap::new();
        for field in ["Section", "Maintainer", "Source", "Installed-Size"] {
            if let Some(value) = record.get(field) {
                metadata.insert(field.to_ascii_lowercase(), value.clone());
            }
        }
        Package {
            source: PackageSource::Apt,
            name: provider_id.clone(),
            provider_id,
            version: value(record, "Version"),
            summary,
            description,
            installed: None,
            kind,
            origin: value(record, "Origin").or_else(|| value(record, "Section")),
            architecture: value(record, "Architecture"),
            homepage: value(record, "Homepage"),
            license: value(record, "License"),
            size_bytes: value(record, "Size").and_then(|size| size.parse().ok()),
            metadata,
        }
    }
}

impl Provider for AptProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Apt
    }

    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let nala = self.runner.is_available("nala");
        SourceInfo {
            source: PackageSource::Apt,
            available,
            state: if available { "ready".into() } else { "unavailable".into() },
            backend: Some(if nala {
                "Nala available; APT metadata backend".into()
            } else {
                "APT tools; Nala not installed".into()
            }),
            capabilities: capabilities(),
            notes: if available {
                vec!["Read-only metadata uses apt-cache and dpkg-query.".into()]
            } else {
                vec!["Install apt to enable Debian/Ubuntu package discovery.".into()]
            },
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Apt,
                program: "apt-cache".into(),
            });
        }
        let output = execute(
            &self.runner,
            PackageSource::Apt,
            "search package metadata",
            CommandSpec::new("apt-cache", ["--no-generate", "search", "--names-only", "--", query])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Apt, "search package metadata", output)?;
        let names_and_summaries: Vec<_> = output
            .stdout
            .lines()
            .filter_map(|line| line.split_once(" - "))
            .filter_map(|(name, summary)| {
                let name = name.trim();
                (!name.is_empty()).then_some((name.to_owned(), summary.trim().to_owned()))
            })
            .take(60)
            .collect();
        let names: Vec<String> = names_and_summaries.iter().map(|(name, _)| name.clone()).collect();
        let installed = self.installed_versions(&names);
        Ok(names_and_summaries
            .into_iter()
            .map(|(name, summary)| Package {
                source: PackageSource::Apt,
                provider_id: name.clone(),
                name: name.clone(),
                version: installed.get(&name).cloned(),
                summary: (!summary.is_empty()).then_some(summary.clone()),
                description: None,
                installed: Some(installed.contains_key(&name)),
                kind: classify(&name, Some(&summary), None),
                origin: None,
                architecture: None,
                homepage: None,
                license: None,
                size_bytes: None,
                metadata: BTreeMap::new(),
            })
            .collect())
    }

    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Apt,
                program: "apt-cache".into(),
            });
        }
        let output = execute(
            &self.runner,
            PackageSource::Apt,
            "read package information",
            CommandSpec::new(
                "apt-cache",
                ["--no-generate", "show", "--no-all-versions", "--", package_id],
            )
            .with_timeout(short_timeout()),
        )?;
        if !output.success() {
            let detail = format!("{} {}", output.stdout, output.stderr).to_ascii_lowercase();
            if detail.contains("no packages found") || detail.contains("unable to locate") {
                return Err(ProviderError::NotFound {
                    package_source: PackageSource::Apt,
                    query: package_id.into(),
                });
            }
        }
        let output = expect_success(PackageSource::Apt, "read package information", output)?;
        let record = parse_control_records(&output.stdout).into_iter().next().ok_or_else(|| {
            ProviderError::NotFound { package_source: PackageSource::Apt, query: package_id.into() }
        })?;
        let mut package = self.package_from_record(&record);
        if package.provider_id.is_empty() {
            return Err(ProviderError::Parse {
                package_source: PackageSource::Apt,
                operation: "read package information".into(),
                technical: "APT record did not contain a Package field".into(),
            });
        }
        let installed = self.installed_versions(std::slice::from_ref(&package.provider_id));
        package.installed = Some(installed.contains_key(&package.provider_id));
        if package.installed == Some(true) {
            package.version = installed.get(&package.provider_id).cloned().or(package.version);
        }
        Ok(package)
    }

    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Apt,
                "APT tools",
                "Install apt to enable APT discovery.",
            );
        }
        let result = execute(
            &self.runner,
            PackageSource::Apt,
            "check local package metadata",
            CommandSpec::new("apt-cache", ["--no-generate", "stats"]).with_timeout(short_timeout()),
        );
        match result.and_then(|output| {
            expect_success(PackageSource::Apt, "check local package metadata", output)
        }) {
            Ok(_) => DiagnosticCheck::passed(PackageSource::Apt, "APT metadata responds"),
            Err(error) => {
                DiagnosticCheck::failed(PackageSource::Apt, "APT metadata", &error.to_string())
            }
        }
    }
}

impl TransactionProvider for AptProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        if request.scope == Some(InstallScope::User) {
            return Err(TransactionError::InvalidRequest(
                "APT packages use the system scope; omit `--scope user`".into(),
            ));
        }
        if request.channel.is_some() {
            return Err(TransactionError::InvalidRequest(
                "`--channel` is only supported for Snap operations".into(),
            ));
        }
        validate_package_id(&target.provider_id)?;
        let current = target.installed.unwrap_or(false);
        match (request.action, current) {
            (OperationAction::Install, true) => {
                return Err(TransactionError::Planning(format!(
                    "APT package `{}` is already installed",
                    target.provider_id
                )));
            }
            (OperationAction::Remove, false) => {
                return Err(TransactionError::Planning(format!(
                    "APT package `{}` is not installed",
                    target.provider_id
                )));
            }
            _ => {}
        }

        let action = match request.action {
            OperationAction::Install => "install",
            OperationAction::Remove => "remove",
        };
        let command = simulation_command(action, &target.provider_id);
        let output =
            execute(&self.runner, PackageSource::Apt, "simulate the APT transaction", command)?;
        let output = expect_success(PackageSource::Apt, "simulate the APT transaction", output)?;
        let mut plan = OperationPlan::new(request.action, target.clone(), InstallScope::System);
        plan.current_state =
            if current { PackageState::Installed } else { PackageState::NotInstalled };
        plan.privilege = PrivilegeRequirement::Administrator;
        plan.completeness = PlanCompleteness::Complete;
        plan.confidence = PlanConfidence::High;
        plan.authoritative_simulation = true;
        plan.changes = parse_simulation_changes(&output.stdout);
        if !plan.changes.iter().any(|change| change.package_id == target.provider_id) {
            plan.changes.push(target_change(&plan));
        }
        plan.download_size_bytes = find_apt_size(&output.stdout, "Need to get");
        plan.disk_delta_bytes = find_apt_disk_delta(&output.stdout);
        let extra_removals = plan
            .changes
            .iter()
            .filter(|change| {
                change.kind == ChangeKind::Remove && change.package_id != target.provider_id
            })
            .count();
        let essential_removal = output
            .stdout
            .to_ascii_lowercase()
            .contains("essential packages will be removed")
            || output.stderr.to_ascii_lowercase().contains("essential packages will be removed");
        plan.risk = if extra_removals > 0 || essential_removal {
            let message = if essential_removal {
                "APT identified an essential package removal; Orbis blocks this transaction."
                    .to_owned()
            } else {
                format!("APT would remove {extra_removals} additional package(s).")
            };
            plan.warnings
                .push(crate::transaction::PlanWarning { level: WarningLevel::Blocked, message });
            RiskLevel::Blocked
        } else {
            base_risk(request.action, target.kind)
        };
        plan.warnings.push(crate::transaction::PlanWarning {
            level: WarningLevel::Info,
            message: "APT simulation is read-only; no package database or index was changed."
                .into(),
        });
        Ok(plan)
    }

    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_package_id(&plan.target.provider_id)?;
        Ok(ProviderOperation::Apt {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
        })
    }

    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        match self.info(&plan.target.provider_id) {
            Ok(package) => {
                let installed = package.installed == Some(true);
                let expected = plan.action == OperationAction::Install;
                Ok(if installed == expected {
                    VerificationResult::Verified
                } else {
                    VerificationResult::Failed
                })
            }
            Err(ProviderError::NotFound { .. }) if plan.action == OperationAction::Remove => {
                Ok(VerificationResult::Verified)
            }
            Err(error) => Err(TransactionError::Verification(error.to_string())),
        }
    }
}

impl MaintenanceProvider for AptProvider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Apt,
                program: "apt-cache".into(),
            });
        }
        let output = execute(
            &self.runner,
            PackageSource::Apt,
            "inspect available APT upgrades",
            apt_simulation_command("upgrade"),
        )?;
        let output = expect_success(PackageSource::Apt, "inspect available APT upgrades", output)?;
        Ok(ProviderUpdateInventory {
            source: PackageSource::Apt,
            available: true,
            candidates: parse_apt_update_candidates(&output.stdout),
            notes: vec![
                "Read-only inventory uses the current local APT package index.".into(),
                "Orbis does not run apt-get update for `orbis update`; refresh metadata with `orbis refresh`.".into(),
            ],
            metadata_state: Some("current_local_index_unknown_freshness".into()),
        })
    }

    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Apt,
                program: "apt-get".into(),
            });
        }
        Ok(vec![ProviderMaintenancePlan {
            operation_id: maintenance_id(PackageSource::Apt, MaintenanceAction::Refresh),
            source: PackageSource::Apt,
            action: MaintenanceAction::Refresh,
            scope: None,
            candidates: Vec::new(),
            cleanup_candidates: Vec::new(),
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: false,
            risk: RiskLevel::Normal,
            supported: true,
            mutates: true,
            warnings: Vec::new(),
            notes: vec!["APT repository indexes will be refreshed; installed packages will not be upgraded.".into()],
            download_size_bytes: None,
            disk_delta_bytes: None,
        }])
    }

    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        let inventory = self.update_inventory()?;
        let output = execute(
            &self.runner,
            PackageSource::Apt,
            "simulate the APT upgrade",
            apt_simulation_command("upgrade"),
        )?;
        let output = expect_success(PackageSource::Apt, "simulate the APT upgrade", output)?;
        let changes = parse_simulation_changes(&output.stdout);
        let removals: Vec<_> =
            changes.iter().filter(|change| change.kind == ChangeKind::Remove).collect();
        let kept_back = parse_kept_back(&output.stdout);
        let mut warnings = Vec::new();
        if !kept_back.is_empty() {
            warnings.push(crate::transaction::PlanWarning {
                level: WarningLevel::Info,
                message: format!("APT will keep back: {}.", kept_back.join(", ")),
            });
        }
        let (risk, supported) = if removals.is_empty() {
            (RiskLevel::Normal, true)
        } else {
            warnings.push(crate::transaction::PlanWarning {
                level: WarningLevel::Blocked,
                message: format!(
                    "APT upgrade simulation reported {} removal(s); Orbis blocks this safe-upgrade plan.",
                    removals.len()
                ),
            });
            (RiskLevel::Blocked, false)
        };
        warnings.push(crate::transaction::PlanWarning {
            level: WarningLevel::Info,
            message: "Execution uses `apt-get upgrade` and never full-upgrade/dist-upgrade, autoremove, or purge.".into(),
        });
        Ok(vec![ProviderMaintenancePlan {
            operation_id: maintenance_id(PackageSource::Apt, MaintenanceAction::Upgrade),
            source: PackageSource::Apt,
            action: MaintenanceAction::Upgrade,
            scope: None,
            candidates: inventory.candidates,
            cleanup_candidates: Vec::new(),
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: true,
            risk,
            supported,
            mutates: true,
            warnings,
            notes: if kept_back.is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "{} package(s) are kept back by ordinary APT upgrade semantics.",
                    kept_back.len()
                )]
            },
            download_size_bytes: find_apt_size(&output.stdout, "Need to get"),
            disk_delta_bytes: find_apt_disk_delta(&output.stdout),
        }])
    }

    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Apt,
                program: "apt-get".into(),
            });
        }
        let output = execute(
            &self.runner,
            PackageSource::Apt,
            "simulate APT autoremove",
            apt_simulation_command("autoremove"),
        )?;
        let output = expect_success(PackageSource::Apt, "simulate APT autoremove", output)?;
        let cleanup_candidates = parse_simulation_changes(&output.stdout)
            .into_iter()
            .filter(|change| change.kind == ChangeKind::Remove)
            .map(|change| {
                let risk = cleanup_risk(&change.package_id);
                CleanupCandidate {
                    source: PackageSource::Apt,
                    provider_id: change.package_id.clone(),
                    name: change.package_id,
                    version: change.version,
                    scope: Some(InstallScope::System),
                    reason: "APT autoremove simulation selected this unused dependency.".into(),
                    risk,
                    notes: (risk >= RiskLevel::HighImpact)
                        .then(|| "Review this package carefully; Orbis will require the high-impact confirmation phrase.".into())
                        .into_iter()
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let risk = cleanup_candidates
            .iter()
            .map(|candidate| candidate.risk)
            .max()
            .unwrap_or(RiskLevel::Normal);
        let has_cleanup = !cleanup_candidates.is_empty();
        Ok(ProviderMaintenancePlan {
            operation_id: maintenance_id(PackageSource::Apt, MaintenanceAction::Cleanup),
            source: PackageSource::Apt,
            action: MaintenanceAction::Cleanup,
            scope: Some(InstallScope::System),
            candidates: Vec::new(),
            cleanup_candidates,
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: true,
            risk,
            supported: has_cleanup,
            mutates: true,
            warnings: vec![crate::transaction::PlanWarning {
                level: WarningLevel::Info,
                message: "Only APT autoremove candidates are considered; configuration files are not purged and package caches are not wiped.".into(),
            }],
            notes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: find_apt_disk_delta(&output.stdout),
        })
    }

    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError> {
        validate_package_id(&package.provider_id).map_err(|error| ProviderError::Parse {
            package_source: PackageSource::Apt,
            operation: "explain package provenance".into(),
            technical: error.to_string(),
        })?;
        let auto = read_marked_packages(&self.runner, "showauto");
        let manual = read_marked_packages(&self.runner, "showmanual");
        let reverse = read_reverse_dependencies(&self.runner, &package.provider_id);
        let installed_as = if auto.contains(&package.provider_id) {
            "Automatic dependency"
        } else if manual.contains(&package.provider_id) {
            "Manually installed"
        } else {
            "Unknown install mark"
        };
        let used_by = reverse
            .into_iter()
            .map(|provider_id| WhyConsumer {
                source: PackageSource::Apt,
                name: provider_id.clone(),
                provider_id,
                relationship: "APT reverse dependency".into(),
                scope: Some(InstallScope::System),
            })
            .collect::<Vec<_>>();
        let mut evidence =
            vec!["APT install marks and installed reverse-dependency metadata.".into()];
        if auto.is_empty() {
            evidence.push(
                "APT automatic-mark data was unavailable or did not list this package.".into(),
            );
        }
        let removal_advice: String = if self
            .cleanup_plan()
            .map(|plan| {
                plan.cleanup_candidates
                    .iter()
                    .any(|candidate| candidate.provider_id == package.provider_id)
            })
            .unwrap_or(false)
        {
            "APT currently considers this an autoremove candidate; review the complete clean plan before acting.".into()
        } else if used_by.is_empty() {
            "No installed reverse dependency was established. This is not proof that manual removal is safe; review APT's autoremove plan.".into()
        } else {
            "Do not remove it manually while installed software depends on it.".into()
        };
        Ok(WhyReport {
            package: package.clone(),
            installed_as: installed_as.into(),
            used_by,
            evidence,
            removal_advice,
            orbis_history: Vec::new(),
            notes: Vec::new(),
        })
    }

    fn maintenance_operation(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError> {
        let operation = maintenance_operation_for(plan).ok_or_else(|| ProviderError::Parse {
            package_source: PackageSource::Apt,
            operation: "build maintenance operation".into(),
            technical: "APT maintenance plan has an unsupported shape".into(),
        })?;
        Ok(ProviderOperation::Maintenance { operation })
    }

    fn verify_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError> {
        match plan.action {
            MaintenanceAction::Refresh => Ok(VerificationResult::Verified),
            MaintenanceAction::Upgrade => {
                Ok(if self.upgrade_plan()?.first().is_some_and(|plan| plan.candidates.is_empty()) {
                    VerificationResult::Verified
                } else {
                    VerificationResult::PartiallyVerified
                })
            }
            MaintenanceAction::Cleanup => {
                Ok(if self.cleanup_plan()?.cleanup_candidates.is_empty() {
                    VerificationResult::Verified
                } else {
                    VerificationResult::PartiallyVerified
                })
            }
        }
    }
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
            "APT package IDs must be exact names without whitespace or option characters".into(),
        ));
    }
    Ok(())
}

fn simulation_command(action: &str, package_id: &str) -> CommandSpec {
    CommandSpec::new("apt-get", ["-s", "-o", "Debug::NoLocking=true", action, "--", package_id])
        .with_env("LC_ALL", "C")
        .with_env("DEBIAN_FRONTEND", "noninteractive")
        .with_timeout(Duration::from_secs(60))
}

fn apt_simulation_command(action: &str) -> CommandSpec {
    CommandSpec::new("apt-get", ["-s", "-o", "Debug::NoLocking=true", action])
        .with_env("LC_ALL", "C")
        .with_env("DEBIAN_FRONTEND", "noninteractive")
        .with_timeout(Duration::from_secs(60))
}

fn parse_apt_update_candidates(output: &str) -> Vec<UpdateCandidate> {
    output
        .lines()
        .filter_map(parse_apt_upgrade_line)
        .map(|(provider_id, current_version, available_version)| UpdateCandidate {
            source: PackageSource::Apt,
            name: provider_id.clone(),
            provider_id,
            current_version,
            available_version,
            architecture: None,
            scope: Some(InstallScope::System),
            channel: None,
            held: None,
            security_relevance: None,
            notes: Vec::new(),
            metadata: BTreeMap::new(),
        })
        .collect()
}

fn parse_apt_upgrade_line(line: &str) -> Option<(String, Option<String>, Option<String>)> {
    let rest = line.trim().strip_prefix("Inst ")?;
    let provider_id = rest.split_whitespace().next()?.to_owned();
    let current_version = rest
        .split_once('(')
        .and_then(|(before_version, _)| before_version.split_once('['))
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(version, _)| version.trim().to_owned())
        .filter(|version| !version.is_empty());
    let available_version = rest
        .split_once('(')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .map(str::to_owned)
        .filter(|version| !version.is_empty());
    Some((provider_id, current_version, available_version))
}

fn parse_kept_back(output: &str) -> Vec<String> {
    let mut kept_back = Vec::new();
    let mut collecting = false;
    for line in output.lines() {
        if line.to_ascii_lowercase().contains("have been kept back") {
            collecting = true;
            continue;
        }
        if collecting {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("The following") {
                break;
            }
            kept_back.extend(trimmed.split_whitespace().map(str::to_owned));
        }
    }
    kept_back
}

fn cleanup_risk(package_id: &str) -> RiskLevel {
    let lower = package_id.to_ascii_lowercase();
    if [
        "apt",
        "dpkg",
        "libc",
        "libssl",
        "openssl",
        "systemd",
        "linux-image",
        "linux-generic",
        "network-manager",
        "sudo",
        "bash",
    ]
    .iter()
    .any(|prefix| {
        lower == *prefix || lower.starts_with(prefix) || lower.starts_with(&format!("{prefix}-"))
    }) {
        RiskLevel::HighImpact
    } else {
        RiskLevel::Caution
    }
}

fn maintenance_id(source: PackageSource, action: MaintenanceAction) -> String {
    let action = action.label().to_ascii_lowercase();
    format!("maint-{}-{}-{}", source.label().to_ascii_lowercase(), action, std::process::id())
}

fn read_marked_packages(runner: &SharedRunner, action: &str) -> std::collections::BTreeSet<String> {
    let Ok(output) = execute(
        runner,
        PackageSource::Apt,
        "read APT install marks",
        CommandSpec::new("apt-mark", [action]).with_timeout(short_timeout()),
    ) else {
        return std::collections::BTreeSet::new();
    };
    if !output.success() {
        return std::collections::BTreeSet::new();
    }
    output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn read_reverse_dependencies(runner: &SharedRunner, package_id: &str) -> Vec<String> {
    let Ok(output) = execute(
        runner,
        PackageSource::Apt,
        "read installed APT reverse dependencies",
        CommandSpec::new(
            "apt-cache",
            ["--no-generate", "rdepends", "--installed", "--", package_id],
        )
        .with_timeout(short_timeout()),
    ) else {
        return Vec::new();
    };
    if !output.success() {
        return Vec::new();
    }
    let dependencies: std::collections::BTreeSet<_> = output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("Reverse Depends") && *line != package_id
        })
        .map(|line| line.trim_start_matches('|').trim().to_owned())
        .filter(|line| !line.is_empty() && !line.contains(' '))
        .collect();
    dependencies.into_iter().collect()
}

fn parse_simulation_changes(output: &str) -> Vec<crate::transaction::PlannedChange> {
    output
        .lines()
        .filter_map(|line| {
            let (kind, rest) = if let Some(rest) = line.trim().strip_prefix("Inst ") {
                (ChangeKind::Install, rest)
            } else if let Some(rest) = line.trim().strip_prefix("Remv ") {
                (ChangeKind::Remove, rest)
            } else if let Some(rest) = line.trim().strip_prefix("Purg ") {
                (ChangeKind::Remove, rest)
            } else {
                (ChangeKind::Configure, line.trim().strip_prefix("Conf ")?)
            };
            let package_id = rest.split_whitespace().next()?.to_owned();
            let version = rest
                .split_once('(')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .map(str::to_owned);
            Some(crate::transaction::PlannedChange {
                kind,
                package_id,
                name: None,
                version,
                reason: None,
            })
        })
        .collect()
}

fn find_apt_size(output: &str, marker: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        let rest = line.split_once(marker)?.1.trim();
        let mut words = rest.split_whitespace();
        let number = words.next()?;
        let unit = words.next()?;
        crate::transaction::parse_human_size(&format!("{number} {unit}"))
    })
}

fn find_apt_disk_delta(output: &str) -> Option<i64> {
    output.lines().find_map(|line| {
        let rest = line.split_once("After this operation,")?.1.trim();
        let sign = if rest.contains("freed") { -1 } else { 1 };
        let mut words = rest.split_whitespace();
        let number = words.next()?;
        let unit = words.next()?;
        crate::transaction::parse_human_size(&format!("{number} {unit}"))
            .map(|size| sign * size as i64)
    })
}

fn capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        search: true,
        info: true,
        installed_state: true,
        installed_list: true,
        mutations: true,
        install: true,
        remove: true,
        updates: true,
        upgrade: true,
        refresh: true,
        cleanup: true,
        why: true,
    }
}

fn value(record: &BTreeMap<String, String>, key: &str) -> Option<String> {
    record.get(key).cloned().filter(|value| !value.trim().is_empty())
}

fn parse_control_records(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut records = Vec::new();
    let mut record: BTreeMap<String, String> = BTreeMap::new();
    let mut last_key: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            if !record.is_empty() {
                records.push(record);
                record = BTreeMap::new();
                last_key = None;
            }
        } else if line.starts_with(char::is_whitespace) {
            if let Some(key) = &last_key
                && let Some(value) = record.get_mut(key)
            {
                value.push('\n');
                value.push_str(line.trim_end());
            }
        } else if let Some((key, value)) = line.split_once(':') {
            let key = key.to_owned();
            record.insert(key.clone(), value.trim().to_owned());
            last_key = Some(key);
        }
    }
    if !record.is_empty() {
        records.push(record);
    }
    records
}

fn classify(name: &str, summary: Option<&str>, description: Option<&str>) -> Option<PackageKind> {
    let lower =
        format!("{} {} {}", name, summary.unwrap_or_default(), description.unwrap_or_default())
            .to_ascii_lowercase();
    if name.ends_with("-dev")
        || name.ends_with("-devel")
        || lower.contains("development files")
        || lower.contains("development headers")
    {
        return Some(PackageKind::DevelopmentFiles);
    }
    if name.starts_with("lib") && !lower.contains("command line") && !lower.contains("monitor") {
        return Some(PackageKind::Library);
    }
    if lower.contains("command line")
        || lower.contains("terminal")
        || name == "btop"
        || name == "ffmpeg"
    {
        return Some(PackageKind::CliTool);
    }
    if lower.contains("daemon") || lower.contains("service") {
        return Some(PackageKind::Service);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_apt_record() {
        let records = parse_control_records(
            "Package: libssl-dev\nVersion: 3.0\nDescription-en: Development files\n for OpenSSL.\n\n",
        );
        assert_eq!(records[0]["Package"], "libssl-dev");
        assert_eq!(records[0]["Description-en"], "Development files\n for OpenSSL.");
    }

    #[test]
    fn classifies_development_files_conservatively() {
        assert_eq!(
            classify("libssl-dev", Some("Secure Sockets Layer toolkit - development files"), None),
            Some(PackageKind::DevelopmentFiles)
        );
    }

    #[test]
    fn normalizes_simulation_changes_and_sizes() {
        let output = "Inst btop (1.4.6-2 Ubuntu:26.04/resolute [amd64])\nConf btop (1.4.6-2 Ubuntu:26.04/resolute [amd64])\nNeed to get 2.5 MB of archives.\nAfter this operation, 8.0 MB of additional disk space will be used.\n";
        let changes = parse_simulation_changes(output);
        assert_eq!(changes[0].kind, ChangeKind::Install);
        assert_eq!(changes[0].package_id, "btop");
        assert_eq!(changes[1].kind, ChangeKind::Configure);
        assert_eq!(find_apt_size(output, "Need to get"), Some(2_500_000));
        assert_eq!(find_apt_disk_delta(output), Some(8_000_000));
    }

    #[test]
    fn planning_command_remains_bounded() {
        assert_eq!(simulation_command("install", "btop").timeout, Some(Duration::from_secs(60)));
    }

    #[test]
    fn parses_upgrade_inventory_without_fabricating_versions() {
        let inventory = parse_apt_update_candidates(
            "Inst curl [8.14] (8.15 Ubuntu:26.04/resolute [amd64])\n\
             Inst held-package (2.0 Ubuntu:26.04/resolute [amd64])\n",
        );
        assert_eq!(inventory.len(), 2);
        assert_eq!(inventory[0].current_version.as_deref(), Some("8.14"));
        assert_eq!(inventory[0].available_version.as_deref(), Some("8.15"));
        assert_eq!(inventory[1].current_version, None);
    }

    #[test]
    fn parses_apt_kept_back_section() {
        let kept = parse_kept_back(
            "The following packages have been kept back:\n\
             \u{20} linux-image-generic linux-headers-generic\n\
             \n",
        );
        assert_eq!(kept, ["linux-image-generic", "linux-headers-generic"]);
    }

    #[test]
    fn cleanup_risk_elevates_core_packages() {
        assert_eq!(cleanup_risk("libc6"), RiskLevel::HighImpact);
        assert_eq!(cleanup_risk("unused-example"), RiskLevel::Caution);
    }
}
