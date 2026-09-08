use super::theme::{Theme, Token};
use orbis_core::{
    SearchReport,
    diagnostics::DoctorReport,
    maintenance::{
        MaintenancePlan, MaintenanceProviderStatus, MaintenanceResult, MaintenanceStatus,
        UpdateInventoryReport, WhyReport,
    },
    models::{Package, PackageSource, SourceInfo},
    transaction::{OperationPlan, TransactionResult, TransactionStatus, VerificationResult},
};

pub(crate) struct Renderer {
    pub(crate) theme: Theme,
}

impl Renderer {
    pub(crate) fn new(color: bool) -> Self {
        Self { theme: Theme::detect(color) }
    }

    pub(crate) fn home(&self, _sources: &[SourceInfo]) -> String {
        let mut output = String::new();
        output.push_str(&format!(
            "{}\n{}\n\n",
            self.theme.paint(self.theme.brand_compact(), Token::Primary),
            self.theme.paint(Theme::brand_tagline(), Token::Muted)
        ));
        output.push_str(
            "COMMON COMMANDS\n\n  orbis find <name>\n  orbis show <name>\n  orbis install <name>\n  orbis remove <name>\n  orbis update\n  orbis refresh\n  orbis clean\n  orbis health\n",
        );
        output.push_str("\nTRY\n  orbis find <name>\n  orbis update\n  orbis health\n");
        output
    }

    pub(crate) fn sources(&self, sources: &[SourceInfo]) -> String {
        let mut output = self.heading("Sources", "A compact view of Orbis's provider boundary.");
        let mut group = None;
        for source in sources {
            let system = matches!(
                source.source,
                PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap
            );
            if group != Some(system) {
                output.push_str(if system { "SYSTEM & DESKTOP\n" } else { "DEVELOPER TOOLS\n" });
                group = Some(system);
            }
            output.push_str(&format!(
                "\n{}  {}\n",
                self.theme.paint(source.source.label(), Token::Provider),
                self.theme.paint(&source.state, state_token(&source.state))
            ));
            if let Some(backend) = &source.backend {
                output.push_str(&format!("  Backend       {backend}\n"));
            }
            output.push_str(&format!("  Read          {}\n", capability_summary(source, true)));
            output.push_str(&format!("  Maintenance   {}\n", capability_summary(source, false)));
            output.push_str(&format!("  Mutations     {}\n", mutation_summary(source)));
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

    pub(crate) fn search(&self, query: &str, report: &SearchReport) -> String {
        let mut output = self.heading("Find", &format!("Software matching {query}"));
        if report.results.is_empty() {
            output.push_str("\nNo software matched that search.\n");
        }
        for package in &report.results {
            output.push_str(&format!("\n{}\n", self.theme.paint(&package.name, Token::Primary),));
            if let Some(summary) = &package.summary {
                output.push_str(&format!("  {}\n", summary));
            }
            output.push_str(&format!("  {}", installed_state(package.installed)));
            if let Some(version) = &package.version {
                output.push_str(&format!(" · {version}"));
            }
            output.push_str(&format!(" · {}\n", friendly_source(package.source)));
        }
        self.render_issues(output, &report.issues)
    }

    pub(crate) fn show(
        &self,
        brief: &orbis_core::explain::PackageBrief,
        why: Option<&WhyReport>,
        issues: &[orbis_core::models::ProviderIssue],
    ) -> String {
        let package = &brief.package;
        let mut output = self.heading(&package.name, &brief.headline);
        output.push_str("\nWHAT IS IT?\n");
        if brief.paragraphs.is_empty() {
            output.push_str("No plain-language description is available from the source.\n");
        } else {
            output.push_str(&format!("{}\n", wrap(&brief.paragraphs[0], self.theme.width)));
        }
        if !brief.examples.is_empty() {
            output.push_str("\nWHY WOULD I USE IT?\n");
            for example in &brief.examples {
                output.push_str(&format!("  • {example}\n"));
            }
        }
        output.push_str("\nON THIS COMPUTER\n");
        self.field(&mut output, "Status", installed_state(package.installed));
        self.field(&mut output, "From", friendly_source(package.source));
        if let Some(version) = &package.version {
            self.field(&mut output, "Version", version);
        }
        if let Some(why) = why {
            output.push_str("\nWHY IS IT INSTALLED?\n");
            output.push_str(&format!("{}\n", why.installed_as));
            if !why.orbis_history.is_empty() {
                output.push_str("  Orbis has a matching installation record.\n");
            }
        }
        if let Some(caution) = &brief.caution {
            output.push_str(&format!("\nNOTE\n{}\n", wrap(caution, self.theme.width)));
        }
        output.push_str("\nDETAILS\n");
        self.field(&mut output, "Confidence", &brief.confidence);
        self.field(
            &mut output,
            "Evidence",
            &brief
                .evidence
                .iter()
                .map(|evidence| match evidence.kind {
                    orbis_core::explain::EvidenceKind::ProviderMetadata => "source metadata",
                    orbis_core::explain::EvidenceKind::OrbisInterpretation => {
                        "Orbis interpretation"
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        self.render_issues(output, issues)
    }

    pub(crate) fn info(
        &self,
        package: &Package,
        issues: &[orbis_core::models::ProviderIssue],
    ) -> String {
        let mut output =
            self.heading(&package.name, package.summary.as_deref().unwrap_or("Package metadata"));
        output.push_str("Details\n");
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
        if let Some(homepage) = &package.homepage {
            self.field(&mut output, "Homepage", homepage);
        }
        if let Some(description) = &package.description {
            output.push_str("\nProvider description\n");
            output.push_str(&indent(&wrap(description, self.theme.width.saturating_sub(2)), "  "));
        }
        self.render_issues(output, issues)
    }

    pub(crate) fn brief(&self, brief: &orbis_core::explain::PackageBrief) -> String {
        let mut output = self.heading("ORBIS Brief", &brief.package.name);
        output.push_str(&format!("{}\n\n", self.theme.paint(&brief.headline, Token::Primary)));
        for paragraph in &brief.paragraphs {
            output.push_str(&format!("{}\n\n", wrap(paragraph, self.theme.width)));
        }
        if let Some(kind) = brief.package.kind {
            self.field(&mut output, "Type", kind.label());
        }
        if !brief.examples.is_empty() {
            output.push_str("\nExamples\n");
            for example in &brief.examples {
                output.push_str(&format!("  - {example}\n"));
            }
        }
        if let Some(caution) = &brief.caution {
            output.push_str(&format!("\nContext\n{}\n", wrap(caution, self.theme.width)));
        }
        output.push_str(&format!("\nConfidence     {}\n", brief.confidence));
        output.push_str("Evidence       ");
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

    pub(crate) fn transaction_plan(&self, plan: &OperationPlan) -> String {
        let verb = plan.action.label();
        let mut output = self
            .heading(&format!("{verb} {}?", plan.target.name), "Review before anything changes.");
        output.push_str("THIS WILL\n");
        if !plan.changes.is_empty() {
            for change in &plan.changes {
                output.push_str(&format!(
                    "  • {}{}\n",
                    change.name.as_deref().unwrap_or(&change.package_id),
                    change.version.as_deref().map(|v| format!(" · {v}")).unwrap_or_default()
                ));
            }
        } else {
            output.push_str("  • Change the selected software\n");
        }
        output.push_str("\nFROM\n");
        self.field(&mut output, "Source", friendly_source(plan.target.source));
        self.field(
            &mut output,
            "Version",
            plan.target.version.as_deref().unwrap_or("Not confirmed"),
        );
        self.field(&mut output, "Permission", privilege_label(plan.privilege));
        if let Some(bytes) = plan.disk_delta_bytes {
            self.field(&mut output, "Disk change", &format_size_delta(bytes));
        }
        output.push_str("\nDETAILS\n");
        self.field(&mut output, "Risk", plan.risk.label());
        self.field(&mut output, "Confidence", confidence_label(plan.confidence));
        for warning in &plan.warnings {
            output.push_str(&format!("  {} {}\n", warning_marker(warning.level), warning.message));
        }
        output.push_str("\nNo package state has been changed by this review.\n");
        output
    }

    pub(crate) fn transaction_result(&self, result: &TransactionResult) -> String {
        let (label, token) = match result.status {
            TransactionStatus::Succeeded => ("Completed", Token::Positive),
            TransactionStatus::PartiallyVerified => {
                ("Completed with limited verification", Token::Caution)
            }
            TransactionStatus::Failed => ("Failed", Token::Destructive),
        };
        let mut output = self
            .heading(label, &format!("{} {}", result.plan.action.label(), result.plan.target.name));
        output.push_str(&format!("  Status          {}\n", self.theme.paint(label, token)));
        output
            .push_str(&format!("  Verification    {}\n", verification_label(result.verification)));
        if let Some(message) = &result.execution.message {
            output.push_str(&format!("  Detail          {message}\n"));
        }
        output.push_str(&format!("  Transaction ID  {}\n", result.plan.operation_id));
        output
    }

    pub(crate) fn updates(&self, report: &UpdateInventoryReport) -> String {
        let mut output =
            self.heading("Software updates", "A read-only check; nothing changes yet.");
        if report.candidates.is_empty() {
            output.push_str("\nNo software updates are available.\n");
        } else {
            output.push_str(&format!(
                "\n{} update{} available\n",
                report.total(),
                if report.total() == 1 { "" } else { "s" }
            ));
            for candidate in &report.candidates {
                output.push_str(&format!(
                    "  {:<28} {} → {} · {}\n",
                    candidate.name,
                    candidate.current_version.as_deref().unwrap_or("current"),
                    candidate.available_version.as_deref().unwrap_or("latest"),
                    friendly_source(candidate.source)
                ));
            }
        }
        for inventory in &report.inventories {
            if inventory
                .metadata_state
                .as_deref()
                .is_some_and(|s| s.contains("incomplete") || s.contains("unknown"))
            {
                output.push_str(&format!(
                    "\n  {} update status is incomplete; unknown is not counted as zero.\n",
                    friendly_source(inventory.source)
                ));
            }
        }
        for issue in &report.issues {
            output.push_str(&format!("\n  {}: {}\n", friendly_source(issue.source), issue.message));
        }
        output.push_str("\n  Read-only status; package state and indexes were not changed.\n");
        output
    }

    pub(crate) fn maintenance_plan(&self, plan: &MaintenancePlan) -> String {
        let title = match plan.action {
            orbis_core::maintenance::MaintenanceAction::Refresh => "Refresh software information",
            orbis_core::maintenance::MaintenanceAction::Upgrade => "Update plan",
            orbis_core::maintenance::MaintenanceAction::Cleanup => "Cleanup plan",
        };
        let mut output = self.heading(title, "Review the read-only plan before applying changes.");
        let total: usize =
            plan.providers.iter().map(|p| p.candidates.len().max(p.cleanup_candidates.len())).sum();
        let summary = match plan.action {
            orbis_core::maintenance::MaintenanceAction::Refresh => format!(
                "\n  Refreshing information from {} software source{}\n",
                plan.providers.len(),
                if plan.providers.len() == 1 { "" } else { "s" }
            ),
            orbis_core::maintenance::MaintenanceAction::Upgrade => format!(
                "\n  {total} update{} across {} software source{}\n",
                if total == 1 { "" } else { "s" },
                plan.providers.len(),
                if plan.providers.len() == 1 { "" } else { "s" }
            ),
            orbis_core::maintenance::MaintenanceAction::Cleanup => format!(
                "\n  {total} cleanup item{} across {} software source{}\n",
                if total == 1 { "" } else { "s" },
                plan.providers.len(),
                if plan.providers.len() == 1 { "" } else { "s" }
            ),
        };
        output.push_str(&summary);
        for provider in &plan.providers {
            output.push_str(&format!(
                "\n{}  {}\n",
                self.theme.paint(friendly_source(provider.source), Token::Provider),
                provider.scope.map(|s| s.label()).unwrap_or("scope not reported")
            ));
            for candidate in &provider.candidates {
                output.push_str(&format!(
                    "  {}  {} → {}\n",
                    candidate.name,
                    candidate.current_version.as_deref().unwrap_or("current"),
                    candidate.available_version.as_deref().unwrap_or("latest")
                ));
            }
            if provider.candidates.is_empty() {
                output.push_str(match plan.action {
                    orbis_core::maintenance::MaintenanceAction::Refresh => {
                        "  Software information will be refreshed.\n"
                    }
                    _ => "  No changes reported.\n",
                });
            }
            for warning in &provider.warnings {
                output.push_str(&format!(
                    "  {} {}\n",
                    warning_marker(warning.level),
                    warning.message
                ));
            }
        }
        output.push_str(&format!(
            "\nRisk          {}\nPlan ID       {}\n",
            plan.risk.label(),
            plan.operation_id
        ));
        output
    }

    pub(crate) fn maintenance_result(&self, result: &MaintenanceResult) -> String {
        let title = match result.status {
            MaintenanceStatus::Succeeded => "Completed",
            MaintenanceStatus::PartiallySucceeded => "Partially completed",
            MaintenanceStatus::Failed => "Failed",
            MaintenanceStatus::Cancelled => "Cancelled",
            MaintenanceStatus::Blocked => "Blocked",
        };
        let mut output =
            self.heading(title, &format!("{} across available sources", result.action.label()));
        for provider in &result.providers {
            let status = match provider.status {
                MaintenanceProviderStatus::Succeeded => "succeeded",
                MaintenanceProviderStatus::PartiallySucceeded => "partially succeeded",
                MaintenanceProviderStatus::Failed => "failed",
                MaintenanceProviderStatus::Skipped => "skipped",
                MaintenanceProviderStatus::Blocked => "blocked",
            };
            output.push_str(&format!(
                "  {:<24} {:<22} {} change{}\n",
                friendly_source(provider.source),
                status,
                provider.candidate_count,
                if provider.candidate_count == 1 { "" } else { "s" }
            ));
            if let Some(message) = &provider.message {
                output.push_str(&format!("    {message}\n"));
            }
        }
        output.push_str(&format!("\n  Maintenance ID  {}\n", result.operation_id));
        output
    }

    pub(crate) fn history(
        &self,
        entries: &[orbis_core::transaction::history::HistoryEntry],
        limit: usize,
    ) -> String {
        let mut output =
            self.heading("History", &format!("Recent Orbis operations (limit {limit})."));
        if entries.is_empty() {
            return output + "\nNo Orbis operations recorded yet.\n";
        }
        for entry in entries {
            output.push_str(&format!(
                "\n  {}  {:<12} {:<10} {}{}\n",
                entry.operation_id,
                entry.action,
                entry.status,
                entry.source.map(friendly_source).unwrap_or("Orbis"),
                entry.package.as_deref().map(|p| format!(" · {p}")).unwrap_or_default()
            ));
            if let Some(message) = &entry.message {
                output.push_str(&format!("    {message}\n"));
            }
        }
        output
    }

    pub(crate) fn history_entry(&self, entry: &serde_json::Value) -> String {
        self.heading("History record", "Sanitized persisted operation record.")
            + &serde_json::to_string_pretty(entry).unwrap_or_else(|_| "{}".into())
            + "\n"
    }

    pub(crate) fn why(&self, report: &WhyReport) -> String {
        let mut output = self.heading("Why", &report.package.name);
        output.push_str(&format!("\nREQUIRED\n{}\n", report.installed_as));
        self.field(&mut output, "From", friendly_source(report.package.source));
        if !report.used_by.is_empty() {
            output.push_str("\nUSED BY\n");
            for consumer in &report.used_by {
                output.push_str(&format!("  {} · {}\n", consumer.name, consumer.relationship));
            }
        }
        output.push_str(&format!(
            "\nREMOVAL CONTEXT\n{}\n",
            wrap(&report.removal_advice, self.theme.width)
        ));
        for evidence in &report.evidence {
            output.push_str(&format!("  Evidence  {evidence}\n"));
        }
        for note in &report.notes {
            output.push_str(&format!("  Note      {note}\n"));
        }
        output
    }

    pub(crate) fn health(&self, report: &DoctorReport) -> String {
        let mut output =
            self.heading("Health", "A safe check of the software tools Orbis can use.");
        let failed = report.checks.iter().filter(|check| !check.passed).count();
        output.push_str(&format!(
            "{}\n\n",
            if failed == 0 { "Everything looks good." } else { "Some things need attention." }
        ));
        output.push_str("SYSTEM\n");
        for check in &report.checks {
            if check.area == "Environment" {
                continue;
            }
            let token = if check.passed { Token::Positive } else { Token::Caution };
            output.push_str(&format!(
                "\n  {} {:<24} {}\n  {}\n",
                self.theme.paint(self.theme.mark(token), token),
                friendly_area(&check.area),
                check.title,
                wrap(&check.message, self.theme.width.saturating_sub(4))
            ));
        }
        let environment = report.checks.iter().filter(|check| check.area == "Environment");
        if environment.clone().next().is_some() {
            output.push_str("\nENVIRONMENT\n");
        }
        for check in environment {
            let token = if check.passed { Token::Positive } else { Token::Caution };
            output.push_str(&format!(
                "  {} {}\n  {}\n",
                self.theme.paint(self.theme.mark(token), token),
                check.title,
                wrap(&check.message, self.theme.width.saturating_sub(4))
            ));
        }
        output
    }

    pub(crate) fn ambiguous(
        &self,
        query: &str,
        matches: &[Package],
        issues: &[orbis_core::models::ProviderIssue],
    ) -> String {
        let mut output =
            self.heading("Choose a source", &format!("{query} is available in several places."));
        output.push_str("\nOrbis will not choose for you. Pick one explicitly:\n");
        for (index, package) in matches.iter().enumerate() {
            output.push_str(&format!(
                "  {}. {}{}\n",
                index + 1,
                friendly_source(package.source),
                if package.source == PackageSource::Apt {
                    "  (recommended for most Linux systems)"
                } else {
                    ""
                }
            ));
        }
        output.push_str("\nFor scripts, use --source or a qualified reference.\n");
        self.render_issues(output, issues)
    }
    pub(crate) fn not_found(
        &self,
        query: &str,
        issues: &[orbis_core::models::ProviderIssue],
    ) -> String {
        self.render_issues(
            self.heading("No software found", query)
                + "\nTry a broader search or check the spelling.\n",
            issues,
        )
    }

    fn heading(&self, title: &str, subtitle: &str) -> String {
        format!(
            "{}  /  {}\n{}\n\n",
            self.theme.paint(self.theme.brand_compact(), Token::Primary),
            self.theme.paint(title, Token::Primary),
            self.theme.paint(subtitle, Token::Muted)
        )
    }
    fn field(&self, output: &mut String, label: &str, value: &str) {
        output.push_str(&format!("  {label:<14}{value}\n"));
    }
    fn render_issues(
        &self,
        mut output: String,
        issues: &[orbis_core::models::ProviderIssue],
    ) -> String {
        for issue in issues {
            output.push_str(&format!("\n  {}: {}\n", friendly_source(issue.source), issue.message));
        }
        output
    }
}

fn state_token(state: &str) -> Token {
    match state {
        "ready" => Token::Positive,
        "unavailable" => Token::Unavailable,
        _ => Token::Caution,
    }
}
fn capability_summary(source: &SourceInfo, read: bool) -> String {
    let mut values = Vec::new();
    if read {
        if source.capabilities.search {
            values.push("search");
        }
        if source.capabilities.info {
            values.push("info");
        }
        if source.capabilities.installed_state {
            values.push("installed-state");
        }
    } else {
        if source.capabilities.updates {
            values.push("updates");
        }
        if source.capabilities.upgrade {
            values.push("upgrade");
        }
        if source.capabilities.refresh {
            values.push("refresh");
        }
        if source.capabilities.cleanup {
            values.push("clean");
        }
    }
    values.join(", ")
}
fn mutation_summary(source: &SourceInfo) -> String {
    let mut values = Vec::new();
    if source.capabilities.install {
        values.push("install");
    }
    if source.capabilities.remove {
        values.push("remove");
    }
    values.join(", ")
}
fn installed_state(installed: Option<bool>) -> &'static str {
    match installed {
        Some(true) => "Installed",
        Some(false) => "Available",
        None => "Availability unknown",
    }
}

fn friendly_source(source: PackageSource) -> &'static str {
    match source {
        PackageSource::Apt => "Ubuntu/Debian repositories",
        PackageSource::Flatpak => "Flatpak apps",
        PackageSource::Snap => "Snap Store",
        PackageSource::Cargo => "Rust tools",
        PackageSource::Npm | PackageSource::Pnpm => "Node.js tools",
        PackageSource::Uv | PackageSource::Pipx => "Python tools",
    }
}

fn friendly_area(area: &str) -> &'static str {
    match area {
        "APT" => friendly_source(PackageSource::Apt),
        "Flatpak" => friendly_source(PackageSource::Flatpak),
        "Snap" => friendly_source(PackageSource::Snap),
        "Cargo" => friendly_source(PackageSource::Cargo),
        "npm" => friendly_source(PackageSource::Npm),
        "pnpm" => friendly_source(PackageSource::Pnpm),
        "uv" => friendly_source(PackageSource::Uv),
        "pipx" => friendly_source(PackageSource::Pipx),
        _ => "Software tools",
    }
}

fn format_size_delta(bytes: i64) -> String {
    let sign = if bytes >= 0 { "+" } else { "-" };
    let value = bytes.unsigned_abs();
    if value < 1024 {
        format!("{sign}{value} B")
    } else if value < 1024 * 1024 {
        format!("{sign}{:.1} KB", value as f64 / 1024.0)
    } else if value < 1024 * 1024 * 1024 {
        format!("{sign}{:.1} MB", value as f64 / (1024.0 * 1024.0))
    } else {
        format!("{sign}{:.1} GB", value as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn confidence_label(value: orbis_core::transaction::PlanConfidence) -> &'static str {
    match value {
        orbis_core::transaction::PlanConfidence::High => "high",
        orbis_core::transaction::PlanConfidence::Medium => "medium",
        orbis_core::transaction::PlanConfidence::Low => "low",
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use orbis_core::{
        diagnostics::{DiagnosticCheck, DoctorReport},
        maintenance::{MaintenanceAction, MaintenancePlan, WhyReport},
        models::{PackageKind, ProviderCapabilities, ProviderIssue},
        transaction::{OperationAction, OperationPlan},
    };
    use std::collections::BTreeMap;

    fn renderer() -> Renderer {
        Renderer { theme: Theme::test(80) }
    }

    fn package(source: PackageSource, id: &str, installed: Option<bool>) -> Package {
        Package {
            source,
            provider_id: id.into(),
            name: id.into(),
            version: Some("1.0.0".into()),
            summary: Some("A deterministic test package.".into()),
            description: None,
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

    #[test]
    fn representative_plain_views_have_hierarchy_and_empty_states() {
        let renderer = renderer();
        let source = SourceInfo {
            source: PackageSource::Apt,
            available: true,
            state: "ready".into(),
            backend: Some("apt-get".into()),
            capabilities: capabilities(),
            notes: Vec::new(),
        };
        assert!(renderer.home(std::slice::from_ref(&source)).contains("Your Linux software"));
        assert!(renderer.home(std::slice::from_ref(&source)).contains("ORBIS"));
        assert!(renderer.sources(&[source]).contains("SYSTEM & DESKTOP"));
        let report = SearchReport {
            results: vec![package(PackageSource::Cargo, "bat", Some(true))],
            issues: Vec::new(),
        };
        assert!(renderer.search("bat", &report).contains("Installed"));
        assert!(
            renderer
                .brief(&orbis_core::explain::build_brief(package(
                    PackageSource::Cargo,
                    "bat",
                    Some(true)
                )))
                .contains("ORBIS Brief")
        );
        let updates = UpdateInventoryReport {
            inventories: Vec::new(),
            candidates: Vec::new(),
            issues: Vec::new(),
        };
        assert!(renderer.updates(&updates).contains("No software updates are available"));
        assert!(renderer.history(&[], 20).contains("No Orbis operations recorded yet"));
    }

    #[test]
    fn representative_plans_explanations_and_errors_render() {
        let renderer = renderer();
        let target = package(PackageSource::Cargo, "bat", Some(false));
        let plan = OperationPlan::new(
            OperationAction::Install,
            target.clone(),
            orbis_core::transaction::InstallScope::User,
        );
        assert!(renderer.transaction_plan(&plan).contains("No package state has been changed"));
        let maintenance =
            MaintenancePlan::new(MaintenanceAction::Cleanup, Some(PackageSource::Apt), Vec::new());
        assert!(renderer.maintenance_plan(&maintenance).contains("Cleanup plan"));
        let why = WhyReport {
            package: target,
            installed_as: "Required by other installed software.".into(),
            used_by: Vec::new(),
            evidence: vec!["provider metadata".into()],
            removal_advice: "No autoremove candidate.".into(),
            orbis_history: Vec::new(),
            notes: Vec::new(),
        };
        assert!(renderer.why(&why).contains("Why"));
        let issue = ProviderIssue {
            source: PackageSource::Flatpak,
            message: "Flatpak is not installed.".into(),
            technical: None,
        };
        assert!(renderer.not_found("xyz", std::slice::from_ref(&issue)).contains("Flatpak"));
        assert!(
            renderer
                .ambiguous("bat", &[package(PackageSource::Cargo, "bat", Some(true))], &[issue])
                .contains("Choose a source")
        );
        let doctor = DoctorReport {
            checks: vec![DiagnosticCheck::environment(true, "Terminal", "plain")],
            read_only: true,
        };
        assert!(renderer.health(&doctor).contains("Health"));
    }

    #[test]
    fn ambiguous_source_chooser_explains_the_safe_choice() {
        let renderer = renderer();
        let output = renderer.ambiguous(
            "btop",
            &[
                package(PackageSource::Apt, "btop", Some(false)),
                package(PackageSource::Snap, "btop", Some(false)),
                package(PackageSource::Cargo, "btop", Some(false)),
            ],
            &[],
        );
        assert!(output.contains("btop is available in several places"));
        assert!(output.contains("Ubuntu/Debian repositories"));
        assert!(output.contains("Snap Store"));
        assert!(output.contains("Orbis will not choose for you"));
        assert!(output.contains("--source"));
    }
}
fn installed_label(installed: Option<bool>) -> &'static str {
    match installed {
        Some(true) => "Yes",
        Some(false) => "No",
        None => "Unknown",
    }
}
fn privilege_label(privilege: orbis_core::transaction::PrivilegeRequirement) -> &'static str {
    match privilege {
        orbis_core::transaction::PrivilegeRequirement::None => "none / user scope",
        orbis_core::transaction::PrivilegeRequirement::Administrator => "administrator",
    }
}
fn verification_label(value: VerificationResult) -> &'static str {
    match value {
        VerificationResult::Verified => "verified",
        VerificationResult::PartiallyVerified => "partially verified",
        VerificationResult::Failed => "failed",
    }
}
fn warning_marker(level: orbis_core::transaction::WarningLevel) -> &'static str {
    match level {
        orbis_core::transaction::WarningLevel::Info => "·",
        orbis_core::transaction::WarningLevel::Caution => "!",
        orbis_core::transaction::WarningLevel::Blocked => "×",
    }
}
fn wrap(text: &str, width: usize) -> String {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.len() + word.len() + 1 > width {
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
