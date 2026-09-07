//! Safe, shell-free process execution with an injectable test seam.

use std::{
    collections::BTreeMap,
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
    /// Environment variables set only for this invocation.
    pub env: BTreeMap<String, String>,
    /// Standard input handling.
    pub stdin: StdioMode,
    /// Standard output handling.
    pub stdout: StdioMode,
    /// Standard error handling.
    pub stderr: StdioMode,
}

/// How a process stream is connected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdioMode {
    /// Do not provide or capture the stream.
    Null,
    /// Capture the stream in [`CommandOutput`].
    Capture,
    /// Inherit the stream from Orbis.
    Inherit,
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
            env: BTreeMap::new(),
            stdin: StdioMode::Null,
            stdout: StdioMode::Capture,
            stderr: StdioMode::Capture,
        }
    }

    /// Adds a timeout to this command.
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Adds or replaces an environment variable for this invocation only.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Configures standard input handling.
    pub const fn with_stdin(mut self, mode: StdioMode) -> Self {
        self.stdin = mode;
        self
    }

    /// Configures standard output and standard error handling together.
    pub const fn with_stdio(mut self, mode: StdioMode) -> Self {
        self.stdout = mode;
        self.stderr = mode;
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

    /// Executes a command and delivers output lines to a callback as they arrive.
    ///
    /// The default implementation falls back to `run()` and replays captured output.
    fn run_streaming(
        &self,
        command: &CommandSpec,
        on_line: &(dyn Fn(&str, crate::progress::OutputStream) + Send + Sync),
    ) -> Result<CommandOutput, ProcessError> {
        let output = self.run(command)?;
        for line in output.stdout.lines() {
            on_line(line, crate::progress::OutputStream::Stdout);
        }
        for line in output.stderr.lines() {
            on_line(line, crate::progress::OutputStream::Stderr);
        }
        Ok(output)
    }
}

impl<T> CommandRunner for Arc<T>
where
    T: CommandRunner + ?Sized,
{
    fn is_available(&self, program: &str) -> bool {
        self.as_ref().is_available(program)
    }

    fn run(&self, command: &CommandSpec) -> Result<CommandOutput, ProcessError> {
        self.as_ref().run(command)
    }

    fn run_streaming(
        &self,
        command: &CommandSpec,
        on_line: &(dyn Fn(&str, crate::progress::OutputStream) + Send + Sync),
    ) -> Result<CommandOutput, ProcessError> {
        self.as_ref().run_streaming(command, on_line)
    }
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
            .envs(&command.env)
            .stdin(to_stdio(command.stdin))
            .stdout(to_stdio(command.stdout))
            .stderr(to_stdio(command.stderr))
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

        let stdout_reader = (command.stdout == StdioMode::Capture).then(|| {
            let stdout = child.stdout.take().expect("stdout was piped");
            thread::spawn(move || read_pipe(stdout))
        });
        let stderr_reader = (command.stderr == StdioMode::Capture).then(|| {
            let stderr = child.stderr.take().expect("stderr was piped");
            thread::spawn(move || read_pipe(stderr))
        });

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

        let stdout = join_reader(stdout_reader, &command.program)?;
        let stderr = join_reader(stderr_reader, &command.program)?;

        Ok(CommandOutput { stdout, stderr, status: status.code() })
    }

    fn run_streaming(
        &self,
        command: &CommandSpec,
        on_line: &(dyn Fn(&str, crate::progress::OutputStream) + Send + Sync),
    ) -> Result<CommandOutput, ProcessError> {
        use std::io::BufRead;

        let mut child = Command::new(&command.program)
            .args(&command.args)
            .envs(&command.env)
            .stdin(to_stdio(command.stdin))
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

        let stdout_pipe = child.stdout.take().expect("stdout was piped");
        let stderr_pipe = child.stderr.take().expect("stderr was piped");

        let mut stdout_output = String::new();
        let mut stderr_output = String::new();
        let mut status_result = None;

        thread::scope(|s| {
            let stdout_thread = s.spawn(|| {
                let mut reader = std::io::BufReader::new(stdout_pipe);
                let mut output = String::new();
                let mut line = String::new();
                while let Ok(bytes) = reader.read_line(&mut line) {
                    if bytes == 0 {
                        break;
                    }
                    let trimmed = if line.ends_with('\n') {
                        line[..line.len() - 1].strip_suffix('\r').unwrap_or(&line[..line.len() - 1])
                    } else {
                        &line
                    };
                    on_line(trimmed, crate::progress::OutputStream::Stdout);
                    output.push_str(&line);
                    line.clear();
                }
                output
            });

            let stderr_thread = s.spawn(|| {
                let mut reader = std::io::BufReader::new(stderr_pipe);
                let mut output = String::new();
                let mut line = String::new();
                while let Ok(bytes) = reader.read_line(&mut line) {
                    if bytes == 0 {
                        break;
                    }
                    let trimmed = if line.ends_with('\n') {
                        line[..line.len() - 1].strip_suffix('\r').unwrap_or(&line[..line.len() - 1])
                    } else {
                        &line
                    };
                    on_line(trimmed, crate::progress::OutputStream::Stderr);
                    output.push_str(&line);
                    line.clear();
                }
                output
            });

            let status = match command.timeout {
                Some(timeout) => match child.wait_timeout(timeout) {
                    Ok(Some(status)) => Ok(status),
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        Err(ProcessError::Timeout {
                            program: command.program.clone(),
                            timeout_ms: timeout.as_millis(),
                        })
                    }
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        Err(ProcessError::Io {
                            program: command.program.clone(),
                            message: error.to_string(),
                        })
                    }
                },
                None => child.wait().map_err(|error| ProcessError::Io {
                    program: command.program.clone(),
                    message: error.to_string(),
                }),
            };

            stdout_output = stdout_thread.join().unwrap_or_default();
            stderr_output = stderr_thread.join().unwrap_or_default();
            status_result = Some(status);
        });

        let status = status_result.expect("status_result populated in scope")?;
        Ok(CommandOutput { stdout: stdout_output, stderr: stderr_output, status: status.code() })
    }
}

fn to_stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Null => Stdio::null(),
        StdioMode::Capture => Stdio::piped(),
        StdioMode::Inherit => Stdio::inherit(),
    }
}

fn join_reader(
    reader: Option<thread::JoinHandle<Result<String, ProcessError>>>,
    program: &str,
) -> Result<String, ProcessError> {
    reader
        .map(|reader| {
            reader.join().map_err(|_| ProcessError::Capture {
                program: program.into(),
                message: "pipe reader panicked".into(),
            })?
        })
        .transpose()
        .map(|output| output.unwrap_or_default())
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

    #[test]
    fn real_runner_streaming_captures_output() {
        let runner = RealCommandRunner::new();
        let delivered = std::sync::Mutex::new(Vec::new());
        let output = runner
            .run_streaming(&CommandSpec::new("printf", ["line1\nline2\n"]), &|line, stream| {
                assert_eq!(stream, crate::progress::OutputStream::Stdout);
                delivered.lock().unwrap().push(line.to_owned());
            })
            .expect("printf works");
        assert_eq!(delivered.into_inner().unwrap(), vec!["line1", "line2"]);
        assert_eq!(output.stdout, "line1\nline2\n");
        assert!(output.success());
    }

    #[test]
    fn real_runner_streaming_timeout_terminates_child_and_returns_error() {
        let runner = RealCommandRunner::new();
        let start = std::time::Instant::now();
        let mut command = CommandSpec::new("sleep", ["2"]);
        command.timeout = Some(Duration::from_millis(50));
        let result = runner.run_streaming(&command, &|_line, _stream| {});
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(ProcessError::Timeout { .. })),
            "expected Timeout error, got {:?}",
            result
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "streaming timeout should bound execution time (took {:?})",
            elapsed
        );
    }
}
