//! Safe, deliberately narrow self-update support for the Orbis executable.
//!
//! This module is intentionally separate from package providers. It only knows
//! how to discover a release from the canonical Orbis GitHub repository, verify
//! its published checksum, inspect the expected cargo-dist archive, and replace
//! the executable that is actually running.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ureq::ResponseExt;
use xz2::read::XzDecoder;

const API_URL: &str = "https://api.github.com/repos/iamaritrasaha/orbis/releases?per_page=30";
const DOWNLOAD_PREFIX: &str = "https://github.com/iamaritrasaha/orbis/releases/download/";
const CACHE_MAX_BYTES: usize = 128 * 1024;
const ARCHIVE_MAX_BYTES: usize = 128 * 1024 * 1024;
const BINARY_MAX_BYTES: usize = 64 * 1024 * 1024;
const CACHE_MAX_AGE: u64 = 24 * 60 * 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelfUpdateStage {
    Checking,
    Downloading,
    Verifying,
    Installing,
    Finishing,
    Done,
    Failed,
}

pub(crate) trait UpdateObserver: Send + Sync {
    fn stage(&self, stage: SelfUpdateStage);
}

pub(crate) struct SilentUpdateObserver;

impl UpdateObserver for SilentUpdateObserver {
    fn stage(&self, _stage: SelfUpdateStage) {}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SelfUpdateState {
    UpToDate,
    UpdateAvailable,
    Updated,
    DevelopmentBuild,
    UnsupportedInstallation,
    VerificationFailed,
    NetworkError,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SelfUpdateReport {
    pub(crate) state: SelfUpdateState,
    pub(crate) current_version: String,
    pub(crate) available_version: Option<String>,
    pub(crate) installed_version: Option<String>,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckReport {
    pub(crate) current_version: Version,
    pub(crate) current_is_development: bool,
    pub(crate) latest: Option<ReleaseMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseMetadata {
    pub(crate) version: Version,
    pub(crate) tag: String,
    pub(crate) release_url: String,
    pub(crate) archive: ReleaseAsset,
    pub(crate) checksum: ReleaseAsset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseAsset {
    pub(crate) name: String,
    pub(crate) url: String,
}

#[derive(Debug, Error)]
pub(crate) enum SelfUpdateError {
    #[error("could not reach the Orbis release service: {0}")]
    Network(String),
    #[error("the published release metadata could not be verified: {0}")]
    Verification(String),
    #[error("this Orbis installation cannot be safely replaced: {0}")]
    Unsupported(String),
    #[error("another Orbis update is already in progress")]
    Concurrent,
    #[error("could not install the verified Orbis update: {0}")]
    Installation(String),
}

pub(crate) trait ReleaseClient: Send + Sync {
    fn releases(&self) -> Result<Vec<ApiRelease>, SelfUpdateError>;
    fn download(&self, url: &str, limit: usize) -> Result<Vec<u8>, SelfUpdateError>;
}

#[derive(Clone)]
struct GitHubReleaseClient {
    agent: ureq::Agent,
}

impl GitHubReleaseClient {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(5)
            .max_redirects_will_error(true)
            .save_redirect_history(true)
            .timeout_global(Some(HTTP_TIMEOUT))
            .user_agent(format!("orbis/{}", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent: config.into() }
    }

    fn request(&self, url: &str, limit: usize) -> Result<Vec<u8>, SelfUpdateError> {
        if !url.starts_with("https://") {
            return Err(SelfUpdateError::Network(
                "only HTTPS release endpoints are allowed".into(),
            ));
        }
        let mut response = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|error| SelfUpdateError::Network(error.to_string()))?;
        let final_uri = response.get_uri();
        if final_uri.scheme_str() != Some("https") || !allowed_host(final_uri.host()) {
            return Err(SelfUpdateError::Network(
                "the release service returned an unexpected or insecure redirect".into(),
            ));
        }
        response
            .body_mut()
            .with_config()
            .limit(limit as u64)
            .read_to_vec()
            .map_err(|error| SelfUpdateError::Network(error.to_string()))
    }
}

impl ReleaseClient for GitHubReleaseClient {
    fn releases(&self) -> Result<Vec<ApiRelease>, SelfUpdateError> {
        let body = self.request(API_URL, CACHE_MAX_BYTES)?;
        serde_json::from_slice(&body).map_err(|error| {
            SelfUpdateError::Verification(format!("invalid GitHub response: {error}"))
        })
    }

    fn download(&self, url: &str, limit: usize) -> Result<Vec<u8>, SelfUpdateError> {
        if !url.starts_with(DOWNLOAD_PREFIX) {
            return Err(SelfUpdateError::Verification(
                "release assets must come from the canonical Orbis GitHub release".into(),
            ));
        }
        self.request(url, limit)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiRelease {
    pub(crate) tag_name: String,
    pub(crate) prerelease: bool,
    pub(crate) draft: bool,
    pub(crate) html_url: String,
    pub(crate) assets: Vec<ApiAsset>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ApiAsset {
    pub(crate) name: String,
    pub(crate) browser_download_url: String,
}

pub(crate) fn current_version() -> Result<Version, SelfUpdateError> {
    Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
        SelfUpdateError::Verification(format!("invalid installed version: {error}"))
    })
}

pub(crate) fn is_development_version(version: &Version) -> bool {
    version.pre.as_str().split('.').any(|identifier| identifier == "dev")
}

pub(crate) fn target_triple() -> Option<&'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some("x86_64-unknown-linux-gnu");
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Some("aarch64-unknown-linux-gnu");
    }
    #[allow(unreachable_code)]
    None
}

fn channel(version: &Version) -> Option<&'static str> {
    if version.pre.is_empty() {
        Some("stable")
    } else if version.pre.as_str().split('.').next() == Some("beta") {
        Some("beta")
    } else {
        None
    }
}

fn parse_public_tag(tag: &str) -> Option<Version> {
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    (!is_development_version(&version) && channel(&version).is_some()).then_some(version)
}

fn allowed_host(host: Option<&str>) -> bool {
    matches!(
        host,
        Some("api.github.com")
            | Some("github.com")
            | Some("objects.githubusercontent.com")
            | Some("release-assets.githubusercontent.com")
            | Some("github-releases.githubusercontent.com")
    )
}

fn valid_download_url(url: &str) -> bool {
    url.starts_with(DOWNLOAD_PREFIX)
        && !url.contains("..")
        && url
            .strip_prefix("https://")
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|host| host == "github.com")
}

fn expected_asset(api: &ApiRelease, name: &str) -> Result<ReleaseAsset, SelfUpdateError> {
    let asset = api
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| SelfUpdateError::Verification(format!("release is missing {name}")))?;
    if !valid_download_url(&asset.browser_download_url) {
        return Err(SelfUpdateError::Verification(format!(
            "release asset {name} is not a canonical HTTPS GitHub download"
        )));
    }
    Ok(ReleaseAsset { name: asset.name.clone(), url: asset.browser_download_url.clone() })
}

fn release_metadata(
    api: &ApiRelease,
    target: &str,
) -> Result<Option<ReleaseMetadata>, SelfUpdateError> {
    let Some(version) = parse_public_tag(&api.tag_name) else { return Ok(None) };
    let Some(release_channel) = channel(&version) else { return Ok(None) };
    if api.draft || api.prerelease != (release_channel == "beta") {
        return Ok(None);
    }
    if !api.html_url.starts_with("https://github.com/iamaritrasaha/orbis/releases/tag/") {
        return Err(SelfUpdateError::Verification(
            "release metadata points outside the canonical Orbis repository".into(),
        ));
    }
    let archive_name = format!("orbis-{target}.tar.xz");
    let archive = expected_asset(api, &archive_name)?;
    let checksum = api
        .assets
        .iter()
        .find(|asset| asset.name == format!("{archive_name}.sha256"))
        .or_else(|| api.assets.iter().find(|asset| asset.name == "sha256.sum"))
        .ok_or_else(|| {
            SelfUpdateError::Verification("release is missing a SHA-256 checksum".into())
        })?;
    if !valid_download_url(&checksum.browser_download_url) {
        return Err(SelfUpdateError::Verification(
            "release checksum is not a canonical HTTPS GitHub download".into(),
        ));
    }
    Ok(Some(ReleaseMetadata {
        version,
        tag: api.tag_name.clone(),
        release_url: api.html_url.clone(),
        archive,
        checksum: ReleaseAsset {
            name: checksum.name.clone(),
            url: checksum.browser_download_url.clone(),
        },
    }))
}

pub(crate) fn check_with_client(
    client: &dyn ReleaseClient,
    current: Version,
) -> Result<CheckReport, SelfUpdateError> {
    let current_is_development = is_development_version(&current);
    let target = target_triple().ok_or_else(|| {
        SelfUpdateError::Unsupported(
            "self-update supports only Linux x86_64 and aarch64 builds".into(),
        )
    })?;
    let wanted_channel = channel(&current).ok_or_else(|| {
        SelfUpdateError::Unsupported("this version is not on a supported release channel".into())
    })?;
    let releases = client.releases()?;
    let mut releases = releases
        .iter()
        .filter_map(|release| parse_public_tag(&release.tag_name).map(|version| (version, release)))
        .collect::<Vec<_>>();
    releases.sort_by(|left, right| right.0.cmp(&left.0));
    let mut latest: Option<ReleaseMetadata> = None;
    for (version, release) in releases {
        if channel(&version) != Some(wanted_channel) {
            continue;
        }
        let Some(metadata) = release_metadata(release, target)? else {
            continue;
        };
        let is_newer = current_is_development || metadata.version > current;
        if is_newer {
            latest = Some(metadata);
            break;
        }
        if !current_is_development && metadata.version <= current {
            break;
        }
    }
    Ok(CheckReport { current_version: current, current_is_development, latest })
}

pub(crate) fn check_current(
    check_public_release_for_dev: bool,
) -> Result<CheckReport, SelfUpdateError> {
    let current = current_version()?;
    if is_development_version(&current)
        && (!check_public_release_for_dev || channel(&current).is_none())
    {
        return Ok(CheckReport {
            current_version: current,
            current_is_development: true,
            latest: None,
        });
    }
    let check = check_with_client(&GitHubReleaseClient::new(), current)?;
    if let Some(root) = cache_dir() {
        let _ = write_cache(&root, &check);
    }
    Ok(check)
}

pub(crate) fn report_for_check(check: &CheckReport) -> SelfUpdateReport {
    let current = check.current_version.to_string();
    if check.current_is_development {
        return SelfUpdateReport {
            state: SelfUpdateState::DevelopmentBuild,
            current_version: current,
            available_version: check.latest.as_ref().map(|release| release.version.to_string()),
            installed_version: None,
            message: "This is a development build. It will not replace itself. Pull the latest source, then reinstall with cargo install --path crates/orbis-cli --locked --force.".into(),
        };
    }
    match &check.latest {
        Some(release) => SelfUpdateReport {
            state: SelfUpdateState::UpdateAvailable,
            current_version: current,
            available_version: Some(release.version.to_string()),
            installed_version: None,
            message: format!(
                "Orbis {} is available from the official release channel.",
                release.version
            ),
        },
        None => SelfUpdateReport {
            state: SelfUpdateState::UpToDate,
            current_version: current,
            available_version: None,
            installed_version: None,
            message: "Orbis is up to date.".into(),
        },
    }
}

pub(crate) fn install_current(
    check: &CheckReport,
    observer: &dyn UpdateObserver,
) -> SelfUpdateReport {
    let current = check.current_version.to_string();
    let Some(release) = &check.latest else {
        return report_for_check(check);
    };
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            observer.stage(SelfUpdateStage::Failed);
            return error_report(SelfUpdateError::Unsupported(format!(
                "could not locate the running executable: {error}"
            )));
        }
    };
    install_release(
        &GitHubReleaseClient::new(),
        release,
        &executable,
        &cache_dir().unwrap_or_else(|| std::env::temp_dir().join("orbis")),
        observer,
        current,
    )
}

pub(crate) fn install_release(
    client: &dyn ReleaseClient,
    release: &ReleaseMetadata,
    executable: &Path,
    cache_root: &Path,
    observer: &dyn UpdateObserver,
    current_version: String,
) -> SelfUpdateReport {
    let target = target_triple().unwrap_or("unsupported");
    let result = install_release_inner(client, release, executable, cache_root, observer, target);
    match result {
        Ok(()) => {
            observer.stage(SelfUpdateStage::Done);
            SelfUpdateReport {
                state: SelfUpdateState::Updated,
                current_version,
                available_version: Some(release.version.to_string()),
                installed_version: Some(release.version.to_string()),
                message: format!(
                    "Orbis was updated from {} to {}. Restart Orbis to use the new version.",
                    release.tag, release.version
                ),
            }
        }
        Err(error) => {
            observer.stage(SelfUpdateStage::Failed);
            error_report(error)
        }
    }
}

fn install_release_inner(
    client: &dyn ReleaseClient,
    release: &ReleaseMetadata,
    executable: &Path,
    cache_root: &Path,
    observer: &dyn UpdateObserver,
    target: &str,
) -> Result<(), SelfUpdateError> {
    ensure_eligible_installation(executable)?;
    fs::create_dir_all(cache_root).map_err(|error| {
        SelfUpdateError::Installation(format!("could not prepare update cache: {error}"))
    })?;
    let lock_path = cache_root.join("self-update.lock");
    let _lock = UpdateLock::acquire_at(&lock_path)?;

    observer.stage(SelfUpdateStage::Downloading);
    let archive_bytes = client.download(&release.archive.url, ARCHIVE_MAX_BYTES)?;
    let archive_file = TempFile::create(cache_root, "orbis-archive", &archive_bytes)?;

    observer.stage(SelfUpdateStage::Verifying);
    let checksum = client.download(&release.checksum.url, CACHE_MAX_BYTES)?;
    let expected = parse_checksum(&checksum, &release.archive.name)?;
    let actual = sha256_file(&archive_file.path)?;
    if expected != actual {
        return Err(SelfUpdateError::Verification(format!(
            "SHA-256 mismatch for {}",
            release.archive.name
        )));
    }
    let binary = extract_binary(&archive_file.path, &format!("orbis-{target}/orbis"))?;
    validate_binary_bytes(&binary)?;

    observer.stage(SelfUpdateStage::Installing);
    atomic_replace(executable, &binary)?;
    observer.stage(SelfUpdateStage::Finishing);
    Ok(())
}

fn error_report(error: SelfUpdateError) -> SelfUpdateReport {
    let state = match error {
        SelfUpdateError::Network(_) => SelfUpdateState::NetworkError,
        SelfUpdateError::Verification(_) => SelfUpdateState::VerificationFailed,
        SelfUpdateError::Unsupported(_) | SelfUpdateError::Concurrent => {
            SelfUpdateState::UnsupportedInstallation
        }
        SelfUpdateError::Installation(_) => SelfUpdateState::VerificationFailed,
    };
    let message = error.to_string();
    SelfUpdateReport {
        state,
        current_version: env!("CARGO_PKG_VERSION").into(),
        available_version: None,
        installed_version: None,
        message,
    }
}

pub(crate) fn error_report_for_cli(error: SelfUpdateError) -> SelfUpdateReport {
    error_report(error)
}

fn ensure_eligible_installation(executable: &Path) -> Result<(), SelfUpdateError> {
    let metadata = fs::symlink_metadata(executable).map_err(|error| {
        SelfUpdateError::Unsupported(format!("could not inspect the running executable: {error}"))
    })?;
    if !metadata.file_type().is_file() {
        return Err(SelfUpdateError::Unsupported("the running path is not a regular file".into()));
    }
    if metadata.file_type().is_symlink() {
        return Err(SelfUpdateError::Unsupported("the running executable is a symlink".into()));
    }
    let parent = executable.parent().ok_or_else(|| {
        SelfUpdateError::Unsupported("the running executable has no parent directory".into())
    })?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|error| {
        SelfUpdateError::Unsupported(format!("could not inspect the install directory: {error}"))
    })?;
    if !parent_meta.is_dir() || parent_meta.file_type().is_symlink() {
        return Err(SelfUpdateError::Unsupported(
            "the install directory is not a real directory".into(),
        ));
    }
    if parent.canonicalize().ok().is_some_and(|canonical| canonical != parent) {
        return Err(SelfUpdateError::Unsupported(
            "the install directory contains a symbolic-link path component".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let Some(uid) = current_uid() else {
            return Err(SelfUpdateError::Unsupported(
                "could not establish the current user".into(),
            ));
        };
        if metadata.uid() != uid || parent_meta.uid() != uid {
            return Err(SelfUpdateError::Unsupported(
                "only a user-owned installation can be replaced; use the original install method for a system installation".into(),
            ));
        }
        if metadata.mode() & 0o6000 != 0 || metadata.permissions().mode() & 0o111 == 0 {
            return Err(SelfUpdateError::Unsupported(
                "the running executable has unsafe permissions or is not executable".into(),
            ));
        }
        if parent_meta.permissions().mode() & 0o300 != 0o300 {
            return Err(SelfUpdateError::Unsupported(
                "the install directory is not writable by its owner".into(),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        let _ = parent_meta;
        return Err(SelfUpdateError::Unsupported("self-update is supported on Linux only".into()));
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some("Uid:")).then(|| fields.next()?.parse::<u32>().ok()).flatten()
    })
}

fn validate_archive_path(path: &Path) -> Result<(), SelfUpdateError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
        })
    {
        return Err(SelfUpdateError::Verification(format!(
            "archive contains an unsafe path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn extract_binary(archive_path: &Path, expected_path: &str) -> Result<Vec<u8>, SelfUpdateError> {
    let file = File::open(archive_path).map_err(|error| {
        SelfUpdateError::Verification(format!("could not open verified archive: {error}"))
    })?;
    let decoder = XzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut binary = None;
    let expected_root = Path::new(expected_path)
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(root) => Some(root.to_owned()),
            _ => None,
        })
        .ok_or_else(|| {
            SelfUpdateError::Verification("the expected archive path is invalid".into())
        })?;
    let entries = archive.entries().map_err(|error| {
        SelfUpdateError::Verification(format!("could not inspect release archive: {error}"))
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            SelfUpdateError::Verification(format!("could not read release archive: {error}"))
        })?;
        let path = entry
            .path()
            .map_err(|error| {
                SelfUpdateError::Verification(format!("could not read archive path: {error}"))
            })?
            .into_owned();
        validate_archive_path(&path)?;
        if path.components().next() != Some(Component::Normal(expected_root.as_os_str())) {
            return Err(SelfUpdateError::Verification(format!(
                "archive contains an unexpected path: {}",
                path.display()
            )));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink()
            || entry_type.is_hard_link()
            || !entry_type.is_file() && !entry_type.is_dir()
        {
            return Err(SelfUpdateError::Verification(format!(
                "archive contains an unsafe non-regular entry: {}",
                path.display()
            )));
        }
        if path == Path::new(expected_path) {
            if !entry_type.is_file() {
                return Err(SelfUpdateError::Verification(
                    "the expected Orbis binary is not a regular file".into(),
                ));
            }
            if entry.size() > BINARY_MAX_BYTES as u64 {
                return Err(SelfUpdateError::Verification(
                    "the release binary is unexpectedly large".into(),
                ));
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes).map_err(|error| {
                SelfUpdateError::Verification(format!(
                    "could not extract the Orbis binary: {error}"
                ))
            })?;
            binary = Some(bytes);
        }
    }
    binary
        .ok_or_else(|| SelfUpdateError::Verification(format!("archive is missing {expected_path}")))
}

fn validate_binary_bytes(bytes: &[u8]) -> Result<(), SelfUpdateError> {
    if bytes.is_empty() {
        return Err(SelfUpdateError::Verification("the release binary is empty".into()));
    }
    Ok(())
}

fn parse_checksum(contents: &[u8], archive_name: &str) -> Result<String, SelfUpdateError> {
    let text = std::str::from_utf8(contents).map_err(|error| {
        SelfUpdateError::Verification(format!("checksum file is not UTF-8: {error}"))
    })?;
    let mut unnamed_hashes = Vec::new();
    for line in text.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let Some(hash) = fields.first().copied() else { continue };
        if !is_sha256(hash) {
            continue;
        }
        if fields.len() >= 2
            && fields.get(1).is_some_and(|name| name.trim_start_matches('*') == archive_name)
        {
            return Ok(hash.to_ascii_lowercase());
        }
        if fields.len() == 1 {
            unnamed_hashes.push(hash.to_ascii_lowercase());
        }
    }
    if unnamed_hashes.len() == 1 {
        return Ok(unnamed_hashes.remove(0));
    }
    Err(SelfUpdateError::Verification(
        "checksum file contains no SHA-256 value for the archive".into(),
    ))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_file(path: &Path) -> Result<String, SelfUpdateError> {
    let mut file = File::open(path).map_err(|error| {
        SelfUpdateError::Verification(format!("could not read archive for verification: {error}"))
    })?;
    let mut hasher = Sha256::new();
    let mut total = 0usize;
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            SelfUpdateError::Verification(format!("could not hash archive: {error}"))
        })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read);
        if total > ARCHIVE_MAX_BYTES {
            return Err(SelfUpdateError::Verification("release archive is too large".into()));
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn atomic_replace(executable: &Path, bytes: &[u8]) -> Result<(), SelfUpdateError> {
    validate_binary_bytes(bytes)?;
    let parent = executable.parent().ok_or_else(|| {
        SelfUpdateError::Unsupported("the executable has no parent directory".into())
    })?;
    let candidate = TempFile::create(parent, ".orbis-new", bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&candidate.path, fs::Permissions::from_mode(0o755)).map_err(
            |error| {
                SelfUpdateError::Installation(format!(
                    "could not set safe executable permissions: {error}"
                ))
            },
        )?;
    }
    let file = OpenOptions::new().read(true).open(&candidate.path).map_err(|error| {
        SelfUpdateError::Installation(format!("could not reopen update candidate: {error}"))
    })?;
    file.sync_all().map_err(|error| {
        SelfUpdateError::Installation(format!("could not sync update candidate: {error}"))
    })?;
    fs::rename(&candidate.path, executable).map_err(|error| {
        SelfUpdateError::Installation(format!("could not atomically replace Orbis: {error}"))
    })?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn create(directory: &Path, prefix: &str, bytes: &[u8]) -> Result<Self, SelfUpdateError> {
        for _ in 0..16 {
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("{prefix}-{}-{counter}", std::process::id()));
            let result = OpenOptions::new().write(true).create_new(true).open(&path);
            match result {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(bytes) {
                        let _ = fs::remove_file(&path);
                        return Err(SelfUpdateError::Installation(format!(
                            "could not write temporary update: {error}"
                        )));
                    }
                    if let Err(error) = file.sync_all() {
                        let _ = fs::remove_file(&path);
                        return Err(SelfUpdateError::Installation(format!(
                            "could not sync temporary update: {error}"
                        )));
                    }
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(SelfUpdateError::Installation(format!(
                        "could not create temporary update: {error}"
                    )));
                }
            }
        }
        Err(SelfUpdateError::Installation("could not choose a temporary update name".into()))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct UpdateLock {
    path: PathBuf,
}

impl UpdateLock {
    fn acquire_at(path: &Path) -> Result<Self, SelfUpdateError> {
        for attempt in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    if let Err(error) = writeln!(file, "{}", std::process::id()) {
                        let _ = fs::remove_file(path);
                        return Err(SelfUpdateError::Installation(format!(
                            "could not write update lock: {error}"
                        )));
                    }
                    return Ok(Self { path: path.to_path_buf() });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt == 0 => {
                    if stale_lock(path) {
                        let _ = fs::remove_file(path);
                        continue;
                    }
                    return Err(SelfUpdateError::Concurrent);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(SelfUpdateError::Concurrent);
                }
                Err(error) => {
                    return Err(SelfUpdateError::Installation(format!(
                        "could not create update lock: {error}"
                    )));
                }
            }
        }
        Err(SelfUpdateError::Concurrent)
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn stale_lock(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else { return false };
    let Ok(pid) = contents.trim().parse::<u32>() else { return false };
    if pid == std::process::id() {
        return false;
    }
    !Path::new("/proc").join(pid.to_string()).exists()
}

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|base| base.join("orbis"))
}

#[derive(Debug, Deserialize, Serialize)]
struct CacheRecord {
    checked_at_unix: u64,
    latest_version: Option<String>,
    release_url: Option<String>,
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
}

fn cache_path(root: &Path) -> PathBuf {
    root.join("self-update.json")
}

fn read_cache(root: &Path) -> Option<CacheRecord> {
    let body = fs::read(cache_path(root)).ok()?;
    (body.len() <= CACHE_MAX_BYTES).then(|| serde_json::from_slice(&body).ok()).flatten()
}

fn write_cache(root: &Path, check: &CheckReport) -> Result<(), SelfUpdateError> {
    fs::create_dir_all(root).map_err(|error| {
        SelfUpdateError::Installation(format!("could not create update cache: {error}"))
    })?;
    let record = CacheRecord {
        checked_at_unix: now_unix(),
        latest_version: check.latest.as_ref().map(|release| release.version.to_string()),
        release_url: check.latest.as_ref().map(|release| release.release_url.clone()),
    };
    let bytes = serde_json::to_vec(&record).map_err(|error| {
        SelfUpdateError::Installation(format!("could not encode update cache: {error}"))
    })?;
    let temporary = TempFile::create(root, ".self-update-cache", &bytes)?;
    fs::rename(temporary.path.clone(), cache_path(root)).map_err(|error| {
        SelfUpdateError::Installation(format!("could not save update cache: {error}"))
    })?;
    Ok(())
}

pub(crate) fn background_notice() -> Option<String> {
    let current = current_version().ok()?;
    if is_development_version(&current) {
        return None;
    }
    let root = cache_dir()?;
    if let Some(record) = read_cache(&root)
        && now_unix().saturating_sub(record.checked_at_unix) < CACHE_MAX_AGE
    {
        return cached_notice(&root, &current);
    }
    let check = check_current(false).ok()?;
    let notice = check
        .latest
        .as_ref()
        .filter(|release| release.version > current)
        .map(|release| format!("Orbis {} is available", release.version));
    let _ = write_cache(&root, &check);
    notice
}

pub(crate) fn cached_notice(root: &Path, current: &Version) -> Option<String> {
    let record = read_cache(root)?;
    let latest = record.latest_version.and_then(|version| Version::parse(&version).ok())?;
    (latest > *current).then(|| format!("Orbis {latest} is available"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    struct FakeClient {
        releases: Vec<ApiRelease>,
        downloads: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl ReleaseClient for FakeClient {
        fn releases(&self) -> Result<Vec<ApiRelease>, SelfUpdateError> {
            Ok(self.releases.clone())
        }

        fn download(&self, url: &str, _limit: usize) -> Result<Vec<u8>, SelfUpdateError> {
            self.downloads
                .lock()
                .expect("download lock")
                .get(url)
                .cloned()
                .ok_or_else(|| SelfUpdateError::Network(format!("missing fixture {url}")))
        }
    }

    fn public_release(
        tag: &str,
        target: &str,
        archive: Vec<u8>,
    ) -> (ApiRelease, HashMap<String, Vec<u8>>) {
        let archive_name = format!("orbis-{target}.tar.xz");
        let archive_url = format!("{DOWNLOAD_PREFIX}{tag}/{archive_name}");
        let checksum_name = format!("{archive_name}.sha256");
        let checksum_url = format!("{DOWNLOAD_PREFIX}{tag}/{checksum_name}");
        let digest = format!("{:x}", Sha256::digest(&archive));
        let mut downloads = HashMap::new();
        downloads.insert(archive_url.clone(), archive);
        downloads.insert(checksum_url.clone(), format!("{digest}  {archive_name}\n").into_bytes());
        (
            ApiRelease {
                tag_name: tag.into(),
                prerelease: tag.contains("beta"),
                draft: false,
                html_url: format!("https://github.com/iamaritrasaha/orbis/releases/tag/{tag}"),
                assets: vec![
                    ApiAsset { name: archive_name, browser_download_url: archive_url },
                    ApiAsset { name: checksum_name, browser_download_url: checksum_url },
                ],
            },
            downloads,
        )
    }

    fn archive(target: &str, contents: &[u8]) -> Vec<u8> {
        let mut compressed = Vec::new();
        {
            let encoder = xz2::write::XzEncoder::new(&mut compressed, 6);
            let mut builder = tar::Builder::new(encoder);
            let path = format!("orbis-{target}/orbis");
            let mut header = tar::Header::new_gnu();
            header.set_path(path).expect("archive path");
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, contents).expect("archive binary");
            let encoder = builder.into_inner().expect("tar finish");
            encoder.finish().expect("xz finish");
        }
        compressed
    }

    #[test]
    fn semver_handles_beta_stable_and_development_builds() {
        let beta1 = Version::parse("0.1.0-beta.1").expect("semver");
        let beta2 = Version::parse("0.1.0-beta.2").expect("semver");
        let stable = Version::parse("0.1.0").expect("semver");
        let dev = Version::parse("0.1.0-beta.1.dev.2").expect("semver");
        assert!(beta2 > beta1);
        assert!(stable > beta2);
        assert!(is_development_version(&dev));
        assert!(!is_development_version(&beta1));
        assert!(parse_public_tag("v0.1.0-beta.2").is_some());
        assert!(parse_public_tag("vnot-semver").is_none());
        assert!(parse_public_tag("v0.1.0-beta.1.dev.9").is_none());
    }

    #[test]
    fn json_states_are_stable_snake_case_values() {
        let states = [
            (SelfUpdateState::UpToDate, "up_to_date"),
            (SelfUpdateState::UpdateAvailable, "update_available"),
            (SelfUpdateState::Updated, "updated"),
            (SelfUpdateState::DevelopmentBuild, "development_build"),
            (SelfUpdateState::UnsupportedInstallation, "unsupported_installation"),
            (SelfUpdateState::VerificationFailed, "verification_failed"),
            (SelfUpdateState::NetworkError, "network_error"),
        ];
        for (state, expected) in states {
            let value = serde_json::to_value(state).expect("state json");
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn check_selects_only_the_current_release_channel() {
        let target = target_triple().expect("linux test target");
        let (beta1, _) = public_release("v0.1.0-beta.1", target, archive(target, b"old"));
        let (beta2, _) = public_release("v0.1.0-beta.2", target, archive(target, b"new"));
        let (stable, _) = public_release("v0.2.0", target, archive(target, b"stable"));
        let client = FakeClient {
            releases: vec![beta1, beta2, stable],
            downloads: Mutex::new(HashMap::new()),
        };
        let check = check_with_client(&client, Version::parse("0.1.0-beta.1").expect("version"))
            .expect("check");
        assert_eq!(check.latest.expect("beta update").tag, "v0.1.0-beta.2");
    }

    #[test]
    fn noncanonical_release_asset_is_rejected() {
        let target = target_triple().expect("linux test target");
        let (mut api, _) = public_release("v0.1.0-beta.2", target, archive(target, b"new"));
        api.assets[0].browser_download_url = "https://example.com/orbis.tar.xz".into();
        assert!(matches!(
            release_metadata(&api, target),
            Err(SelfUpdateError::Verification(message)) if message.contains("canonical")
        ));
    }

    #[test]
    fn development_build_never_becomes_eligible_for_replacement() {
        let current = Version::parse("0.1.0-beta.1.dev.2").expect("version");
        let check =
            CheckReport { current_version: current, current_is_development: true, latest: None };
        let report = report_for_check(&check);
        assert_eq!(report.state, SelfUpdateState::DevelopmentBuild);
        assert!(report.message.contains("will not replace itself"));
    }

    #[test]
    fn checksum_and_archive_validation_reject_tampering_and_symlinks() {
        let target = target_triple().expect("linux test target");
        let archive_path = tempfile_path("orbis-archive-test");
        fs::write(&archive_path, archive(target, b"binary")).expect("archive");
        let extracted = extract_binary(&archive_path, &format!("orbis-{target}/orbis"))
            .expect("extract binary");
        assert_eq!(extracted, b"binary");
        fs::remove_file(&archive_path).expect("cleanup");
        fs::remove_dir_all(archive_path.parent().expect("temp parent")).expect("cleanup directory");
        assert!(parse_checksum(b"not a checksum\n", "archive").is_err());
        assert!(
            parse_checksum(
                b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa other.tar.xz\n",
                "archive.tar.xz"
            )
            .is_err()
        );
        assert!(validate_archive_path(Path::new("../orbis")).is_err());
        assert!(validate_archive_path(Path::new("/tmp/orbis")).is_err());
    }

    #[test]
    fn verified_release_replaces_only_the_named_user_executable() {
        let target = target_triple().expect("linux test target");
        let new_binary = b"new-orbis-binary";
        let archive = archive(target, new_binary);
        let (api, downloads) = public_release("v0.1.0-beta.2", target, archive);
        let release = release_metadata(&api, target).expect("release metadata").expect("release");
        let client = FakeClient { releases: vec![], downloads: Mutex::new(downloads) };
        let directory = tempfile_dir("orbis-install");
        let executable = directory.join("orbis");
        fs::write(&executable, b"old-orbis-binary").expect("old executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("executable mode");
        }
        let report = install_release(
            &client,
            &release,
            &executable,
            &directory.join("cache"),
            &SilentUpdateObserver,
            "0.1.0-beta.1".into(),
        );
        assert_eq!(report.state, SelfUpdateState::Updated);
        assert_eq!(fs::read(&executable).expect("new executable"), new_binary);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn archive_symlink_is_rejected_before_extraction() {
        let target = target_triple().expect("linux test target");
        let path = tempfile_path("orbis-symlink-archive");
        let mut compressed = Vec::new();
        {
            let encoder = xz2::write::XzEncoder::new(&mut compressed, 6);
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_path(format!("orbis-{target}/orbis")).expect("archive path");
            header.set_entry_type(tar::EntryType::symlink());
            header.set_link_name("/etc/passwd").expect("link name");
            header.set_size(0);
            header.set_cksum();
            builder.append(&header, io::empty()).expect("symlink entry");
            let encoder = builder.into_inner().expect("tar finish");
            encoder.finish().expect("xz finish");
        }
        fs::write(&path, compressed).expect("archive");
        assert!(matches!(
            extract_binary(&path, &format!("orbis-{target}/orbis")),
            Err(SelfUpdateError::Verification(message)) if message.contains("unsafe non-regular")
        ));
        fs::remove_dir_all(path.parent().expect("temp parent")).expect("cleanup");
    }

    #[test]
    fn atomic_replacement_keeps_original_on_candidate_failure() {
        let directory = tempfile_dir("orbis-atomic");
        let executable = directory.join("orbis");
        fs::write(&executable, b"original").expect("old executable");
        assert!(atomic_replace(&executable, b"").is_err());
        assert_eq!(fs::read(&executable).expect("original remains"), b"original");
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn lock_rejects_concurrent_updates_and_cleans_up() {
        let directory = tempfile_dir("orbis-lock");
        let path = directory.join("self-update.lock");
        let first = UpdateLock::acquire_at(&path).expect("first lock");
        assert!(matches!(UpdateLock::acquire_at(&path), Err(SelfUpdateError::Concurrent)));
        drop(first);
        let second = UpdateLock::acquire_at(&path).expect("lock can be reacquired");
        drop(second);
        assert!(!path.exists());
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn cache_notice_is_quiet_when_current() {
        let directory = tempfile_dir("orbis-cache");
        let record = CacheRecord {
            checked_at_unix: now_unix(),
            latest_version: Some("0.1.0-beta.1".into()),
            release_url: None,
        };
        fs::write(cache_path(&directory), serde_json::to_vec(&record).expect("cache json"))
            .expect("cache");
        assert!(
            cached_notice(&directory, &Version::parse("0.1.0-beta.1").expect("version")).is_none()
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    fn tempfile_dir(prefix: &str) -> PathBuf {
        let directory = std::env::temp_dir()
            .join(format!("{prefix}-{}", TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&directory).expect("temp directory");
        directory
    }

    fn tempfile_path(prefix: &str) -> PathBuf {
        let directory = tempfile_dir(prefix);
        directory.join("fixture")
    }
}
