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
    /// Cached credentials are absent or expired; execution must not prompt.
    #[error("administrator credentials expired or are required; retry the operation")]
    AuthorizationRequired,
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

    /// Preflights administrator authorization once for a coordinated maintenance run.
    pub fn authorize_administrator(&self) -> Result<(), PrivilegeError> {
        if !self.runner.is_available("sudo") {
            return Err(PrivilegeError::Unavailable);
        }
        let output = self
            .runner
            .run(&authorization_command())
            .map_err(|error| PrivilegeError::Execution(PackageSource::Apt, error.to_string()))?;
        if !output.success() {
            return Err(PrivilegeError::Authorization);
        }
        Ok(())
    }

    /// Checks cached authorization without ever prompting.
    pub fn verify_administrator(&self) -> Result<(), PrivilegeError> {
        if !self.runner.is_available("sudo") {
            return Err(PrivilegeError::Unavailable);
        }
        let output = self
            .runner
            .run(&CommandSpec::new("sudo", ["-n", "-v"]).with_timeout(Duration::from_secs(10)))
            .map_err(|error| PrivilegeError::Execution(PackageSource::Apt, error.to_string()))?;
        if !output.success() {
            return Err(PrivilegeError::AuthorizationRequired);
        }
        Ok(())
    }

    /// Executes a provider operation with streaming output forwarded to an observer.
    ///
    /// This method uses the same privilege escalation and command construction as
    /// the standard `execute()`, but streams output lines to the provided observer
    /// as they arrive from the provider process.
    pub fn execute_streaming(
        &self,
        operation: &ProviderOperation,
        requirement: PrivilegeRequirement,
        observer: &dyn crate::progress::ProgressObserver,
    ) -> Result<CommandOutput, PrivilegeError> {
        if requirement == PrivilegeRequirement::Administrator {
            self.verify_administrator()?;
        }
        let command =
            provider_command(operation, requirement == PrivilegeRequirement::Administrator);
        let source = operation.source();
        let output = self
            .runner
            .run_streaming(&command, &|line, stream| {
                observer.on_event(&crate::progress::OperationEvent::ProviderOutput(
                    crate::progress::OutputLine { stream, content: line.to_owned() },
                ));
            })
            .map_err(|error| PrivilegeError::Execution(source, error.to_string()))?;
        if requirement == PrivilegeRequirement::Administrator && !output.success() {
            self.verify_administrator()?;
        }
        Ok(output)
    }
}

impl OperationExecutor for RealOperationExecutor {
    fn execute(
        &self,
        operation: &ProviderOperation,
        requirement: PrivilegeRequirement,
    ) -> Result<CommandOutput, PrivilegeError> {
        if requirement == PrivilegeRequirement::Administrator {
            self.verify_administrator()?;
        }

        let command =
            provider_command(operation, requirement == PrivilegeRequirement::Administrator);
        let source = operation.source();
        let output = self
            .runner
            .run(&command)
            .map_err(|error| PrivilegeError::Execution(source, error.to_string()))?;
        if requirement == PrivilegeRequirement::Administrator && !output.success() {
            self.verify_administrator()?;
        }
        Ok(output)
    }

    fn execute_with_observer(
        &self,
        operation: &ProviderOperation,
        requirement: PrivilegeRequirement,
        observer: &dyn crate::progress::ProgressObserver,
    ) -> Result<CommandOutput, PrivilegeError> {
        self.execute_streaming(operation, requirement, observer)
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
        ProviderOperation::Cargo { action, package_id } => ("cargo", *action, package_id.as_str()),
        ProviderOperation::Npm { action, package_id } => ("npm", *action, package_id.as_str()),
        ProviderOperation::Pnpm { action, package_id } => ("pnpm", *action, package_id.as_str()),
        ProviderOperation::Uv { action, package_id } => ("uv", *action, package_id.as_str()),
        ProviderOperation::Pipx { action, package_id } => ("pipx", *action, package_id.as_str()),
        ProviderOperation::Maintenance { operation } => {
            return maintenance_command(operation, elevated);
        }
    };

    let subcommand = match action {
        OperationAction::Install => "install",
        OperationAction::Remove => match operation.source() {
            PackageSource::Flatpak => "uninstall",
            PackageSource::Cargo => "uninstall",
            PackageSource::Apt
            | PackageSource::Snap
            | PackageSource::Pnpm
            | PackageSource::Uv
            | PackageSource::Pipx => "remove",
            PackageSource::Npm => "uninstall",
        },
    };
    if matches!(operation.source(), PackageSource::Apt) {
        // apt-get's `--` is the explicit end of options for the package name.
        args.insert(0, subcommand.into());
        args.push("--".into());
        args.push(package_id.into());
    } else if matches!(operation.source(), PackageSource::Cargo) {
        let command_args = if action == OperationAction::Remove {
            vec![subcommand.into(), "--package".into(), package_id.into()]
        } else {
            vec![subcommand.into(), package_id.into()]
        };
        args = command_args;
    } else if matches!(operation.source(), PackageSource::Npm | PackageSource::Pnpm) {
        let mut command_args = vec![subcommand.into()];
        if matches!(operation.source(), PackageSource::Npm | PackageSource::Pnpm) {
            command_args.push("--global".into());
        }
        command_args.extend(args);
        if matches!(operation.source(), PackageSource::Npm | PackageSource::Pnpm) {
            command_args.push("--".into());
        }
        command_args.push(package_id.into());
        args = command_args;
    } else if matches!(operation.source(), PackageSource::Uv) {
        let command = match action {
            OperationAction::Install => "install",
            OperationAction::Remove => "uninstall",
        };
        args = vec!["tool".into(), command.into(), package_id.into()];
    } else if matches!(operation.source(), PackageSource::Pipx) {
        let command = match action {
            OperationAction::Install => "install",
            OperationAction::Remove => "uninstall",
        };
        args = vec![command.into(), "--skip-maintenance".into(), package_id.into()];
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

fn maintenance_command(
    operation: &crate::transaction::MaintenanceOperation,
    elevated: bool,
) -> CommandSpec {
    use crate::transaction::MaintenanceOperation;

    let (program, mut args, apt_environment) = match operation {
        MaintenanceOperation::AptRefresh => {
            ("apt-get", vec!["update".into(), "-o".into(), "Dpkg::Use-Pty=0".into()], true)
        }
        MaintenanceOperation::AptUpgrade => (
            "apt-get",
            vec![
                "upgrade".into(),
                "--assume-yes".into(),
                "--no-remove".into(),
                "-o".into(),
                "Dpkg::Use-Pty=0".into(),
            ],
            true,
        ),
        MaintenanceOperation::AptAutoremove => (
            "apt-get",
            vec!["autoremove".into(), "--assume-yes".into(), "-o".into(), "Dpkg::Use-Pty=0".into()],
            true,
        ),
        MaintenanceOperation::FlatpakAppstream { scope } => (
            "flatpak",
            vec![
                scope_flag(*scope).into(),
                "update".into(),
                "--appstream".into(),
                "--assumeyes".into(),
                "--noninteractive".into(),
            ],
            false,
        ),
        MaintenanceOperation::FlatpakUpgrade { scope, refs } => {
            let mut args = vec![
                scope_flag(*scope).into(),
                "update".into(),
                "--no-related".into(),
                "--assumeyes".into(),
                "--noninteractive".into(),
            ];
            args.extend(refs.iter().cloned());
            ("flatpak", args, false)
        }
        MaintenanceOperation::SnapRefreshCheck => {
            ("snap", vec!["refresh".into(), "--list".into()], false)
        }
        MaintenanceOperation::SnapUpgrade { package_ids } => {
            let mut args = vec!["refresh".into()];
            args.extend(package_ids.iter().cloned());
            ("snap", args, false)
        }
        MaintenanceOperation::NpmUpgrade { package_ids } => {
            let mut args = vec!["install".into(), "--global".into(), "--".into()];
            args.extend(package_ids.iter().cloned());
            ("npm", args, false)
        }
        MaintenanceOperation::PnpmUpgrade { package_ids } => {
            let mut args = vec!["update".into(), "--global".into(), "--latest".into(), "--".into()];
            args.extend(package_ids.iter().cloned());
            ("pnpm", args, false)
        }
        MaintenanceOperation::UvUpgrade { package_ids } => {
            let mut args = vec!["tool".into(), "upgrade".into()];
            args.extend(package_ids.iter().cloned());
            ("uv", args, false)
        }
        MaintenanceOperation::PipxUpgrade { package_ids } => {
            let mut args = vec!["upgrade".into(), "--skip-maintenance".into()];
            args.extend(package_ids.iter().cloned());
            ("pipx", args, false)
        }
    };

    let mut command = if elevated {
        let mut sudo_args = vec!["-n".into(), program.into()];
        sudo_args.append(&mut args);
        CommandSpec::new("sudo", sudo_args)
    } else {
        CommandSpec::new(program, args)
    };
    if apt_environment {
        command = command.with_env("LC_ALL", "C").with_env("DEBIAN_FRONTEND", "noninteractive");
    }
    // Real maintenance mutations intentionally have no generic wall-clock timeout.
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
    fn developer_mutations_are_user_local_and_shell_free() {
        let cases = [
            (
                ProviderOperation::Cargo {
                    action: OperationAction::Remove,
                    package_id: "cargo-edit".into(),
                },
                "cargo",
                vec!["uninstall", "--package", "cargo-edit"],
            ),
            (
                ProviderOperation::Npm {
                    action: OperationAction::Remove,
                    package_id: "@scope/tool".into(),
                },
                "npm",
                vec!["uninstall", "--global", "--", "@scope/tool"],
            ),
            (
                ProviderOperation::Pnpm {
                    action: OperationAction::Remove,
                    package_id: "tool".into(),
                },
                "pnpm",
                vec!["remove", "--global", "--", "tool"],
            ),
            (
                ProviderOperation::Uv {
                    action: OperationAction::Install,
                    package_id: "ruff".into(),
                },
                "uv",
                vec!["tool", "install", "ruff"],
            ),
            (
                ProviderOperation::Pipx {
                    action: OperationAction::Remove,
                    package_id: "black".into(),
                },
                "pipx",
                vec!["uninstall", "--skip-maintenance", "black"],
            ),
        ];
        for (operation, program, args) in cases {
            let command = provider_command(&operation, false);
            assert_eq!(command.program, program);
            assert_eq!(command.args, args);
            assert_eq!(command.timeout, None);
            assert!(!command.args.iter().any(|argument| argument == "sh" || argument == "-c"));
        }
    }

    #[test]
    fn developer_maintenance_is_exact_and_unprivileged() {
        let operation = ProviderOperation::Maintenance {
            operation: crate::transaction::MaintenanceOperation::UvUpgrade {
                package_ids: vec!["ruff".into(), "black".into()],
            },
        };
        let command = provider_command(&operation, false);
        assert_eq!(command.program, "uv");
        assert_eq!(command.args, ["tool", "upgrade", "ruff", "black"]);
        assert_eq!(command.timeout, None);
    }

    #[test]
    fn authorization_has_a_separate_bounded_timeout() {
        let auth = authorization_command();
        assert_eq!(auth.timeout, Some(Duration::from_secs(300)));
    }

    #[test]
    fn maintenance_upgrade_uses_ordinary_apt_upgrade_without_removal_override() {
        let operation = ProviderOperation::Maintenance {
            operation: crate::transaction::MaintenanceOperation::AptUpgrade,
        };
        let command = provider_command(&operation, true);
        assert_eq!(command.program, "sudo");
        assert!(command.args.contains(&"upgrade".into()));
        assert!(command.args.contains(&"--no-remove".into()));
        assert!(!command.args.iter().any(|argument| argument.contains("full-upgrade")));
        assert_eq!(command.timeout, None);
    }

    #[test]
    fn snap_check_is_read_only_and_unprivileged() {
        let operation = ProviderOperation::Maintenance {
            operation: crate::transaction::MaintenanceOperation::SnapRefreshCheck,
        };
        let command = provider_command(&operation, false);
        assert_eq!(command.program, "snap");
        assert_eq!(command.args, ["refresh", "--list"]);
        assert_eq!(command.timeout, None);
    }
    #[derive(Default)]
    struct RecordingRunner {
        commands: std::sync::Mutex<Vec<CommandSpec>>,
        reject_auth: bool,
    }
    impl crate::process::CommandRunner for RecordingRunner {
        fn is_available(&self, _: &str) -> bool {
            true
        }
        fn run(
            &self,
            command: &CommandSpec,
        ) -> Result<CommandOutput, crate::process::ProcessError> {
            self.commands.lock().unwrap().push(command.clone());
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(if self.reject_auth { 1 } else { 0 }),
            })
        }
    }

    #[test]
    fn worker_execution_never_requests_interactive_credentials() {
        for reject_auth in [true, false] {
            let runner = std::sync::Arc::new(RecordingRunner { reject_auth, ..Default::default() });
            let executor = RealOperationExecutor::new(runner.clone());
            let operation = ProviderOperation::Maintenance {
                operation: crate::transaction::MaintenanceOperation::AptRefresh,
            };
            let result = executor.execute_streaming(
                &operation,
                PrivilegeRequirement::Administrator,
                &crate::progress::SilentObserver,
            );
            let commands = runner.commands.lock().unwrap();
            assert!(
                commands.iter().all(|command| command.args.first().is_some_and(|arg| arg == "-n"))
            );
            if reject_auth {
                assert!(matches!(result, Err(PrivilegeError::AuthorizationRequired)));
                assert_eq!(commands.len(), 1, "no provider mutation after expired auth");
            } else {
                assert_eq!(commands.len(), 2);
                assert!(result.is_ok());
            }
        }
    }

    #[test]
    fn explicit_authorization_inherits_secure_terminal_and_user_operations_skip_it() {
        let runner = std::sync::Arc::new(RecordingRunner::default());
        let executor = RealOperationExecutor::new(runner.clone());
        executor.authorize_administrator().unwrap();
        let commands = runner.commands.lock().unwrap();
        assert_eq!(commands[0].args, ["-v"]);
        assert_eq!(commands[0].stdin, StdioMode::Inherit);
        assert_eq!(commands[0].stdout, StdioMode::Inherit);
        drop(commands);
        let runner = std::sync::Arc::new(RecordingRunner::default());
        let executor = RealOperationExecutor::new(runner.clone());
        executor
            .execute(
                &ProviderOperation::Uv {
                    action: OperationAction::Install,
                    package_id: "ruff".into(),
                },
                PrivilegeRequirement::None,
            )
            .unwrap();
        let commands = runner.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "uv");
    }
}
