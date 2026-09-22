//! APT failure diagnosis and host observation helpers.
//!
//! Diagnosis is based on provider output patterns, never treated as verification by itself.
//! Observation helpers read authoritative package/system state after mutations.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::facts::{ChangeKind, FailureCause, FailureDiagnosis, PackageChange};
use crate::models::PackageSource;
use crate::process::{CommandOutput, CommandRunner, CommandSpec, ProcessError, SharedRunner};
use crate::providers::{execute, short_timeout};
use crate::transaction::VerificationResult;

/// Paths consulted for reboot-required signals on Debian/Ubuntu hosts.
#[derive(Clone, Debug, Default)]
pub struct RebootPaths {
    /// Typically `/var/run/reboot-required` or `/run/reboot-required`.
    pub flag: PathBuf,
    /// Typically `/var/run/reboot-required.pkgs`.
    pub packages: PathBuf,
}

impl RebootPaths {
    /// Standard Debian/Ubuntu locations under `/run` with `/var/run` fallback.
    pub fn system_default() -> Self {
        let run = if Path::new("/run/reboot-required").exists() {
            PathBuf::from("/run")
        } else {
            PathBuf::from("/var/run")
        };
        Self { flag: run.join("reboot-required"), packages: run.join("reboot-required.pkgs") }
    }
}

/// Reboot-required observation from the host.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RebootRequired {
    /// Whether a reboot flag is present.
    pub required: bool,
    /// Packages listed as triggering the reboot, when available.
    pub packages: Vec<String>,
}

impl RebootRequired {
    /// Warning line for operation records when a reboot is required.
    pub fn warning_line(&self) -> Option<String> {
        if !self.required {
            return None;
        }
        if self.packages.is_empty() {
            Some("Reboot required".into())
        } else {
            let preview: Vec<_> = self.packages.iter().take(3).cloned().collect();
            let extra = self.packages.len().saturating_sub(preview.len());
            let mut message = format!("Reboot required ({})", preview.join(", "));
            if extra > 0 {
                message.push_str(&format!(" +{extra} more"));
            }
            Some(message)
        }
    }
}

/// Reads reboot-required state from the given paths (injectable for tests).
pub fn read_reboot_required(paths: &RebootPaths) -> RebootRequired {
    if !paths.flag.exists() {
        return RebootRequired::default();
    }
    let packages = std::fs::read_to_string(&paths.packages)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    RebootRequired { required: true, packages }
}

/// Diagnose APT/dpkg failure text into a useful cause. Pure and fully unit-tested.
pub fn diagnose_apt_failure(
    stdout: &str,
    stderr: &str,
    exit_status: Option<i32>,
) -> FailureDiagnosis {
    let combined = format!("{stderr}\n{stdout}").to_ascii_lowercase();
    let (cause, summary, hint) = if contains_any(
        &combined,
        &[
            "could not get lock",
            "unable to acquire the dpkg frontend lock",
            "unable to lock the administration directory",
            "is another process using it",
        ],
    ) {
        (
            FailureCause::AptLock,
            "Another package manager holds the APT/dpkg lock.",
            Some("Wait for the other apt/dpkg/packagekit process to finish, then retry."),
        )
    } else if contains_any(
        &combined,
        &["dpkg was interrupted", "dpkg --configure -a", "you need to run 'dpkg --configure -a'"],
    ) {
        (
            FailureCause::InterruptedDpkg,
            "Package database is in an interrupted dpkg state.",
            Some("Finish the interrupted configuration with your package manager before retrying."),
        )
    } else if contains_any(
        &combined,
        &[
            "unmet dependencies",
            "broken packages",
            "depends on",
            "but it is not going to be installed",
            "held broken packages",
        ],
    ) {
        (
            FailureCause::BrokenDependencies,
            "APT reported broken or unmet dependencies.",
            Some("Inspect broken packages with `orbis health` / APT diagnostics before retrying."),
        )
    } else if contains_any(
        &combined,
        &["permission denied", "are you root", "must be root", "not allowed to execute"],
    ) {
        (
            FailureCause::PermissionFailure,
            "Administrator authorization was missing or refused.",
            Some("Confirm administrator access and retry the operation."),
        )
    } else if contains_any(
        &combined,
        &[
            "temporary failure resolving",
            "failed to fetch",
            "network is unreachable",
            "connection timed out",
            "could not resolve",
            "unable to connect",
        ],
    ) {
        (
            FailureCause::NetworkFailure,
            "Network or download failure while talking to package repositories.",
            Some("Check network connectivity and repository reachability, then retry."),
        )
    } else if contains_any(
        &combined,
        &[
            "hash sum mismatch",
            "release file",
            "does not have a release file",
            "repository",
            "404  not found",
            "index files failed",
            "gpg error",
            "no signature",
        ],
    ) {
        (
            FailureCause::RepositoryError,
            "A package repository or index error prevented the operation.",
            Some("Refresh metadata after fixing repository configuration, then retry."),
        )
    } else if contains_any(
        &combined,
        &["held packages were changed", "is held", "were kept back because of held packages"],
    ) || (combined.contains("held") && combined.contains("package"))
    {
        (
            FailureCause::HeldPackage,
            "A held package blocked the requested change.",
            Some("Review held packages before forcing an upgrade of those packages."),
        )
    } else if contains_any(
        &combined,
        &[
            "unable to locate package",
            "has no installation candidate",
            "couldn't find any package",
            "package not found",
            "is not available",
        ],
    ) {
        (
            FailureCause::InvalidPackage,
            "The requested package name is invalid or has no installable candidate.",
            Some("Check the package name with `orbis find` and refresh metadata if needed."),
        )
    } else {
        let excerpt = stderr
            .lines()
            .chain(stdout.lines())
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.chars().take(200).collect::<String>())
            .unwrap_or_else(|| {
                exit_status
                    .map(|code| format!("APT exited with status {code}"))
                    .unwrap_or_else(|| "APT failed without a usable message".into())
            });
        return FailureDiagnosis {
            cause: FailureCause::Unknown,
            summary: "Package operation failed.".into(),
            hint: Some(excerpt),
        };
    };

    FailureDiagnosis { cause, summary: summary.into(), hint: hint.map(str::to_owned) }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Probe outcome for dependency health. Failure to probe is never "healthy".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyHealthStatus {
    /// Packages appear consistent (`apt-get check` succeeded).
    Healthy,
    /// Unmet dependencies or interrupted dpkg were observed.
    Broken,
    /// The probe could not run or its result is not evidence of health.
    #[default]
    Unknown,
}

/// Broken-dependency observation from `apt-get check` / `dpkg --audit`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DependencyHealth {
    /// Tri-state probe result.
    pub status: DependencyHealthStatus,
    /// Short diagnosis when broken or when the probe failed.
    pub summary: Option<String>,
}

impl DependencyHealth {
    /// True only when a probe positively observed a broken state.
    pub fn is_broken(&self) -> bool {
        self.status == DependencyHealthStatus::Broken
    }
}

/// Why installed-version observation could not be completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationFailure {
    /// `dpkg-query` is not available on PATH.
    CommandUnavailable,
    /// The query could not be executed or returned an unclassified failure.
    QueryFailed,
    /// Output was present but could not be parsed as package status rows.
    ParseFailed,
}

impl ObservationFailure {
    /// Stable short label for verification evidence.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommandUnavailable => "dpkg-query unavailable",
            Self::QueryFailed => "dpkg-query failed",
            Self::ParseFailed => "dpkg-query output could not be parsed",
        }
    }
}

/// Distinguishes a completed installed-state query from an unavailable probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstalledObservation {
    /// Query completed. The map contains only packages that are `install ok installed`.
    /// Names omitted from the map are absent, not unknown.
    Observed(BTreeMap<String, String>),
    /// The probe did not produce authoritative installed-state evidence.
    Unavailable {
        /// Why observation could not be completed.
        reason: ObservationFailure,
    },
}

impl InstalledObservation {
    /// Lookup one package without collapsing unavailable into absent.
    pub fn lookup<'a>(&'a self, package_id: &str) -> PackageLookup<'a> {
        match self {
            Self::Observed(versions) => match versions.get(package_id) {
                Some(version) => PackageLookup::Installed(version),
                None => PackageLookup::Absent,
            },
            Self::Unavailable { reason } => PackageLookup::Unavailable(*reason),
        }
    }

    /// Observed version map when the probe completed.
    pub fn versions(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::Observed(versions) => Some(versions),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Result of looking up one package after an installed-state observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageLookup<'a> {
    /// Package is `install ok installed` at this version.
    Installed(&'a str),
    /// Query completed and the package is not installed.
    Absent,
    /// Installed state could not be observed.
    Unavailable(ObservationFailure),
}

/// Runs read-only dependency health checks through the injected runner.
pub fn check_dependency_health(runner: &SharedRunner) -> DependencyHealth {
    if !runner.is_available("apt-get") {
        return DependencyHealth {
            status: DependencyHealthStatus::Unknown,
            summary: Some("apt-get is unavailable".into()),
        };
    }
    let Ok(output) = execute(
        runner,
        PackageSource::Apt,
        "check APT dependency health",
        CommandSpec::new("apt-get", ["check"])
            .with_env("LC_ALL", "C")
            .with_env("DEBIAN_FRONTEND", "noninteractive")
            .with_timeout(Duration::from_secs(30)),
    ) else {
        return DependencyHealth {
            status: DependencyHealthStatus::Unknown,
            summary: Some("Dependency health could not be probed".into()),
        };
    };
    if output.success() {
        return DependencyHealth { status: DependencyHealthStatus::Healthy, summary: None };
    }
    let diagnosis = diagnose_apt_failure(&output.stdout, &output.stderr, output.status);
    // A busy lock is not proof of broken packages; leave dependency health unknown.
    if matches!(diagnosis.cause, FailureCause::AptLock | FailureCause::PermissionFailure) {
        return DependencyHealth {
            status: DependencyHealthStatus::Unknown,
            summary: Some(diagnosis.summary),
        };
    }
    if matches!(
        diagnosis.cause,
        FailureCause::BrokenDependencies | FailureCause::InterruptedDpkg | FailureCause::Unknown
    ) {
        return DependencyHealth {
            status: DependencyHealthStatus::Broken,
            summary: Some(diagnosis.summary),
        };
    }
    DependencyHealth { status: DependencyHealthStatus::Unknown, summary: Some(diagnosis.summary) }
}

/// Observe installed versions for exact package names via `dpkg-query`.
///
/// Absent packages and probe failures are distinct. Callers must not treat an
/// empty map from an unavailable probe as proof of removal.
pub fn observe_installed_versions(runner: &SharedRunner, names: &[String]) -> InstalledObservation {
    if names.is_empty() {
        return InstalledObservation::Observed(BTreeMap::new());
    }
    if !runner.is_available("dpkg-query") {
        return InstalledObservation::Unavailable {
            reason: ObservationFailure::CommandUnavailable,
        };
    }
    let mut args =
        vec!["-W".to_owned(), "-f=${binary:Package}\t${Status}\t${Version}\n".to_owned()];
    args.extend(names.iter().cloned());
    let output = match runner
        .run(&CommandSpec::new("dpkg-query", args).with_timeout(short_timeout()))
    {
        Ok(output) => output,
        Err(ProcessError::NotFound { .. }) => {
            return InstalledObservation::Unavailable {
                reason: ObservationFailure::CommandUnavailable,
            };
        }
        Err(_) => {
            return InstalledObservation::Unavailable { reason: ObservationFailure::QueryFailed };
        }
    };
    classify_dpkg_output(&output)
}

fn classify_dpkg_output(output: &CommandOutput) -> InstalledObservation {
    let parsed = parse_dpkg_versions(&output.stdout);
    let stdout_blank = output.stdout.trim().is_empty();
    if !stdout_blank && parsed.is_empty() {
        return InstalledObservation::Unavailable { reason: ObservationFailure::ParseFailed };
    }
    if output.success() {
        return InstalledObservation::Observed(parsed);
    }
    if package_absent_stderr(&output.stderr) {
        return InstalledObservation::Observed(parsed);
    }
    InstalledObservation::Unavailable { reason: ObservationFailure::QueryFailed }
}

fn package_absent_stderr(stderr: &str) -> bool {
    let lines: Vec<_> = stderr.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    !lines.is_empty()
        && lines.iter().all(|line| {
            line.contains("no packages found matching") || line.contains("no packages found")
        })
}

/// Read installed versions, collapsing unavailable probes to an empty map.
/// Prefer [`observe_installed_versions`] at verification boundaries.
pub fn installed_versions(runner: &SharedRunner, names: &[String]) -> BTreeMap<String, String> {
    match observe_installed_versions(runner, names) {
        InstalledObservation::Observed(versions) => versions,
        InstalledObservation::Unavailable { .. } => BTreeMap::new(),
    }
}

/// Pure parser for dpkg-query tabular version output.
pub fn parse_dpkg_versions(stdout: &str) -> BTreeMap<String, String> {
    let mut installed = BTreeMap::new();
    for line in stdout.lines() {
        let mut fields = line.split('\t');
        let Some(name) = fields.next().map(str::to_owned) else { continue };
        let Some(status) = fields.next() else { continue };
        let Some(version) = fields.next().map(str::to_owned) else { continue };
        if status.contains("install ok installed") {
            installed.insert(name.clone(), version.clone());
            if let Some(base_name) = name.split_once(':').map(|(base, _)| base) {
                installed.entry(base_name.to_owned()).or_insert(version);
            }
        }
    }
    installed
}

/// Build package changes for an install/remove by comparing before/after maps.
pub fn transaction_changes(
    package_id: &str,
    display_name: &str,
    action_install: bool,
    before: Option<&str>,
    after: Option<&str>,
) -> Vec<PackageChange> {
    if action_install {
        match (before, after) {
            (_, Some(to)) => vec![PackageChange {
                package_id: package_id.to_owned(),
                name: Some(display_name.to_owned()),
                kind: ChangeKind::Installed,
                from_version: before.map(str::to_owned),
                to_version: Some(to.to_owned()),
            }],
            _ => Vec::new(),
        }
    } else {
        match (before, after) {
            (Some(from), None) => vec![PackageChange {
                package_id: package_id.to_owned(),
                name: Some(display_name.to_owned()),
                kind: ChangeKind::Removed,
                from_version: Some(from.to_owned()),
                to_version: None,
            }],
            _ => Vec::new(),
        }
    }
}

/// Compare planned upgrade candidates against post-upgrade installed versions.
pub fn upgrade_changes(
    planned: &[(String, Option<String>, Option<String>)],
    after: &BTreeMap<String, String>,
) -> Vec<PackageChange> {
    let mut changes = Vec::new();
    for (package_id, from_version, expected_to) in planned {
        let installed = after.get(package_id).cloned();
        let changed = match (from_version.as_ref(), installed.as_ref()) {
            (Some(from), Some(to)) => from != to,
            (None, Some(_)) => true,
            _ => false,
        };
        if !changed {
            continue;
        }
        changes.push(PackageChange {
            package_id: package_id.clone(),
            name: Some(package_id.clone()),
            kind: ChangeKind::Upgraded,
            from_version: from_version.clone(),
            to_version: installed.or_else(|| expected_to.clone()),
        });
    }
    changes
}

/// Verify an install/remove against observed installed state and version.
///
/// Unavailable observation never becomes [`VerificationResult::Verified`].
pub fn verify_package_state(
    action_install: bool,
    expected_version: Option<&str>,
    after: PackageLookup<'_>,
) -> (VerificationResult, Vec<String>) {
    let mut checks = Vec::new();
    match after {
        PackageLookup::Unavailable(reason) => {
            checks
                .push(format!("dpkg package state observation unavailable ({})", reason.as_str()));
            (VerificationResult::Failed, checks)
        }
        PackageLookup::Installed(version) if action_install => {
            checks.push("dpkg package state observed".into());
            checks.push(format!("installed version observed: {version}"));
            if let Some(expected) = expected_version
                && expected != version
            {
                checks.push(format!("Installed version {version} differs from planned {expected}"));
                return (VerificationResult::PartiallyVerified, checks);
            }
            (VerificationResult::Verified, checks)
        }
        PackageLookup::Absent if action_install => {
            checks.push("dpkg package state observed".into());
            checks.push("Package is not installed after install".into());
            (VerificationResult::Failed, checks)
        }
        PackageLookup::Absent => {
            checks.push("dpkg package state observed".into());
            checks.push("package is not installed".into());
            (VerificationResult::Verified, checks)
        }
        PackageLookup::Installed(_) => {
            checks.push("dpkg package state observed".into());
            checks.push("Package is still installed after remove".into());
            (VerificationResult::Failed, checks)
        }
    }
}

/// APT-focused outcome helper used by the registry after mutations.
pub struct AptOutcomeProbe {
    runner: SharedRunner,
    reboot_paths: RebootPaths,
}

impl AptOutcomeProbe {
    /// Production probe using system reboot paths.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner, reboot_paths: RebootPaths::system_default() }
    }

    /// Test probe with injectable reboot paths.
    pub fn with_reboot_paths(runner: Arc<dyn CommandRunner>, reboot_paths: RebootPaths) -> Self {
        Self { runner, reboot_paths }
    }

    /// Current reboot-required observation.
    pub fn reboot_required(&self) -> RebootRequired {
        read_reboot_required(&self.reboot_paths)
    }

    /// Dependency health via apt-get check.
    pub fn dependency_health(&self) -> DependencyHealth {
        check_dependency_health(&self.runner)
    }

    /// Installed versions for the given package ids, distinguishing absent from unavailable.
    pub fn versions(&self, names: &[String]) -> InstalledObservation {
        observe_installed_versions(&self.runner, names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_lock_broken_network_held_and_invalid() {
        let lock = diagnose_apt_failure(
            "",
            "E: Could not get lock /var/lib/dpkg/lock-frontend",
            Some(100),
        );
        assert_eq!(lock.cause, FailureCause::AptLock);

        let broken =
            diagnose_apt_failure("The following packages have unmet dependencies:", "", Some(100));
        assert_eq!(broken.cause, FailureCause::BrokenDependencies);

        let network =
            diagnose_apt_failure("", "Temporary failure resolving 'archive.ubuntu.com'", Some(100));
        assert_eq!(network.cause, FailureCause::NetworkFailure);

        let held = diagnose_apt_failure("", "The following held packages were changed:", Some(100));
        assert_eq!(held.cause, FailureCause::HeldPackage);

        let invalid = diagnose_apt_failure("", "E: Unable to locate package nosuchpkg", Some(100));
        assert_eq!(invalid.cause, FailureCause::InvalidPackage);

        let interrupted = diagnose_apt_failure(
            "",
            "dpkg was interrupted, you must manually run 'dpkg --configure -a'",
            Some(100),
        );
        assert_eq!(interrupted.cause, FailureCause::InterruptedDpkg);

        let repo = diagnose_apt_failure("", "Hash Sum mismatch", Some(100));
        assert_eq!(repo.cause, FailureCause::RepositoryError);

        let perm = diagnose_apt_failure("", "Permission denied", Some(100));
        assert_eq!(perm.cause, FailureCause::PermissionFailure);
    }

    #[test]
    fn reboot_required_reads_flag_and_packages() {
        let root = std::env::temp_dir().join(format!("orbis-reboot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let paths = RebootPaths {
            flag: root.join("reboot-required"),
            packages: root.join("reboot-required.pkgs"),
        };
        assert!(!read_reboot_required(&paths).required);
        std::fs::write(&paths.flag, "*** System restart required ***\n").unwrap();
        std::fs::write(&paths.packages, "linux-image-6.8\nlibc6\n").unwrap();
        let observed = read_reboot_required(&paths);
        assert!(observed.required);
        assert_eq!(observed.packages, vec!["linux-image-6.8".to_owned(), "libc6".to_owned()]);
        assert!(observed.warning_line().unwrap().contains("Reboot required"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn upgrade_changes_records_old_to_new() {
        let planned = vec![
            ("openssl".into(), Some("1.0".into()), Some("3.0".into())),
            ("curl".into(), Some("8.0".into()), Some("8.1".into())),
            ("unchanged".into(), Some("1".into()), Some("1".into())),
        ];
        let after = BTreeMap::from([
            ("openssl".into(), "3.0".into()),
            ("curl".into(), "8.0".into()), // failed to upgrade
            ("unchanged".into(), "1".into()),
        ]);
        let changes = upgrade_changes(&planned, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].package_id, "openssl");
        assert_eq!(changes[0].version_span().as_deref(), Some("1.0 → 3.0"));
    }

    #[test]
    fn verify_install_requires_installed_version() {
        let (ok, _) = verify_package_state(true, Some("1.2"), PackageLookup::Installed("1.2"));
        assert_eq!(ok, VerificationResult::Verified);
        let (missing, _) = verify_package_state(true, Some("1.2"), PackageLookup::Absent);
        assert_eq!(missing, VerificationResult::Failed);
        let (removed, _) = verify_package_state(false, None, PackageLookup::Absent);
        assert_eq!(removed, VerificationResult::Verified);
        let (unavailable_remove, _) = verify_package_state(
            false,
            None,
            PackageLookup::Unavailable(ObservationFailure::QueryFailed),
        );
        assert_ne!(unavailable_remove, VerificationResult::Verified);
        let (unavailable_install, _) = verify_package_state(
            true,
            Some("1.2"),
            PackageLookup::Unavailable(ObservationFailure::QueryFailed),
        );
        assert_ne!(unavailable_install, VerificationResult::Verified);
    }

    #[test]
    fn dpkg_query_failure_is_unavailable_not_absent() {
        let failed = classify_dpkg_output(&CommandOutput {
            stdout: String::new(),
            stderr: "dpkg-query: error: failed to open package info file".into(),
            status: Some(1),
        });
        assert_eq!(
            failed,
            InstalledObservation::Unavailable { reason: ObservationFailure::QueryFailed }
        );

        let absent = classify_dpkg_output(&CommandOutput {
            stdout: String::new(),
            stderr: "dpkg-query: no packages found matching curl".into(),
            status: Some(1),
        });
        assert_eq!(absent, InstalledObservation::Observed(BTreeMap::new()));

        let unparsed = classify_dpkg_output(&CommandOutput {
            stdout: "this is not a dpkg status table".into(),
            stderr: String::new(),
            status: Some(0),
        });
        assert_eq!(
            unparsed,
            InstalledObservation::Unavailable { reason: ObservationFailure::ParseFailed }
        );
    }

    #[test]
    fn parses_dpkg_query_versions() {
        let map = parse_dpkg_versions(
            "ripgrep\tinstall ok installed\t14.1.0-1\nlibssl3:amd64\tinstall ok installed\t3.0.13\n",
        );
        assert_eq!(map.get("ripgrep").map(String::as_str), Some("14.1.0-1"));
        assert_eq!(map.get("libssl3:amd64").map(String::as_str), Some("3.0.13"));
        assert_eq!(map.get("libssl3").map(String::as_str), Some("3.0.13"));
    }

    #[test]
    fn apt_health_probe_unavailable_is_unknown() {
        struct NoneRunner;
        impl CommandRunner for NoneRunner {
            fn is_available(&self, _: &str) -> bool {
                false
            }
            fn run(
                &self,
                command: &CommandSpec,
            ) -> Result<CommandOutput, crate::process::ProcessError> {
                Err(crate::process::ProcessError::NotFound { program: command.program.clone() })
            }
        }
        let health = check_dependency_health(&(std::sync::Arc::new(NoneRunner) as SharedRunner));
        assert_eq!(health.status, DependencyHealthStatus::Unknown);
        assert!(!health.is_broken());
    }
}
