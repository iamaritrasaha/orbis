//! Safe, shell-free process execution with an injectable test seam.

use std::{
    collections::BTreeMap,
    io::{self, Read},
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
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

    #[cfg(not(unix))]
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

    #[cfg(not(unix))]
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

    #[cfg(unix)]
    fn run_streaming(
        &self,
        command: &CommandSpec,
        on_line: &(dyn Fn(&str, crate::progress::OutputStream) + Send + Sync),
    ) -> Result<CommandOutput, ProcessError> {
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
        let (events_tx, events_rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let stdout_thread = thread::spawn({
            let events_tx = events_tx.clone();
            let cancel_rx = cancel_rx;
            move || {
                read_stream(
                    stdout_pipe,
                    crate::progress::OutputStream::Stdout,
                    events_tx,
                    cancel_rx,
                )
            }
        });
        let (cancel_stderr_tx, cancel_stderr_rx) = mpsc::channel();
        let stderr_thread = thread::spawn({
            let events_tx = events_tx.clone();
            move || {
                read_stream(
                    stderr_pipe,
                    crate::progress::OutputStream::Stderr,
                    events_tx,
                    cancel_stderr_rx,
                )
            }
        });
        drop(events_tx);

        let mut stdout_capture = StreamCapture::new(crate::progress::OutputStream::Stdout);
        let mut stderr_capture = StreamCapture::new(crate::progress::OutputStream::Stderr);
        let mut reader_error = None;
        let mut reader_done = [false, false];
        let start = Instant::now();
        let status = loop {
            let wait = command
                .timeout
                .map(|timeout| timeout.saturating_sub(start.elapsed()).min(STREAM_POLL_INTERVAL))
                .unwrap_or(STREAM_POLL_INTERVAL);
            match child.wait_timeout(wait) {
                Ok(Some(status)) => break status,
                Ok(None) if command.timeout.is_some_and(|timeout| start.elapsed() >= timeout) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    cancel_stream_readers(
                        cancel_tx,
                        cancel_stderr_tx,
                        stdout_thread,
                        stderr_thread,
                    );
                    return Err(ProcessError::Timeout {
                        program: command.program.clone(),
                        timeout_ms: command.timeout.expect("timeout is set").as_millis(),
                    });
                }
                Ok(None) => {
                    while let Ok(event) = events_rx.try_recv() {
                        mark_stream_done(&event, &mut reader_done);
                        handle_stream_event(
                            event,
                            on_line,
                            &mut stdout_capture,
                            &mut stderr_capture,
                            &mut reader_error,
                        );
                    }
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    cancel_stream_readers(
                        cancel_tx,
                        cancel_stderr_tx,
                        stdout_thread,
                        stderr_thread,
                    );
                    return Err(ProcessError::Io {
                        program: command.program.clone(),
                        message: error.to_string(),
                    });
                }
            }
        };

        let grace_deadline = Instant::now() + STREAM_DRAIN_GRACE;
        if reader_done != [true, true] {
            while Instant::now() < grace_deadline {
                let remaining = grace_deadline.saturating_duration_since(Instant::now());
                match events_rx.recv_timeout(remaining.min(STREAM_POLL_INTERVAL)) {
                    Ok(event) => {
                        mark_stream_done(&event, &mut reader_done);
                        handle_stream_event(
                            event,
                            on_line,
                            &mut stdout_capture,
                            &mut stderr_capture,
                            &mut reader_error,
                        );
                        if reader_done == [true, true] {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if reader_error.is_some() {
                    break;
                }
            }
        }
        cancel_stream_readers(cancel_tx, cancel_stderr_tx, stdout_thread, stderr_thread);
        while let Ok(event) = events_rx.try_recv() {
            mark_stream_done(&event, &mut reader_done);
            handle_stream_event(
                event,
                on_line,
                &mut stdout_capture,
                &mut stderr_capture,
                &mut reader_error,
            );
        }
        stdout_capture.flush(on_line);
        stderr_capture.flush(on_line);

        if let Some(message) = reader_error {
            return Err(ProcessError::Capture { program: command.program.clone(), message });
        }
        Ok(CommandOutput {
            stdout: stdout_capture.output,
            stderr: stderr_capture.output,
            status: status.code(),
        })
    }
}

#[cfg(unix)]
const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(unix)]
const STREAM_DRAIN_GRACE: Duration = Duration::from_millis(100);

#[cfg(unix)]
enum StreamEvent {
    Data(crate::progress::OutputStream, Vec<u8>),
    Done(crate::progress::OutputStream),
    Error(crate::progress::OutputStream, String),
}

#[cfg(unix)]
struct StreamCapture {
    stream: crate::progress::OutputStream,
    output: String,
    pending: String,
}

#[cfg(unix)]
impl StreamCapture {
    fn new(stream: crate::progress::OutputStream) -> Self {
        Self { stream, output: String::new(), pending: String::new() }
    }

    fn push(
        &mut self,
        bytes: &[u8],
        on_line: &(dyn Fn(&str, crate::progress::OutputStream) + Send + Sync),
    ) {
        let text = String::from_utf8_lossy(bytes);
        self.output.push_str(&text);
        self.pending.push_str(&text);
        while let Some(index) = self.pending.find('\n') {
            let mut line = self.pending.drain(..=index).collect::<String>();
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            on_line(&line, self.stream);
        }
    }

    fn flush(&mut self, on_line: &(dyn Fn(&str, crate::progress::OutputStream) + Send + Sync)) {
        if !self.pending.is_empty() {
            on_line(&self.pending, self.stream);
            self.pending.clear();
        }
    }
}

#[cfg(unix)]
fn read_stream(
    mut pipe: impl Read + std::os::fd::AsFd,
    stream: crate::progress::OutputStream,
    events_tx: mpsc::Sender<StreamEvent>,
    cancel_rx: mpsc::Receiver<()>,
) {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    let timeout = Timespec::try_from(Duration::from_millis(20)).expect("valid poll interval");
    let mut buffer = [0_u8; 8192];
    loop {
        if cancel_rx.try_recv().is_ok() {
            break;
        }
        let ready = {
            let mut poll_fds =
                [PollFd::new(&pipe, PollFlags::IN | PollFlags::HUP | PollFlags::ERR)];
            poll(&mut poll_fds, Some(&timeout)).map(|_| !poll_fds[0].revents().is_empty())
        };
        match ready {
            Ok(false) => continue,
            Ok(true) => match pipe.read(&mut buffer) {
                Ok(0) => {
                    let _ = events_tx.send(StreamEvent::Done(stream));
                    break;
                }
                Ok(bytes) => {
                    if events_tx.send(StreamEvent::Data(stream, buffer[..bytes].to_vec())).is_err()
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let _ = events_tx.send(StreamEvent::Error(stream, error.to_string()));
                    break;
                }
            },
            Err(error) => {
                let _ = events_tx.send(StreamEvent::Error(stream, error.to_string()));
                break;
            }
        }
    }
}

#[cfg(unix)]
fn mark_stream_done(event: &StreamEvent, done: &mut [bool; 2]) {
    if let StreamEvent::Done(stream) = event {
        let index = if *stream == crate::progress::OutputStream::Stderr { 1 } else { 0 };
        done[index] = true;
    }
}

#[cfg(unix)]
fn handle_stream_event(
    event: StreamEvent,
    on_line: &(dyn Fn(&str, crate::progress::OutputStream) + Send + Sync),
    stdout: &mut StreamCapture,
    stderr: &mut StreamCapture,
    reader_error: &mut Option<String>,
) {
    match event {
        StreamEvent::Data(crate::progress::OutputStream::Stdout, bytes) => {
            stdout.push(&bytes, on_line)
        }
        StreamEvent::Data(_, bytes) => stderr.push(&bytes, on_line),
        StreamEvent::Done(stream) => {
            if stream == crate::progress::OutputStream::Stdout {
                stdout.flush(on_line);
            } else {
                stderr.flush(on_line);
            }
        }
        StreamEvent::Error(stream, message) => {
            *reader_error = Some(format!("{stream:?}: {message}"));
        }
    }
}

#[cfg(unix)]
fn cancel_stream_readers(
    stdout_cancel: mpsc::Sender<()>,
    stderr_cancel: mpsc::Sender<()>,
    stdout_thread: thread::JoinHandle<()>,
    stderr_thread: thread::JoinHandle<()>,
) {
    let _ = stdout_cancel.send(());
    let _ = stderr_cancel.send(());
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
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
    fn real_runner_streaming_delivers_stderr_and_final_unterminated_line() {
        let runner = RealCommandRunner::new();
        let delivered = std::sync::Mutex::new(Vec::new());
        let output = runner
            .run_streaming(
                &CommandSpec::new("sh", ["-c", "printf 'final stdout'; printf 'final stderr' >&2"]),
                &|line, stream| delivered.lock().unwrap().push((stream, line.to_owned())),
            )
            .expect("shell exits successfully");
        let delivered = delivered.into_inner().unwrap();
        assert!(
            delivered.contains(&(crate::progress::OutputStream::Stdout, "final stdout".into()))
        );
        assert!(
            delivered.contains(&(crate::progress::OutputStream::Stderr, "final stderr".into()))
        );
        assert_eq!(output.stdout, "final stdout");
        assert_eq!(output.stderr, "final stderr");
    }

    #[test]
    fn real_runner_streaming_preserves_nonzero_provider_status() {
        let runner = RealCommandRunner::new();
        let output = runner
            .run_streaming(&CommandSpec::new("sh", ["-c", "printf 'failed'; exit 7"]), &|_, _| {})
            .expect("provider process started");
        assert_eq!(output.status, Some(7));
        assert!(!output.success());
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

    #[cfg(unix)]
    #[test]
    fn real_runner_streaming_does_not_wait_for_descendant_pipe_lifetime() {
        let runner = RealCommandRunner::new();
        let start = std::time::Instant::now();
        let output = runner
            .run_streaming(
                &CommandSpec::new(
                    "sh",
                    ["-c", "sleep 2 & printf 'parent stdout\\n'; printf 'parent stderr\\n' >&2; exit 0"],
                ),
                &|_line, _stream| {},
            )
            .expect("shell parent exits successfully");
        let elapsed = start.elapsed();
        assert_eq!(output.status, Some(0));
        assert!(output.stdout.contains("parent stdout"));
        assert!(output.stderr.contains("parent stderr"));
        assert!(
            elapsed < Duration::from_secs(1),
            "runner waited for an inherited pipe (took {:?})",
            elapsed
        );
    }
}
