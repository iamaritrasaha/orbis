//! Safe, shell-free process execution with an injectable test seam.

use std::{
    io::{self, Read},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::Duration,
};

use thiserror::Error;
use wait_timeout::ChildExt;

/// A structured command invocation. Arguments never pass through a shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    /// Executable name or absolute path.
    pub program: String,
    /// Individual arguments.
    pub args: Vec<String>,
    /// Optional upper bound for the process lifetime.
    pub timeout: Option<Duration>,
}

impl CommandSpec {
    /// Creates a command specification with no timeout.
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            timeout: None,
        }
    }

    /// Adds a timeout to this command.
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// Captured process result, including non-zero exit statuses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Exit status code, or `None` when terminated without one.
    pub status: Option<i32>,
}

impl CommandOutput {
    /// Whether the process exited successfully.
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Failures belonging to process setup or lifecycle rather than provider parsing.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum ProcessError {
    /// The executable was not found on PATH.
    #[error("executable `{program}` was not found on PATH")]
    NotFound { program: String },
    /// The process could not be started or its pipes could not be read.
    #[error("could not execute `{program}`: {message}")]
    Io { program: String, message: String },
    /// The process exceeded its configured deadline.
    #[error("`{program}` exceeded its {timeout_ms} ms timeout")]
    Timeout { program: String, timeout_ms: u128 },
    /// A captured pipe reader failed.
    #[error("could not capture `{program}` output: {message}")]
    Capture { program: String, message: String },
}

/// The process seam used by every provider.
pub trait CommandRunner: Send + Sync {
    /// Returns whether an executable can be found without running it.
    fn is_available(&self, program: &str) -> bool;
    /// Runs a structured command.
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput, ProcessError>;
}

/// The production command runner.
#[derive(Clone, Default)]
pub struct RealCommandRunner;

impl RealCommandRunner {
    /// Creates a system command runner.
    pub const fn new() -> Self {
        Self
    }
}

impl CommandRunner for RealCommandRunner {
    fn is_available(&self, program: &str) -> bool {
        find_on_path(program).is_some()
    }

    fn run(&self, command: &CommandSpec) -> Result<CommandOutput, ProcessError> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    ProcessError::NotFound { program: command.program.clone() }
                } else {
                    ProcessError::Io {
                        program: command.program.clone(),
                        message: error.to_string(),
                    }
                }
            })?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let stdout_reader = thread::spawn(move || read_pipe(stdout));
        let stderr_reader = thread::spawn(move || read_pipe(stderr));

        let status = match command.timeout {
            Some(timeout) => match child.wait_timeout(timeout).map_err(|error| {
                ProcessError::Io { program: command.program.clone(), message: error.to_string() }
            })? {
                Some(status) => status,
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProcessError::Timeout {
                        program: command.program.clone(),
                        timeout_ms: timeout.as_millis(),
                    });
                }
            },
            None => child.wait().map_err(|error| ProcessError::Io {
                program: command.program.clone(),
                message: error.to_string(),
            })?,
        };

        let stdout = stdout_reader.join().map_err(|_| ProcessError::Capture {
            program: command.program.clone(),
            message: "stdout reader panicked".into(),
        })??;
        let stderr = stderr_reader.join().map_err(|_| ProcessError::Capture {
            program: command.program.clone(),
            message: "stderr reader panicked".into(),
        })??;

        Ok(CommandOutput { stdout, stderr, status: status.code() })
    }
}

fn read_pipe(mut pipe: impl Read) -> Result<String, ProcessError> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).map_err(|error| ProcessError::Capture {
        program: "process".into(),
        message: error.to_string(),
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn find_on_path(program: &str) -> Option<std::path::PathBuf> {
    if program.contains('/') {
        let path = std::path::Path::new(program);
        return path.is_file().then(|| path.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// Shared ownership type used by providers.
pub type SharedRunner = Arc<dyn CommandRunner>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_spec_keeps_arguments_separate() {
        let command = CommandSpec::new("apt-cache", ["search", "a package; echo unsafe"]);
        assert_eq!(command.args[1], "a package; echo unsafe");
        assert_eq!(command.program, "apt-cache");
    }

    #[test]
    fn real_runner_captures_output() {
        let runner = RealCommandRunner::new();
        let output = runner.run(&CommandSpec::new("printf", ["hello"])).expect("printf works");
        assert_eq!(output.stdout, "hello");
        assert!(output.success());
    }
}
