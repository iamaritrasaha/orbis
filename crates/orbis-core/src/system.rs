//! Actionable system insight for Orbis — not a generic dashboard of metrics.
//!
//! Prefer signals that can lead to an action: pending updates, reboot required,
//! broken package state, failed services, disk pressure.

use std::{path::Path, sync::Arc, time::Duration};

use serde::Serialize;

use crate::apt_ops::{
    AptOutcomeProbe, DependencyHealth, RebootPaths, RebootRequired, read_reboot_required,
};
use crate::models::PackageSource;
use crate::operation::{OperationJournal, OperationRecord, OperationStatus};
use crate::process::{CommandRunner, CommandSpec, SharedRunner};
use crate::providers::{execute, short_timeout};

/// One actionable attention item for the home/launcher surface.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AttentionItem {
    /// Short label shown to the user.
    pub title: String,
    /// Optional supporting detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Suggested next Orbis action when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Severity for presentation.
    pub severity: AttentionSeverity,
}

/// Severity of an attention item.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSeverity {
    /// Informational; useful but not urgent.
    Info,
    /// Needs attention soon.
    Caution,
    /// Blocking or failed state.
    Critical,
}

/// Compact system insight used by home / health surfaces.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SystemInsight {
    /// Kernel release string when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// Pending APT upgrades when the inventory was gathered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_updates: Option<usize>,
    /// Reboot-required observation.
    pub reboot: RebootRequiredView,
    /// Broken dependency observation.
    pub dependencies: DependencyHealthView,
    /// Failed systemd units (subset).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_units: Vec<String>,
    /// Disk pressure warnings for important mounts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disk_pressure: Vec<DiskPressure>,
    /// Held package names when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_packages: Vec<String>,
    /// Derived attention items ready for UI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention: Vec<AttentionItem>,
}

/// Serializable reboot view.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RebootRequiredView {
    /// Whether reboot is required.
    pub required: bool,
    /// Packages that triggered it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}

impl From<RebootRequired> for RebootRequiredView {
    fn from(value: RebootRequired) -> Self {
        Self { required: value.required, packages: value.packages }
    }
}

/// Serializable dependency health view.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DependencyHealthView {
    /// True when broken.
    pub broken: bool,
    /// Summary when broken.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl From<DependencyHealth> for DependencyHealthView {
    fn from(value: DependencyHealth) -> Self {
        Self { broken: value.broken, summary: value.summary }
    }
}

/// Disk usage pressure for one mount.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiskPressure {
    /// Mount point.
    pub mount: String,
    /// Used percentage 0–100 when known.
    pub used_percent: u8,
}

/// Inputs for building system insight. Heavy inventory is optional so the
/// launcher can stay instant while `health`/`update` pass richer data.
#[derive(Clone, Debug, Default)]
pub struct InsightRequest {
    /// Pending update count from a prior inventory, when known.
    pub pending_updates: Option<usize>,
    /// Whether to probe failed systemd units (read-only).
    pub include_services: bool,
    /// Whether to probe disk pressure.
    pub include_disk: bool,
    /// Whether to run apt-get check for broken dependencies.
    pub include_dependencies: bool,
    /// Whether to read held packages.
    pub include_holds: bool,
    /// Recent operation records to fold into attention (failures / reboot).
    pub recent_operations: Vec<OperationRecord>,
}

/// Collects actionable system insight using an injected runner and reboot paths.
pub fn collect_insight(
    runner: &SharedRunner,
    reboot_paths: &RebootPaths,
    request: InsightRequest,
) -> SystemInsight {
    let probe = AptOutcomeProbe::with_reboot_paths(runner.clone(), reboot_paths.clone());
    let reboot = probe.reboot_required();
    let dependencies = if request.include_dependencies {
        probe.dependency_health()
    } else {
        DependencyHealth::default()
    };
    let kernel = read_kernel(runner);
    let failed_units =
        if request.include_services { read_failed_units(runner) } else { Vec::new() };
    let disk_pressure = if request.include_disk { read_disk_pressure(runner) } else { Vec::new() };
    let held_packages = if request.include_holds { read_held_packages(runner) } else { Vec::new() };

    let mut insight = SystemInsight {
        kernel,
        pending_updates: request.pending_updates,
        reboot: reboot.into(),
        dependencies: dependencies.into(),
        failed_units,
        disk_pressure,
        held_packages,
        attention: Vec::new(),
    };
    insight.attention = build_attention(&insight, &request.recent_operations);
    insight
}

/// Fast insight for launcher: reboot flag + recent journal only (no apt/systemctl).
pub fn launcher_insight(recent: &[OperationRecord]) -> SystemInsight {
    let reboot = read_reboot_required(&RebootPaths::system_default());
    let mut insight =
        SystemInsight { reboot: reboot.into(), attention: Vec::new(), ..SystemInsight::default() };
    insight.attention = build_attention(&insight, recent);
    insight
}

/// Full read-only insight for `orbis health` / home emphasis.
pub fn system_insight(
    runner: Arc<dyn CommandRunner>,
    pending_updates: Option<usize>,
) -> SystemInsight {
    let shared: SharedRunner = runner;
    let recent = OperationJournal::default_location()
        .ok()
        .and_then(|journal| journal.recent(8).ok())
        .map(|(records, _)| records)
        .unwrap_or_default();
    collect_insight(
        &shared,
        &RebootPaths::system_default(),
        InsightRequest {
            pending_updates,
            include_services: true,
            include_disk: true,
            include_dependencies: true,
            include_holds: true,
            recent_operations: recent,
        },
    )
}

fn build_attention(insight: &SystemInsight, recent: &[OperationRecord]) -> Vec<AttentionItem> {
    let mut items = Vec::new();
    if let Some(count) = insight.pending_updates.filter(|count| *count > 0) {
        items.push(AttentionItem {
            title: format!("{count} update{} available", if count == 1 { "" } else { "s" }),
            detail: None,
            action: Some("orbis update --apply".into()),
            severity: AttentionSeverity::Info,
        });
    }
    if insight.reboot.required {
        let detail = if insight.reboot.packages.is_empty() {
            None
        } else {
            Some(format!(
                "Triggered by {}",
                insight.reboot.packages.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ))
        };
        items.push(AttentionItem {
            title: "Reboot required".into(),
            detail,
            action: None,
            severity: AttentionSeverity::Caution,
        });
    }
    if insight.dependencies.broken {
        items.push(AttentionItem {
            title: "Broken package dependencies".into(),
            detail: insight.dependencies.summary.clone(),
            action: Some("orbis health".into()),
            severity: AttentionSeverity::Critical,
        });
    }
    if !insight.failed_units.is_empty() {
        let preview = insight.failed_units.iter().take(2).cloned().collect::<Vec<_>>().join(", ");
        items.push(AttentionItem {
            title: format!(
                "{} failed systemd unit{}",
                insight.failed_units.len(),
                if insight.failed_units.len() == 1 { "" } else { "s" }
            ),
            detail: Some(preview),
            action: None,
            severity: AttentionSeverity::Critical,
        });
    }
    for disk in &insight.disk_pressure {
        items.push(AttentionItem {
            title: format!("Disk pressure on {}", disk.mount),
            detail: Some(format!("{}% used", disk.used_percent)),
            action: None,
            severity: if disk.used_percent >= 95 {
                AttentionSeverity::Critical
            } else {
                AttentionSeverity::Caution
            },
        });
    }
    if !insight.held_packages.is_empty() {
        items.push(AttentionItem {
            title: format!(
                "{} held package{}",
                insight.held_packages.len(),
                if insight.held_packages.len() == 1 { "" } else { "s" }
            ),
            detail: Some(
                insight.held_packages.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
            ),
            action: None,
            severity: AttentionSeverity::Info,
        });
    }
    for record in recent.iter().filter(|record| record.status == OperationStatus::Failed).take(2) {
        items.push(AttentionItem {
            title: record.activity_title(),
            detail: record.diagnosis.as_ref().map(|d| d.summary.clone()).or(record.message.clone()),
            action: Some("orbis history".into()),
            severity: AttentionSeverity::Critical,
        });
    }
    items
}

fn read_kernel(runner: &SharedRunner) -> Option<String> {
    if !runner.is_available("uname") {
        return None;
    }
    let output = execute(
        runner,
        PackageSource::Apt,
        "read kernel release",
        CommandSpec::new("uname", ["-r"]).with_timeout(Duration::from_secs(5)),
    )
    .ok()?;
    if !output.success() {
        return None;
    }
    let release = output.stdout.trim();
    (!release.is_empty()).then(|| release.to_owned())
}

fn read_failed_units(runner: &SharedRunner) -> Vec<String> {
    if !runner.is_available("systemctl") {
        return Vec::new();
    }
    let Ok(output) = execute(
        runner,
        PackageSource::Apt,
        "list failed systemd units",
        CommandSpec::new("systemctl", ["--failed", "--no-legend", "--no-pager", "--plain"])
            .with_timeout(short_timeout()),
    ) else {
        return Vec::new();
    };
    if !output.success() {
        return Vec::new();
    }
    output
        .stdout
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?.trim();
            (!name.is_empty()).then(|| name.to_owned())
        })
        .take(8)
        .collect()
}

fn read_disk_pressure(runner: &SharedRunner) -> Vec<DiskPressure> {
    if !runner.is_available("df") {
        return Vec::new();
    }
    let Ok(output) = execute(
        runner,
        PackageSource::Apt,
        "read filesystem usage",
        CommandSpec::new("df", ["-P", "-x", "tmpfs", "-x", "devtmpfs"])
            .with_timeout(short_timeout()),
    ) else {
        return Vec::new();
    };
    if !output.success() {
        return Vec::new();
    }
    parse_df_pressure(&output.stdout)
}

/// Pure parser for POSIX `df -P` output; only surfaces mounts at or above 85%.
pub fn parse_df_pressure(stdout: &str) -> Vec<DiskPressure> {
    let mut pressures = Vec::new();
    for line in stdout.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let mount = fields[5];
        if !matches!(mount, "/" | "/home" | "/var" | "/boot") {
            continue;
        }
        let Some(percent) = fields[4].trim_end_matches('%').parse::<u8>().ok() else {
            continue;
        };
        if percent >= 85 {
            pressures.push(DiskPressure { mount: mount.to_owned(), used_percent: percent });
        }
    }
    pressures
}

fn read_held_packages(runner: &SharedRunner) -> Vec<String> {
    if !runner.is_available("apt-mark") {
        return Vec::new();
    }
    let Ok(output) = execute(
        runner,
        PackageSource::Apt,
        "list held packages",
        CommandSpec::new("apt-mark", ["showhold"]).with_timeout(short_timeout()),
    ) else {
        return Vec::new();
    };
    if !output.success() {
        return Vec::new();
    }
    output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .take(20)
        .collect()
}

/// Convenience: recent activity from the default journal, empty if unavailable.
pub fn recent_activity(limit: usize) -> Vec<OperationRecord> {
    OperationJournal::default_location()
        .ok()
        .and_then(|journal| journal.recent(limit).ok())
        .map(|(records, _)| records)
        .unwrap_or_default()
}

/// True when the path looks like a reboot-required flag file (tests / adapters).
pub fn reboot_flag_present(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{OperationType, running};

    #[test]
    fn df_parser_only_flags_important_full_mounts() {
        let stdout = "\
Filesystem     1024-blocks      Used Available Capacity Mounted on
/dev/sda1         1000000    900000    100000      90% /
/dev/sdb1         1000000    500000    500000      50% /data
tmpfs              100000     90000     10000      90% /tmp
/dev/sdc1         1000000    960000     40000      96% /home
";
        let pressure = parse_df_pressure(stdout);
        assert_eq!(pressure.len(), 2);
        assert_eq!(pressure[0].mount, "/");
        assert_eq!(pressure[0].used_percent, 90);
        assert_eq!(pressure[1].mount, "/home");
    }

    #[test]
    fn attention_includes_updates_reboot_and_failed_ops() {
        let mut failed =
            running("tx-fail", OperationType::Install, "Install x", Some(PackageSource::Apt));
        failed.status = OperationStatus::Failed;
        failed.summary = "failed".into();
        failed.message = Some("lock held".into());
        let insight = SystemInsight {
            pending_updates: Some(12),
            reboot: RebootRequiredView { required: true, packages: vec!["linux-image".into()] },
            ..SystemInsight::default()
        };
        let attention = build_attention(&insight, &[failed]);
        assert!(attention.iter().any(|item| item.title.contains("12 update")));
        assert!(attention.iter().any(|item| item.title.contains("Reboot required")));
        assert!(attention.iter().any(|item| item.title.contains("failed")));
    }
}
