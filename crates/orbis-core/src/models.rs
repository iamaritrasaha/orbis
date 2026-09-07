//! Provider-neutral models shared by the CLI and providers.

use std::{collections::BTreeMap, fmt};

use serde::Serialize;

/// A supported package ecosystem.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageSource {
    /// Debian/Ubuntu packages, queried through APT tools.
    Apt,
    /// Flatpak applications and runtimes.
    Flatpak,
    /// Snap packages.
    Snap,
}

impl PackageSource {
    /// Parses a command-line source name.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "apt" | "nala" | "deb" | "debian" | "ubuntu" => Some(Self::Apt),
            "flatpak" => Some(Self::Flatpak),
            "snap" => Some(Self::Snap),
            _ => None,
        }
    }

    /// Returns the canonical user-facing name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Apt => "APT",
            Self::Flatpak => "Flatpak",
            Self::Snap => "Snap",
        }
    }
}

impl fmt::Display for PackageSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// A provider-neutral package classification, when confidently inferred.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageKind {
    /// A desktop or end-user application.
    Application,
    /// A terminal-facing command-line program.
    CliTool,
    /// A library intended to be consumed by other software.
    Library,
    /// Development headers, link libraries, or build metadata.
    DevelopmentFiles,
    /// A runtime or shared execution environment.
    Runtime,
    /// A background service or daemon.
    Service,
    /// No reliable classification is available.
    Unknown,
}

impl PackageKind {
    /// Returns a concise human label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Application => "Application",
            Self::CliTool => "CLI tool",
            Self::Library => "Library",
            Self::DevelopmentFiles => "Development files",
            Self::Runtime => "Runtime",
            Self::Service => "Service",
            Self::Unknown => "Unknown",
        }
    }
}

/// A normalized package record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Package {
    /// Ecosystem that supplied this record.
    pub source: PackageSource,
    /// The provider's canonical identifier.
    pub provider_id: String,
    /// Friendly display name, when one differs from the identifier.
    pub name: String,
    /// Available version string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// One-line provider summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Full provider description, without Orbis interpretation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the package is installed; `None` means the provider did not establish it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<bool>,
    /// A classification inferred from provider metadata or a conservative rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<PackageKind>,
    /// Repository, remote, or store origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Architecture when the provider exposes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    /// Project homepage when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Declared license when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Download or installed size in bytes, only when reliably parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Provider-specific fields kept outside the shared schema.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

/// A package reference supplied by the user.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRef {
    /// An explicit source, if provided.
    pub source: Option<PackageSource>,
    /// Provider ID or friendly query.
    pub query: String,
}

/// Capabilities explicitly exposed by a provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderCapabilities {
    /// Search is supported.
    pub search: bool,
    /// Package information is supported.
    pub info: bool,
    /// Installed-state detection is supported.
    pub installed_state: bool,
    /// Installed package listing is supported.
    pub installed_list: bool,
    /// Mutation is deliberately false in Milestone 1.
    pub mutations: bool,
}

/// A provider's current availability snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceInfo {
    /// Canonical source name.
    pub source: PackageSource,
    /// Whether the required read-only backend is present.
    pub available: bool,
    /// `ready`, `unavailable`, or `unhealthy`.
    pub state: String,
    /// Backend details, such as Nala availability for APT.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Operations exposed by this provider.
    pub capabilities: ProviderCapabilities,
    /// Concise notes for users.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A non-fatal provider-specific problem.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderIssue {
    /// Provider that reported the problem.
    pub source: PackageSource,
    /// Human-readable problem and suggestion.
    pub message: String,
    /// Technical detail retained for JSON/debugging consumers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub technical: Option<String>,
}
