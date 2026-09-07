use std::{
    io::{self, IsTerminal},
    process::ExitCode,
};

use clap::{Parser, Subcommand, ValueEnum};
use orbis_core::{
    ProviderRegistry, ResolveReport, SearchReport,
    diagnostics::DoctorReport,
    explain::build_brief,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SourceArg {
    Apt,
    Flatpak,
    Snap,
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
                    "supported_mutations": ["install", "remove"]
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
            let marker = if source.available {
                self.paint(if self.unicode { "●" } else { "*" }, Tone::Good)
            } else {
                self.paint(if self.unicode { "○" } else { "o" }, Tone::Muted)
            };
            output.push_str(&format!(
                "  {marker} {:<9} {}\n",
                source.source.label(),
                if source.available {
                    self.paint("ready", Tone::Good)
                } else {
                    self.paint("unavailable", Tone::Muted)
                }
            ));
        }
        output.push_str(
            "\nTry\n  orbis search <package>\n  orbis explain <package>\n  orbis doctor\n",
        );
        output
    }

    fn sources(&self, sources: &[SourceInfo]) -> String {
        let mut output = self.heading("Sources", "Read-only package discovery available to Orbis.");
        for source in sources {
            output.push_str(&format!(
                "\n{}  {}\n",
                self.paint(source.source.label(), Tone::Title),
                if source.available {
                    self.paint("ready", Tone::Good)
                } else {
                    self.paint("unavailable", Tone::Muted)
                }
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
            output.push_str("  Mutations     install, remove (single package)\n");
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
}
