//! User-scoped developer-tool providers.
//!
//! This module deliberately models global tools, not project dependency graphs. The providers
//! use their documented CLIs and keep registry-specific parsing local to the corresponding
//! provider implementation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::Value;

use crate::{
    diagnostics::DiagnosticCheck,
    maintenance::{
        MaintenanceAction, MaintenanceProvider, ProviderMaintenancePlan, ProviderUpdateInventory,
        UpdateCandidate, WhyReport, maintenance_operation_for,
    },
    models::{Package, PackageKind, PackageSource, ProviderCapabilities, SourceInfo},
    process::{CommandRunner, CommandSpec, SharedRunner},
    providers::{
        Provider, ProviderError, TransactionProvider, execute, expect_success, short_timeout,
    },
    transaction::{
        InstallScope, OperationAction, OperationPlan, OperationRequest, PlanCompleteness,
        PlanConfidence, PrivilegeRequirement, ProviderOperation, RiskLevel, TransactionError,
        VerificationResult, WarningLevel, base_risk, target_change,
    },
};

/// Cargo's user-installed binary-crate provider.
pub struct CargoProvider {
    runner: SharedRunner,
}

/// npm's global-package provider.
pub struct NpmProvider {
    runner: SharedRunner,
}

/// pnpm's global-package provider.
pub struct PnpmProvider {
    runner: SharedRunner,
}

/// uv's persistent tool provider.
pub struct UvProvider {
    runner: SharedRunner,
}

/// pipx's current-user application provider.
pub struct PipxProvider {
    runner: SharedRunner,
}

impl CargoProvider {
    /// Creates a Cargo provider.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn available(&self) -> bool {
        self.runner.is_available("cargo")
    }

    fn installed(&self) -> Result<BTreeMap<String, InstalledTool>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Cargo,
            "list installed Cargo tools",
            CommandSpec::new("cargo", ["install", "--list"]).with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Cargo, "list installed Cargo tools", output)?;
        Ok(parse_cargo_installed(&output.stdout))
    }
}

impl Provider for CargoProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Cargo
    }

    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let root = cargo_root();
        let writable = available && user_writable(&root);
        SourceInfo {
            source: PackageSource::Cargo,
            available,
            state: if !available {
                "unavailable".into()
            } else if writable {
                "ready".into()
            } else {
                "restricted".into()
            },
            backend: Some("Cargo stable CLI".into()),
            capabilities: cargo_capabilities(),
            notes: if !available {
                vec!["Install Rust/Cargo to manage user-level binary crates.".into()]
            } else if writable {
                vec![format!("User tool root: {}.", root.display())]
            } else {
                vec![format!(
                    "Cargo install root is not writable by the current user: {}. Orbis will not use sudo.",
                    root.display()
                )]
            },
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Cargo, "cargo"));
        }
        let output = execute(
            &self.runner,
            PackageSource::Cargo,
            "search Cargo crates",
            CommandSpec::new("cargo", ["search", query, "--limit", "20"])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Cargo, "search Cargo crates", output)?;
        Ok(parse_cargo_search(&output.stdout))
    }

    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !valid_cargo_name(package_id) {
            return Err(ProviderError::NotFound {
                package_source: PackageSource::Cargo,
                query: package_id.into(),
            });
        }
        if !self.available() {
            return Err(unavailable(PackageSource::Cargo, "cargo"));
        }
        let output = execute(
            &self.runner,
            PackageSource::Cargo,
            "read Cargo crate information",
            CommandSpec::new("cargo", ["info", package_id]).with_timeout(short_timeout()),
        )?;
        if !output.success() {
            let detail = format!("{} {}", output.stdout, output.stderr).to_ascii_lowercase();
            if detail.contains("could not find") || detail.contains("not found") {
                return Err(ProviderError::NotFound {
                    package_source: PackageSource::Cargo,
                    query: package_id.into(),
                });
            }
        }
        let output = expect_success(PackageSource::Cargo, "read Cargo crate information", output)?;
        let installed = self.installed().unwrap_or_default();
        parse_cargo_info(&output.stdout, package_id, installed.get(package_id))
    }

    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Cargo,
                "Cargo",
                "Install Rust/Cargo to enable Cargo tool management.",
            );
        }
        if !user_writable(&cargo_root()) {
            return DiagnosticCheck::failed(
                PackageSource::Cargo,
                "Cargo install root",
                "Cargo is available, but its user install root is not writable. Configure a user-owned Cargo root; Orbis will not use sudo.",
            );
        }
        match execute(
            &self.runner,
            PackageSource::Cargo,
            "check Cargo",
            CommandSpec::new("cargo", ["--version"]).with_timeout(short_timeout()),
        ) {
            Ok(output) if output.success() => DiagnosticCheck::passed(
                PackageSource::Cargo,
                "Cargo stable CLI and user install root are usable",
            ),
            Ok(output) => {
                DiagnosticCheck::failed(PackageSource::Cargo, "Cargo", &first_error(&output))
            }
            Err(error) => {
                DiagnosticCheck::failed(PackageSource::Cargo, "Cargo", &error.to_string())
            }
        }
    }
}

impl TransactionProvider for CargoProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        developer_plan(
            request,
            target,
            DeveloperPlanOptions {
                source: PackageSource::Cargo,
                validator: validate_cargo_transaction_name,
                root_ok: user_writable(&cargo_root()),
                completeness: PlanCompleteness::Partial,
                confidence: PlanConfidence::Medium,
                warnings: vec![
                    "Cargo's stable install dry run is unavailable; this is a truthful metadata plan.".into(),
                    "Cargo installation compiles the crate and dependencies; build scripts and procedural build-time code may execute.".into(),
                ],
            },
        )
    }

    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_cargo_transaction_name(&plan.target.provider_id)?;
        Ok(ProviderOperation::Cargo {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
        })
    }

    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        let installed = self.installed()?.contains_key(&plan.target.provider_id);
        Ok(if installed == (plan.action == OperationAction::Install) {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        })
    }
}

impl MaintenanceProvider for CargoProvider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError> {
        let installed = self.installed()?;
        Ok(ProviderUpdateInventory {
            source: PackageSource::Cargo,
            available: true,
            candidates: Vec::new(),
            notes: if installed.is_empty() {
                vec!["No Cargo-installed tools were detected.".into()]
            } else {
                vec!["Installed tools detected. Automatic upgrade is unavailable because Orbis cannot safely prove each tool's original registry, Git, path, or alternate-registry source.".into()]
            },
            metadata_state: Some("incomplete_unknown_install_provenance".into()),
        })
    }

    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Cargo, "cargo"));
        }
        Ok(vec![developer_refresh_plan(
            PackageSource::Cargo,
            "Cargo metadata is queried live; no separate catalog refresh is required.",
        )])
    }

    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        let inventory = self.update_inventory()?;
        let mut plan = ProviderMaintenancePlan::blocked(
            PackageSource::Cargo,
            MaintenanceAction::Upgrade,
            inventory.notes.join(" "),
        );
        plan.notes = inventory.notes;
        Ok(vec![plan])
    }

    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError> {
        Ok(ProviderMaintenancePlan::blocked(
            PackageSource::Cargo,
            MaintenanceAction::Cleanup,
            "Cargo cache/build cleanup is outside Orbis's installed-tool cleanup scope.",
        ))
    }

    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError> {
        Ok(developer_why(
            package,
            "Installed as a user-level Cargo binary crate.",
            "Use `cargo uninstall` through Orbis to remove the exact crate; Orbis does not delete Cargo binaries manually.",
        ))
    }

    fn maintenance_operation(
        &self,
        _plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError> {
        Err(unsupported_maintenance(PackageSource::Cargo, "Cargo upgrade provenance is incomplete"))
    }

    fn verify_maintenance(
        &self,
        _plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError> {
        Ok(VerificationResult::Failed)
    }
}

impl NpmProvider {
    /// Creates an npm provider.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn available(&self) -> bool {
        self.runner.is_available("npm")
    }

    fn prefix(&self) -> Result<PathBuf, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Npm,
            "read npm global prefix",
            CommandSpec::new("npm", ["prefix", "--global"]).with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Npm, "read npm global prefix", output)?;
        let prefix =
            output.stdout.lines().map(str::trim).find(|line| !line.is_empty()).ok_or_else(
                || ProviderError::Parse {
                    package_source: PackageSource::Npm,
                    operation: "read npm global prefix".into(),
                    technical: "npm returned an empty prefix".into(),
                },
            )?;
        Ok(PathBuf::from(prefix))
    }

    fn installed(&self) -> Result<BTreeMap<String, InstalledTool>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Npm,
            "list global npm packages",
            CommandSpec::new("npm", ["ls", "--global", "--depth=0", "--json"])
                .with_timeout(short_timeout()),
        )?;
        parse_npm_installed(&output.stdout, output.status)
    }
}

impl Provider for NpmProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Npm
    }

    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let prefix = available.then(|| self.prefix().ok()).flatten();
        let writable = prefix.as_deref().is_some_and(user_writable);
        SourceInfo {
            source: PackageSource::Npm,
            available,
            state: if !available {
                "unavailable".into()
            } else if prefix.is_some() && writable {
                "ready".into()
            } else {
                "restricted".into()
            },
            backend: Some("npm global mode".into()),
            capabilities: npm_capabilities(),
            notes: if !available {
                vec!["Install Node.js/npm to manage global packages.".into()]
            } else if let Some(prefix) = prefix {
                vec![
                    format!("Global prefix: {}.", prefix.display()),
                    if writable {
                        "Global prefix is writable by the current user.".into()
                    } else {
                        "Mutation is blocked because the global prefix is not user-writable; Orbis will not use sudo.".into()
                    },
                ]
            } else {
                vec!["npm global prefix could not be read; mutation is blocked.".into()]
            },
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Npm, "npm"));
        }
        let output = execute(
            &self.runner,
            PackageSource::Npm,
            "search npm registry",
            CommandSpec::new("npm", ["search", query, "--json", "--searchlimit=20"])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Npm, "search npm registry", output)?;
        parse_registry_search(PackageSource::Npm, &output.stdout)
    }

    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !valid_npm_name(package_id) {
            return Err(ProviderError::NotFound {
                package_source: PackageSource::Npm,
                query: package_id.into(),
            });
        }
        if !self.available() {
            return Err(unavailable(PackageSource::Npm, "npm"));
        }
        let output = execute(
            &self.runner,
            PackageSource::Npm,
            "read npm package information",
            CommandSpec::new(
                "npm",
                [
                    "view",
                    package_id,
                    "name",
                    "version",
                    "description",
                    "homepage",
                    "license",
                    "bin",
                    "--json",
                ],
            )
            .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Npm, "read npm package information", output)?;
        let installed = self.installed().unwrap_or_default();
        parse_registry_info(
            PackageSource::Npm,
            &output.stdout,
            package_id,
            installed.get(package_id),
        )
    }

    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Npm,
                "npm",
                "Install Node.js/npm to enable global package management.",
            );
        }
        let Ok(prefix) = self.prefix() else {
            return DiagnosticCheck::failed(
                PackageSource::Npm,
                "Global prefix",
                "npm is available but its global prefix could not be read.",
            );
        };
        if !user_writable(&prefix) {
            return DiagnosticCheck::failed(
                PackageSource::Npm,
                "Global prefix",
                "The npm prefix is system-owned or not writable. Configure a user prefix; Orbis deliberately refuses sudo global installs.",
            );
        }
        DiagnosticCheck::passed(
            PackageSource::Npm,
            "npm registry and user-writable global prefix are available",
        )
    }
}

impl TransactionProvider for NpmProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        let prefix_ok = self.prefix().map(|prefix| user_writable(&prefix)).unwrap_or(false);
        let mut plan = developer_plan(
            request,
            target,
            DeveloperPlanOptions {
                source: PackageSource::Npm,
                validator: validate_npm_transaction_name,
                root_ok: prefix_ok,
                completeness: PlanCompleteness::Partial,
                confidence: PlanConfidence::Medium,
                warnings: vec![
                    "npm packages may run package lifecycle scripts during installation.".into(),
                ],
            },
        )?;
        let action = match request.action {
            OperationAction::Install => "install",
            OperationAction::Remove => "uninstall",
        };
        if prefix_ok && self.available() {
            let output = execute(
                &self.runner,
                PackageSource::Npm,
                "simulate npm global transaction",
                CommandSpec::new(
                    "npm",
                    [action, "--global", "--dry-run", "--json", "--", target.provider_id.as_str()],
                )
                .with_timeout(short_timeout()),
            );
            match output.and_then(|output| expect_success(PackageSource::Npm, "simulate npm global transaction", output)) {
                Ok(_) => { plan.completeness = PlanCompleteness::Complete; plan.confidence = PlanConfidence::High; plan.authoritative_simulation = true; plan.warnings.push(crate::transaction::PlanWarning { level: WarningLevel::Info, message: "npm's documented global dry run was used; no package state or project files were changed.".into() }); }
                Err(error) => plan.warnings.push(crate::transaction::PlanWarning { level: WarningLevel::Caution, message: format!("npm dry-run metadata was unavailable; Orbis retained a partial plan: {error}") }),
            }
        }
        Ok(plan)
    }

    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_npm_transaction_name(&plan.target.provider_id)?;
        Ok(ProviderOperation::Npm {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
        })
    }
    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        let installed = self.installed()?.contains_key(&plan.target.provider_id);
        Ok(if installed == (plan.action == OperationAction::Install) {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        })
    }
}

impl MaintenanceProvider for NpmProvider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Npm,
            "inspect outdated global npm packages",
            CommandSpec::new("npm", ["outdated", "--global", "--json"])
                .with_timeout(short_timeout()),
        )?;
        parse_npm_outdated(&output.stdout, output.status)
    }
    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Npm, "npm"));
        }
        Ok(vec![developer_refresh_plan(
            PackageSource::Npm,
            "npm registry metadata is queried live; no separate catalog refresh is required.",
        )])
    }
    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        let prefix_ok = self.prefix().map(|prefix| user_writable(&prefix)).unwrap_or(false);
        if !prefix_ok {
            return Ok(vec![ProviderMaintenancePlan::blocked(
                PackageSource::Npm,
                MaintenanceAction::Upgrade,
                "npm's global prefix is not writable by the current user; Orbis refuses sudo global upgrades.",
            )]);
        }
        let inventory = self.update_inventory()?;
        let candidates = inventory.candidates;
        let mut warnings = vec![crate::transaction::PlanWarning { level: WarningLevel::Info, message: "Execution uses exact reviewed package@latest candidates, not blanket `npm update -g`; this prevents npm's documented newer-than-latest downgrade hazard.".into() }];
        for note in inventory.notes {
            warnings.push(crate::transaction::PlanWarning {
                level: WarningLevel::Caution,
                message: note,
            });
        }
        Ok(vec![developer_maintenance_plan(DeveloperMaintenanceOptions {
            source: PackageSource::Npm,
            action: MaintenanceAction::Upgrade,
            candidates: candidates.clone(),
            supported: !candidates.is_empty(),
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            warnings,
            note: "npm global packages are upgraded one exact candidate at a time.".into(),
        })])
    }
    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError> {
        Ok(ProviderMaintenancePlan::blocked(
            PackageSource::Npm,
            MaintenanceAction::Cleanup,
            "npm cache cleanup is outside Orbis's installed-tool cleanup scope.",
        ))
    }
    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError> {
        Ok(developer_why(
            package,
            "Installed as a top-level global npm package.",
            "Use npm global removal through Orbis; local project files are outside this provider's scope.",
        ))
    }
    fn maintenance_operation(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError> {
        let specs = plan
            .candidates
            .iter()
            .filter_map(|candidate| {
                candidate
                    .available_version
                    .as_ref()
                    .map(|version| format!("{}@{}", candidate.provider_id, version))
            })
            .collect();
        Ok(ProviderOperation::Maintenance {
            operation: maintenance_operation_for(plan)
                .ok_or_else(|| {
                    unsupported_maintenance(PackageSource::Npm, "invalid npm maintenance plan")
                })?
                .with_npm_specs(specs)?,
        })
    }
    fn verify_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError> {
        let remaining = self.update_inventory()?.candidates;
        let ids: BTreeSet<_> =
            plan.candidates.iter().map(|candidate| candidate.provider_id.as_str()).collect();
        Ok(if remaining.iter().any(|candidate| ids.contains(candidate.provider_id.as_str())) {
            VerificationResult::PartiallyVerified
        } else {
            VerificationResult::Verified
        })
    }
}

impl PnpmProvider {
    /// Creates a pnpm provider.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
    fn available(&self) -> bool {
        self.runner.is_available("pnpm")
    }
    fn global_bin(&self) -> PathBuf {
        std::env::var_os("PNPM_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local/share/pnpm/bin"))
    }
    fn installed(&self) -> Result<BTreeMap<String, InstalledTool>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Pnpm,
            "list global pnpm packages",
            CommandSpec::new("pnpm", ["list", "--global", "--json", "--depth", "0"])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Pnpm, "list global pnpm packages", output)?;
        parse_pnpm_installed(&output.stdout)
    }
}

impl Provider for PnpmProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Pnpm
    }
    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let bin = self.global_bin();
        let configured = available && bin_in_path(&bin);
        let writable = configured && user_writable(&bin);
        SourceInfo {
            source: PackageSource::Pnpm,
            available,
            state: if !available {
                "unavailable".into()
            } else if configured && writable {
                "ready".into()
            } else {
                "restricted".into()
            },
            backend: Some("pnpm global mode".into()),
            capabilities: pnpm_capabilities(),
            notes: if !available {
                vec!["Install pnpm to manage global packages.".into()]
            } else if !configured {
                vec![format!(
                    "Global bin directory {} is not on PATH; Orbis will not run pnpm setup or rewrite shell files.",
                    bin.display()
                )]
            } else if !writable {
                vec![format!(
                    "Global bin directory {} is not user-writable; Orbis will not use sudo.",
                    bin.display()
                )]
            } else {
                vec![format!("Global bin directory: {}.", bin.display())]
            },
        }
    }
    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Pnpm, "pnpm"));
        }
        let output = execute(
            &self.runner,
            PackageSource::Pnpm,
            "search pnpm registry",
            CommandSpec::new("pnpm", ["search", query, "--json", "--search-limit", "20"])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Pnpm, "search pnpm registry", output)?;
        parse_registry_search(PackageSource::Pnpm, &output.stdout)
    }
    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !valid_npm_name(package_id) {
            return Err(ProviderError::NotFound {
                package_source: PackageSource::Pnpm,
                query: package_id.into(),
            });
        }
        if !self.available() {
            return Err(unavailable(PackageSource::Pnpm, "pnpm"));
        }
        let output = execute(
            &self.runner,
            PackageSource::Pnpm,
            "read pnpm package information",
            CommandSpec::new("pnpm", ["view", package_id, "--json"]).with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Pnpm, "read pnpm package information", output)?;
        let installed = self.installed().unwrap_or_default();
        parse_registry_info(
            PackageSource::Pnpm,
            &output.stdout,
            package_id,
            installed.get(package_id),
        )
    }
    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Pnpm,
                "pnpm",
                "Install pnpm to enable global package management.",
            );
        }
        let bin = self.global_bin();
        if !bin_in_path(&bin) {
            return DiagnosticCheck::failed(
                PackageSource::Pnpm,
                "Global environment",
                "pnpm's global bin directory is not on PATH. Run pnpm setup yourself if desired; Orbis will not edit shell configuration.",
            );
        }
        if !user_writable(&bin) {
            return DiagnosticCheck::failed(
                PackageSource::Pnpm,
                "Global environment",
                "pnpm's global bin directory is not user-writable; Orbis will not use sudo.",
            );
        }
        match execute(
            &self.runner,
            PackageSource::Pnpm,
            "check pnpm",
            CommandSpec::new("pnpm", ["--version"]).with_timeout(short_timeout()),
        ) {
            Ok(output) if output.success() => DiagnosticCheck::passed(
                PackageSource::Pnpm,
                "pnpm and its user global environment are ready",
            ),
            Ok(output) => {
                DiagnosticCheck::failed(PackageSource::Pnpm, "pnpm", &first_error(&output))
            }
            Err(error) => DiagnosticCheck::failed(PackageSource::Pnpm, "pnpm", &error.to_string()),
        }
    }
    fn requires_source_qualification(&self) -> bool {
        false
    }
}

impl TransactionProvider for PnpmProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        let bin = self.global_bin();
        developer_plan(
            request,
            target,
            DeveloperPlanOptions {
                source: PackageSource::Pnpm,
                validator: validate_npm_transaction_name,
                root_ok: bin_in_path(&bin) && user_writable(&bin),
                completeness: PlanCompleteness::Partial,
                confidence: PlanConfidence::Medium,
                warnings: vec![
                    "pnpm's current global operation has no authoritative zero-mutation dry run; this is a partial metadata plan.".into(),
                    "pnpm's configured lifecycle/build-script approval policy is preserved.".into(),
                ],
            },
        )
    }
    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_npm_transaction_name(&plan.target.provider_id)?;
        Ok(ProviderOperation::Pnpm {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
        })
    }
    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        let installed = self.installed()?.contains_key(&plan.target.provider_id);
        Ok(if installed == (plan.action == OperationAction::Install) {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        })
    }
}

impl MaintenanceProvider for PnpmProvider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Pnpm,
            "inspect outdated global pnpm packages",
            CommandSpec::new("pnpm", ["outdated", "--global", "--format", "json"])
                .with_timeout(short_timeout()),
        )?;
        parse_pnpm_outdated(&output.stdout, output.status)
    }
    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Pnpm, "pnpm"));
        }
        Ok(vec![developer_refresh_plan(
            PackageSource::Pnpm,
            "pnpm registry metadata is queried live; no separate catalog refresh is required.",
        )])
    }
    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        let bin = self.global_bin();
        if !bin_in_path(&bin) || !user_writable(&bin) {
            return Ok(vec![ProviderMaintenancePlan::blocked(
                PackageSource::Pnpm,
                MaintenanceAction::Upgrade,
                "pnpm's global bin directory is not a writable current-user destination on PATH; Orbis refuses sudo and does not run pnpm setup.",
            )]);
        }
        let inventory = self.update_inventory()?;
        let candidates = inventory.candidates;
        let mut warnings = vec![crate::transaction::PlanWarning { level: WarningLevel::Info, message: "Execution targets exact reviewed global package names with pnpm's latest-update semantics; project files are not involved.".into() }];
        warnings.extend(inventory.notes.into_iter().map(|note| crate::transaction::PlanWarning {
            level: WarningLevel::Caution,
            message: note,
        }));
        Ok(vec![developer_maintenance_plan(DeveloperMaintenanceOptions {
            source: PackageSource::Pnpm,
            action: MaintenanceAction::Upgrade,
            candidates: candidates.clone(),
            supported: !candidates.is_empty(),
            completeness: PlanCompleteness::Partial,
            confidence: PlanConfidence::Medium,
            warnings,
            note: "pnpm global packages are upgraded as exact reviewed candidates.".into(),
        })])
    }
    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError> {
        Ok(ProviderMaintenancePlan::blocked(
            PackageSource::Pnpm,
            MaintenanceAction::Cleanup,
            "pnpm store pruning is outside Orbis's installed-tool cleanup scope.",
        ))
    }
    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError> {
        Ok(developer_why(
            package,
            "Installed as a top-level global pnpm package.",
            "Use pnpm global removal through Orbis; project/workspace files are outside this provider's scope.",
        ))
    }
    fn maintenance_operation(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError> {
        let ids = plan.candidates.iter().map(|candidate| candidate.provider_id.clone()).collect();
        Ok(ProviderOperation::Maintenance {
            operation: maintenance_operation_for(plan)
                .ok_or_else(|| {
                    unsupported_maintenance(PackageSource::Pnpm, "invalid pnpm maintenance plan")
                })?
                .with_pnpm_ids(ids)?,
        })
    }
    fn verify_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError> {
        let remaining = self.update_inventory()?.candidates;
        let ids: BTreeSet<_> =
            plan.candidates.iter().map(|candidate| candidate.provider_id.as_str()).collect();
        Ok(if remaining.iter().any(|candidate| ids.contains(candidate.provider_id.as_str())) {
            VerificationResult::PartiallyVerified
        } else {
            VerificationResult::Verified
        })
    }
}

impl UvProvider {
    /// Creates a uv provider.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
    fn available(&self) -> bool {
        self.runner.is_available("uv")
    }
    fn installed(&self) -> Result<BTreeMap<String, InstalledTool>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Uv,
            "list uv tools",
            CommandSpec::new(
                "uv",
                ["tool", "list", "--show-paths", "--show-version-specifiers", "--show-python"],
            )
            .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Uv, "list uv tools", output)?;
        Ok(parse_uv_list(&output.stdout))
    }
}

impl Provider for UvProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Uv
    }
    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let bin = uv_bin_dir();
        let writable = available && user_writable(&bin);
        SourceInfo {
            source: PackageSource::Uv,
            available,
            state: if !available {
                "unavailable".into()
            } else if writable {
                "ready".into()
            } else {
                "restricted".into()
            },
            backend: Some("uv tool".into()),
            capabilities: uv_capabilities(),
            notes: if !available {
                vec!["Install uv to manage persistent Python CLI tools.".into()]
            } else {
                vec![format!("Tool executable directory: {}.", bin.display()), "Fuzzy registry search is intentionally unsupported; use an explicit uv:package reference for installation.".into()]
            },
        }
    }
    fn search(&self, _query: &str) -> Result<Vec<Package>, ProviderError> {
        Ok(Vec::new())
    }
    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !valid_python_name(package_id) {
            return Err(ProviderError::NotFound {
                package_source: PackageSource::Uv,
                query: package_id.into(),
            });
        }
        if !self.available() {
            return Err(unavailable(PackageSource::Uv, "uv"));
        }
        let installed = self.installed()?;
        Ok(installed.get(package_id).map(|tool| tool.package.clone()).unwrap_or_else(|| {
            placeholder_package(
                PackageSource::Uv,
                package_id,
                Some(false),
                "Python CLI tool managed in an isolated uv tool environment.",
            )
        }))
    }
    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Uv,
                "uv",
                "Install uv to enable persistent tool management.",
            );
        }
        let bin = uv_bin_dir();
        if !user_writable(&bin) {
            return DiagnosticCheck::failed(
                PackageSource::Uv,
                "Tool directory",
                "uv's tool executable directory is not user-writable; Orbis will not use sudo.",
            );
        }
        DiagnosticCheck::passed(
            PackageSource::Uv,
            "uv tool interface and user tool directory are available",
        )
    }
    fn supports_unqualified_resolution(&self) -> bool {
        false
    }
    fn requires_source_qualification(&self) -> bool {
        true
    }
}

impl TransactionProvider for UvProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        developer_plan(
            request,
            target,
            DeveloperPlanOptions {
                source: PackageSource::Uv,
                validator: validate_python_transaction_name,
                root_ok: user_writable(&uv_bin_dir()),
                completeness: PlanCompleteness::Partial,
                confidence: PlanConfidence::Medium,
                warnings: vec![
                    "uv tool install/uninstall has no supported true dry-run in the current CLI; this plan is based on the explicit package name and installed-tool metadata.".into(),
                    "uv tool environments are isolated from the current project; Orbis does not touch pyproject.toml, uv.lock, .venv, or uv pip environments.".into(),
                ],
            },
        )
    }
    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_python_transaction_name(&plan.target.provider_id)?;
        Ok(ProviderOperation::Uv {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
        })
    }
    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        let installed = self.installed()?.contains_key(&plan.target.provider_id);
        Ok(if installed == (plan.action == OperationAction::Install) {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        })
    }
}

impl MaintenanceProvider for UvProvider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Uv,
            "inspect outdated uv tools",
            CommandSpec::new("uv", ["tool", "list", "--outdated"]).with_timeout(short_timeout()),
        )?;
        let installed = self.installed().unwrap_or_default();
        Ok(ProviderUpdateInventory { source: PackageSource::Uv, available: true, candidates: parse_uv_outdated(&output.stdout, &installed), notes: vec!["uv's own constrained upgrade semantics are preserved; Orbis does not replace recorded version constraints or settings.".into()], metadata_state: Some("uv_tool_outdated".into()) })
    }
    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Uv, "uv"));
        }
        Ok(vec![developer_refresh_plan(
            PackageSource::Uv,
            "uv tool index metadata is resolved on demand; no separate catalog refresh is required.",
        )])
    }
    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !user_writable(&uv_bin_dir()) {
            return Ok(vec![ProviderMaintenancePlan::blocked(
                PackageSource::Uv,
                MaintenanceAction::Upgrade,
                "uv's tool executable directory is not writable by the current user; Orbis refuses sudo.",
            )]);
        }
        let inventory = self.update_inventory()?;
        let candidates = inventory.candidates;
        let warnings = inventory
            .notes
            .into_iter()
            .map(|note| crate::transaction::PlanWarning {
                level: WarningLevel::Info,
                message: note,
            })
            .collect();
        Ok(vec![developer_maintenance_plan(DeveloperMaintenanceOptions {
            source: PackageSource::Uv,
            action: MaintenanceAction::Upgrade,
            candidates: candidates.clone(),
            supported: !candidates.is_empty(),
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            warnings,
            note: "uv upgrades exact tools and retains their original constraints/settings.".into(),
        })])
    }
    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError> {
        Ok(ProviderMaintenancePlan::blocked(
            PackageSource::Uv,
            MaintenanceAction::Cleanup,
            "uv cache cleanup is outside Orbis's installed-tool cleanup scope.",
        ))
    }
    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError> {
        Ok(developer_why(
            package,
            "Installed as a persistent uv tool environment.",
            "Use uv tool uninstall through Orbis; project dependencies and virtual environments are outside this provider's scope.",
        ))
    }
    fn maintenance_operation(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError> {
        let ids = plan.candidates.iter().map(|candidate| candidate.provider_id.clone()).collect();
        Ok(ProviderOperation::Maintenance {
            operation: maintenance_operation_for(plan)
                .ok_or_else(|| {
                    unsupported_maintenance(PackageSource::Uv, "invalid uv maintenance plan")
                })?
                .with_uv_ids(ids)?,
        })
    }
    fn verify_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError> {
        let remaining = self.update_inventory()?.candidates;
        let ids: BTreeSet<_> =
            plan.candidates.iter().map(|candidate| candidate.provider_id.as_str()).collect();
        Ok(if remaining.iter().any(|candidate| ids.contains(candidate.provider_id.as_str())) {
            VerificationResult::PartiallyVerified
        } else {
            VerificationResult::Verified
        })
    }
}

impl PipxProvider {
    /// Creates a pipx provider.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
    fn available(&self) -> bool {
        self.runner.is_available("pipx")
    }
    fn snapshot(&self, outdated: bool) -> Result<Value, ProviderError> {
        let mut args = vec!["list", "--output", "json", "--skip-maintenance"];
        if outdated {
            args.push("--outdated");
        }
        let output = execute(
            &self.runner,
            PackageSource::Pipx,
            "read pipx installed snapshot",
            CommandSpec::new("pipx", args).with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Pipx, "read pipx installed snapshot", output)?;
        serde_json::from_str(&output.stdout).map_err(|error| ProviderError::Parse {
            package_source: PackageSource::Pipx,
            operation: "parse pipx JSON snapshot".into(),
            technical: error.to_string(),
        })
    }
    fn installed(&self) -> Result<BTreeMap<String, InstalledTool>, ProviderError> {
        Ok(parse_pipx_snapshot(&self.snapshot(false)?))
    }
}

impl Provider for PipxProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Pipx
    }
    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let bin = pipx_bin_dir();
        let writable = available && user_writable(&bin);
        SourceInfo {
            source: PackageSource::Pipx,
            available,
            state: if !available {
                "unavailable".into()
            } else if writable {
                "ready".into()
            } else {
                "restricted".into()
            },
            backend: Some("pipx user applications".into()),
            capabilities: pipx_capabilities(),
            notes: if !available {
                vec!["Install pipx to manage isolated user Python applications.".into()]
            } else {
                vec![format!("User application bin directory: {}.", bin.display()), "Fuzzy PyPI search is intentionally unsupported; use an explicit pipx:package reference for installation.".into()]
            },
        }
    }
    fn search(&self, _query: &str) -> Result<Vec<Package>, ProviderError> {
        Ok(Vec::new())
    }
    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !valid_python_name(package_id) {
            return Err(ProviderError::NotFound {
                package_source: PackageSource::Pipx,
                query: package_id.into(),
            });
        }
        if !self.available() {
            return Err(unavailable(PackageSource::Pipx, "pipx"));
        }
        let installed = self.installed()?;
        Ok(installed.get(package_id).map(|tool| tool.package.clone()).unwrap_or_else(|| {
            placeholder_package(
                PackageSource::Pipx,
                package_id,
                Some(false),
                "Python command-line tool managed in an isolated pipx environment.",
            )
        }))
    }
    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Pipx,
                "pipx",
                "Install pipx to enable user application management.",
            );
        }
        let bin = pipx_bin_dir();
        if !user_writable(&bin) {
            return DiagnosticCheck::failed(
                PackageSource::Pipx,
                "User application bin",
                "pipx's user application directory is not user-writable; Orbis will not use sudo.",
            );
        }
        match execute(
            &self.runner,
            PackageSource::Pipx,
            "check pipx",
            CommandSpec::new("pipx", ["--version"]).with_timeout(short_timeout()),
        ) {
            Ok(output) if output.success() => {
                DiagnosticCheck::passed(PackageSource::Pipx, "pipx user environment is available")
            }
            Ok(output) => {
                DiagnosticCheck::failed(PackageSource::Pipx, "pipx", &first_error(&output))
            }
            Err(error) => DiagnosticCheck::failed(PackageSource::Pipx, "pipx", &error.to_string()),
        }
    }
    fn supports_unqualified_resolution(&self) -> bool {
        false
    }
    fn requires_source_qualification(&self) -> bool {
        true
    }
}

impl TransactionProvider for PipxProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        developer_plan(
            request,
            target,
            DeveloperPlanOptions {
                source: PackageSource::Pipx,
                validator: validate_python_transaction_name,
                root_ok: user_writable(&pipx_bin_dir()),
                completeness: PlanCompleteness::Partial,
                confidence: PlanConfidence::Medium,
                warnings: vec![
                    "pipx JSON planning is not a mutation simulation; the plan is based on the explicit package name and installed snapshot.".into(),
                    "Orbis uses pipx's current-user commands and `--skip-maintenance` where supported so unrelated shared libraries are not silently upgraded.".into(),
                    "Existing pipx environments retain their recorded backend; Orbis does not force pip or uv.".into(),
                ],
            },
        )
    }
    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_python_transaction_name(&plan.target.provider_id)?;
        Ok(ProviderOperation::Pipx {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
        })
    }
    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        let installed = self.installed()?.contains_key(&plan.target.provider_id);
        Ok(if installed == (plan.action == OperationAction::Install) {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        })
    }
}

impl MaintenanceProvider for PipxProvider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError> {
        let snapshot = self.snapshot(true)?;
        let tools = parse_pipx_snapshot(&snapshot);
        let mut candidates = Vec::new();
        let mut notes = Vec::new();
        for tool in tools.values() {
            let metadata = &tool.package.metadata;
            let pinned = metadata.get("pinned").is_some_and(|value| value == "true");
            if pinned {
                notes.push(format!(
                    "{} is held by pipx pin and was not included in upgrades.",
                    tool.package.provider_id
                ));
                continue;
            }
            if let Some(latest) = metadata.get("latest_version") {
                if tool.package.version.as_deref() != Some(latest) {
                    candidates.push(UpdateCandidate {
                        source: PackageSource::Pipx,
                        provider_id: tool.package.provider_id.clone(),
                        name: tool.package.name.clone(),
                        current_version: tool.package.version.clone(),
                        available_version: Some(latest.clone()),
                        architecture: None,
                        scope: Some(InstallScope::User),
                        channel: None,
                        held: Some(false),
                        security_relevance: None,
                        notes: vec![
                            "pipx upgrade candidate from pipx's structured outdated snapshot."
                                .into(),
                        ],
                        metadata: metadata.clone(),
                    });
                }
            }
        }
        Ok(ProviderUpdateInventory {
            source: PackageSource::Pipx,
            available: true,
            candidates,
            notes,
            metadata_state: Some("pipx_json_outdated_snapshot".into()),
        })
    }
    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !self.available() {
            return Err(unavailable(PackageSource::Pipx, "pipx"));
        }
        Ok(vec![developer_refresh_plan(
            PackageSource::Pipx,
            "pipx resolves package index metadata on demand; no separate catalog refresh is required.",
        )])
    }
    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError> {
        if !user_writable(&pipx_bin_dir()) {
            return Ok(vec![ProviderMaintenancePlan::blocked(
                PackageSource::Pipx,
                MaintenanceAction::Upgrade,
                "pipx's user application directory is not writable by the current user; Orbis refuses sudo.",
            )]);
        }
        let inventory = self.update_inventory()?;
        let candidates = inventory.candidates;
        let mut warnings = inventory
            .notes
            .into_iter()
            .map(|note| crate::transaction::PlanWarning {
                level: WarningLevel::Info,
                message: note,
            })
            .collect::<Vec<_>>();
        warnings.push(crate::transaction::PlanWarning { level: WarningLevel::Info, message: "Execution upgrades exact reviewed non-pinned applications with pipx --skip-maintenance; pinned applications remain untouched.".into() });
        Ok(vec![developer_maintenance_plan(DeveloperMaintenanceOptions {
            source: PackageSource::Pipx,
            action: MaintenanceAction::Upgrade,
            candidates: candidates.clone(),
            supported: !candidates.is_empty(),
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            warnings,
            note:
                "pipx upgrades exact reviewed applications without changing their recorded backend."
                    .into(),
        })])
    }
    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError> {
        Ok(ProviderMaintenancePlan::blocked(
            PackageSource::Pipx,
            MaintenanceAction::Cleanup,
            "pipx cache purge is outside Orbis's installed-tool cleanup scope.",
        ))
    }
    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError> {
        Ok(developer_why(
            package,
            "Installed as an isolated pipx application environment.",
            "Use pipx uninstall through Orbis; Orbis does not manipulate pipx virtualenvs or app links manually.",
        ))
    }
    fn maintenance_operation(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError> {
        let ids = plan.candidates.iter().map(|candidate| candidate.provider_id.clone()).collect();
        Ok(ProviderOperation::Maintenance {
            operation: maintenance_operation_for(plan)
                .ok_or_else(|| {
                    unsupported_maintenance(PackageSource::Pipx, "invalid pipx maintenance plan")
                })?
                .with_pipx_ids(ids)?,
        })
    }
    fn verify_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError> {
        let remaining = self.update_inventory()?.candidates;
        let ids: BTreeSet<_> =
            plan.candidates.iter().map(|candidate| candidate.provider_id.as_str()).collect();
        Ok(if remaining.iter().any(|candidate| ids.contains(candidate.provider_id.as_str())) {
            VerificationResult::PartiallyVerified
        } else {
            VerificationResult::Verified
        })
    }
}

trait MaintenanceOperationExt {
    fn with_npm_specs(
        self,
        specs: Vec<String>,
    ) -> Result<crate::transaction::MaintenanceOperation, ProviderError>;
    fn with_pnpm_ids(
        self,
        ids: Vec<String>,
    ) -> Result<crate::transaction::MaintenanceOperation, ProviderError>;
    fn with_uv_ids(
        self,
        ids: Vec<String>,
    ) -> Result<crate::transaction::MaintenanceOperation, ProviderError>;
    fn with_pipx_ids(
        self,
        ids: Vec<String>,
    ) -> Result<crate::transaction::MaintenanceOperation, ProviderError>;
}

impl MaintenanceOperationExt for crate::transaction::MaintenanceOperation {
    fn with_npm_specs(self, specs: Vec<String>) -> Result<Self, ProviderError> {
        match self {
            Self::NpmUpgrade { .. } => Ok(Self::NpmUpgrade { package_ids: specs }),
            _ => Err(unsupported_maintenance(PackageSource::Npm, "expected npm upgrade operation")),
        }
    }
    fn with_pnpm_ids(self, ids: Vec<String>) -> Result<Self, ProviderError> {
        match self {
            Self::PnpmUpgrade { .. } => Ok(Self::PnpmUpgrade { package_ids: ids }),
            _ => {
                Err(unsupported_maintenance(PackageSource::Pnpm, "expected pnpm upgrade operation"))
            }
        }
    }
    fn with_uv_ids(self, ids: Vec<String>) -> Result<Self, ProviderError> {
        match self {
            Self::UvUpgrade { .. } => Ok(Self::UvUpgrade { package_ids: ids }),
            _ => Err(unsupported_maintenance(PackageSource::Uv, "expected uv upgrade operation")),
        }
    }
    fn with_pipx_ids(self, ids: Vec<String>) -> Result<Self, ProviderError> {
        match self {
            Self::PipxUpgrade { .. } => Ok(Self::PipxUpgrade { package_ids: ids }),
            _ => {
                Err(unsupported_maintenance(PackageSource::Pipx, "expected pipx upgrade operation"))
            }
        }
    }
}

#[derive(Clone, Debug)]
struct InstalledTool {
    package: Package,
}

fn unavailable(source: PackageSource, program: &str) -> ProviderError {
    ProviderError::Unavailable { package_source: source, program: program.into() }
}
fn unsupported_maintenance(source: PackageSource, detail: &str) -> ProviderError {
    ProviderError::Command {
        package_source: source,
        operation: "build maintenance operation".into(),
        message: detail.into(),
        technical: None,
    }
}
fn first_error(output: &crate::process::CommandOutput) -> String {
    output
        .stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("provider returned a non-zero status")
        .trim()
        .to_owned()
}
fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}
fn cargo_root() -> PathBuf {
    std::env::var_os("CARGO_INSTALL_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CARGO_HOME").map(|path| PathBuf::from(path).join("bin")))
        .unwrap_or_else(|| home_dir().join(".cargo/bin"))
}
fn uv_bin_dir() -> PathBuf {
    std::env::var_os("UV_TOOL_BIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/bin"))
}
fn pipx_bin_dir() -> PathBuf {
    std::env::var_os("PIPX_BIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/bin"))
}
fn bin_in_path(bin: &Path) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|entry| entry == bin))
}
fn user_writable(path: &Path) -> bool {
    let Some(home_metadata) = fs::metadata(home_dir()).ok() else { return false };
    let owner = owner_uid(&home_metadata);
    let mut current = path;
    loop {
        if let Ok(metadata) = fs::metadata(current) {
            return owner_uid(&metadata) == owner && writable_mode(&metadata);
        }
        let Some(parent) = current.parent() else { return false };
        if parent == current {
            return false;
        }
        current = parent;
    }
}
fn owner_uid(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.uid()
    }
    #[cfg(not(unix))]
    {
        0
    }
}
fn writable_mode(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.is_dir() && metadata.mode() & 0o200 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_dir()
    }
}

struct DeveloperPlanOptions {
    source: PackageSource,
    validator: fn(&str) -> Result<(), TransactionError>,
    root_ok: bool,
    completeness: PlanCompleteness,
    confidence: PlanConfidence,
    warnings: Vec<String>,
}

fn developer_plan(
    request: &OperationRequest,
    target: &Package,
    options: DeveloperPlanOptions,
) -> Result<OperationPlan, TransactionError> {
    (options.validator)(&target.provider_id)?;
    if request.scope.is_some_and(|scope| scope != InstallScope::User) {
        return Err(TransactionError::InvalidRequest(format!(
            "{} developer tools are user-scoped; use `--scope user` only if a scope is needed",
            options.source
        )));
    }
    if request.channel.is_some() {
        return Err(TransactionError::InvalidRequest(
            "`--channel` is only supported for Snap operations".into(),
        ));
    }
    let installed = target.installed == Some(true);
    if matches!(
        (request.action, installed),
        (OperationAction::Install, true) | (OperationAction::Remove, false)
    ) {
        return Err(TransactionError::Planning(format!(
            "{} `{}` is already in the requested state",
            options.source, target.provider_id
        )));
    }
    let mut plan = OperationPlan::new(request.action, target.clone(), InstallScope::User);
    plan.privilege = PrivilegeRequirement::None;
    plan.completeness = options.completeness;
    plan.confidence = options.confidence;
    plan.authoritative_simulation = false;
    plan.changes.push(target_change(&plan));
    plan.risk = base_risk(request.action, Some(PackageKind::CliTool));
    plan.warnings.extend(
        options
            .warnings
            .into_iter()
            .map(|message| crate::transaction::PlanWarning { level: WarningLevel::Info, message }),
    );
    if !options.root_ok {
        plan.risk = RiskLevel::Blocked;
        plan.completeness = PlanCompleteness::Unknown;
        plan.confidence = PlanConfidence::Low;
        plan.warnings.push(crate::transaction::PlanWarning { level: WarningLevel::Blocked, message: format!("{} is configured outside a writable current-user destination; Orbis refuses sudo and blocks this mutation.", options.source) });
    }
    Ok(plan)
}

fn developer_refresh_plan(source: PackageSource, note: &str) -> ProviderMaintenancePlan {
    ProviderMaintenancePlan {
        operation_id: maintenance_id(source, MaintenanceAction::Refresh),
        source,
        action: MaintenanceAction::Refresh,
        scope: Some(InstallScope::User),
        candidates: Vec::new(),
        cleanup_candidates: Vec::new(),
        privilege: PrivilegeRequirement::None,
        completeness: PlanCompleteness::Complete,
        confidence: PlanConfidence::High,
        authoritative_simulation: false,
        risk: RiskLevel::Normal,
        supported: true,
        mutates: false,
        warnings: Vec::new(),
        notes: vec![note.into()],
        download_size_bytes: None,
        disk_delta_bytes: None,
    }
}
struct DeveloperMaintenanceOptions {
    source: PackageSource,
    action: MaintenanceAction,
    candidates: Vec<UpdateCandidate>,
    supported: bool,
    completeness: PlanCompleteness,
    confidence: PlanConfidence,
    warnings: Vec<crate::transaction::PlanWarning>,
    note: String,
}

fn developer_maintenance_plan(options: DeveloperMaintenanceOptions) -> ProviderMaintenancePlan {
    ProviderMaintenancePlan {
        operation_id: maintenance_id(options.source, options.action),
        source: options.source,
        action: options.action,
        scope: Some(InstallScope::User),
        candidates: options.candidates,
        cleanup_candidates: Vec::new(),
        privilege: PrivilegeRequirement::None,
        completeness: options.completeness,
        confidence: options.confidence,
        authoritative_simulation: false,
        risk: if options.supported { RiskLevel::Normal } else { RiskLevel::Blocked },
        supported: options.supported,
        mutates: options.supported,
        warnings: options.warnings,
        notes: vec![options.note],
        download_size_bytes: None,
        disk_delta_bytes: None,
    }
}
fn developer_why(package: &Package, installed_as: &str, advice: &str) -> WhyReport {
    WhyReport {
        package: package.clone(),
        installed_as: installed_as.into(),
        used_by: Vec::new(),
        evidence: vec!["Provider-owned installed-tool metadata and current user scope.".into()],
        removal_advice: advice.into(),
        orbis_history: Vec::new(),
        notes: Vec::new(),
    }
}
fn maintenance_id(source: PackageSource, action: MaintenanceAction) -> String {
    format!(
        "maint-{}-{}-{}",
        source.label().to_ascii_lowercase(),
        action.label().to_ascii_lowercase(),
        std::process::id()
    )
}

fn base_package(
    source: PackageSource,
    id: &str,
    version: Option<String>,
    summary: Option<String>,
    installed: Option<bool>,
) -> Package {
    Package {
        source,
        provider_id: id.into(),
        name: id.into(),
        version,
        summary: summary.clone(),
        description: summary,
        installed,
        kind: Some(PackageKind::CliTool),
        origin: None,
        architecture: None,
        homepage: None,
        license: None,
        size_bytes: None,
        metadata: BTreeMap::new(),
    }
}
fn placeholder_package(
    source: PackageSource,
    id: &str,
    installed: Option<bool>,
    summary: &str,
) -> Package {
    base_package(source, id, None, Some(summary.into()), installed)
}

fn valid_name(value: &str, scoped: bool) -> bool {
    if value.is_empty()
        || value.len() > 214
        || value.starts_with('-')
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, ';' | '&' | '|' | '$' | '`' | '\'' | '"')
        })
    {
        return false;
    }
    if scoped {
        let Some((scope, name)) = value.strip_prefix('@').and_then(|value| value.split_once('/'))
        else {
            return false;
        };
        !scope.is_empty() && valid_plain(scope) && valid_plain(name) && !name.contains('/')
    } else {
        valid_plain(value)
    }
}
fn valid_plain(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '~')
        })
}
fn valid_npm_name(value: &str) -> bool {
    valid_name(value, value.starts_with('@'))
}
fn valid_cargo_name(value: &str) -> bool {
    valid_name(value, false)
}
fn valid_python_name(value: &str) -> bool {
    valid_name(value, false)
}
fn validate_npm_transaction_name(value: &str) -> Result<(), TransactionError> {
    valid_npm_name(value).then_some(()).ok_or_else(|| TransactionError::InvalidRequest("npm/pnpm package names must be registry names; local paths, URLs, aliases, options, and shell-like characters are rejected".into()))
}
fn validate_cargo_transaction_name(value: &str) -> Result<(), TransactionError> {
    valid_cargo_name(value).then_some(()).ok_or_else(|| TransactionError::InvalidRequest("Cargo package names must be registry crate names; Git, path, option, and shell-like specifications are rejected".into()))
}
fn validate_python_transaction_name(value: &str) -> Result<(), TransactionError> {
    valid_python_name(value).then_some(()).ok_or_else(|| TransactionError::InvalidRequest("Python tool names must be registry package names without paths, URLs, options, or shell-like characters".into()))
}

fn parse_cargo_installed(text: &str) -> BTreeMap<String, InstalledTool> {
    let mut result = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let trimmed = line.trim();
        let Some(version_start) = trimmed.find(" v") else { continue };
        let Some(end) = trimmed.rfind(':') else { continue };
        let name = trimmed[..version_start].trim();
        let version = &trimmed[version_start + 2..end];
        if valid_cargo_name(name) && !version.is_empty() {
            let mut package =
                base_package(PackageSource::Cargo, name, Some(version.into()), None, Some(true));
            package.metadata.insert("install_root".into(), cargo_root().display().to_string());
            result.insert(name.into(), InstalledTool { package });
        }
    }
    result
}
fn parse_cargo_search(text: &str) -> Vec<Package> {
    text.lines()
        .filter_map(|line| {
            let (left, description) = line.split_once('#').unwrap_or((line, ""));
            let (name, version_tail) = left.split_once(" = \"")?;
            let version_end = version_tail.find('"')?;
            let version = version_tail[..version_end].trim();
            valid_cargo_name(name.trim()).then(|| {
                let mut package = base_package(
                    PackageSource::Cargo,
                    name.trim(),
                    Some(version.into()),
                    (!description.trim().is_empty()).then(|| description.trim().into()),
                    Some(false),
                );
                package.description = package.summary.clone();
                package
            })
        })
        .take(60)
        .collect()
}
fn parse_cargo_info(
    text: &str,
    fallback: &str,
    installed: Option<&InstalledTool>,
) -> Result<Package, ProviderError> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let heading = lines.next().unwrap_or(fallback);
    let name = heading.split_whitespace().next().unwrap_or(fallback);
    let mut package =
        base_package(PackageSource::Cargo, name, None, None, Some(installed.is_some()));
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "version" => package.version = Some(value.into()),
                "license" => package.license = Some(value.into()),
                "homepage" => package.homepage = Some(value.into()),
                "description" => package.description = Some(value.into()),
                "repository" => {
                    package.metadata.insert("repository".into(), value.into());
                }
                _ => {}
            }
        } else if package.summary.is_none() {
            package.summary = Some(line.trim().into());
        }
    }
    if let Some(installed) = installed {
        package.version = installed.package.version.clone().or(package.version);
        package.metadata.extend(installed.package.metadata.clone());
    }
    Ok(package)
}

fn parse_json(text: &str, source: PackageSource, operation: &str) -> Result<Value, ProviderError> {
    serde_json::from_str(text).map_err(|error| ProviderError::Parse {
        package_source: source,
        operation: operation.into(),
        technical: error.to_string(),
    })
}
fn parse_registry_search(source: PackageSource, text: &str) -> Result<Vec<Package>, ProviderError> {
    let value = parse_json(text, source, "parse registry search JSON")?;
    let array = value.as_array().ok_or_else(|| ProviderError::Parse {
        package_source: source,
        operation: "parse registry search JSON".into(),
        technical: "expected a JSON array".into(),
    })?;
    Ok(array
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            if !valid_npm_name(name) {
                return None;
            }
            let version = item.get("version").and_then(Value::as_str).map(str::to_owned);
            let summary = item.get("description").and_then(Value::as_str).map(str::to_owned);
            Some(base_package(source, name, version, summary, Some(false)))
        })
        .take(60)
        .collect())
}
fn parse_registry_info(
    source: PackageSource,
    text: &str,
    fallback: &str,
    installed: Option<&InstalledTool>,
) -> Result<Package, ProviderError> {
    let value = parse_json(text, source, "parse registry package JSON")?;
    let object = value
        .as_object()
        .or_else(|| value.as_array().and_then(|items| items.first()).and_then(Value::as_object))
        .ok_or_else(|| ProviderError::Parse {
            package_source: source,
            operation: "parse registry package JSON".into(),
            technical: "expected a JSON object".into(),
        })?;
    let name = object.get("name").and_then(Value::as_str).unwrap_or(fallback);
    let mut package = base_package(
        source,
        name,
        object.get("version").and_then(Value::as_str).map(str::to_owned),
        object.get("description").and_then(Value::as_str).map(str::to_owned),
        Some(installed.is_some()),
    );
    package.homepage = object.get("homepage").and_then(Value::as_str).map(str::to_owned);
    package.license = object.get("license").and_then(Value::as_str).map(str::to_owned);
    if let Some(bin) = object.get("bin") {
        package.metadata.insert("bin".into(), compact_json(bin));
    }
    if let Some(installed) = installed {
        package.version = installed.package.version.clone().or(package.version);
        package.metadata.extend(installed.package.metadata.clone());
    }
    Ok(package)
}
fn compact_json(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}
fn parse_npm_installed(
    text: &str,
    status: Option<i32>,
) -> Result<BTreeMap<String, InstalledTool>, ProviderError> {
    let value = parse_json(text, PackageSource::Npm, "parse npm global JSON")?;
    let Some(deps) = value.get("dependencies").and_then(Value::as_object) else {
        return if status == Some(0) {
            Ok(BTreeMap::new())
        } else {
            Err(ProviderError::Parse {
                package_source: PackageSource::Npm,
                operation: "parse npm global JSON".into(),
                technical: "npm returned no dependencies object".into(),
            })
        };
    };
    Ok(deps
        .iter()
        .filter_map(|(name, item)| {
            let version = item.get("version").and_then(Value::as_str)?.to_owned();
            let mut package = base_package(
                PackageSource::Npm,
                name,
                Some(version),
                item.get("description").and_then(Value::as_str).map(str::to_owned),
                Some(true),
            );
            package.metadata.insert(
                "global_prefix".into(),
                item.get("path").and_then(Value::as_str).unwrap_or("").to_owned(),
            );
            Some((name.clone(), InstalledTool { package }))
        })
        .collect())
}
fn parse_npm_outdated(
    text: &str,
    status: Option<i32>,
) -> Result<ProviderUpdateInventory, ProviderError> {
    let value = parse_json(text, PackageSource::Npm, "parse npm outdated JSON")?;
    let Some(object) = value.as_object() else {
        return Err(ProviderError::Parse {
            package_source: PackageSource::Npm,
            operation: "parse npm outdated JSON".into(),
            technical: "expected an object".into(),
        });
    };
    let mut candidates = Vec::new();
    let mut notes = Vec::new();
    for (name, item) in object {
        let current = item.get("current").and_then(Value::as_str).map(str::to_owned);
        let latest = item.get("latest").and_then(Value::as_str).map(str::to_owned);
        let wanted = item.get("wanted").and_then(Value::as_str).map(str::to_owned);
        if let (Some(current), Some(latest)) = (current.clone(), latest.clone()) {
            if version_cmp(&current, &latest)
                .is_some_and(|ordering| ordering == std::cmp::Ordering::Greater)
            {
                notes.push(format!("npm reported {name} at {current}, newer than its latest dist-tag {latest}; Orbis excluded it to prevent a downgrade."));
                continue;
            }
            if current != latest {
                candidates.push(update_candidate(
                    PackageSource::Npm,
                    name,
                    current,
                    Some(latest),
                    wanted,
                ));
            }
        }
    }
    Ok(ProviderUpdateInventory {
        source: PackageSource::Npm,
        available: true,
        candidates,
        notes: if status.is_some_and(|code| code != 0) {
            notes.into_iter().chain(["npm uses a non-zero outdated status when updates are present; valid JSON was still accepted.".into()]).collect()
        } else {
            notes
        },
        metadata_state: Some("npm_global_outdated_json".into()),
    })
}
fn parse_pnpm_installed(text: &str) -> Result<BTreeMap<String, InstalledTool>, ProviderError> {
    let value = parse_json(text, PackageSource::Pnpm, "parse pnpm global JSON")?;
    let mut result = BTreeMap::new();
    let items = if let Some(array) = value.as_array() {
        array.iter().collect::<Vec<_>>()
    } else {
        vec![&value]
    };
    for item in items {
        if let Some(deps) = item.get("dependencies").and_then(Value::as_object) {
            for (name, dep) in deps {
                if let Some(version) = dep.get("version").and_then(Value::as_str) {
                    result.insert(
                        name.clone(),
                        InstalledTool {
                            package: base_package(
                                PackageSource::Pnpm,
                                name,
                                Some(version.into()),
                                dep.get("description").and_then(Value::as_str).map(str::to_owned),
                                Some(true),
                            ),
                        },
                    );
                }
            }
        }
    }
    Ok(result)
}
fn parse_pnpm_outdated(
    text: &str,
    status: Option<i32>,
) -> Result<ProviderUpdateInventory, ProviderError> {
    let value = parse_json(text, PackageSource::Pnpm, "parse pnpm outdated JSON")?;
    let mut candidates = Vec::new();
    let mut visit = |name: &str, item: &Value| {
        let current =
            item.get("current").or_else(|| item.get("installedVersion")).and_then(Value::as_str);
        let latest =
            item.get("latest").or_else(|| item.get("latestVersion")).and_then(Value::as_str);
        let wanted =
            item.get("wanted").or_else(|| item.get("wantedVersion")).and_then(Value::as_str);
        if let (Some(current), Some(latest)) = (current, latest) {
            if current != latest {
                candidates.push(update_candidate(
                    PackageSource::Pnpm,
                    name,
                    current.into(),
                    Some(latest.into()),
                    wanted.map(str::to_owned),
                ));
            }
        }
    };
    if let Some(object) = value.as_object() {
        for (name, item) in object {
            visit(name, item);
        }
    } else if let Some(array) = value.as_array() {
        for item in array {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                visit(name, item);
            }
        }
    }
    Ok(ProviderUpdateInventory {
        source: PackageSource::Pnpm,
        available: true,
        candidates,
        notes: if status.is_some_and(|code| code != 0) {
            vec!["pnpm may use a benign non-zero status when outdated packages are present; valid JSON was accepted.".into()]
        } else {
            Vec::new()
        },
        metadata_state: Some("pnpm_global_outdated_json".into()),
    })
}
fn update_candidate(
    source: PackageSource,
    id: &str,
    current: String,
    latest: Option<String>,
    wanted: Option<String>,
) -> UpdateCandidate {
    let available = wanted.or(latest);
    UpdateCandidate {
        source,
        provider_id: id.into(),
        name: id.into(),
        current_version: Some(current),
        available_version: available,
        architecture: None,
        scope: Some(InstallScope::User),
        channel: None,
        held: Some(false),
        security_relevance: None,
        notes: Vec::new(),
        metadata: BTreeMap::new(),
    }
}
fn parse_uv_list(text: &str) -> BTreeMap<String, InstalledTool> {
    let mut result = BTreeMap::new();
    for line in
        text.lines().filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('-'))
    {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        let Some(version) = fields.next().map(|value| value.trim_start_matches('v')) else {
            continue;
        };
        if valid_python_name(name)
            && version.chars().next().is_some_and(|character| character.is_ascii_digit())
        {
            let mut package = placeholder_package(
                PackageSource::Uv,
                name,
                Some(true),
                "Python CLI tool managed in an isolated uv tool environment.",
            );
            package.version = Some(version.into());
            package.metadata.insert(
                "tool_directory".into(),
                home_dir().join(".local/share/uv/tools").display().to_string(),
            );
            result.insert(name.into(), InstalledTool { package });
        }
    }
    result
}
fn parse_uv_outdated(
    text: &str,
    installed: &BTreeMap<String, InstalledTool>,
) -> Vec<UpdateCandidate> {
    let mut result = Vec::new();
    for line in text.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        let Some(name) = words.first().copied() else { continue };
        let Some(current) = installed.get(name).and_then(|tool| tool.package.version.clone())
        else {
            continue;
        };
        let latest = words
            .windows(2)
            .find(|window| window[0].trim_end_matches(':').eq_ignore_ascii_case("latest"))
            .map(|window| window[1].trim_start_matches('v').to_owned())
            .or_else(|| {
                words
                    .windows(2)
                    .find(|window| window[0] == "->")
                    .map(|window| window[1].trim_start_matches('v').to_owned())
            });
        if let Some(latest) = latest.filter(|latest| latest != &current) {
            result.push(update_candidate(PackageSource::Uv, name, current, Some(latest), None));
        }
    }
    result
}

fn parse_pipx_snapshot(value: &Value) -> BTreeMap<String, InstalledTool> {
    let mut result = BTreeMap::new();
    let Some(venvs) = value.get("venvs").and_then(Value::as_object) else { return result };
    for (name, venv) in venvs {
        let metadata = venv.get("metadata").and_then(Value::as_object);
        let main = metadata.and_then(|metadata| metadata.get("main_package")).unwrap_or(venv);
        let package_name = main.get("package").and_then(Value::as_str).unwrap_or(name);
        let version = main.get("package_version").and_then(Value::as_str).map(str::to_owned);
        let mut package = placeholder_package(
            PackageSource::Pipx,
            package_name,
            Some(true),
            "Python command-line tool managed in an isolated pipx environment.",
        );
        package.version = version;
        if let Some(apps) = venv.get("apps").or_else(|| main.get("apps_of_bin")) {
            package.metadata.insert("apps".into(), compact_json(apps));
        }
        if let Some(pinned) = venv.get("pinned").or_else(|| main.get("pinned")) {
            package.metadata.insert("pinned".into(), pinned.as_bool().unwrap_or(false).to_string());
        }
        if let Some(latest) = main.get("latest_version").and_then(Value::as_str) {
            package.metadata.insert("latest_version".into(), latest.into());
        }
        if let Some(python) = venv.get("python_version").and_then(Value::as_str) {
            package.metadata.insert("python_version".into(), python.into());
        }
        if let Some(backend) = venv.get("backend").and_then(Value::as_str) {
            package.metadata.insert("backend".into(), backend.into());
        }
        result.insert(package_name.into(), InstalledTool { package });
    }
    result
}

fn version_cmp(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let parse = |value: &str| {
        value
            .split(['.', '-', '+'])
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()
    };
    Some(parse(left)?.cmp(&parse(right)?))
}

fn cargo_capabilities() -> ProviderCapabilities {
    capabilities(true, true, true, false, true, false, true)
}
fn npm_capabilities() -> ProviderCapabilities {
    capabilities(true, true, true, true, true, false, true)
}
fn pnpm_capabilities() -> ProviderCapabilities {
    capabilities(true, true, true, true, true, false, true)
}
fn uv_capabilities() -> ProviderCapabilities {
    capabilities(false, true, true, true, true, false, true)
}
fn pipx_capabilities() -> ProviderCapabilities {
    capabilities(false, true, true, true, true, false, true)
}
fn capabilities(
    search: bool,
    info: bool,
    installed: bool,
    updates: bool,
    upgrade: bool,
    cleanup: bool,
    why: bool,
) -> ProviderCapabilities {
    ProviderCapabilities {
        search,
        info,
        installed_state: installed,
        installed_list: installed,
        mutations: true,
        install: true,
        remove: true,
        updates,
        upgrade,
        refresh: true,
        cleanup,
        why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_installed_parser_keeps_crates_and_binaries() {
        let installed = parse_cargo_installed(
            "ripgrep v15.2.0:\n    rg\n\nfoo-bar v1.0.0 (/tmp/foo):\n    foo\n",
        );
        assert_eq!(installed["ripgrep"].package.version.as_deref(), Some("15.2.0"));
        assert!(installed.contains_key("foo-bar"));
    }

    #[test]
    fn cargo_search_parser_handles_aligned_descriptions() {
        let results =
            parse_cargo_search("ripgrep = \"15.2.0\"    # fast search\nplain = \"1.0.0\"\n");
        assert_eq!(results[0].version.as_deref(), Some("15.2.0"));
        assert_eq!(results[0].summary.as_deref(), Some("fast search"));
        assert_eq!(results[1].version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn npm_scoped_and_python_names_are_conservative() {
        assert!(valid_npm_name("@scope/package"));
        assert!(valid_python_name("my_tool.name-2"));
        assert!(!valid_npm_name("file:../package"));
        assert!(!valid_npm_name("package;echo"));
        assert!(!valid_python_name("--help"));
    }

    #[test]
    fn npm_outdated_nonzero_status_is_not_a_provider_failure() {
        let report = parse_npm_outdated(
            r#"{"typescript":{"current":"1.0.0","wanted":"1.1.0","latest":"1.1.0"}}"#,
            Some(1),
        )
        .expect("valid outdated JSON");
        assert_eq!(report.candidates.len(), 1);
        assert!(report.notes.iter().any(|note| note.contains("non-zero")));
    }

    #[test]
    fn npm_newer_than_latest_is_excluded_as_a_downgrade() {
        let report = parse_npm_outdated(
            r#"{"tool":{"current":"3.0.0","wanted":"2.0.0","latest":"2.0.0"}}"#,
            Some(1),
        )
        .expect("valid outdated JSON");
        assert!(report.candidates.is_empty());
        assert!(report.notes[0].contains("downgrade"));
    }

    #[test]
    fn pipx_snapshot_preserves_pin_backend_and_apps() {
        let value: Value = serde_json::from_str(r#"{"venvs":{"black":{"metadata":{"main_package":{"package":"black","package_version":"24.0"}},"apps":["black"],"pinned":true,"backend":"pip"}}}"#).expect("json");
        let package = parse_pipx_snapshot(&value).remove("black").expect("black").package;
        assert_eq!(package.metadata["pinned"], "true");
        assert_eq!(package.metadata["backend"], "pip");
        assert!(package.metadata["apps"].contains("black"));
    }

    #[test]
    fn uv_has_no_fuzzy_search_capability() {
        assert!(!uv_capabilities().search);
    }
}
