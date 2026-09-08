use clap::{Parser, Subcommand, ValueEnum};
use orbis_core::{models::PackageSource, transaction::InstallScope};

#[derive(Debug, Parser)]
#[command(
    name = "orbis",
    version,
    about = "Your Linux software, in one place.",
    long_about = "A calm, beginner-friendly way to find, understand, update, and safely manage software on Linux.",
    help_template = "{name} {version}\n\nCOMMON COMMANDS\n\n  find       Find software\n  show       Learn about software\n  install    Install software\n  remove     Remove software\n  update     Check for updates\n  refresh    Refresh software information\n  clean      Remove unused software safely\n  history    See previous Orbis actions\n  health     Check that everything is working\n\nEXAMPLES\n\n  orbis find firefox\n  orbis show btop\n  orbis install btop\n  orbis update\n  orbis health\n\nADVANCED\n\n  upgrade    Apply a reviewed update plan\n  sources    Inspect software sources\n  why        Inspect installation reasoning\n  search     Compatibility alias for find\n  doctor     Compatibility alias for health\n  --json     Use the stable machine-readable interface\n\n{about}\n\nUse 'orbis <command> --help' for command details.\n"
)]
pub(crate) struct Cli {
    /// Emit structured JSON instead of terminal presentation.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Disable ANSI styling even when stdout is a terminal.
    #[arg(long, global = true)]
    pub(crate) no_color: bool,
    /// Force the traditional terminal presentation.
    #[arg(long, global = true)]
    pub(crate) plain: bool,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Launch the interactive Orbis dashboard.
    #[command(alias = "ui")]
    Dashboard,
    /// Inspect software sources and their capabilities (advanced).
    Sources,
    /// Find software across supported ecosystems.
    #[command(aliases = ["search"])]
    Find {
        /// Human package name, keyword, or application ID.
        query: String,
        /// Restrict the search to one advanced source.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Show the beginner-friendly Orbis Brief for one package.
    Show {
        /// A package name, application ID, or source-qualified reference.
        package: String,
        /// Restrict resolution to one advanced source.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Show normalized metadata for one package (advanced compatibility command).
    #[command(hide = true)]
    Info {
        package: String,
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Explain what a package is in plain language (advanced compatibility command).
    #[command(hide = true)]
    Explain {
        package: String,
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Run safe diagnostics (advanced alias: doctor).
    #[command(aliases = ["doctor"])]
    Health,
    /// Plan and, after confirmation, install one exact package.
    Install {
        /// Software name or source-qualified reference.
        package: String,
        /// Restrict resolution to one advanced source.
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
        /// Software name or source-qualified reference.
        package: String,
        /// Restrict resolution to one advanced source.
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
    /// Check for available software updates. Read-only by default.
    #[command(aliases = ["updates"])]
    Update {
        /// Restrict the check to one advanced source.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Build and show the reviewed plan for applying available updates.
        #[arg(long, conflicts_with_all = ["apply", "yes"])]
        plan: bool,
        /// Apply updates through the existing plan, confirmation, and execution flow.
        #[arg(long, conflicts_with = "plan")]
        apply: bool,
        /// Apply the exact reviewed update plan without asking again.
        #[arg(long, conflicts_with = "plan")]
        yes: bool,
    },
    /// Refresh software information without changing installed software.
    Refresh {
        /// Restrict the refresh to one advanced source.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Show the refresh plan without changing catalog metadata.
        #[arg(long)]
        plan: bool,
        /// Skip Orbis's confirmation prompt for this exact refresh plan.
        #[arg(long)]
        yes: bool,
    },
    /// Plan and, after confirmation, apply safe available updates.
    Upgrade {
        /// Restrict the upgrade to one advanced source.
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
        /// Restrict cleanup to one advanced source.
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
        /// Restrict history rows to one advanced source.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
    /// Explain why one installed package or ref is present.
    Why {
        /// Package name or source-qualified reference.
        package: String,
        /// Restrict resolution to one advanced source.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum SourceArg {
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
pub(crate) enum ScopeArg {
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
