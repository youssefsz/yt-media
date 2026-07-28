//! The process-runner port and production Tokio adapter.

use std::{
    io,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use async_trait::async_trait;
use command_group::AsyncCommandGroup;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

use crate::{
    cancellation::CancellationToken,
    path::{ExecutablePath, PathValidationError},
};

use super::{
    event::{CapturedOutput, OutputStream, ProcessEvent, StreamCapture},
    spec::{OutputLimit, ProcessSpec},
};

const READ_BUFFER_SIZE: usize = 8 * 1024;
const PIPE_CHANNEL_CAPACITY: usize = 64;
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Asynchronous process execution boundary used by engine adapters.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Runs a process to completion, cancellation, or timeout.
    ///
    /// Dropping the returned future also causes the owner task to terminate and reap the complete
    /// process group.
    async fn run(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError>;
}

/// Tokio-backed process runner with one owned process group per invocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioProcessRunner;

#[async_trait]
impl ProcessRunner for TokioProcessRunner {
    async fn run(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        let (owner_guard, abandoned) = oneshot::channel();
        let task = tokio::spawn(run_owned(spec, cancellation, abandoned));
        let result = task.await.map_err(ProcessError::OwnerTask)?;
        drop(owner_guard);
        result
    }
}

async fn run_owned(
    mut spec: ProcessSpec,
    cancellation: CancellationToken,
    mut abandoned: oneshot::Receiver<()>,
) -> Result<ProcessOutput, ProcessError> {
    let mut process = spawn_owned_process(&mut spec).await?;
    let deadline = spec.timeout.map(|timeout| Instant::now() + timeout);
    let mut poll = tokio::time::interval(STATUS_POLL_INTERVAL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut collector = OutputCollector::new(spec.output_limit);
    let mut readers_open = 2_u8;
    let mut exit_status = None;
    let mut reader_failure = None;

    while exit_status.is_none() || readers_open > 0 {
        if exit_status.is_none() {
            exit_status = process.child.try_wait().map_err(ProcessError::Wait)?;
        }
        if exit_status.is_some() && readers_open == 0 {
            break;
        }

        tokio::select! {
            biased;
            () = cancellation.cancelled(), if exit_status.is_none() => {
                terminate_and_reap(&mut process.child).await?;
                drain_messages(&mut process.pipe_receiver, &mut collector, &mut readers_open, &mut reader_failure).await;
                await_io_tasks(process.stdout_task, process.stderr_task, process.stdin_task).await?;
                return Err(ProcessError::Cancelled {
                    output: collector.finish(),
                });
            }
            result = &mut abandoned, if exit_status.is_none() => {
                if result.is_err() {
                    terminate_and_reap(&mut process.child).await?;
                    drain_messages(&mut process.pipe_receiver, &mut collector, &mut readers_open, &mut reader_failure).await;
                    await_io_tasks(process.stdout_task, process.stderr_task, process.stdin_task).await?;
                    return Err(ProcessError::Abandoned);
                }
            }
            () = sleep_until_optional(deadline), if exit_status.is_none() && deadline.is_some() => {
                terminate_and_reap(&mut process.child).await?;
                drain_messages(&mut process.pipe_receiver, &mut collector, &mut readers_open, &mut reader_failure).await;
                await_io_tasks(process.stdout_task, process.stderr_task, process.stdin_task).await?;
                return Err(ProcessError::TimedOut {
                    timeout: spec.timeout.unwrap_or_default(),
                    output: collector.finish(),
                });
            }
            message = process.pipe_receiver.recv(), if readers_open > 0 => {
                apply_reader_message(message, &mut collector, &mut readers_open, &mut reader_failure);
            }
            _ = poll.tick(), if exit_status.is_none() => {}
        }

        if let Some((stream, source)) = reader_failure.take() {
            if exit_status.is_none() {
                terminate_and_reap(&mut process.child).await?;
            }
            await_io_tasks(process.stdout_task, process.stderr_task, process.stdin_task).await?;
            return Err(ProcessError::Read { stream, source });
        }
    }

    let status = match exit_status {
        Some(status) => status,
        None => process.child.wait().await.map_err(ProcessError::Wait)?,
    };
    await_io_tasks(process.stdout_task, process.stderr_task, process.stdin_task).await?;
    let capture = collector.finish();
    Ok(ProcessOutput {
        status: status.into(),
        capture,
    })
}

async fn spawn_owned_process(spec: &mut ProcessSpec) -> Result<OwnedProcess, ProcessError> {
    let executable_request = spec.executable.clone();
    let executable =
        tokio::task::spawn_blocking(move || ExecutablePath::validate(executable_request))
            .await
            .map_err(ProcessError::PathTask)??;
    let mut command = Command::new(executable.as_path());
    command
        .args(&spec.arguments)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !spec.inherit_environment {
        command.env_clear();
    }
    command.envs(&spec.environment);
    if let Some(directory) = &spec.current_directory {
        command.current_dir(directory);
    }

    let mut child = command
        .group()
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| ProcessError::Spawn {
            executable: executable.clone(),
            source,
        })?;

    let (pipe_sender, pipe_receiver) = mpsc::channel(PIPE_CHANNEL_CAPACITY);
    let Some(stdout) = child.inner().stdout.take() else {
        terminate_and_reap(&mut child).await?;
        return Err(ProcessError::MissingPipe(OutputStream::Stdout));
    };
    let Some(stderr) = child.inner().stderr.take() else {
        terminate_and_reap(&mut child).await?;
        return Err(ProcessError::MissingPipe(OutputStream::Stderr));
    };
    let stdout_task = spawn_reader(stdout, OutputStream::Stdout, pipe_sender.clone());
    let stderr_task = spawn_reader(stderr, OutputStream::Stderr, pipe_sender);
    let stdin_task = match (spec.stdin.take(), child.inner().stdin.take()) {
        (Some(bytes), Some(mut stdin)) => Some(tokio::spawn(async move {
            stdin.write_all(&bytes).await?;
            stdin.shutdown().await
        })),
        (None, _) => None,
        (Some(_), None) => {
            terminate_and_reap(&mut child).await?;
            return Err(ProcessError::MissingStdin);
        }
    };

    Ok(OwnedProcess {
        child,
        pipe_receiver,
        stdout_task,
        stderr_task,
        stdin_task,
    })
}

struct OwnedProcess {
    child: command_group::AsyncGroupChild,
    pipe_receiver: mpsc::Receiver<ReaderMessage>,
    stdout_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    stdin_task: Option<JoinHandle<Result<(), io::Error>>>,
}

async fn sleep_until_optional(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

async fn terminate_and_reap(
    child: &mut command_group::AsyncGroupChild,
) -> Result<(), ProcessError> {
    match child.kill().await {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::InvalidInput => {
            child.wait().await.map(|_| ()).map_err(ProcessError::Wait)
        }
        Err(source) => Err(ProcessError::Terminate(source)),
    }
}

fn spawn_reader<R>(
    mut reader: R,
    stream: OutputStream,
    sender: mpsc::Sender<ReaderMessage>,
) -> JoinHandle<()>
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; READ_BUFFER_SIZE];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => {
                    let _ignored = sender.send(ReaderMessage::Closed(stream)).await;
                    break;
                }
                Ok(length) => {
                    let message = ReaderMessage::Bytes(stream, buffer[..length].to_vec());
                    if sender.send(message).await.is_err() {
                        break;
                    }
                }
                Err(source) => {
                    let _ignored = sender.send(ReaderMessage::Failed(stream, source)).await;
                    break;
                }
            }
        }
    })
}

async fn drain_messages(
    receiver: &mut mpsc::Receiver<ReaderMessage>,
    collector: &mut OutputCollector,
    readers_open: &mut u8,
    reader_failure: &mut Option<(OutputStream, io::Error)>,
) {
    while *readers_open > 0 {
        let message = receiver.recv().await;
        apply_reader_message(message, collector, readers_open, reader_failure);
    }
}

fn apply_reader_message(
    message: Option<ReaderMessage>,
    collector: &mut OutputCollector,
    readers_open: &mut u8,
    reader_failure: &mut Option<(OutputStream, io::Error)>,
) {
    match message {
        Some(ReaderMessage::Bytes(stream, bytes)) => collector.push(stream, &bytes),
        Some(ReaderMessage::Closed(_stream)) => {
            *readers_open = readers_open.saturating_sub(1);
        }
        Some(ReaderMessage::Failed(stream, source)) => {
            *readers_open = readers_open.saturating_sub(1);
            *reader_failure = Some((stream, source));
        }
        None => {
            *readers_open = 0;
        }
    }
}

async fn await_io_tasks(
    stdout_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    stdin_task: Option<JoinHandle<Result<(), io::Error>>>,
) -> Result<(), ProcessError> {
    stdout_task.await.map_err(ProcessError::ReaderTask)?;
    stderr_task.await.map_err(ProcessError::ReaderTask)?;
    if let Some(stdin_task) = stdin_task {
        stdin_task
            .await
            .map_err(ProcessError::WriterTask)?
            .map_err(ProcessError::Write)?;
    }
    Ok(())
}

enum ReaderMessage {
    Bytes(OutputStream, Vec<u8>),
    Closed(OutputStream),
    Failed(OutputStream, io::Error),
}

struct OutputCollector {
    stdout: BoundedStream,
    stderr: BoundedStream,
    events: Vec<ProcessEvent>,
    next_sequence: u64,
}

impl OutputCollector {
    fn new(limit: OutputLimit) -> Self {
        Self {
            stdout: BoundedStream::new(limit),
            stderr: BoundedStream::new(limit),
            events: Vec::new(),
            next_sequence: 0,
        }
    }

    fn push(&mut self, stream: OutputStream, bytes: &[u8]) {
        let retained = match stream {
            OutputStream::Stdout => self.stdout.push(bytes),
            OutputStream::Stderr => self.stderr.push(bytes),
        };
        if !retained.is_empty() {
            self.events.push(ProcessEvent {
                sequence: self.next_sequence,
                stream,
                bytes: retained,
            });
            self.next_sequence = self.next_sequence.saturating_add(1);
        }
    }

    fn finish(mut self) -> CapturedOutput {
        self.stdout.finish();
        self.stderr.finish();
        CapturedOutput {
            stdout: self.stdout.capture,
            stderr: self.stderr.capture,
            events: self.events,
        }
    }
}

struct BoundedStream {
    limit: OutputLimit,
    capture: StreamCapture,
    observed_newlines: u64,
    retained_newlines: usize,
    observed_last: Option<u8>,
    retained_last: Option<u8>,
}

impl BoundedStream {
    fn new(limit: OutputLimit) -> Self {
        Self {
            limit,
            capture: StreamCapture::default(),
            observed_newlines: 0,
            retained_newlines: 0,
            observed_last: None,
            retained_last: None,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.capture.observed_bytes = self
            .capture
            .observed_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        for byte in bytes {
            if *byte == b'\n' {
                self.observed_newlines = self.observed_newlines.saturating_add(1);
            }
        }
        self.observed_last = bytes.last().copied().or(self.observed_last);

        let mut retained = Vec::new();
        for byte in bytes {
            let byte_limit_reached = self.capture.bytes.len() >= self.limit.max_bytes_per_stream;
            let line_limit_reached = self.retained_newlines >= self.limit.max_lines_per_stream;
            if byte_limit_reached || line_limit_reached {
                self.capture.truncated = true;
                continue;
            }
            self.capture.bytes.push(*byte);
            retained.push(*byte);
            self.retained_last = Some(*byte);
            if *byte == b'\n' {
                self.retained_newlines = self.retained_newlines.saturating_add(1);
            }
        }
        retained
    }

    fn finish(&mut self) {
        self.capture.observed_lines = self.observed_newlines.saturating_add(u64::from(
            self.capture.observed_bytes > 0 && self.observed_last != Some(b'\n'),
        ));
        let retained_partial =
            usize::from(!self.capture.bytes.is_empty() && self.retained_last != Some(b'\n'));
        let retained_lines = self.retained_newlines.saturating_add(retained_partial);
        if self.capture.observed_lines > u64::try_from(retained_lines).unwrap_or(u64::MAX) {
            self.capture.truncated = true;
        }
    }
}

/// Portable exit information.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExitStatus {
    /// Whether the process reported success.
    pub success: bool,
    /// Platform exit code, when the platform supplied one.
    pub code: Option<i32>,
}

impl From<ExitStatus> for ProcessExitStatus {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
        }
    }
}

/// Completed process result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    /// Exit information.
    pub status: ProcessExitStatus,
    /// Exact bounded process output.
    pub capture: CapturedOutput,
}

/// Typed process execution failure.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// The requested executable path was invalid.
    #[error(transparent)]
    InvalidExecutable(#[from] PathValidationError),
    /// The process group could not be created.
    #[error("failed to spawn process group for `{executable}`")]
    Spawn {
        /// Validated executable.
        executable: ExecutablePath,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// A configured pipe was unexpectedly unavailable.
    #[error("spawned process did not expose its {0:?} pipe")]
    MissingPipe(OutputStream),
    /// Configured standard input was unexpectedly unavailable.
    #[error("spawned process did not expose its standard input pipe")]
    MissingStdin,
    /// Reading a child stream failed.
    #[error("failed reading child {stream:?}")]
    Read {
        /// Stream that failed.
        stream: OutputStream,
        /// I/O failure.
        #[source]
        source: io::Error,
    },
    /// Writing child stdin failed.
    #[error("failed writing child standard input")]
    Write(#[source] io::Error),
    /// Waiting for the process group failed.
    #[error("failed waiting for child process group")]
    Wait(#[source] io::Error),
    /// Terminating the process group failed.
    #[error("failed terminating child process group")]
    Terminate(#[source] io::Error),
    /// Cancellation terminated the complete process group.
    #[error("process was cancelled")]
    Cancelled {
        /// Bounded diagnostic output produced before cancellation.
        output: CapturedOutput,
    },
    /// The configured deadline terminated the complete process group.
    #[error("process exceeded timeout of {timeout:?}")]
    TimedOut {
        /// Configured timeout.
        timeout: Duration,
        /// Bounded diagnostic output produced before the timeout.
        output: CapturedOutput,
    },
    /// The caller dropped the process future; the owner still cleaned up the process tree.
    #[error("process owner was abandoned")]
    Abandoned,
    /// The process-owner task failed.
    #[error("process owner task failed")]
    OwnerTask(#[source] tokio::task::JoinError),
    /// The blocking executable-path validation task failed.
    #[error("executable path validation task failed")]
    PathTask(#[source] tokio::task::JoinError),
    /// A stream-reader task failed.
    #[error("process stream reader task failed")]
    ReaderTask(#[source] tokio::task::JoinError),
    /// The stdin-writer task failed.
    #[error("process stdin writer task failed")]
    WriterTask(#[source] tokio::task::JoinError),
}
