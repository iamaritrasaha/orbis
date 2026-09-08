use std::io::{self, IsTerminal, Write};

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
    self_update::{self, SelfUpdateReport, SilentUpdateObserver},
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
        Some(Command::Find { query, source }) => {
            let report = registry.search(&query, source.map(Into::into));
            if cli.json {
                print_json(&report)
            } else {
                print!("{}", renderer.search(&query, &report));
                Ok(())
            }
        }
        Some(Command::Show { package, source }) => {
            run_show(registry, renderer, cli.json, package, source.map(Into::into))
        }
        Some(Command::Info { package, source }) => {
            run_info(registry, renderer, cli.json, package, source.map(Into::into))
        }
        Some(Command::Explain { package, source }) => {
            run_explain(registry, renderer, cli.json, package, source.map(Into::into))
        }
        Some(Command::Health) => {
            let report = registry.diagnostics(None).with_environment();
            if cli.json {
                print_json(&report)
            } else {
                print!("{}", renderer.health(&report));
                Ok(())
            }
        }
        Some(Command::SelfUpdate { check, yes }) => {
            run_self_update(renderer, cli.json, cli.plain, check, yes)
        }
        Some(Command::Install { package, source, scope, channel, plan, yes }) => run_transaction(
            registry,
            renderer,
            cli.json,
            cli.plain,
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
            cli.plain,
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
        Some(Command::Update { source, plan, apply, yes }) if plan || apply || yes => {
            run_maintenance(
                registry,
                renderer,
                cli.json,
                cli.plain,
                MaintenanceOptions {
                    action: MaintenanceAction::Upgrade,
                    source: source.map(Into::into),
                    plan_only: plan,
                    yes,
                },
            )
        }
        Some(Command::Update { source, .. })
            if !cli.json && !cli.plain && crate::tui::should_launch_operation() =>
        {
            crate::tui::run_update(registry, renderer.theme, source.map(Into::into))
        }
        Some(Command::Update { source, .. }) => {
            let report = registry.updates(source.map(Into::into));
            if cli.json {
                print_json(&report)
            } else {
                print!("{}", renderer.updates(&report));
                Ok(())
            }
        }
        Some(Command::Refresh { source, plan, yes }) => run_maintenance(
            registry,
            renderer,
            cli.json,
            cli.plain,
            MaintenanceOptions {
                action: MaintenanceAction::Refresh,
                source: source.map(Into::into),
                plan_only: plan,
                yes,
            },
        ),
        Some(Command::Upgrade { source, plan, yes }) => run_maintenance(
            registry,
            renderer,
            cli.json,
            cli.plain,
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
            cli.plain,
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

fn run_self_update(
    renderer: &Renderer,
    json: bool,
    plain: bool,
    check_only: bool,
    yes: bool,
) -> Result<(), String> {
    if !json && !plain && crate::tui::should_launch_operation() {
        return crate::tui::run_self_update(renderer.theme, check_only, yes);
    }

    let current = self_update::current_version().map_err(|error| error.to_string())?;
    if self_update::is_development_version(&current) && !check_only {
        let report = self_update::report_for_check(&self_update::CheckReport {
            current_version: current,
            current_is_development: true,
            latest: None,
        });
        return print_self_update_report(json, report);
    }

    let check = match self_update::check_current(check_only) {
        Ok(check) => check,
        Err(error) => {
            return print_self_update_report(json, self_update::error_report_for_cli(error));
        }
    };
    let mut report = self_update::report_for_check(&check);
    if check_only || check.latest.is_none() {
        return print_self_update_report(json, report);
    }
    if !yes {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            report.message = "An update is available. Review it in an interactive terminal or pass --yes after reviewing the release details.".into();
            return print_self_update_report(json, report);
        }
        println!("Current version: {}", report.current_version);
        if let Some(version) = &report.available_version {
            println!("Available release: {version}");
        }
        println!("Destination: the user-owned Orbis executable currently running");
        println!("The package software on this computer will not be changed.");
        println!("{}", report.message);
        eprint!("Update Orbis now? [Y/n] ");
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(|error| error.to_string())?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes") {
            report.message = "Update cancelled. Orbis was not changed.".into();
            return print_self_update_report(json, report);
        }
    }
    report = self_update::install_current(&check, &SilentUpdateObserver);
    print_self_update_report(json, report)
}

fn print_self_update_report(json: bool, report: SelfUpdateReport) -> Result<(), String> {
    if json {
        println!("{}", serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?);
    } else {
        println!("◈ ORBIS");
        println!();
        println!("{}", report.message);
        if let Some(version) = report.available_version {
            println!("Available release: {version}");
        }
    }
    Ok(())
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

fn run_show(
    registry: &ProviderRegistry,
    renderer: &Renderer,
    json: bool,
    package: String,
    source: Option<PackageSource>,
) -> Result<(), String> {
    let reference = parse_reference(&package, source)?;
    match registry.resolve(&reference) {
        ResolveReport::Found { package, issues } => {
            let brief = orbis_core::explain::build_brief(*package.clone());
            let why =
                if package.installed == Some(true) { registry.why(&package).ok() } else { None };
            if json {
                print_json(&serde_json::json!({"brief": brief, "why": why, "issues": issues}))
            } else {
                print!("{}", renderer.show(&brief, why.as_ref(), &issues));
                Ok(())
            }
        }
        ResolveReport::Ambiguous { matches, issues } => {
            if json {
                print_json(&serde_json::json!({
                    "status": "ambiguous",
                    "matches": matches,
                    "issues": issues
                }))?;
            } else {
                print!("{}", renderer.ambiguous(&reference.query, &matches, &issues));
            }
            Err("software name matches more than one source".into())
        }
        ResolveReport::NotFound { issues } => {
            if json {
                print_json(&serde_json::json!({"status": "not_found", "issues": issues}))?;
            } else {
                print!("{}", renderer.not_found(&reference.query, &issues));
            }
            Err("software was not found".into())
        }
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
    plain: bool,
    options: TransactionOptions,
) -> Result<(), String> {
    let package_ref = match resolve_mutation_reference(
        registry,
        renderer,
        json,
        &options.package,
        options.source,
    ) {
        Ok(reference) => reference,
        Err(error) => return report_error(json, error),
    };
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
        if !options.yes && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
            return report_transaction_error(
                json,
                "confirmation is required: use an interactive terminal or pass --yes after reviewing the plan",
                &plan,
            );
        }
        if !plain && crate::tui::should_launch_operation() {
            return crate::tui::run_transaction(
                registry,
                renderer.theme,
                request,
                plan,
                options.yes,
            );
        }
        if !options.yes {
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

fn resolve_mutation_reference(
    registry: &ProviderRegistry,
    renderer: &Renderer,
    json: bool,
    input: &str,
    source: Option<PackageSource>,
) -> Result<PackageRef, String> {
    let mut reference = parse_reference(input, source)?;
    if reference.source.is_some() {
        return Ok(reference);
    }
    match registry.resolve(&reference) {
        ResolveReport::Found { package, .. } => {
            reference.source = Some(package.source);
            Ok(reference)
        }
        ResolveReport::Ambiguous { matches, issues } => {
            if json || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return Err(format!(
                    "{input} is available from several sources; use --source or a qualified reference.\n{}",
                    renderer.ambiguous(input, &matches, &issues)
                ));
            }
            print!("{}", renderer.ambiguous(input, &matches, &issues));
            io::stdout().flush().map_err(|error| error.to_string())?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer).map_err(|error| error.to_string())?;
            let choice = answer
                .trim()
                .parse::<usize>()
                .map_err(|_| "choose one of the numbered software sources".to_owned())?;
            let selected = matches
                .get(choice.saturating_sub(1))
                .ok_or_else(|| "that source choice is not available".to_owned())?;
            reference.source = Some(selected.source);
            Ok(reference)
        }
        ResolveReport::NotFound { issues } => {
            Err(format!("{input} was not found.\n{}", renderer.not_found(input, &issues)))
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
    plain: bool,
    options: MaintenanceOptions,
) -> Result<(), String> {
    if !json && !plain && !options.plan_only && crate::tui::should_launch_operation() {
        return crate::tui::run_maintenance(
            registry,
            renderer.theme,
            options.action,
            options.source,
            options.yes,
        );
    }
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
                target: format!(
                    "{candidate_count} planned change{}",
                    if candidate_count == 1 { "" } else { "s" }
                ),
                source: provider_plan.source,
                scope: provider_plan
                    .scope
                    .map(|s| s.label().to_string())
                    .unwrap_or_else(|| "scope not reported".into()),
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

/// Executes a reviewed maintenance plan while forwarding the same typed core
/// events used by the interactive progress view. This keeps the plan/history/
/// privilege boundary identical for TUI and non-TUI execution.
pub(crate) fn execute_confirmed_maintenance_with_observer(
    registry: &ProviderRegistry,
    plan: MaintenancePlan,
    observer: &dyn orbis_core::progress::ProgressObserver,
) -> Result<MaintenanceResult, String> {
    if !plan.executable() {
        return Err("no executable provider plan is available; nothing was changed".into());
    }
    if plan.action == MaintenanceAction::Upgrade {
        registry.revalidate_upgrade_plan(&plan)?;
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
        match registry.execute_maintenance_with_progress(provider_plan, &executor, observer) {
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
    Ok(result)
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
