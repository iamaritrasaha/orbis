//! Typed planning and execution records for the first mutation-capable milestone.
//!
//! Providers produce plans and typed provider operations. The CLI is responsible for
//! presentation and confirmation; the privilege layer is responsible for executing only
//! those operations. There is intentionally no API for arbitrary programs or shell strings.

pub mod history;

use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    models::{Package, PackageKind, PackageRef, PackageSource},
    process::CommandOutput,
    providers::ProviderError,
};

/// The only package mutations supported in Milestone 2.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationAction {
    /// Add one exact package to the selected provider scope.
    Install,
    /// Remove one exact package without purge/autoremove/data deletion.
    Remove,
}

impl OperationAction {
    /// Human-readable action label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::Remove => "Remove",
        }
    }
}

/// Provider scope where a package will be changed.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallScope {
    /// Machine-wide package state.
    System,
    /// Per-user package state, supported by Flatpak.
    User,
}

impl InstallScope {
    /// Human-readable scope label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
        }
    }
}

/// The package state observed while planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageState {
    /// The exact package is installed in the selected scope.
    Installed,
    /// The exact package is not installed in the selected scope.
    NotInstalled,
    /// The provider could not establish the state.
    Unknown,
}

/// How complete and authoritative a provider's plan is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanCompleteness {
    /// The provider supplied an authoritative simulation or exact impact.
    Complete,
    /// The provider supplied useful metadata but resolves some impact at commit time.
    Partial,
    /// The provider could not establish enough impact to safely execute.
    Unknown,
}

/// Confidence in the provider facts shown in a plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanConfidence {
    /// The provider supplied authoritative, exact facts.
    High,
    /// The provider supplied useful facts with a known limitation.
    Medium,
    /// Important facts remain unknown or could not be verified.
    Low,
}

/// Whether an operation needs narrow administrator authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivilegeRequirement {
    /// The selected provider scope is user-local.
    None,
    /// The selected provider scope changes machine state.
    Administrator,
}

/// Conservative risk classification shown before confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// A normal single-package install.
    Normal,
    /// A removal or package with system-level context.
    Caution,
    /// The provider reports additional removals or a high-impact package.
    HighImpact,
    /// Planning is insufficient or a safety invariant failed.
    Blocked,
}

impl RiskLevel {
    /// Human-readable risk label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Caution => "caution",
            Self::HighImpact => "high impact",
            Self::Blocked => "blocked",
        }
    }
}

/// A normalized change within one provider transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedChange {
    /// Change kind reported by the provider.
    pub kind: ChangeKind,
    /// Exact provider package identifier.
    pub package_id: String,
    /// Display name when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Version involved in the change, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why the package appears, when it is not the requested target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A normalized provider change type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A package will be installed.
    Install,
    /// A package will be removed.
    Remove,
    /// An installed package will be configured.
    Configure,
}

/// A warning or limitation that must be visible before confirmation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanWarning {
    /// Warning severity.
    pub level: WarningLevel,
    /// Human-readable warning.
    pub message: String,
}

/// Severity of a plan warning.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WarningLevel {
    /// Context that does not prevent execution.
    Info,
    /// A meaningful limitation or caution.
    Caution,
    /// The plan cannot be safely executed.
    Blocked,
}

/// The exact operation a provider is allowed to execute.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum ProviderOperation {
    /// APT install/remove through apt-get semantics.
    Apt {
        /// Requested action.
        action: OperationAction,
        /// Validated exact Debian package name.
        package_id: String,
    },
    /// Flatpak install/uninstall in an explicit scope.
    Flatpak {
        /// Requested action.
        action: OperationAction,
        /// Validated application or runtime ID.
        package_id: String,
        /// Explicit Flatpak scope.
        scope: InstallScope,
        /// Exact remote for installs when known.
        remote: Option<String>,
    },
    /// Snap install/remove with an optional explicit channel.
    Snap {
        /// Requested action.
        action: OperationAction,
        /// Validated exact snap name.
        package_id: String,
        /// Optional validated channel.
        channel: Option<String>,
    },
    /// A provider-specific maintenance command produced by the maintenance planner.
    Maintenance {
        /// Closed, validated maintenance operation.
        operation: MaintenanceOperation,
    },
}

/// The closed set of provider-wide maintenance commands Orbis may execute.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MaintenanceOperation {
    /// Refresh APT repository indexes without installing packages.
    AptRefresh,
    /// Apply the ordinary APT upgrade without removals.
    AptUpgrade,
    /// Remove APT packages selected by an autoremove simulation.
    AptAutoremove,
    /// Refresh Flatpak AppStream metadata in one installation scope.
    FlatpakAppstream {
        /// Flatpak installation scope.
        scope: InstallScope,
    },
    /// Update an exact set of Flatpak refs in one installation scope.
    FlatpakUpgrade {
        /// Flatpak installation scope.
        scope: InstallScope,
        /// Validated application or runtime refs.
        refs: Vec<String>,
    },
    /// Query Snap's pending refresh list without changing state.
    SnapRefreshCheck,
    /// Refresh an exact set of Snap names.
    SnapUpgrade {
        /// Validated Snap names.
        package_ids: Vec<String>,
    },
}

impl MaintenanceOperation {
    /// Returns the provider represented by this operation.
    pub const fn source(&self) -> PackageSource {
        match self {
            Self::AptRefresh | Self::AptUpgrade | Self::AptAutoremove => PackageSource::Apt,
            Self::FlatpakAppstream { .. } | Self::FlatpakUpgrade { .. } => PackageSource::Flatpak,
            Self::SnapRefreshCheck | Self::SnapUpgrade { .. } => PackageSource::Snap,
        }
    }
}

impl ProviderOperation {
    /// The normalized source represented by this operation.
    pub const fn source(&self) -> PackageSource {
        match self {
            Self::Apt { .. } => PackageSource::Apt,
            Self::Flatpak { .. } => PackageSource::Flatpak,
            Self::Snap { .. } => PackageSource::Snap,
            Self::Maintenance { operation } => operation.source(),
        }
    }

    /// The operation action represented by this operation.
    pub const fn action(&self) -> OperationAction {
        match self {
            Self::Apt { action, .. } | Self::Flatpak { action, .. } | Self::Snap { action, .. } => {
                *action
            }
            // Maintenance has its own action enum. This method remains for the Milestone 2
            // transaction renderer and is not used to classify maintenance history.
            Self::Maintenance { .. } => OperationAction::Install,
        }
    }
}

/// A fully resolved, provider-produced transaction plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationPlan {
    /// Stable ID used to connect the plan, execution result, and history record.
    pub operation_id: String,
    /// Requested action.
    pub action: OperationAction,
    /// Exact normalized target.
    pub target: Package,
    /// Exact scope selected for this operation.
    pub scope: InstallScope,
    /// State observed before planning.
    pub current_state: PackageState,
    /// State requested by the user.
    pub requested_state: PackageState,
    /// Normalized provider impact.
    pub changes: Vec<PlannedChange>,
    /// Download size when the provider exposes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_size_bytes: Option<u64>,
    /// Net disk change when the provider exposes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_delta_bytes: Option<i64>,
    /// Required authorization boundary.
    pub privilege: PrivilegeRequirement,
    /// Provider plan quality.
    pub completeness: PlanCompleteness,
    /// Confidence in the facts shown by the provider.
    pub confidence: PlanConfidence,
    /// Whether the provider supplied an authoritative simulation.
    pub authoritative_simulation: bool,
    /// Conservative safety classification.
    pub risk: RiskLevel,
    /// Limitations and cautions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PlanWarning>,
}

impl OperationPlan {
    /// Creates a plan with a fresh local operation ID.
    pub fn new(action: OperationAction, target: Package, scope: InstallScope) -> Self {
        let current_state = match target.installed {
            Some(true) => PackageState::Installed,
            Some(false) => PackageState::NotInstalled,
            None => PackageState::Unknown,
        };
        let requested_state = match action {
            OperationAction::Install => PackageState::Installed,
            OperationAction::Remove => PackageState::NotInstalled,
        };
        Self {
            operation_id: next_operation_id(),
            action,
            target,
            scope,
            current_state,
            requested_state,
            changes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Unknown,
            confidence: PlanConfidence::Low,
            authoritative_simulation: false,
            risk: RiskLevel::Blocked,
            warnings: Vec::new(),
        }
    }

    /// Whether the plan may cross the confirmation boundary.
    pub fn executable(&self) -> bool {
        self.risk != RiskLevel::Blocked && self.completeness != PlanCompleteness::Unknown
    }
}

/// A transaction request before provider resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationRequest {
    /// Requested mutation.
    pub action: OperationAction,
    /// User-supplied provider-qualified or unqualified package reference.
    pub package: PackageRefJson,
    /// Optional exact Flatpak scope.
    pub scope: Option<InstallScope>,
    /// Optional exact Snap channel.
    pub channel: Option<String>,
}

/// Serializable form of [`PackageRef`] used in records and JSON output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageRefJson {
    /// Explicit source, if supplied.
    pub source: Option<PackageSource>,
    /// User query.
    pub query: String,
}

impl From<&PackageRef> for PackageRefJson {
    fn from(value: &PackageRef) -> Self {
        Self { source: value.source, query: value.query.clone() }
    }
}

/// Verification state after an attempted provider operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationResult {
    /// The exact requested state was observed.
    Verified,
    /// The provider completed but exact post-state could not be fully established.
    PartiallyVerified,
    /// The exact requested state was not observed.
    Failed,
}

/// High-level result of one attempted transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStatus {
    /// Process succeeded and the target state was verified.
    Succeeded,
    /// Process succeeded but verification was incomplete.
    PartiallyVerified,
    /// Process or verification failed.
    Failed,
}

/// Sanitized process facts retained for the caller and transaction record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSummary {
    /// Provider command exit code.
    pub exit_status: Option<i32>,
    /// Whether the provider command exited successfully.
    pub process_succeeded: bool,
    /// First useful provider error, with no raw command line or full dump.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Complete result returned after execution and verification.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransactionResult {
    /// The resolved plan that was executed.
    pub plan: OperationPlan,
    /// Process facts.
    pub execution: ExecutionSummary,
    /// Post-execution target-state check.
    pub verification: VerificationResult,
    /// Final transaction status.
    pub status: TransactionStatus,
}

/// Transaction failures that prevent or invalidate an operation.
#[derive(Debug, Error)]
pub enum TransactionError {
    /// The user request violates a safety or input invariant.
    #[error("invalid transaction request: {0}")]
    InvalidRequest(String),
    /// No safe provider result exists.
    #[error("no supported package matched `{query}`")]
    NotFound {
        /// User query that was not found.
        query: String,
    },
    /// Multiple providers matched and must be disambiguated.
    #[error("`{query}` matches multiple providers; specify a source")]
    Ambiguous {
        /// User query with multiple matches.
        query: String,
        /// All normalized provider matches.
        matches: Vec<Package>,
    },
    /// A selected provider could not answer.
    #[error("{0}")]
    Provider(#[from] ProviderError),
    /// The provider could not produce an executable plan.
    #[error("could not plan the transaction: {0}")]
    Planning(String),
    /// The plan is deliberately blocked.
    #[error("transaction blocked: {0}")]
    Blocked(String),
    /// Authorization or execution failed.
    #[error("transaction execution failed: {0}")]
    Execution(String),
    /// Post-execution verification failed.
    #[error("transaction verification failed: {0}")]
    Verification(String),
    /// The durable transaction record could not be written.
    #[error("could not record transaction: {0}")]
    History(String),
}

impl From<crate::privilege::PrivilegeError> for TransactionError {
    fn from(value: crate::privilege::PrivilegeError) -> Self {
        Self::Execution(value.to_string())
    }
}

/// A provider-neutral execution seam, implemented by the privilege layer.
pub trait OperationExecutor: Send + Sync {
    /// Executes only the already validated typed provider operation.
    fn execute(
        &self,
        operation: &ProviderOperation,
        requirement: PrivilegeRequirement,
    ) -> Result<CommandOutput, crate::privilege::PrivilegeError>;
}

/// Produces a unique local operation identifier without a dependency on wall-clock formatting.
fn next_operation_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    format!("tx-{millis}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Converts a provider error into a safe first-line message for records.
pub fn safe_process_message(output: &CommandOutput) -> Option<String> {
    output
        .stderr
        .lines()
        .chain(output.stdout.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(300).collect())
}

/// Creates the requested package change entry.
pub fn target_change(plan: &OperationPlan) -> PlannedChange {
    PlannedChange {
        kind: match plan.action {
            OperationAction::Install => ChangeKind::Install,
            OperationAction::Remove => ChangeKind::Remove,
        },
        package_id: plan.target.provider_id.clone(),
        name: Some(plan.target.name.clone()),
        version: plan.target.version.clone(),
        reason: None,
    }
}

/// Infers conservative risk from a package classification and normalized changes.
pub fn base_risk(action: OperationAction, kind: Option<PackageKind>) -> RiskLevel {
    match action {
        OperationAction::Install => RiskLevel::Normal,
        OperationAction::Remove => match kind {
            Some(PackageKind::Service | PackageKind::Runtime | PackageKind::Library) => {
                RiskLevel::HighImpact
            }
            _ => RiskLevel::Caution,
        },
    }
}

/// Parses a provider size such as `1.5 MB` into bytes.
pub fn parse_human_size(value: &str) -> Option<u64> {
    let mut pieces = value.split_whitespace();
    let number = pieces.next()?.replace(',', ".").parse::<f64>().ok()?;
    let unit = pieces.next().unwrap_or("B").to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "b" => 1.0,
        "kb" | "kib" => 1_000.0,
        "mb" | "mib" => 1_000_000.0,
        "gb" | "gib" => 1_000_000_000.0,
        "tb" | "tib" => 1_000_000_000_000.0,
        _ => return None,
    };
    (number >= 0.0).then_some((number * multiplier).round() as u64)
}

/// Validates user package input before any provider is queried for a mutation.
pub fn validate_query(query: &str) -> Result<(), TransactionError> {
    if query.is_empty() || query.len() > 256 {
        return Err(TransactionError::InvalidRequest(
            "package references must contain 1–256 characters".into(),
        ));
    }
    if query.chars().any(|character| character.is_control() || character.is_whitespace()) {
        return Err(TransactionError::InvalidRequest(
            "package references cannot contain whitespace or control characters".into(),
        ));
    }
    if query.starts_with('-')
        || query.contains("..")
        || query
            .chars()
            .any(|character| matches!(character, ';' | '&' | '|' | '$' | '`' | '\'' | '"'))
    {
        return Err(TransactionError::InvalidRequest(
            "package references contain unsupported shell-like or path characters".into(),
        ));
    }
    Ok(())
}

impl fmt::Display for PackageRefJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.source {
            Some(source) => {
                write!(formatter, "{}:{}", source.label().to_ascii_lowercase(), self.query)
            }
            None => formatter.write_str(&self.query),
        }
    }
}
