//! Package-provider implementations and their shared contract.

pub mod apt;
pub mod developer;
pub mod flatpak;
pub mod snap;

use std::time::Duration;

use thiserror::Error;

use crate::{
    diagnostics::DiagnosticCheck,
    maintenance::MaintenanceProvider,
    models::{Package, PackageSource, SourceInfo},
    process::{CommandOutput, CommandSpec, ProcessError, SharedRunner},
    transaction::{
        OperationPlan, OperationRequest, ProviderOperation, TransactionError, VerificationResult,
    },
};

/// A provider's safe read-only interface for Milestone 1.
pub trait Provider: Send + Sync {
    /// Canonical source identity.
    fn source(&self) -> PackageSource;
    /// Current availability and capabilities.
    fn source_info(&self) -> SourceInfo;
    /// Search provider metadata.
    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError>;
    /// Resolve one provider-specific package ID or friendly name.
    fn info(&self, package_id: &str) -> Result<Package, ProviderError>;
    /// Run a bounded, safe health check.
    fn diagnostic(&self) -> DiagnosticCheck;
    /// Whether this provider can safely participate in unqualified resolution.
    fn supports_unqualified_resolution(&self) -> bool {
        true
    }
    /// Whether incomplete resolution means mutations must be source-qualified.
    fn requires_source_qualification(&self) -> bool {
        false
    }
}

/// Mutation capability kept separate from the read-only provider contract.
pub trait TransactionProvider: Provider + MaintenanceProvider {
    /// Produces a provider-backed plan without mutating the machine.
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError>;
    /// Converts a completed plan into one exact typed provider operation.
    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError>;
    /// Verifies the exact target state after an attempted operation.
    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError>;
}

/// Provider-level failures with both friendly and technical context.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The provider's required executable is absent.
    #[error("{package_source} is unavailable because `{program}` is not installed")]
    Unavailable { package_source: PackageSource, program: String },
    /// No package matched a provider-specific ID.
    #[error("{package_source} has no package matching `{query}`")]
    NotFound { package_source: PackageSource, query: String },
    /// A command returned a failure status.
    #[error("{package_source} could not {operation}. {message}")]
    Command {
        package_source: PackageSource,
        operation: String,
        message: String,
        technical: Option<String>,
    },
    /// Provider output did not match the documented shape.
    #[error("{package_source} returned unreadable metadata while trying to {operation}")]
    Parse { package_source: PackageSource, operation: String, technical: String },
}

impl ProviderError {
    /// Technical context suitable for JSON or debug output.
    pub fn technical_message(&self) -> Option<String> {
        match self {
            Self::Command { technical, .. } => technical.clone(),
            Self::Parse { technical, .. } => Some(technical.clone()),
            Self::Unavailable { .. } | Self::NotFound { .. } => None,
        }
    }
}

pub(crate) fn execute(
    runner: &SharedRunner,
    source: PackageSource,
    operation: &str,
    command: CommandSpec,
) -> Result<CommandOutput, ProviderError> {
    runner.run(&command).map_err(|error| match error {
        ProcessError::NotFound { program } => {
            ProviderError::Unavailable { package_source: source, program }
        }
        other => ProviderError::Command {
            package_source: source,
            operation: operation.into(),
            message: "the local command could not be completed".into(),
            technical: Some(other.to_string()),
        },
    })
}

pub(crate) fn expect_success(
    source: PackageSource,
    operation: &str,
    output: CommandOutput,
) -> Result<CommandOutput, ProviderError> {
    if output.success() {
        return Ok(output);
    }
    let detail = output
        .stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("the provider returned a non-zero status")
        .trim();
    Err(ProviderError::Command {
        package_source: source,
        operation: operation.into(),
        message: friendly_command_failure(source, detail),
        technical: Some(format!("exit={:?}; stderr={}", output.status, output.stderr.trim())),
    })
}

pub(crate) fn short_timeout() -> Duration {
    Duration::from_secs(12)
}

fn friendly_command_failure(source: PackageSource, detail: &str) -> String {
    match source {
        PackageSource::Flatpak => format!(
            "Check `flatpak remotes` and ensure the configured remote is reachable. ({detail})"
        ),
        PackageSource::Snap => {
            format!("Check that snapd is running and that the Snap Store is reachable. ({detail})")
        }
        PackageSource::Apt => {
            format!("Check the local package metadata. ({detail})")
        }
        PackageSource::Cargo => {
            format!("Check Cargo's configured registry and toolchain. ({detail})")
        }
        PackageSource::Npm => {
            format!("Check the npm registry and global prefix configuration. ({detail})")
        }
        PackageSource::Pnpm => {
            format!("Check pnpm's global directory and registry configuration. ({detail})")
        }
        PackageSource::Uv => {
            format!("Check uv's tool directory and Python index configuration. ({detail})")
        }
        PackageSource::Pipx => {
            format!("Check pipx's user environment and Python interpreter. ({detail})")
        }
    }
}
