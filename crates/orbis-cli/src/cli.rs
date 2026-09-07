use clap::{Parser, Subcommand, ValueEnum};
use orbis_core::{models::PackageSource, transaction::InstallScope};

#[derive(Debug, Parser)]
#[command(
    name = "orbis",
    version,
    about = "Your Linux software, in one place.",
    long_about = "A calm, provider-neutral view of software available to your Linux system. Read-only discovery and carefully confirmed single-package operations."
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
