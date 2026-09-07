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
};

#[derive(Debug, Parser)]
#[command(
    name = "orbis",
    version,
    about = "Your Linux software, in one place.",
    long_about = "A calm, provider-neutral view of software available to your Linux system. Milestone 1 is read-only."
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SourceArg {
    Apt,
    Flatpak,
    Snap,
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
                    "read_only": true
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
            for note in &source.notes {
                output.push_str(&format!("  Note          {note}\n"));
            }
        }
        output.push_str(
            "\n  Mutating package operations are intentionally unavailable in Milestone 1.\n",
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
}
