//! Provider-neutral maintenance planning and explanation models.

#![allow(missing_docs)]

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    models::{Package, PackageSource, ProviderIssue},
    providers::ProviderError,
    transaction::{
        InstallScope, MaintenanceOperation, PlanCompleteness, PlanConfidence, PlanWarning,
        PrivilegeRequirement, ProviderOperation, RiskLevel, TransactionStatus, VerificationResult,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaintenanceAction {
    Refresh,
    Upgrade,
    Cleanup,
}

impl MaintenanceAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Refresh => "Refresh",
            Self::Upgrade => "Update",
            Self::Cleanup => "Clean up",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateCandidate {
    pub source: PackageSource,
    pub provider_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstallScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_relevance: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl UpdateCandidate {
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.source.label().to_ascii_lowercase(),
            self.scope.map_or("", |scope| scope.label()),
            self.provider_id
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderUpdateInventory {
    pub source: PackageSource,
    pub available: bool,
    #[serde(default)]
    pub candidates: Vec<UpdateCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_state: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateInventoryReport {
    pub inventories: Vec<ProviderUpdateInventory>,
    pub candidates: Vec<UpdateCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<ProviderIssue>,
}

impl UpdateInventoryReport {
    pub fn total(&self) -> usize {
        self.candidates.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanupCandidate {
    pub source: PackageSource,
    pub provider_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstallScope>,
    pub reason: String,
    pub risk: RiskLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderMaintenancePlan {
    pub operation_id: String,
    pub source: PackageSource,
    pub action: MaintenanceAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstallScope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<UpdateCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_candidates: Vec<CleanupCandidate>,
    pub privilege: PrivilegeRequirement,
    pub completeness: PlanCompleteness,
    pub confidence: PlanConfidence,
    pub authoritative_simulation: bool,
    pub risk: RiskLevel,
    pub supported: bool,
    pub mutates: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PlanWarning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_delta_bytes: Option<i64>,
}

impl ProviderMaintenancePlan {
    pub fn executable(&self) -> bool {
        self.supported
            && self.risk != RiskLevel::Blocked
            && self.completeness != PlanCompleteness::Unknown
            && (self.action != MaintenanceAction::Upgrade || !self.candidates.is_empty())
    }

    pub fn blocked(
        source: PackageSource,
        action: MaintenanceAction,
        message: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: next_maintenance_id(),
            source,
            action,
            scope: None,
            candidates: Vec::new(),
            cleanup_candidates: Vec::new(),
            privilege: PrivilegeRequirement::None,
            completeness: PlanCompleteness::Unknown,
            confidence: PlanConfidence::Low,
            authoritative_simulation: false,
            risk: RiskLevel::Blocked,
            supported: false,
            mutates: false,
            warnings: vec![PlanWarning {
                level: crate::transaction::WarningLevel::Blocked,
                message: message.into(),
            }],
            notes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MaintenancePlan {
    pub operation_id: String,
    pub action: MaintenanceAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PackageSource>,
    pub providers: Vec<ProviderMaintenancePlan>,
    pub risk: RiskLevel,
    pub completeness: PlanCompleteness,
    pub privilege: PrivilegeRequirement,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PlanWarning>,
    pub mutates: bool,
}

impl MaintenancePlan {
    pub fn new(
        action: MaintenanceAction,
        source: Option<PackageSource>,
        providers: Vec<ProviderMaintenancePlan>,
    ) -> Self {
        // Confirmation risk describes only mutations that can cross the
        // execution boundary. An unavailable optional provider belongs in
        // completeness/coverage, not in the risk of executable work.
        let risk = providers
            .iter()
            .filter(|plan| plan.mutates && plan.executable())
            .map(|plan| plan.risk)
            .max()
            .or_else(|| {
                providers
                    .iter()
                    .any(|plan| plan.risk == RiskLevel::Blocked && !plan.executable())
                    .then_some(RiskLevel::Blocked)
            })
            .unwrap_or(RiskLevel::Normal);
        let completeness =
            if providers.iter().any(|plan| plan.completeness == PlanCompleteness::Unknown) {
                PlanCompleteness::Unknown
            } else if providers.iter().any(|plan| plan.completeness == PlanCompleteness::Partial) {
                PlanCompleteness::Partial
            } else {
                PlanCompleteness::Complete
            };
        let privilege =
            if providers.iter().any(|plan| plan.privilege == PrivilegeRequirement::Administrator) {
                PrivilegeRequirement::Administrator
            } else {
                PrivilegeRequirement::None
            };
        let mutates = providers.iter().any(|plan| plan.mutates && plan.executable());
        Self {
            operation_id: next_maintenance_id(),
            action,
            source,
            providers,
            risk,
            completeness,
            privilege,
            warnings: Vec::new(),
            mutates,
        }
    }

    pub fn executable_providers(&self) -> impl Iterator<Item = &ProviderMaintenancePlan> {
        self.providers.iter().filter(|plan| plan.executable())
    }

    pub fn executable(&self) -> bool {
        self.executable_providers().next().is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceProviderStatus {
    Succeeded,
    PartiallySucceeded,
    Failed,
    Skipped,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceProviderResult {
    pub source: PackageSource,
    pub action: MaintenanceAction,
    pub status: MaintenanceProviderStatus,
    pub candidate_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceStatus {
    Succeeded,
    PartiallySucceeded,
    Failed,
    Cancelled,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceResult {
    pub operation_id: String,
    pub action: MaintenanceAction,
    pub status: MaintenanceStatus,
    pub providers: Vec<MaintenanceProviderResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhyConsumer {
    pub source: PackageSource,
    pub provider_id: String,
    pub name: String,
    pub relationship: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstallScope>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhyReport {
    pub package: Package,
    pub installed_as: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<WhyConsumer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    pub removal_advice: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orbis_history: Vec<WhyHistoryEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhyHistoryEntry {
    pub operation_id: String,
    pub action: String,
    pub recorded_at_unix_ms: u64,
}

/// Read-only and mutating maintenance capabilities implemented inside providers.
pub trait MaintenanceProvider: crate::providers::Provider {
    fn update_inventory(&self) -> Result<ProviderUpdateInventory, ProviderError>;
    fn refresh_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError>;
    fn upgrade_plan(&self) -> Result<Vec<ProviderMaintenancePlan>, ProviderError>;
    fn cleanup_plan(&self) -> Result<ProviderMaintenancePlan, ProviderError>;
    fn why(&self, package: &Package) -> Result<WhyReport, ProviderError>;
    fn maintenance_operation(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<ProviderOperation, ProviderError>;
    fn verify_maintenance(
        &self,
        plan: &ProviderMaintenancePlan,
    ) -> Result<VerificationResult, ProviderError>;
}

pub fn aggregate_inventory(
    inventories: Vec<ProviderUpdateInventory>,
    issues: Vec<ProviderIssue>,
) -> UpdateInventoryReport {
    let mut candidates: Vec<_> =
        inventories.iter().flat_map(|inventory| inventory.candidates.clone()).collect();
    candidates.sort_by(|left, right| {
        (left.source, left.name.to_ascii_lowercase(), left.provider_id.clone()).cmp(&(
            right.source,
            right.name.to_ascii_lowercase(),
            right.provider_id.clone(),
        ))
    });
    UpdateInventoryReport { inventories, candidates, issues }
}

pub fn maintenance_operation_for(plan: &ProviderMaintenancePlan) -> Option<MaintenanceOperation> {
    match (plan.source, plan.action, plan.scope) {
        (PackageSource::Apt, MaintenanceAction::Refresh, _) => {
            Some(MaintenanceOperation::AptRefresh)
        }
        (PackageSource::Apt, MaintenanceAction::Upgrade, _) => {
            Some(MaintenanceOperation::AptUpgrade)
        }
        (PackageSource::Apt, MaintenanceAction::Cleanup, _) => {
            Some(MaintenanceOperation::AptAutoremove)
        }
        (PackageSource::Flatpak, MaintenanceAction::Refresh, Some(scope)) => {
            Some(MaintenanceOperation::FlatpakAppstream { scope })
        }
        (PackageSource::Flatpak, MaintenanceAction::Upgrade, Some(scope)) => {
            Some(MaintenanceOperation::FlatpakUpgrade {
                scope,
                refs: plan
                    .candidates
                    .iter()
                    .map(|candidate| candidate.provider_id.clone())
                    .collect(),
            })
        }
        (PackageSource::Snap, MaintenanceAction::Refresh, _) => {
            Some(MaintenanceOperation::SnapRefreshCheck)
        }
        (PackageSource::Snap, MaintenanceAction::Upgrade, _) => {
            Some(MaintenanceOperation::SnapUpgrade {
                package_ids: plan
                    .candidates
                    .iter()
                    .map(|candidate| candidate.provider_id.clone())
                    .collect(),
            })
        }
        (PackageSource::Npm, MaintenanceAction::Upgrade, _) => {
            Some(MaintenanceOperation::NpmUpgrade {
                package_ids: plan
                    .candidates
                    .iter()
                    .filter_map(|candidate| {
                        candidate
                            .available_version
                            .as_ref()
                            .map(|version| format!("{}@{}", candidate.provider_id, version))
                    })
                    .collect(),
            })
        }
        (PackageSource::Pnpm, MaintenanceAction::Upgrade, _) => {
            Some(MaintenanceOperation::PnpmUpgrade {
                package_ids: plan
                    .candidates
                    .iter()
                    .map(|candidate| candidate.provider_id.clone())
                    .collect(),
            })
        }
        (PackageSource::Uv, MaintenanceAction::Upgrade, _) => {
            Some(MaintenanceOperation::UvUpgrade {
                package_ids: plan
                    .candidates
                    .iter()
                    .map(|candidate| candidate.provider_id.clone())
                    .collect(),
            })
        }
        (PackageSource::Pipx, MaintenanceAction::Upgrade, _) => {
            Some(MaintenanceOperation::PipxUpgrade {
                package_ids: plan
                    .candidates
                    .iter()
                    .map(|candidate| candidate.provider_id.clone())
                    .collect(),
            })
        }
        _ => None,
    }
}

fn next_maintenance_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    format!("maint-{millis}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

impl From<MaintenanceStatus> for TransactionStatus {
    fn from(value: MaintenanceStatus) -> Self {
        match value {
            MaintenanceStatus::Succeeded => Self::Succeeded,
            MaintenanceStatus::PartiallySucceeded => Self::PartiallyVerified,
            MaintenanceStatus::Failed
            | MaintenanceStatus::Cancelled
            | MaintenanceStatus::Blocked => Self::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(source: PackageSource, scope: Option<InstallScope>, id: &str) -> UpdateCandidate {
        UpdateCandidate {
            source,
            provider_id: id.into(),
            name: id.into(),
            current_version: Some("1".into()),
            available_version: Some("2".into()),
            architecture: None,
            scope,
            channel: None,
            held: Some(false),
            security_relevance: None,
            notes: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn inventory_aggregation_preserves_provider_and_scope_identity() {
        let report = aggregate_inventory(
            vec![
                ProviderUpdateInventory {
                    source: PackageSource::Flatpak,
                    available: true,
                    candidates: vec![candidate(
                        PackageSource::Flatpak,
                        Some(InstallScope::System),
                        "org.example.App",
                    )],
                    notes: Vec::new(),
                    metadata_state: None,
                },
                ProviderUpdateInventory {
                    source: PackageSource::Flatpak,
                    available: true,
                    candidates: vec![candidate(
                        PackageSource::Flatpak,
                        Some(InstallScope::User),
                        "org.example.App",
                    )],
                    notes: Vec::new(),
                    metadata_state: None,
                },
            ],
            Vec::new(),
        );
        assert_eq!(report.total(), 2);
        assert_ne!(report.candidates[0].key(), report.candidates[1].key());
    }

    #[test]
    fn blocked_plan_cannot_cross_execution_boundary() {
        let plan = ProviderMaintenancePlan::blocked(
            PackageSource::Flatpak,
            MaintenanceAction::Upgrade,
            "partial provider plan",
        );
        assert!(!plan.executable());
        assert_eq!(plan.completeness, PlanCompleteness::Unknown);
    }

    fn executable_plan(source: PackageSource, risk: RiskLevel) -> ProviderMaintenancePlan {
        ProviderMaintenancePlan {
            operation_id: format!("maint-{source:?}"),
            source,
            action: MaintenanceAction::Upgrade,
            scope: None,
            candidates: vec![candidate(source, None, "example")],
            cleanup_candidates: Vec::new(),
            privilege: PrivilegeRequirement::None,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: true,
            risk,
            supported: true,
            mutates: true,
            warnings: Vec::new(),
            notes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
        }
    }

    #[test]
    fn execution_risk_ignores_blocked_optional_provider() {
        let blocked = ProviderMaintenancePlan::blocked(
            PackageSource::Pipx,
            MaintenanceAction::Upgrade,
            "pipx is unavailable",
        );
        let normal = MaintenancePlan::new(
            MaintenanceAction::Upgrade,
            None,
            vec![executable_plan(PackageSource::Snap, RiskLevel::Normal), blocked.clone()],
        );
        let caution = MaintenancePlan::new(
            MaintenanceAction::Upgrade,
            None,
            vec![executable_plan(PackageSource::Snap, RiskLevel::Caution), blocked.clone()],
        );
        let high = MaintenancePlan::new(
            MaintenanceAction::Upgrade,
            None,
            vec![executable_plan(PackageSource::Snap, RiskLevel::HighImpact), blocked],
        );

        assert_eq!(normal.risk, RiskLevel::Normal);
        assert_eq!(caution.risk, RiskLevel::Caution);
        assert_eq!(high.risk, RiskLevel::HighImpact);
        assert!(normal.executable());
        assert!(caution.executable());
        assert!(high.executable());
    }

    #[test]
    fn all_blocked_plan_is_blocked_and_not_executable() {
        let plan = MaintenancePlan::new(
            MaintenanceAction::Upgrade,
            None,
            vec![ProviderMaintenancePlan::blocked(
                PackageSource::Pipx,
                MaintenanceAction::Upgrade,
                "pipx is unavailable",
            )],
        );

        assert_eq!(plan.risk, RiskLevel::Blocked);
        assert!(!plan.executable());
    }

    #[test]
    fn upgrade_without_candidates_is_not_executable() {
        let mut plan = executable_plan(PackageSource::Snap, RiskLevel::Normal);
        plan.candidates.clear();
        assert!(!plan.executable());
    }

    #[test]
    fn maintenance_operation_never_constructs_full_upgrade() {
        let plan = ProviderMaintenancePlan {
            operation_id: "maint-apt-upgrade-test".into(),
            source: PackageSource::Apt,
            action: MaintenanceAction::Upgrade,
            scope: None,
            candidates: Vec::new(),
            cleanup_candidates: Vec::new(),
            privilege: PrivilegeRequirement::Administrator,
            completeness: PlanCompleteness::Complete,
            confidence: PlanConfidence::High,
            authoritative_simulation: true,
            risk: RiskLevel::Normal,
            supported: true,
            mutates: true,
            warnings: Vec::new(),
            notes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
        };
        assert_eq!(maintenance_operation_for(&plan), Some(MaintenanceOperation::AptUpgrade));
    }

    #[test]
    fn maintenance_operation_preserves_reviewed_developer_targets() {
        let mut plan = executable_plan(PackageSource::Npm, RiskLevel::Normal);
        plan.candidates.push(candidate(PackageSource::Npm, None, "second"));
        plan.candidates[0].available_version = Some("2.1".into());
        plan.candidates[1].available_version = Some("3.4".into());
        assert_eq!(
            maintenance_operation_for(&plan),
            Some(MaintenanceOperation::NpmUpgrade {
                package_ids: vec!["example@2.1".into(), "second@3.4".into()]
            })
        );
        plan.source = PackageSource::Snap;
        assert_eq!(
            maintenance_operation_for(&plan),
            Some(MaintenanceOperation::SnapUpgrade {
                package_ids: vec!["example".into(), "second".into()]
            })
        );
    }
}
