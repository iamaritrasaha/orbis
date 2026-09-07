use std::io::{self, IsTerminal};

use orbis_core::{
    ProviderRegistry, ResolveReport,
    maintenance::{
        MaintenanceAction, MaintenancePlan, MaintenanceProviderResult, MaintenanceProviderStatus,
        MaintenanceResult, MaintenanceStatus,
    },
    models::{PackageRef, PackageSource},
    privilege::RealOperationExecutor,
    transaction::{
        InstallScope, OperationAction, OperationPlan, OperationRequest, PackageRefJson,
        TransactionResult,
    },
};

use crate::{
    cli::{Cli, Command},
    render::Renderer,
};

pub(crate) fn dispatch(
    cli: Cli,
    registry: &ProviderRegistry,
    renderer: &Renderer,
) -> Result<(), String> {
    match cli.command {
        Some(Command::Dashboard) if cli.json || cli.plain => {
            let sources = registry.sources();
            if cli.json {
                print_json(
                    &serde_json::json!({"name":"Orbis","version":env!("CARGO_PKG_VERSION"),"tagline":"Your Linux software, in one place.","sources":sources,"planning_read_only":true,"supported_mutations":["install","remove","update","upgrade","clean"],"read_only_maintenance":["updates","history","why"]}),
                )
            } else {
                print!("{}", renderer.home(&sources));
                Ok(())
            }
        }
        Some(Command::Dashboard) => crate::tui::run(registry, renderer.theme, false),
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
            run_info(registry, renderer, cli.json, package, source.map(Into::into))
        }
        Some(Command::Explain { package, source }) => {
            run_explain(registry, renderer, cli.json, package, source.map(Into::into))
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
            registry,
            renderer,
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
            registry,
            renderer,
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
            registry,
            renderer,
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
            registry,
            renderer,
            cli.json,
            MaintenanceOptions {
                action: MaintenanceAction::Upgrade,
                source: source.map(Into::into),
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::Clean { source, plan, yes }) => run_maintenance(
            registry,
            renderer,
            cli.json,
            MaintenanceOptions {
                action: MaintenanceAction::Cleanup,
                source: source.map(Into::into),
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::History { operation_id, limit, source }) => {
            run_history(renderer, cli.json, operation_id, limit, source.map(Into::into))
        }
        Some(Command::Why { package, source }) => {
            run_why(registry, renderer, cli.json, package, source.map(Into::into))
        }
        None => {
            let sources = registry.sources();
            if cli.json {
                print_json(
                    &serde_json::json!({"name":"Orbis","version":env!("CARGO_PKG_VERSION"),"tagline":"Your Linux software, in one place.","sources":sources,"planning_read_only":true,"supported_mutations":["install","remove","update","upgrade","clean"],"read_only_maintenance":["updates","history","why"]}),
                )
            } else {
                print!("{}", renderer.home(&sources));
                Ok(())
            }
        }
    }
}

/// Executes a previously generated, already-confirmed plan with live progress observation.
pub(crate) fn execute_confirmed_transaction_with_observer(
    registry: &ProviderRegistry,
    request: &OperationRequest,
    plan: OperationPlan,
    observer: &dyn orbis_core::progress::ProgressObserver,
) -> Result<TransactionResult, String> {
    if !plan.executable() {
        return Err("this plan is blocked or incomplete; no package state was changed".into());
    }
    let history = orbis_core::transaction::history::HistoryStore::default_location()
        .map_err(|error| format!("could not start transaction: {error}"))?;
    let executor = RealOperationExecutor::new(registry.runner());
    registry
        .execute_transaction_with_progress(request, plan, &executor, &history, observer)
        .map_err(|error| error.to_string())
}

/// Executes a previously generated, already-confirmed plan.
///
/// This is intentionally the only mutation entry point exposed to the TUI.
/// The TUI must generate and render an executable plan before calling it.
#[allow(dead_code)]
pub(crate) fn execute_confirmed_transaction(
    registry: &ProviderRegistry,
    request: &OperationRequest,
    plan: OperationPlan,
) -> Result<TransactionResult, String> {
    execute_confirmed_transaction_with_observer(
        registry,
        request,
        plan,
        &orbis_core::progress::SilentObserver,
    )
}

fn run_info(
    registry: &ProviderRegistry,
    renderer: &Renderer,
    json: bool,
    package: String,
    source: Option<PackageSource>,
) -> Result<(), String> {
    let reference = parse_reference(&package, source)?;
    let report = registry.resolve(&reference);
    if json {
        return print_json(&report);
    }
    match &report {
        ResolveReport::Found { package, issues } => print!("{}", renderer.info(package, issues)),
        ResolveReport::Ambiguous { matches, issues } => {
            print!("{}", renderer.ambiguous(&reference.query, matches, issues))
        }
        ResolveReport::NotFound { issues } => {
            print!("{}", renderer.not_found(&reference.query, issues))
        }
    }
    if matches!(report, ResolveReport::NotFound { .. }) {
        Err("package was not found".into())
    } else {
        Ok(())
    }
}

fn run_explain(
    registry: &ProviderRegistry,
    renderer: &Renderer,
    json: bool,
    package: String,
    source: Option<PackageSource>,
) -> Result<(), String> {
    let reference = parse_reference(&package, source)?;
    match registry.resolve(&reference) {
        ResolveReport::Found { package, .. } => {
            let brief = orbis_core::explain::build_brief(*package);
            if json {
                print_json(&brief)
            } else {
                print!("{}", renderer.brief(&brief));
                Ok(())
            }
        }
        ResolveReport::Ambiguous { matches, issues } => {
            if json {
                print_json(
                    &serde_json::json!({"status":"ambiguous","matches":matches,"issues":issues}),
                )?;
            } else {
                print!("{}", renderer.ambiguous(&reference.query, &matches, &issues));
            }
            Err("package reference is ambiguous".into())
        }
        ResolveReport::NotFound { issues } => {
            if json {
                print_json(&serde_json::json!({"status":"not_found","issues":issues}))?;
            } else {
                print!("{}", renderer.not_found(&reference.query, &issues));
            }
            Err("package was not found".into())
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
    let package_ref = parse_reference(&options.package, options.source)?;
    let request = OperationRequest {
        action: options.action,
        package: PackageRefJson::from(&package_ref),
        scope: options.scope,
        channel: options.channel,
    };
    let plan = match registry.plan_transaction(&request) {
        Ok(plan) => plan,
        Err(error) => return report_error(json, error.to_string()),
    };
    if options.plan_only {
        if json {
            print_json(&plan)
        } else {
            print!("{}", renderer.transaction_plan(&plan));
            Ok(())
        }
    } else if !plan.executable() {
        report_transaction_error(
            json,
            "this plan is incomplete or contains a blocked safety condition; nothing was executed",
            &plan,
        )
    } else {
        if !options.yes {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return report_transaction_error(
                    json,
                    "confirmation is required: use an interactive terminal or pass --yes after reviewing the plan",
                    &plan,
                );
            }
            print!("{}", renderer.transaction_plan(&plan));
            if !confirm(&plan)? {
                return Err("operation cancelled; no package state was changed".into());
            }
        } else if !json {
            print!("{}", renderer.transaction_plan(&plan));
        }
        let history = orbis_core::transaction::history::HistoryStore::default_location()
            .map_err(|e| format!("could not start transaction: {e}"))?;
        let executor = RealOperationExecutor::new(registry.runner());
        let result = if json {
            registry
                .execute_transaction_with_history(&request, plan, &executor, &history)
                .map_err(|e| e.to_string())?
        } else {
            let header = orbis_core::progress::OperationHeader {
                title: format!("{} {}", plan.action.label(), plan.target.name),
                target: plan.target.name.clone(),
                source: plan.target.source,
                scope: plan.scope.label().to_string(),
                privileged: plan.privilege
                    == orbis_core::transaction::PrivilegeRequirement::Administrator,
            };
            let progress = crate::render::progress::PlainProgressRenderer::new(
                renderer.theme,
                header,
                orbis_core::progress::ExecutionStage::transaction_stages(),
                io::stderr().is_terminal(),
                8,
            );
            progress.print_header();
            registry
                .execute_transaction_with_progress(&request, plan, &executor, &history, &progress)
                .map_err(|e| e.to_string())?
        };
        if json {
            print_json(&result)
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
    let plan = registry.maintenance_plan(options.action, options.source)?;
    if options.plan_only {
        if json {
            return print_json(&plan);
        }
        print!("{}", renderer.maintenance_plan(&plan));
        return Ok(());
    }
    if !plan.executable() {
        return report_error(json, "no executable provider plan is available; unsupported or blocked providers were not changed".into());
    }
    if options.action == MaintenanceAction::Upgrade {
        registry.revalidate_upgrade_plan(&plan)?;
    }
    if plan.mutates && !options.yes {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return report_error(json, "confirmation is required: use an interactive terminal or pass --yes after reviewing the plan".into());
        }
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
            && provider.privilege == orbis_core::transaction::PrivilegeRequirement::Administrator
    }) {
        executor.authorize_administrator().map_err(|e| {
            format!("administrator authorization failed before provider mutations: {e}")
        })?;
    }
    let history = if plan.mutates {
        let history = orbis_core::transaction::history::HistoryStore::default_location()
            .map_err(|e| format!("could not open history: {e}"))?;
        history
            .write_maintenance(
                &orbis_core::transaction::history::MaintenanceRecord::execution_started(
                    plan.clone(),
                ),
                &plan.operation_id,
            )
            .map_err(|e| format!("could not start maintenance record: {e}"))?;
        Some(history)
    } else {
        None
    };
    let mut providers = Vec::new();
    for provider_plan in &plan.providers {
        if !provider_plan.executable() {
            providers.push(skipped_provider(provider_plan));
            continue;
        }
        let exec_res = if json {
            registry.execute_maintenance(provider_plan, &executor)
        } else {
            let candidate_count =
                provider_plan.candidates.len().max(provider_plan.cleanup_candidates.len());
            let header = orbis_core::progress::OperationHeader {
                title: format!("{} {}", provider_plan.action.label(), provider_plan.source.label()),
                target: format!("{candidate_count} candidate(s)"),
                source: provider_plan.source,
                scope: provider_plan
                    .scope
                    .map(|s| s.label().to_string())
                    .unwrap_or_else(|| "provider scope".into()),
                privileged: provider_plan.privilege
                    == orbis_core::transaction::PrivilegeRequirement::Administrator,
            };
            let progress = crate::render::progress::PlainProgressRenderer::new(
                renderer.theme,
                header,
                orbis_core::progress::ExecutionStage::maintenance_stages(),
                io::stderr().is_terminal(),
                8,
            );
            progress.print_header();
            registry.execute_maintenance_with_progress(provider_plan, &executor, &progress)
        };
        match exec_res {
            Ok(result) => providers.extend(result.providers),
            Err(error) => providers.push(MaintenanceProviderResult {
                source: provider_plan.source,
                action: provider_plan.action,
                status: MaintenanceProviderStatus::Failed,
                candidate_count: provider_plan
                    .candidates
                    .len()
                    .max(provider_plan.cleanup_candidates.len()),
                verification: None,
                message: Some(error),
            }),
        }
    }
    let result = MaintenanceResult {
        operation_id: plan.operation_id.clone(),
        action: plan.action,
        status: maintenance_status(&providers),
        providers,
    };
    if let Some(history) = history {
        history
            .write_maintenance(
                &orbis_core::transaction::history::MaintenanceRecord::completed(
                    plan,
                    result.clone(),
                ),
                &result.operation_id,
            )
            .map_err(|e| format!("could not record maintenance: {e}"))?;
    }
    if json {
        print_json(&result)
    } else {
        print!("{}", renderer.maintenance_result(&result));
        Ok(())
    }
}

fn skipped_provider(
    plan: &orbis_core::maintenance::ProviderMaintenancePlan,
) -> MaintenanceProviderResult {
    MaintenanceProviderResult {
        source: plan.source,
        action: plan.action,
        status: MaintenanceProviderStatus::Skipped,
        candidate_count: plan.candidates.len().max(plan.cleanup_candidates.len()),
        verification: None,
        message: plan.warnings.iter().find_map(|warning| {
            (warning.level == orbis_core::transaction::WarningLevel::Blocked)
                .then(|| warning.message.clone())
        }),
    }
}
fn maintenance_status(results: &[MaintenanceProviderResult]) -> MaintenanceStatus {
    if results.is_empty() {
        return MaintenanceStatus::Blocked;
    }
    let success = results
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                MaintenanceProviderStatus::Succeeded
                    | MaintenanceProviderStatus::PartiallySucceeded
            )
        })
        .count();
    let failed = results.iter().any(|r| r.status == MaintenanceProviderStatus::Failed);
    let skipped = results.iter().any(|r| {
        matches!(r.status, MaintenanceProviderStatus::Skipped | MaintenanceProviderStatus::Blocked)
    });
    if failed && success > 0 || skipped && success > 0 {
        MaintenanceStatus::PartiallySucceeded
    } else if failed {
        MaintenanceStatus::Failed
    } else if success == results.len()
        && results.iter().any(|r| r.status == MaintenanceProviderStatus::PartiallySucceeded)
    {
        MaintenanceStatus::PartiallySucceeded
    } else if success == results.len() {
        MaintenanceStatus::Succeeded
    } else {
        MaintenanceStatus::Blocked
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
        .map_err(|e| format!("could not open history: {e}"))?;
    if let Some(id) = operation_id {
        let entry =
            history.entry(&id)?.ok_or_else(|| format!("history operation {id} was not found"))?;
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
    let reference = parse_reference(&package, source)?;
    let ResolveReport::Found { package, .. } = registry.resolve(&reference) else {
        return report_error(json, "package reference was not resolved uniquely".into());
    };
    let mut report = registry.why(&package)?;
    if let Ok(history) = orbis_core::transaction::history::HistoryStore::default_location()
        && let Ok(entries) = history.entries()
    {
        report.orbis_history = entries
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
    if report.orbis_history.is_empty() {
        report.notes.push("Orbis has no installation record for this package; that does not establish who installed it.".into());
    }
    if json {
        print_json(&report)
    } else {
        print!("{}", renderer.why(&report));
        Ok(())
    }
}

fn parse_reference(input: &str, source: Option<PackageSource>) -> Result<PackageRef, String> {
    if let Some((prefix, _)) = input.split_once(':')
        && let Some(qualified) = PackageSource::parse(prefix)
        && source.is_some_and(|explicit| explicit != qualified)
    {
        return Err(format!(
            "package reference selects {qualified}, which conflicts with --source"
        ));
    }
    let reference = orbis_core::parse_package_ref(input, source);
    if reference.query.is_empty() {
        Err("package reference cannot be empty".into())
    } else {
        Ok(reference)
    }
}
fn confirm(plan: &OperationPlan) -> Result<bool, String> {
    if plan.risk >= orbis_core::transaction::RiskLevel::HighImpact {
        eprint!("This is a high-impact removal. Type YES to continue: ");
    } else {
        eprint!("Continue with this operation? [Y/n] ");
    }
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    Ok(if plan.risk >= orbis_core::transaction::RiskLevel::HighImpact {
        answer.trim() == "YES"
    } else {
        matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
    })
}
fn confirm_maintenance(plan: &MaintenancePlan) -> Result<bool, String> {
    if plan.risk >= orbis_core::transaction::RiskLevel::HighImpact {
        eprint!("This maintenance plan contains high-impact changes. Type YES to continue: ");
    } else {
        eprint!("Continue with this maintenance plan? [Y/n] ");
    }
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    Ok(if plan.risk >= orbis_core::transaction::RiskLevel::HighImpact {
        answer.trim() == "YES"
    } else {
        matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
    })
}
fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    println!("{}", serde_json::to_string_pretty(value).map_err(|e| e.to_string())?);
    Ok(())
}
fn report_error(json: bool, message: String) -> Result<(), String> {
    if json {
        print_json(&serde_json::json!({"status":"error","message":message}))?;
    }
    Err(message)
}
fn report_transaction_error(json: bool, message: &str, plan: &OperationPlan) -> Result<(), String> {
    if json {
        print_json(&serde_json::json!({"status":"blocked","plan":plan,"message":message}))?;
    }
    Err(message.into())
}
