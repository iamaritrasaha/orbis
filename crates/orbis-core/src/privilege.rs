//! Narrow privilege boundary for validated provider operations.

use std::time::Duration;

use thiserror::Error;

use crate::{
    models::PackageSource,
    process::{CommandOutput, CommandSpec, SharedRunner, StdioMode},
    transaction::{
        InstallScope, OperationAction, OperationExecutor, PrivilegeRequirement, ProviderOperation,
    },
};

/// Errors from authorization or execution at the privilege boundary.
#[derive(Debug, Error)]
pub enum PrivilegeError {
    /// The local authorization helper is unavailable.
    #[error("administrator authorization is unavailable because `sudo` is not installed")]
    Unavailable,
    /// The user did not complete authorization.
    #[error("administrator authorization was not granted")]
    Authorization,
    /// The typed provider command could not be completed.
    #[error("{0} command could not be completed: {1}")]
    Execution(PackageSource, String),
}

/// Production executor. It never accepts a caller-provided executable or shell string.
pub struct RealOperationExecutor {
    runner: SharedRunner,
}

impl RealOperationExecutor {
    /// Creates an executor using the same injected process seam as the providers.
    pub fn new(runner: SharedRunner) -> Self {
        Self { runner }
    }
}

impl OperationExecutor for RealOperationExecutor {
    fn execute(
        &self,
        operation: &ProviderOperation,
        requirement: PrivilegeRequirement,
    ) -> Result<CommandOutput, PrivilegeError> {
        if requirement == PrivilegeRequirement::Administrator {
            if !self.runner.is_available("sudo") {
                return Err(PrivilegeError::Unavailable);
            }
            let auth = authorization_command();
            let output = self.runner.run(&auth).map_err(|error| {
                PrivilegeError::Execution(PackageSource::Apt, error.to_string())
            })?;
            if !output.success() {
                return Err(PrivilegeError::Authorization);
            }
        }

        let command =
            provider_command(operation, requirement == PrivilegeRequirement::Administrator);
        let source = operation.source();
        let output = self
            .runner
            .run(&command)
            .map_err(|error| PrivilegeError::Execution(source, error.to_string()))?;
        Ok(output)
    }
}

fn provider_command(operation: &ProviderOperation, elevated: bool) -> CommandSpec {
    let mut args = Vec::new();
    let (program, action, package_id) = match operation {
        ProviderOperation::Apt { action, package_id } => {
            args.extend(["-o".into(), "Dpkg::Use-Pty=0".into(), "--assume-yes".into()]);
            ("apt-get", *action, package_id.as_str())
        }
        ProviderOperation::Flatpak { action, package_id, scope, remote } => {
            args.extend([
                scope_flag(*scope).into(),
                "--assumeyes".into(),
                "--noninteractive".into(),
            ]);
            if let Some(remote) = remote {
                args.push(remote.clone());
            }
            ("flatpak", *action, package_id.as_str())
        }
        ProviderOperation::Snap { action, package_id, channel } => {
            if let Some(channel) = channel {
                args.push(format!("--channel={channel}"));
            }
            ("snap", *action, package_id.as_str())
        }
    };

    let subcommand = match action {
        OperationAction::Install => "install",
        OperationAction::Remove => match operation.source() {
            PackageSource::Flatpak => "uninstall",
            PackageSource::Apt | PackageSource::Snap => "remove",
        },
    };
    if matches!(operation.source(), PackageSource::Apt) {
        // apt-get's `--` is the explicit end of options for the package name.
        args.insert(0, subcommand.into());
        args.push("--".into());
        args.push(package_id.into());
    } else {
        let mut command_args = vec![subcommand.into()];
        command_args.extend(args);
        command_args.push(package_id.into());
        args = command_args;
    }

    let mut command = if elevated {
        let mut sudo_args = vec!["-n".into()];
        sudo_args.push(program.into());
        sudo_args.extend(args);
        CommandSpec::new("sudo", sudo_args)
    } else {
        CommandSpec::new(program, args)
    };
    if matches!(operation.source(), PackageSource::Apt) {
        command = command.with_env("LC_ALL", "C").with_env("DEBIAN_FRONTEND", "noninteractive");
    }
    // Mutation commands intentionally have no generic wall-clock timeout. A provider-aware
    // cancellation design can be added later without killing an active package transaction.
    command
}

fn authorization_command() -> CommandSpec {
    CommandSpec::new("sudo", ["-v"])
        .with_timeout(Duration::from_secs(300))
        .with_stdio(StdioMode::Inherit)
        .with_stdin(StdioMode::Inherit)
}

fn scope_flag(scope: InstallScope) -> &'static str {
    match scope {
        InstallScope::System => "--system",
        InstallScope::User => "--user",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_apt_operation_has_no_shell_interpretation() {
        let operation = ProviderOperation::Apt {
            action: OperationAction::Install,
            package_id: "package; echo unsafe".into(),
        };
        let command = provider_command(&operation, true);
        assert_eq!(command.program, "sudo");
        assert!(command.args.contains(&"package; echo unsafe".into()));
        assert!(!command.args.iter().any(|argument| argument == "sh" || argument == "-c"));
        assert_eq!(command.args[0..3], ["-n", "apt-get", "install"]);
    }

    #[test]
    fn flatpak_scope_is_explicit() {
        let operation = ProviderOperation::Flatpak {
            action: OperationAction::Remove,
            package_id: "org.example.App".into(),
            scope: InstallScope::User,
            remote: None,
        };
        let command = provider_command(&operation, false);
        assert_eq!(command.program, "flatpak");
        assert!(command.args.contains(&"--user".into()));
        assert!(!command.args.contains(&"--delete-data".into()));
    }

    #[test]
    fn elevated_flatpak_does_not_pass_sudo_flags_to_flatpak() {
        let operation = ProviderOperation::Flatpak {
            action: OperationAction::Install,
            package_id: "org.example.App".into(),
            scope: InstallScope::System,
            remote: Some("flathub".into()),
        };
        let command = provider_command(&operation, true);
        assert_eq!(command.args[0..3], ["-n", "flatpak", "install"]);
        assert!(!command.args[3..].contains(&"-n".into()));
        assert!(command.args.contains(&"--system".into()));
    }

    #[test]
    fn typed_mutation_commands_have_no_generic_timeout() {
        let operation = ProviderOperation::Snap {
            action: OperationAction::Remove,
            package_id: "example".into(),
            channel: None,
        };
        assert_eq!(provider_command(&operation, true).timeout, None);
    }

    #[test]
    fn authorization_has_a_separate_bounded_timeout() {
        let auth = authorization_command();
        assert_eq!(auth.timeout, Some(Duration::from_secs(300)));
    }
}
