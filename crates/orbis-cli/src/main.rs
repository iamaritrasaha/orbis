use std::{
    io::{self, IsTerminal},
    process::ExitCode,
};

use clap::{Parser, Subcommand, ValueEnum};
use orbis_core::{
    ProviderRegistry, ResolveReport, SearchReport,
    diagnostics::DoctorReport,
    explain::build_brief,
    maintenance::{
        MaintenanceAction, MaintenancePlan, MaintenanceProviderResult, MaintenanceProviderStatus,
        MaintenanceResult, MaintenanceStatus, UpdateInventoryReport, WhyReport,
    },
    models::{Package, PackageSource, SourceInfo},
    parse_package_ref,
    privilege::RealOperationExecutor,
    transaction::{
        InstallScope, OperationAction, OperationPlan, OperationRequest, PackageRefJson,
        TransactionError, TransactionResult,
    },
};

#[derive(Debug, Parser)]
#[command(
    name = "orbis",
    version,
    about = "Your Linux software, in one place.",
    long_about = "A calm, provider-neutral view of software available to your Linux system. Read-only discovery and carefully confirmed single-package operations."
)]
struct Cli {
    /// Emit structured JSON instead of terminal presentation.
    #[arg(long, global = true)]
    json: bool,
    /// Disable ANSI styling even when stdout is a terminal.
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show detected providers and read-only capabilities.
    Sources,
    /// Search all available providers, or one selected source.
    Search {
        /// Human package name, keyword, or application ID.
        query: String,
        /// Restrict the search to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Show normalized metadata for one package.
    Info {
        /// Package ID, friendly name, or source-qualified reference such as apt:curl.
        package: String,
        /// Restrict resolution to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Explain what a package is in plain language.
    Explain {
        /// Package ID, friendly name, or source-qualified reference.
        package: String,
        /// Restrict resolution to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Run safe provider and environment diagnostics.
    Doctor,
    /// Plan and, after confirmation, install one exact package.
    Install {
        /// Package ID, friendly name, or source-qualified reference.
        package: String,
        /// Restrict resolution to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Flatpak scope; defaults to system for new installs.
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        /// Optional Snap channel.
        #[arg(long)]
        channel: Option<String>,
        /// Show the plan without executing it. Alias: --dry-run.
        #[arg(long, alias = "dry-run")]
        plan: bool,
        /// Skip Orbis's confirmation prompt for this exact displayed plan.
        #[arg(long)]
        yes: bool,
    },
    /// Plan and, after confirmation, remove one exact package without purge/autoremove.
    Remove {
        /// Package ID, friendly name, or source-qualified reference.
        package: String,
        /// Restrict resolution to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Flatpak scope; required when installed in both scopes.
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        /// Show the plan without executing it. Alias: --dry-run.
        #[arg(long, alias = "dry-run")]
        plan: bool,
        /// Skip Orbis's confirmation prompt for this exact displayed plan.
        #[arg(long)]
        yes: bool,
    },
    /// Refresh package catalogs without upgrading installed software.
    Update {
        /// Restrict the refresh to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Show the refresh plan without changing catalog metadata.
        #[arg(long)]
        plan: bool,
        /// Skip Orbis's confirmation prompt for this exact refresh plan.
        #[arg(long)]
        yes: bool,
    },
    /// Show installed software with updates available. Read-only.
    Updates {
        /// Restrict the inventory to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Plan and, after confirmation, apply safe available updates.
    Upgrade {
        /// Restrict the upgrade to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Show the coordinated upgrade plan without changing package state.
        #[arg(long)]
        plan: bool,
        /// Skip Orbis's confirmation prompt for this exact plan.
        #[arg(long)]
        yes: bool,
    },
    /// Plan and, after confirmation, remove confidently unused package-manager artifacts.
    Clean {
        /// Restrict cleanup to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Show cleanup candidates without changing package state.
        #[arg(long)]
        plan: bool,
        /// Skip Orbis's confirmation prompt for this exact plan.
        #[arg(long)]
        yes: bool,
    },
    /// Read Orbis transaction and maintenance history.
    History {
        /// Show one exact operation ID instead of the recent list.
        operation_id: Option<String>,
        /// Maximum number of rows to display.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Restrict history rows to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Explain why one installed package or ref is present.
    Why {
        /// Package ID or source-qualified reference.
        package: String,
        /// Restrict resolution to one provider.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SourceArg {
    Apt,
    Flatpak,
    Snap,
    Cargo,
    Npm,
    Pnpm,
    Uv,
    Pipx,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ScopeArg {
    System,
    User,
}

impl From<ScopeArg> for InstallScope {
    fn from(scope: ScopeArg) -> Self {
        match scope {
            ScopeArg::System => Self::System,
            ScopeArg::User => Self::User,
        }
    }
}

impl From<SourceArg> for PackageSource {
    fn from(source: SourceArg) -> Self {
        match source {
            SourceArg::Apt => Self::Apt,
            SourceArg::Flatpak => Self::Flatpak,
            SourceArg::Snap => Self::Snap,
            SourceArg::Cargo => Self::Cargo,
            SourceArg::Npm => Self::Npm,
            SourceArg::Pnpm => Self::Pnpm,
            SourceArg::Uv => Self::Uv,
            SourceArg::Pipx => Self::Pipx,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("orbis: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let registry = ProviderRegistry::system();
    let renderer = Renderer::new(
        !cli.no_color && io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    );

    match cli.command {
        None => {
            if cli.json {
                let payload = serde_json::json!({
                    "name": "Orbis",
                    "version": env!("CARGO_PKG_VERSION"),
                    "tagline": "Your Linux software, in one place.",
                    "sources": registry.sources(),
                    "planning_read_only": true,
                    "supported_mutations": ["install", "remove", "update", "upgrade", "clean"],
                    "read_only_maintenance": ["updates", "history", "why"]
                });
                print_json(&payload)
            } else {
                print!("{}", renderer.home(&registry.sources()));
                Ok(())
            }
        }
        Some(Command::Sources) => {
            let sources = registry.sources();
            if cli.json {
                print_json(&sources)
            } else {
                print!("{}", renderer.sources(&sources));
                Ok(())
            }
        }
        Some(Command::Search { query, source }) => {
            let report = registry.search(&query, source.map(Into::into));
            if cli.json {
                print_json(&report)
            } else {
                print!("{}", renderer.search(&query, &report));
                Ok(())
            }
        }
        Some(Command::Info { package, source }) => {
            let package_ref = parse_package_ref(&package, source.map(Into::into));
            let report = registry.resolve(&package_ref);
            if cli.json {
                print_json(&report)
            } else {
                match &report {
                    ResolveReport::Found { package, issues } => {
                        print!("{}", renderer.info(package, issues))
                    }
                    ResolveReport::Ambiguous { matches, issues } => {
                        print!("{}", renderer.ambiguous(&package, matches, issues))
                    }
                    ResolveReport::NotFound { issues } => {
                        print!("{}", renderer.not_found(&package, issues))
                    }
                }
                if matches!(report, ResolveReport::NotFound { .. }) {
                    Err("package was not found".into())
                } else {
                    Ok(())
                }
            }
        }
        Some(Command::Explain { package, source }) => {
            let package_ref = parse_package_ref(&package, source.map(Into::into));
            let report = registry.resolve(&package_ref);
            match report {
                ResolveReport::Found { package, .. } => {
                    let brief = build_brief(*package);
                    if cli.json {
                        print_json(&brief)
                    } else {
                        print!("{}", renderer.brief(&brief));
                        Ok(())
                    }
                }
                ResolveReport::Ambiguous { matches, issues } => {
                    if cli.json {
                        print_json(
                            &serde_json::json!({ "status": "ambiguous", "matches": matches, "issues": issues }),
                        )?
                    } else {
                        print!("{}", renderer.ambiguous(&package, &matches, &issues));
                    }
                    Err("package reference is ambiguous".into())
                }
                ResolveReport::NotFound { issues } => {
                    if cli.json {
                        print_json(&serde_json::json!({ "status": "not_found", "issues": issues }))?
                    } else {
                        print!("{}", renderer.not_found(&package, &issues));
                    }
                    Err("package was not found".into())
                }
            }
        }
        Some(Command::Doctor) => {
            let report = registry.diagnostics(None).with_environment();
            if cli.json {
                print_json(&report)
            } else {
                print!("{}", renderer.doctor(&report));
                Ok(())
            }
        }
        Some(Command::Install { package, source, scope, channel, plan, yes }) => run_transaction(
            &registry,
            &renderer,
            cli.json,
            TransactionOptions {
                action: OperationAction::Install,
                package,
                source: source.map(Into::into),
                scope: scope.map(Into::into),
                channel,
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::Remove { package, source, scope, plan, yes }) => run_transaction(
            &registry,
            &renderer,
            cli.json,
            TransactionOptions {
                action: OperationAction::Remove,
                package,
                source: source.map(Into::into),
                scope: scope.map(Into::into),
                channel: None,
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::Update { source, plan, yes }) => run_maintenance(
            &registry,
            &renderer,
            cli.json,
            MaintenanceOptions {
                action: MaintenanceAction::Refresh,
                source: source.map(Into::into),
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::Updates { source }) => {
            let report = registry.updates(source.map(Into::into));
            if cli.json {
                print_json(&report)
            } else {
                print!("{}", renderer.updates(&report));
                Ok(())
            }
        }
        Some(Command::Upgrade { source, plan, yes }) => run_maintenance(
            &registry,
            &renderer,
            cli.json,
            MaintenanceOptions {
                action: MaintenanceAction::Upgrade,
                source: source.map(Into::into),
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::Clean { source, plan, yes }) => run_maintenance(
            &registry,
            &renderer,
            cli.json,
            MaintenanceOptions {
                action: MaintenanceAction::Cleanup,
                source: source.map(Into::into),
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::History { operation_id, limit, source }) => {
            run_history(&renderer, cli.json, operation_id, limit, source.map(Into::into))
        }
        Some(Command::Why { package, source }) => {
            run_why(&registry, &renderer, cli.json, package, source.map(Into::into))
        }
    }
}

struct TransactionOptions {
    action: OperationAction,
    package: String,
    source: Option<PackageSource>,
    scope: Option<InstallScope>,
    channel: Option<String>,
    plan_only: bool,
    yes: bool,
}

fn run_transaction(
    registry: &ProviderRegistry,
    renderer: &Renderer,
    json: bool,
    options: TransactionOptions,
) -> Result<(), String> {
    let package_ref = parse_transaction_ref(&options.package, options.source)?;
    let request = OperationRequest {
        action: options.action,
        package: PackageRefJson::from(&package_ref),
        scope: options.scope,
        channel: options.channel,
    };
    let plan = match registry.plan_transaction(&request) {
        Ok(plan) => plan,
        Err(error) => {
            if json {
                print_json(&serde_json::json!({
                    "status": "error",
                    "message": error.to_string()
                }))?;
            }
            return Err(error.to_string());
        }
    };

    if options.plan_only {
        if json {
            print_json(&plan)
        } else {
            print!("{}", renderer.transaction_plan(&plan));
            Ok(())
        }
    } else {
        if !plan.executable() {
            let error = TransactionError::Blocked(
                "this plan is incomplete or contains a blocked safety condition; nothing was executed".into(),
            );
            if json {
                print_json(&serde_json::json!({
                    "status": "blocked",
                    "plan": plan,
                    "message": error.to_string()
                }))?;
            }
            return Err(error.to_string());
        }
        if !options.yes {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                let error = "confirmation is required: use an interactive terminal or pass --yes after reviewing the plan";
                if json {
                    print_json(&serde_json::json!({
                        "status": "confirmation_required",
                        "plan": plan,
                        "message": error
                    }))?;
                }
                return Err(error.into());
            }
            if !json {
                print!("{}", renderer.transaction_plan(&plan));
            }
            if !confirm(&plan)? {
                return Err("operation cancelled; no package state was changed".into());
            }
        } else if !json {
            print!("{}", renderer.transaction_plan(&plan));
        }

        let history = match orbis_core::transaction::history::HistoryStore::default_location() {
            Ok(history) => history,
            Err(error) => {
                if json {
                    print_json(&serde_json::json!({
                        "status": "error",
                        "message": format!("could not start transaction: {error}")
                    }))?;
                }
                return Err(format!("could not start transaction: {error}"));
            }
        };
        let executor = RealOperationExecutor::new(registry.runner());
        let result = match registry.execute_transaction_with_history(
            &request,
            plan.clone(),
            &executor,
            &history,
        ) {
            Ok(result) => result,
            Err(error) => {
                if json {
                    print_json(&serde_json::json!({
                        "status": "error",
                        "message": error.to_string()
                    }))?;
                }
                return Err(error.to_string());
            }
        };
        if json {
            print_json(&result)?;
        }
        if json {
            Ok(())
        } else {
            print!("{}", renderer.transaction_result(&result));
            Ok(())
        }
    }
}

struct MaintenanceOptions {
    action: MaintenanceAction,
    source: Option<PackageSource>,
    plan_only: bool,
    yes: bool,
}

fn run_maintenance(
    registry: &ProviderRegistry,
    renderer: &Renderer,
    json: bool,
    options: MaintenanceOptions,
) -> Result<(), String> {
    let plan = match registry.maintenance_plan(options.action, options.source) {
        Ok(plan) => plan,
        Err(error) => {
            if json {
                print_json(&serde_json::json!({ "status": "error", "message": error }))?;
            }
            return Err(error);
        }
    };
    if options.plan_only {
        if json {
            print_json(&plan)
        } else {
            print!("{}", renderer.maintenance_plan(&plan));
            Ok(())
        }
    } else if !plan.executable() {
        let message = "no executable provider plan is available; unsupported or blocked providers were not changed";
        if json {
            print_json(&serde_json::json!({
                "status": "blocked",
                "plan": plan,
                "message": message
            }))?;
        }
        Err(message.into())
    } else if plan.mutates
        && !options.yes
        && (!io::stdin().is_terminal() || !io::stdout().is_terminal())
    {
        let message = "confirmation is required: use an interactive terminal or pass --yes after reviewing the plan";
        if json {
            print_json(&serde_json::json!({
                "status": "confirmation_required",
                "plan": plan,
                "message": message
            }))?;
        }
        Err(message.into())
    } else {
        if options.action == MaintenanceAction::Upgrade {
            registry.revalidate_upgrade_plan(&plan)?;
        }
        if plan.mutates && !options.yes {
            print!("{}", renderer.maintenance_plan(&plan));
            if !confirm_maintenance(&plan)? {
                return Err("operation cancelled; no package state was changed".into());
            }
            if options.action == MaintenanceAction::Upgrade {
                registry.revalidate_upgrade_plan(&plan)?;
            }
        } else if !json {
            print!("{}", renderer.maintenance_plan(&plan));
        }

        let executor = RealOperationExecutor::new(registry.runner());
        if plan.providers.iter().any(|provider| {
            provider.executable()
                && provider.privilege
                    == orbis_core::transaction::PrivilegeRequirement::Administrator
        }) {
            if let Err(error) = executor.authorize_administrator() {
                let message = format!(
                    "administrator authorization failed before provider mutations: {error}"
                );
                if json {
                    print_json(&serde_json::json!({
                        "status": "authorization_failed",
                        "plan": plan,
                        "message": message
                    }))?;
                }
                return Err(message);
            }
        }
        let history = if plan.mutates {
            let history = orbis_core::transaction::history::HistoryStore::default_location()
                .map_err(|error| format!("could not open history: {error}"))?;
            history
                .write_maintenance(
                    &orbis_core::transaction::history::MaintenanceRecord::execution_started(
                        plan.clone(),
                    ),
                    &plan.operation_id,
                )
                .map_err(|error| format!("could not start maintenance record: {error}"))?;
            Some(history)
        } else {
            None
        };
        let mut provider_results = Vec::new();
        let mut authorization_failed = false;
        for provider_plan in &plan.providers {
            if !provider_plan.executable() {
                provider_results.push(MaintenanceProviderResult {
                    source: provider_plan.source,
                    action: provider_plan.action,
                    status: MaintenanceProviderStatus::Skipped,
                    candidate_count: provider_plan
                        .candidates
                        .len()
                        .max(provider_plan.cleanup_candidates.len()),
                    verification: None,
                    message: provider_plan.warnings.iter().find_map(|warning| {
                        (warning.level == orbis_core::transaction::WarningLevel::Blocked)
                            .then(|| warning.message.clone())
                    }),
                });
                continue;
            }
            match registry.execute_maintenance(provider_plan, &executor) {
                Ok(result) => provider_results.extend(result.providers),
                Err(error) => {
                    provider_results.push(MaintenanceProviderResult {
                        source: provider_plan.source,
                        action: provider_plan.action,
                        status: MaintenanceProviderStatus::Failed,
                        candidate_count: provider_plan
                            .candidates
                            .len()
                            .max(provider_plan.cleanup_candidates.len()),
                        verification: None,
                        message: Some(error.clone()),
                    });
                    if provider_plan.privilege
                        == orbis_core::transaction::PrivilegeRequirement::Administrator
                        && error.to_ascii_lowercase().contains("authoriz")
                    {
                        authorization_failed = true;
                    }
                    if authorization_failed {
                        break;
                    }
                }
            }
        }
        let status = maintenance_status(&provider_results);
        let result = MaintenanceResult {
            operation_id: plan.operation_id.clone(),
            action: plan.action,
            status,
            providers: provider_results,
        };

        if let Some(history) = history {
            history
                .write_maintenance(
                    &orbis_core::transaction::history::MaintenanceRecord::completed(
                        plan.clone(),
                        result.clone(),
                    ),
                    &plan.operation_id,
                )
                .map_err(|error| format!("could not record maintenance: {error}"))?;
        }
        if json {
            print_json(&result)?;
        } else {
            print!("{}", renderer.maintenance_result(&result));
        }
        if matches!(result.status, MaintenanceStatus::Failed | MaintenanceStatus::Blocked) {
            Err(format!("{} run did not complete successfully", options.action.label()))
        } else {
            Ok(())
        }
    }
}

fn maintenance_status(results: &[MaintenanceProviderResult]) -> MaintenanceStatus {
    if results.is_empty() {
        return MaintenanceStatus::Blocked;
    }
    let succeeded = results
        .iter()
        .filter(|result| {
            matches!(
                result.status,
                MaintenanceProviderStatus::Succeeded
                    | MaintenanceProviderStatus::PartiallySucceeded
            )
        })
        .count();
    let failed = results.iter().any(|result| result.status == MaintenanceProviderStatus::Failed);
    let skipped = results.iter().any(|result| {
        matches!(
            result.status,
            MaintenanceProviderStatus::Skipped | MaintenanceProviderStatus::Blocked
        )
    });
    if failed && succeeded > 0 || skipped && succeeded > 0 {
        MaintenanceStatus::PartiallySucceeded
    } else if failed {
        MaintenanceStatus::Failed
    } else if succeeded == results.len() {
        if results
            .iter()
            .any(|result| result.status == MaintenanceProviderStatus::PartiallySucceeded)
        {
            MaintenanceStatus::PartiallySucceeded
        } else {
            MaintenanceStatus::Succeeded
        }
    } else {
        MaintenanceStatus::Blocked
    }
}

fn confirm_maintenance(plan: &MaintenancePlan) -> Result<bool, String> {
    if plan.risk >= orbis_core::transaction::RiskLevel::HighImpact {
        eprint!("This maintenance plan contains high-impact changes. Type YES to continue: ");
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(|error| error.to_string())?;
        Ok(answer.trim() == "YES")
    } else {
        eprint!("Continue with this maintenance plan? [Y/n] ");
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(|error| error.to_string())?;
        Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes"))
    }
}

fn run_history(
    renderer: &Renderer,
    json: bool,
    operation_id: Option<String>,
    limit: usize,
    source: Option<PackageSource>,
) -> Result<(), String> {
    let history = orbis_core::transaction::history::HistoryStore::default_location()
        .map_err(|error| format!("could not open history: {error}"))?;
    if let Some(operation_id) = operation_id {
        let entry = history
            .entry(&operation_id)?
            .ok_or_else(|| format!("history operation {operation_id} was not found"))?;
        if json {
            print_json(&entry)
        } else {
            print!("{}", renderer.history_entry(&entry));
            Ok(())
        }
    } else {
        let entries = history
            .entries()?
            .into_iter()
            .filter(|entry| source.is_none_or(|wanted| entry.source == Some(wanted)))
            .take(limit)
            .collect::<Vec<_>>();
        if json {
            print_json(&entries)
        } else {
            print!("{}", renderer.history(&entries, limit));
            Ok(())
        }
    }
}

fn run_why(
    registry: &ProviderRegistry,
    renderer: &Renderer,
    json: bool,
    package: String,
    source: Option<PackageSource>,
) -> Result<(), String> {
    let package_ref = parse_transaction_ref(&package, source)?;
    let report = registry.resolve(&package_ref);
    let ResolveReport::Found { package, .. } = report else {
        let message = "package reference was not resolved uniquely";
        if json {
            print_json(
                &serde_json::json!({ "status": "not_found_or_ambiguous", "message": message }),
            )?;
        }
        return Err(message.into());
    };
    let mut why = registry.why(&package)?;
    if let Ok(history) = orbis_core::transaction::history::HistoryStore::default_location() {
        if let Ok(entries) = history.entries() {
            why.orbis_history = entries
                .into_iter()
                .filter(|entry| {
                    entry.source == Some(package.source)
                        && entry.package.as_deref().is_some_and(|name| {
                            name == package.provider_id || name.eq_ignore_ascii_case(&package.name)
                        })
                        && entry.action.eq_ignore_ascii_case("install")
                })
                .map(|entry| orbis_core::maintenance::WhyHistoryEntry {
                    operation_id: entry.operation_id,
                    action: entry.action,
                    recorded_at_unix_ms: entry.recorded_at_unix_ms,
                })
                .collect();
        }
    }
    if why.orbis_history.is_empty() {
        why.notes.push("Orbis has no installation record for this package; that does not establish who installed it.".into());
    }
    if json {
        print_json(&why)
    } else {
        print!("{}", renderer.why(&why));
        Ok(())
    }
}

fn parse_transaction_ref(
    input: &str,
    source: Option<PackageSource>,
) -> Result<orbis_core::models::PackageRef, String> {
    if let Some((prefix, _)) = input.split_once(':') {
        if let Some(qualified) = PackageSource::parse(prefix) {
            if source.is_some_and(|explicit| explicit != qualified) {
                return Err(format!(
                    "package reference selects {qualified}, which conflicts with --source"
                ));
            }
        }
    }
    let package_ref = parse_package_ref(input, source);
    if package_ref.query.is_empty() {
        return Err("package reference cannot be empty".into());
    }
    Ok(package_ref)
}

fn confirm(plan: &OperationPlan) -> Result<bool, String> {
    if plan.risk >= orbis_core::transaction::RiskLevel::HighImpact {
        eprint!("This is a high-impact removal. Type YES to continue: ");
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(|error| error.to_string())?;
        Ok(answer.trim() == "YES")
    } else {
        eprint!("Continue with this operation? [Y/n] ");
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(|error| error.to_string())?;
        Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes"))
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    let output = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    println!("{output}");
    Ok(())
}

struct Renderer {
    color: bool,
    unicode: bool,
    width: usize,
}

impl Renderer {
    fn new(color: bool) -> Self {
        let unicode = std::env::var("TERM").map(|term| term != "dumb").unwrap_or(true);
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(88)
            .clamp(40, 120);
        Self { color, unicode, width }
    }

    fn home(&self, sources: &[SourceInfo]) -> String {
        let mut output = String::new();
        output.push_str(&self.heading("Orbis", "Your Linux software, in one place."));
        output.push_str("\nSources\n");
        for source in sources {
            let tone = source_state_tone(source);
            let marker = self.paint(
                if source.state == "ready" {
                    if self.unicode { "●" } else { "*" }
                } else if self.unicode {
                    "○"
                } else {
                    "o"
                },
                tone,
            );
            output.push_str(&format!(
                "  {marker} {:<9} {}\n",
                source.source.label(),
                self.paint(&source.state, tone)
            ));
        }
        output.push_str(
            "\nTry\n  orbis search <package>\n  orbis updates\n  orbis upgrade --plan\n  orbis explain <package>\n  orbis doctor\n",
        );
        output
    }

    fn sources(&self, sources: &[SourceInfo]) -> String {
        let mut output = self.heading("Sources", "Read-only package discovery available to Orbis.");
        let mut last_group = None;
        for source in sources {
            let group = matches!(
                source.source,
                PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap
            );
            if last_group != Some(group) {
                output.push_str(if group { "\nSystem & desktop\n" } else { "\nDeveloper tools\n" });
                last_group = Some(group);
            }
            output.push_str(&format!(
                "\n{}  {}\n",
                self.paint(source.source.label(), Tone::Title),
                self.paint(&source.state, source_state_tone(source))
            ));
            if let Some(backend) = &source.backend {
                output.push_str(&format!("  Backend       {backend}\n"));
            }
            output.push_str("  Read          ");
            let mut capabilities = Vec::new();
            if source.capabilities.search {
                capabilities.push("search");
            }
            if source.capabilities.info {
                capabilities.push("info");
            }
            if source.capabilities.installed_state {
                capabilities.push("installed-state");
            }
            output.push_str(&capabilities.join(", "));
            output.push('\n');
            output.push_str("  Maintenance   ");
            let mut maintenance = Vec::new();
            if source.capabilities.updates {
                maintenance.push("updates");
            }
            if source.capabilities.upgrade {
                maintenance.push("upgrade");
            }
            if source.capabilities.refresh {
                maintenance.push("refresh");
            }
            if source.capabilities.cleanup {
                maintenance.push("clean");
            }
            output.push_str(&maintenance.join(", "));
            output.push('\n');
            output.push_str("  Mutations     ");
            let mut mutations = Vec::new();
            if source.capabilities.install {
                mutations.push("install");
            }
            if source.capabilities.remove {
                mutations.push("remove");
            }
            output.push_str(&mutations.join(", "));
            output.push('\n');
            output.push_str(&format!(
                "  Why           {}\n",
                if source.capabilities.why { "supported" } else { "unsupported" }
            ));
            for note in &source.notes {
                output.push_str(&format!("  Note          {note}\n"));
            }
        }
        output.push_str(
            "\n  Plans are read-only; execution always requires confirmation or --yes.\n",
        );
        output
    }

    fn search(&self, query: &str, report: &SearchReport) -> String {
        let mut output = self.heading("Search", &format!("Results for {query}"));
        if report.results.is_empty() {
            output.push_str("\nNo matching packages were returned by the available sources.\n");
        }
        for package in &report.results {
            output.push_str(&format!("\n{}\n", self.paint(&package.name, Tone::Title)));
            if let Some(summary) = &package.summary {
                output.push_str(&format!("  {summary}\n"));
            }
            output.push_str(&format!("  {} · {}\n", package.source, status(package.installed)));
            if let Some(version) = &package.version {
                output.push_str(&format!("  Version       {version}\n"));
            }
        }
        self.render_issues(output, &report.issues)
    }

    fn info(&self, package: &Package, issues: &[orbis_core::models::ProviderIssue]) -> String {
        let mut output = self.heading("Package", &package.name);
        if let Some(summary) = &package.summary {
            output.push_str(&format!("\n  {summary}\n"));
        }
        output.push_str("\n  Details\n");
        self.field(&mut output, "Source", &package.source.to_string());
        self.field(&mut output, "Provider ID", &package.provider_id);
        self.field(&mut output, "Installed", installed_label(package.installed));
        if let Some(version) = &package.version {
            self.field(&mut output, "Version", version);
        }
        if let Some(kind) = package.kind {
            self.field(&mut output, "Type", kind.label());
        }
        if let Some(origin) = &package.origin {
            self.field(&mut output, "Origin", origin);
        }
        if let Some(architecture) = &package.architecture {
            self.field(&mut output, "Architecture", architecture);
        }
        if let Some(size) = package.size_bytes {
            self.field(&mut output, "Size", &human_size(size));
        }
        if let Some(homepage) = &package.homepage {
            self.field(&mut output, "Homepage", homepage);
        }
        if let Some(license) = &package.license {
            self.field(&mut output, "License", license);
        }
        if let Some(description) = &package.description {
            output.push_str("\n  Provider description\n");
            output.push_str(&indent(&wrap(description, self.width.saturating_sub(4)), "  "));
            output.push('\n');
        }
        self.render_issues(output, issues)
    }

    fn brief(&self, brief: &orbis_core::explain::PackageBrief) -> String {
        let mut output = self.heading("Orbis Brief", &brief.package.name);
        output.push_str(&format!("\n{}\n", self.paint(&brief.headline, Tone::Title)));
        for paragraph in &brief.paragraphs {
            output.push('\n');
            output.push_str(&wrap(paragraph, self.width));
            output.push('\n');
        }
        if let Some(kind) = brief.package.kind {
            self.field(&mut output, "Type", kind.label());
        }
        if let Some(direct) = brief.normally_run_directly {
            self.field(
                &mut output,
                "Normally run",
                if direct { "Yes" } else { "No — it supports other software" },
            );
        }
        if !brief.examples.is_empty() {
            output.push_str("\n  Examples\n");
            for example in &brief.examples {
                output.push_str(&format!("  - {example}\n"));
            }
        }
        if let Some(caution) = &brief.caution {
            output.push_str(&format!("\n{}\n", self.paint("Context", Tone::Warning)));
            output.push_str(&format!("{}\n", wrap(caution, self.width)));
        }
        output.push_str(&format!("\n  Confidence     {}\n", brief.confidence));
        output.push_str("  Evidence       ");
        output.push_str(
            &brief
                .evidence
                .iter()
                .map(|e| match e.kind {
                    orbis_core::explain::EvidenceKind::ProviderMetadata => "provider metadata",
                    orbis_core::explain::EvidenceKind::OrbisInterpretation => {
                        "Orbis interpretation"
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        output.push('\n');
        output
    }

    fn transaction_plan(&self, plan: &OperationPlan) -> String {
        let mut output = self.heading(
            "Transaction plan",
            &format!("{} {} through {}", plan.action.label(), plan.target.name, plan.target.source),
        );
        self.field(&mut output, "Target", &plan.target.provider_id);
        self.field(&mut output, "Source", &plan.target.source.to_string());
        self.field(&mut output, "Scope", plan.scope.label());
        self.field(&mut output, "Installed", installed_label(plan.target.installed));
        self.field(&mut output, "Risk", plan.risk.label());
        self.field(
            &mut output,
            "Plan quality",
            match plan.completeness {
                orbis_core::transaction::PlanCompleteness::Complete => "complete",
                orbis_core::transaction::PlanCompleteness::Partial => "partial",
                orbis_core::transaction::PlanCompleteness::Unknown => "unknown",
            },
        );
        self.field(
            &mut output,
            "Confidence",
            match plan.confidence {
                orbis_core::transaction::PlanConfidence::High => "high",
                orbis_core::transaction::PlanConfidence::Medium => "medium",
                orbis_core::transaction::PlanConfidence::Low => "low",
            },
        );
        self.field(
            &mut output,
            "Privilege",
            match plan.privilege {
                orbis_core::transaction::PrivilegeRequirement::None => "user scope",
                orbis_core::transaction::PrivilegeRequirement::Administrator => "administrator",
            },
        );
        if let Some(size) = plan.download_size_bytes {
            self.field(&mut output, "Download", &human_size(size));
        }
        if let Some(delta) = plan.disk_delta_bytes {
            self.field(&mut output, "Disk change", &format_disk_delta(delta));
        }
        output.push_str("\n  Changes\n");
        for change in &plan.changes {
            let marker = match change.kind {
                orbis_core::transaction::ChangeKind::Install => "+",
                orbis_core::transaction::ChangeKind::Remove => "-",
                orbis_core::transaction::ChangeKind::Configure => "~",
            };
            let version =
                change.version.as_deref().map(|version| format!("  {version}")).unwrap_or_default();
            let reason =
                change.reason.as_deref().map(|reason| format!("  ({reason})")).unwrap_or_default();
            output.push_str(&format!("  {marker} {}{version}{reason}\n", change.package_id));
        }
        if !plan.warnings.is_empty() {
            output.push_str("\n  Notes\n");
            for warning in &plan.warnings {
                let marker = match warning.level {
                    orbis_core::transaction::WarningLevel::Info => "·",
                    orbis_core::transaction::WarningLevel::Caution => "!",
                    orbis_core::transaction::WarningLevel::Blocked => "×",
                };
                output.push_str(&format!("  {marker} {}\n", warning.message));
            }
        }
        output.push_str(&format!("\n  Plan ID       {}\n", plan.operation_id));
        output.push_str("  No package state has been changed by planning.\n");
        output
    }

    fn transaction_result(&self, result: &TransactionResult) -> String {
        let title = match result.status {
            orbis_core::transaction::TransactionStatus::Succeeded => "Completed",
            orbis_core::transaction::TransactionStatus::PartiallyVerified => {
                "Completed with verification limits"
            }
            orbis_core::transaction::TransactionStatus::Failed => "Failed",
        };
        let tone = match result.status {
            orbis_core::transaction::TransactionStatus::Succeeded => Tone::Good,
            orbis_core::transaction::TransactionStatus::PartiallyVerified => Tone::Warning,
            orbis_core::transaction::TransactionStatus::Failed => Tone::Warning,
        };
        let mut output = self.heading(
            title,
            &format!("{} {}", result.plan.action.label(), result.plan.target.provider_id),
        );
        output.push_str(&format!(
            "\n  Process         {}\n",
            if result.execution.process_succeeded {
                self.paint("succeeded", tone)
            } else {
                self.paint("failed", tone)
            }
        ));
        output.push_str(&format!(
            "  Verification    {}\n",
            match result.verification {
                orbis_core::transaction::VerificationResult::Verified =>
                    self.paint("verified", Tone::Good),
                orbis_core::transaction::VerificationResult::PartiallyVerified => {
                    self.paint("partially verified", Tone::Warning)
                }
                orbis_core::transaction::VerificationResult::Failed =>
                    self.paint("failed", Tone::Warning),
            }
        ));
        if let Some(message) = &result.execution.message {
            output.push_str(&format!("  Detail          {message}\n"));
        }
        output.push_str(&format!("  Transaction ID  {}\n", result.plan.operation_id));
        output
    }

    fn updates(&self, report: &UpdateInventoryReport) -> String {
        let mut output = self.heading("Updates", "Installed software with updates available.");
        if report.candidates.is_empty() {
            output.push_str("\n  No pending updates were reported by the available sources.\n");
        } else {
            output.push_str(&format!(
                "\n  {} update{} available\n",
                report.total(),
                if report.total() == 1 { "" } else { "s" }
            ));
            for source in [
                PackageSource::Apt,
                PackageSource::Flatpak,
                PackageSource::Snap,
                PackageSource::Cargo,
                PackageSource::Npm,
                PackageSource::Pnpm,
                PackageSource::Uv,
                PackageSource::Pipx,
            ] {
                let candidates: Vec<_> = report
                    .candidates
                    .iter()
                    .filter(|candidate| candidate.source == source)
                    .collect();
                if candidates.is_empty() {
                    continue;
                }
                output.push_str(&format!(
                    "\n{}  {}\n",
                    self.paint(source.label(), Tone::Title),
                    candidates.len()
                ));
                for candidate in candidates {
                    let current = candidate.current_version.as_deref().unwrap_or("current");
                    let available = candidate.available_version.as_deref().unwrap_or("latest");
                    let scope = candidate
                        .scope
                        .map(|scope| format!(" · {}", scope.label()))
                        .unwrap_or_default();
                    let held = candidate.held == Some(true);
                    output.push_str(&format!(
                        "  {:<28} {current} → {available}{scope}{}\n",
                        candidate.name,
                        if held { " · held" } else { "" }
                    ));
                }
            }
        }
        for inventory in &report.inventories {
            if inventory
                .metadata_state
                .as_deref()
                .is_some_and(|state| state.contains("incomplete") || state.contains("unknown"))
            {
                output.push_str(&format!(
                    "\n  {} update status is incomplete; unknown is not counted as zero.\n",
                    inventory.source
                ));
            }
        }
        for inventory in &report.inventories {
            for note in &inventory.notes {
                if inventory.candidates.is_empty() && !note.is_empty() {
                    output.push_str(&format!("\n  {}: {note}\n", inventory.source));
                }
            }
        }
        if !report.issues.is_empty() {
            output.push_str("\n  Source notes\n");
            for issue in &report.issues {
                output.push_str(&format!("  - {}: {}\n", issue.source, issue.message));
            }
        }
        output.push_str("\n  Read-only inventory; package state and indexes were not changed.\n");
        output.push_str("  Run orbis upgrade --plan to review the coordinated upgrade.\n");
        output
    }

    fn maintenance_plan(&self, plan: &MaintenancePlan) -> String {
        let mut output = self.heading(
            &format!("{} plan", plan.action.label()),
            "Provider plans are coordinated for review, not atomic.",
        );
        let total_updates: usize =
            plan.providers.iter().map(|provider| provider.candidates.len()).sum();
        let total_cleanup: usize =
            plan.providers.iter().map(|provider| provider.cleanup_candidates.len()).sum();
        if plan.action == MaintenanceAction::Upgrade {
            output.push_str(&format!(
                "\n  {total_updates} update{} across {} provider plan{}\n",
                if total_updates == 1 { "" } else { "s" },
                plan.providers.len(),
                if plan.providers.len() == 1 { "" } else { "s" }
            ));
        } else if plan.action == MaintenanceAction::Cleanup {
            output.push_str(&format!(
                "\n  {total_cleanup} cleanup candidate{} across {} provider plan{}\n",
                if total_cleanup == 1 { "" } else { "s" },
                plan.providers.len(),
                if plan.providers.len() == 1 { "" } else { "s" }
            ));
        }
        for provider in &plan.providers {
            output.push_str(&format!(
                "\n{}{}\n",
                self.paint(provider.source.label(), Tone::Title),
                provider.scope.map(|scope| format!(" · {}", scope.label())).unwrap_or_default()
            ));
            if provider.action == MaintenanceAction::Upgrade {
                for candidate in &provider.candidates {
                    output.push_str(&format!(
                        "  {:<28} {} → {}\n",
                        candidate.name,
                        candidate.current_version.as_deref().unwrap_or("current"),
                        candidate.available_version.as_deref().unwrap_or("latest")
                    ));
                }
            }
            for candidate in &provider.cleanup_candidates {
                output.push_str(&format!(
                    "  - {:<28} {} · {}\n",
                    candidate.name,
                    candidate.reason,
                    candidate.risk.label()
                ));
            }
            if provider.candidates.is_empty()
                && provider.cleanup_candidates.is_empty()
                && provider.action != MaintenanceAction::Refresh
            {
                output.push_str("  No changes reported.\n");
            }
            for note in &provider.notes {
                output.push_str(&format!("  Note  {note}\n"));
            }
            for warning in &provider.warnings {
                let marker = match warning.level {
                    orbis_core::transaction::WarningLevel::Info => "·",
                    orbis_core::transaction::WarningLevel::Caution => "!",
                    orbis_core::transaction::WarningLevel::Blocked => "×",
                };
                output.push_str(&format!("  {marker} {}\n", warning.message));
            }
            self.field(
                &mut output,
                "  Privilege",
                match provider.privilege {
                    orbis_core::transaction::PrivilegeRequirement::None => "user scope / none",
                    orbis_core::transaction::PrivilegeRequirement::Administrator => "administrator",
                },
            );
        }
        if !plan.warnings.is_empty() {
            output.push_str("\n  Notes\n");
            for warning in &plan.warnings {
                output.push_str(&format!("  · {}\n", warning.message));
            }
        }
        self.field(&mut output, "\n  Risk", plan.risk.label());
        self.field(&mut output, "  Plan ID", &plan.operation_id);
        output.push_str("  No package state has been changed by planning.\n");
        output
    }

    fn maintenance_result(&self, result: &MaintenanceResult) -> String {
        let title = match result.status {
            MaintenanceStatus::Succeeded => "Completed",
            MaintenanceStatus::PartiallySucceeded => "Partially completed",
            MaintenanceStatus::Failed => "Failed",
            MaintenanceStatus::Cancelled => "Cancelled",
            MaintenanceStatus::Blocked => "Blocked",
        };
        let mut output =
            self.heading(title, &format!("{} maintenance across providers", result.action.label()));
        for provider in &result.providers {
            let status = match provider.status {
                MaintenanceProviderStatus::Succeeded => "succeeded",
                MaintenanceProviderStatus::PartiallySucceeded => "partially succeeded",
                MaintenanceProviderStatus::Failed => "failed",
                MaintenanceProviderStatus::Skipped => "skipped",
                MaintenanceProviderStatus::Blocked => "blocked",
            };
            output.push_str(&format!(
                "  {:<10} {:<20} {} candidate(s)\n",
                provider.source.label(),
                status,
                provider.candidate_count
            ));
            if let Some(message) = &provider.message {
                output.push_str(&format!("    {message}\n"));
            }
        }
        output.push_str(&format!("\n  Maintenance ID  {}\n", result.operation_id));
        output
    }

    fn history(
        &self,
        entries: &[orbis_core::transaction::history::HistoryEntry],
        limit: usize,
    ) -> String {
        let mut output =
            self.heading("History", &format!("Recent Orbis operations (limit {limit})."));
        if entries.is_empty() {
            output.push_str("\n  No recorded operations.\n");
            return output;
        }
        for entry in entries {
            output.push_str(&format!(
                "\n  {}  {:<12} {:<10} {}{}\n",
                entry.operation_id,
                entry.action,
                entry.status,
                entry.source.map(|source| source.label()).unwrap_or("Orbis"),
                entry.package.as_deref().map(|package| format!(" · {package}")).unwrap_or_default()
            ));
            if let Some(message) = &entry.message {
                output.push_str(&format!("    {message}\n"));
            }
        }
        output
    }

    fn history_entry(&self, entry: &serde_json::Value) -> String {
        let body = serde_json::to_string_pretty(entry).unwrap_or_else(|_| "{}".into());
        self.heading("History record", "Sanitized persisted operation record.") + &body + "\n"
    }

    fn why(&self, report: &WhyReport) -> String {
        let mut output = self.heading("Why", &report.package.name);
        output.push_str(&format!("\n  {}\n", report.installed_as));
        self.field(&mut output, "Source", &report.package.source.to_string());
        self.field(&mut output, "Provider ID", &report.package.provider_id);
        if !report.used_by.is_empty() {
            output.push_str("\n  Used by\n");
            for consumer in &report.used_by {
                output.push_str(&format!(
                    "  - {} · {} ({})\n",
                    consumer.name, consumer.provider_id, consumer.relationship
                ));
            }
        }
        if !report.orbis_history.is_empty() {
            output.push_str("\n  Installed through Orbis\n");
            for record in &report.orbis_history {
                output.push_str(&format!("  - {} · {}\n", record.action, record.operation_id));
            }
        }
        output.push_str(&format!(
            "\n  Removal advice\n  {}\n",
            wrap(&report.removal_advice, self.width.saturating_sub(2))
        ));
        if !report.evidence.is_empty() {
            output.push_str("\n  Evidence\n");
            for evidence in &report.evidence {
                output.push_str(&format!("  - {evidence}\n"));
            }
        }
        for note in &report.notes {
            output.push_str(&format!("  Note  {note}\n"));
        }
        output
    }

    fn doctor(&self, report: &DoctorReport) -> String {
        let mut output = self.heading("Doctor", "Safe checks only; no package state was changed.");
        for check in &report.checks {
            let marker = if check.passed {
                self.paint(if self.unicode { "✓" } else { "OK" }, Tone::Good)
            } else {
                self.paint("!", Tone::Warning)
            };
            output.push_str(&format!(
                "\n  {marker} {:<12} {}\n  {}\n",
                check.area,
                check.title,
                wrap(&check.message, self.width.saturating_sub(4))
            ));
        }
        output
    }

    fn ambiguous(
        &self,
        query: &str,
        matches: &[Package],
        issues: &[orbis_core::models::ProviderIssue],
    ) -> String {
        let mut output =
            self.heading("Choose a source", &format!("{query} matches more than one provider."));
        output.push_str("\nUse one of these explicit references:\n");
        for package in matches {
            output.push_str(&format!(
                "  orbis info {}:{}  ({})\n",
                package.source.to_string().to_ascii_lowercase(),
                package.provider_id,
                status(package.installed)
            ));
        }
        self.render_issues(output, issues)
    }

    fn not_found(&self, query: &str, issues: &[orbis_core::models::ProviderIssue]) -> String {
        let mut output = self.heading("No package found", query);
        output.push_str(
            "\nTry a broader search or qualify the source, for example apt:curl or snap:firefox.\n",
        );
        self.render_issues(output, issues)
    }

    fn heading(&self, title: &str, subtitle: &str) -> String {
        format!("{}\n{}\n\n", self.paint(title, Tone::Title), self.paint(subtitle, Tone::Muted))
    }

    fn field(&self, output: &mut String, label: &str, value: &str) {
        output.push_str(&format!("  {label:<14}{value}\n"));
    }

    fn render_issues(
        &self,
        mut output: String,
        issues: &[orbis_core::models::ProviderIssue],
    ) -> String {
        if !issues.is_empty() {
            output.push_str("\n  Source notes\n");
            for issue in issues {
                output.push_str(&format!("  - {}: {}\n", issue.source, issue.message));
            }
        }
        output
    }

    fn paint(&self, value: &str, tone: Tone) -> String {
        if !self.color {
            return value.into();
        }
        let code = match tone {
            Tone::Title => 36,
            Tone::Good => 32,
            Tone::Warning => 33,
            Tone::Muted => 90,
        };
        format!("\x1b[{code}m{value}\x1b[0m")
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Title,
    Good,
    Warning,
    Muted,
}

fn source_state_tone(source: &SourceInfo) -> Tone {
    match source.state.as_str() {
        "ready" => Tone::Good,
        "unavailable" => Tone::Muted,
        _ => Tone::Warning,
    }
}

fn status(installed: Option<bool>) -> &'static str {
    match installed {
        Some(true) => "installed",
        Some(false) => "not installed",
        None => "installed state unknown",
    }
}

fn installed_label(installed: Option<bool>) -> &'static str {
    match installed {
        Some(true) => "Yes",
        Some(false) => "No",
        None => "Unknown",
    }
}

fn human_size(size: u64) -> String {
    if size >= 1_000_000_000 {
        format!("{:.1} GB", size as f64 / 1_000_000_000.0)
    } else if size >= 1_000_000 {
        format!("{:.1} MB", size as f64 / 1_000_000.0)
    } else if size >= 1_000 {
        format!("{:.1} kB", size as f64 / 1_000.0)
    } else {
        format!("{size} B")
    }
}

fn format_disk_delta(delta: i64) -> String {
    let magnitude = delta.unsigned_abs();
    let prefix = if delta < 0 { "-" } else { "+" };
    format!("{prefix}{}", human_size(magnitude))
}

fn wrap(text: &str, width: usize) -> String {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.len() + word.len() + usize::from(!line.is_empty()) > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines().map(|line| format!("{prefix}{line}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_accepts_provider_qualified_commands() {
        let cli = Cli::try_parse_from(["orbis", "info", "apt:libssl-dev"]).expect("valid args");
        assert!(
            matches!(cli.command, Some(Command::Info { package, source: None }) if package == "apt:libssl-dev")
        );
    }

    #[test]
    fn no_color_renderer_contains_no_escape_sequences() {
        let renderer = Renderer::new(false);
        let output = renderer.home(&[]);
        assert!(!output.contains('\x1b'));
    }

    #[test]
    fn cli_json_flag_is_global() {
        let cli = Cli::try_parse_from(["orbis", "search", "btop", "--json"]).expect("valid args");
        assert!(cli.json);
    }

    #[test]
    fn mutation_arguments_keep_plan_and_scope_explicit() {
        let cli = Cli::try_parse_from([
            "orbis",
            "install",
            "flatpak:org.example.App",
            "--scope",
            "user",
            "--plan",
            "--yes",
        ])
        .expect("valid mutation args");
        assert!(matches!(
            cli.command,
            Some(Command::Install { plan: true, yes: true, scope: Some(ScopeArg::User), .. })
        ));
    }

    #[test]
    fn maintenance_commands_keep_read_only_and_mutating_paths_distinct() {
        let updates =
            Cli::try_parse_from(["orbis", "updates", "--source", "apt"]).expect("updates args");
        assert!(matches!(updates.command, Some(Command::Updates { source: Some(SourceArg::Apt) })));
        let upgrade =
            Cli::try_parse_from(["orbis", "upgrade", "--plan", "--yes"]).expect("upgrade args");
        assert!(matches!(upgrade.command, Some(Command::Upgrade { plan: true, yes: true, .. })));
    }

    #[test]
    fn maintenance_renderer_is_plain_when_color_is_disabled() {
        let renderer = Renderer::new(false);
        let plan =
            MaintenancePlan::new(MaintenanceAction::Upgrade, Some(PackageSource::Apt), Vec::new());
        assert!(!renderer.maintenance_plan(&plan).contains('\x1b'));
    }
}
