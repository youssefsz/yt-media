//! Public download requests, lifecycle events, controls, results, and errors.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;

use crate::{analysis::MediaUrl, cancellation::CancellationToken};

/// A supported constant MP3 bitrate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AudioQuality(u16);

impl AudioQuality {
    /// Returns the selected bitrate in kilobits per second.
    #[must_use]
    pub const fn bitrate_kbps(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for AudioQuality {
    type Error = DownloadRequestError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if matches!(value, 128 | 192 | 256 | 320) {
            Ok(Self(value))
        } else {
            Err(DownloadRequestError::InvalidAudioQuality(value))
        }
    }
}

/// A requested source-height MP4 choice.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct VideoQuality(u32);

impl VideoQuality {
    /// Returns the requested source height in pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for VideoQuality {
    type Error = DownloadRequestError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if (1..=16_384).contains(&value) {
            Ok(Self(value))
        } else {
            Err(DownloadRequestError::InvalidVideoQuality(value))
        }
    }
}

/// The desired output and normalized quality.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "format", content = "quality", rename_all = "lowercase")]
pub enum OutputSelection {
    /// Constant-bitrate MP3 audio.
    Mp3(AudioQuality),
    /// H.264/AAC MP4 at one available source height.
    Mp4(VideoQuality),
}

impl OutputSelection {
    /// Returns the engine-owned file extension.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp3(_) => "mp3",
            Self::Mp4(_) => "mp4",
        }
    }
}

/// A caller-selected output directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Destination(PathBuf);

impl Destination {
    /// Creates a bounded destination request.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or excessively long path. Existence, directory type,
    /// writability, and canonicalization are checked asynchronously when the job starts.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, DownloadRequestError> {
        let path = path.into();
        if path.to_str().is_none() {
            return Err(DownloadRequestError::NonUnicodeDestination);
        }
        let length = path.as_os_str().to_string_lossy().chars().count();
        if length == 0 {
            Err(DownloadRequestError::EmptyDestination)
        } else if length > 4_096 {
            Err(DownloadRequestError::DestinationTooLong)
        } else {
            Ok(Self(path))
        }
    }

    /// Returns the requested path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// An optional untrusted output stem, before sanitization and extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OutputName(String);

impl OutputName {
    /// Creates a bounded requested name.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or excessively long value. Portable filename sanitization is
    /// performed by the engine after analysis.
    pub fn new(value: impl Into<String>) -> Result<Self, DownloadRequestError> {
        let value = value.into();
        let length = value.chars().count();
        if value.trim().is_empty() {
            Err(DownloadRequestError::EmptyOutputName)
        } else if length > 1_024 {
            Err(DownloadRequestError::OutputNameTooLong)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the untrusted requested stem.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One complete engine-owned download request.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    /// Validated public media URL.
    pub url: MediaUrl,
    /// Requested output format and quality.
    pub output: OutputSelection,
    /// Caller-selected output directory.
    pub destination: Destination,
    /// Optional caller-selected name; the analyzed title is used when absent.
    pub name: Option<OutputName>,
}

/// Invalid download request data.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DownloadRequestError {
    /// MP3 supports only four locked bitrates.
    #[error("invalid MP3 quality `{0}`; expected one of 128, 192, 256, or 320")]
    InvalidAudioQuality(u16),
    /// A video height was outside the engine's safe numeric range.
    #[error("invalid MP4 quality `{0}`; expected a positive height no greater than 16384")]
    InvalidVideoQuality(u32),
    /// No destination was supplied.
    #[error("output destination must not be empty")]
    EmptyDestination,
    /// The destination exceeded the transport bound.
    #[error("output destination exceeds the 4096-character limit")]
    DestinationTooLong,
    /// The JSON and IPC contract requires a Unicode destination.
    #[error("output destination must be valid Unicode")]
    NonUnicodeDestination,
    /// No meaningful output stem was supplied.
    #[error("output name must not be empty")]
    EmptyOutputName,
    /// The name exceeded the input bound.
    #[error("output name exceeds the 1024-character limit")]
    OutputNameTooLong,
}

/// One process-local job identity suitable for paths and event correlation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct JobId(pub(crate) String);

impl JobId {
    /// Creates a time-ordered `UUIDv7` job identity.
    #[must_use]
    pub fn new_v7() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    /// Parses and validates a persisted `UUIDv7` job identity.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed UUIDs and UUID versions other than seven.
    pub fn parse(value: &str) -> Result<Self, JobIdError> {
        let parsed = Uuid::parse_str(value).map_err(|source| JobIdError::Malformed {
            value: value.chars().take(64).collect(),
            source,
        })?;
        if parsed.get_version_num() != 7 {
            return Err(JobIdError::WrongVersion(parsed.get_version_num()));
        }
        Ok(Self(parsed.hyphenated().to_string()))
    }

    /// Returns the bounded printable identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A malformed or non-v7 persisted job identity.
#[derive(Debug, Error)]
pub enum JobIdError {
    /// UUID syntax was malformed.
    #[error("job ID `{value}` is not a valid UUID")]
    Malformed {
        /// Bounded rejected value.
        value: String,
        /// UUID parser failure.
        #[source]
        source: uuid::Error,
    },
    /// UUID syntax was valid, but its version was not seven.
    #[error("job ID must be UUIDv7, found UUID version {0}")]
    WrongVersion(usize),
}

/// Authoritative engine lifecycle stages.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStage {
    /// Re-analyzing immediately before source selection.
    Analyzing,
    /// Downloading one or more selected source streams.
    Downloading,
    /// Stream-copying compatible sources.
    Merging,
    /// Encoding one or more streams for compatibility.
    Converting,
    /// Probing and atomically publishing the completed output.
    Finalizing,
    /// Output was verified and published.
    Completed,
    /// The caller requested a resumable stop.
    Paused,
    /// The caller cancelled and owned temporary files were removed.
    Cancelled,
    /// The job failed and owned non-resumable files were removed.
    Failed,
}

/// Bounded progress at the current stage.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct JobProgress {
    /// Current stage.
    pub stage: JobStage,
    /// Work completed in protocol-native units.
    pub completed: u64,
    /// Known total, when available.
    pub total: Option<u64>,
    /// Derived percentage from zero through one hundred, when a total is known.
    pub percent: Option<f64>,
    /// Download speed in bytes per second, when reported.
    pub bytes_per_second: Option<u64>,
    /// Estimated seconds remaining, when reported.
    pub eta_seconds: Option<u64>,
}

/// One schema-independent engine event.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum JobEventKind {
    /// The authoritative stage changed.
    Stage {
        /// New stage.
        stage: JobStage,
    },
    /// Bounded progress changed.
    Progress {
        /// New progress snapshot.
        progress: JobProgress,
    },
    /// A bounded non-fatal adapter diagnostic.
    Warning {
        /// Human-readable warning.
        message: String,
    },
}

/// One monotonically sequenced job event.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JobEvent {
    /// Correlated job.
    pub job_id: JobId,
    /// Monotonic event sequence.
    pub sequence: u64,
    /// Event payload.
    #[serde(flatten)]
    pub kind: JobEventKind,
}

/// Successful verified output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DownloadResult {
    /// Correlated job.
    pub job_id: JobId,
    /// Canonical published output path.
    pub path: PathBuf,
    /// Published non-zero byte length.
    pub size_bytes: u64,
    /// Requested output selection.
    pub output: OutputSelection,
}

/// Download, conversion, cleanup, or verification failure.
#[derive(Debug, Error)]
pub enum DownloadError {
    /// Request validation failed.
    #[error(transparent)]
    InvalidRequest(#[from] DownloadRequestError),
    /// Immediate re-analysis failed.
    #[error("could not analyze media immediately before download")]
    Analysis(#[source] crate::analysis::AnalyzeError),
    /// The requested normalized format was not in the fresh analysis.
    #[error("requested {requested} is unavailable; available choices: {available}")]
    FormatUnavailable {
        /// Requested stable description.
        requested: String,
        /// Bounded available-choice description.
        available: String,
    },
    /// The selected destination is invalid or unusable.
    #[error("invalid output destination `{path}`: {reason}")]
    Destination {
        /// Bounded requested path.
        path: String,
        /// Bounded reason.
        reason: String,
    },
    /// A filesystem operation failed.
    #[error("filesystem operation `{operation}` failed for `{path}`")]
    Filesystem {
        /// Stable operation name.
        operation: &'static str,
        /// Bounded affected path.
        path: String,
        /// Original operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// A process specification could not be built.
    #[error("invalid external process specification")]
    ProcessSpecification(#[source] crate::process::ProcessSpecError),
    /// An owned external process failed.
    #[error("{tool} process execution failed")]
    Process {
        /// Stable tool name.
        tool: &'static str,
        /// Original process failure.
        #[source]
        source: Box<crate::process::ProcessError>,
    },
    /// An external process returned a non-zero status.
    #[error("{tool} exited unsuccessfully with status {status:?}: {diagnostics}")]
    NonZero {
        /// Stable tool name.
        tool: &'static str,
        /// Portable exit status.
        status: Option<i32>,
        /// Bounded diagnostic text.
        diagnostics: String,
    },
    /// A bounded machine protocol was malformed or unsafe.
    #[error("invalid {protocol} protocol: {reason}")]
    Protocol {
        /// Stable protocol name.
        protocol: &'static str,
        /// Bounded reason.
        reason: String,
    },
    /// Final output did not satisfy the requested compatibility contract.
    #[error("output verification failed: {0}")]
    Verification(String),
    /// Deterministic collision suffixes were exhausted.
    #[error("could not reserve an output name after 10000 collision attempts")]
    CollisionLimit,
    /// The caller requested pause and resumable partials were retained.
    #[error("download was paused")]
    Paused,
    /// The caller cancelled and owned temporary files were removed.
    #[error("download was cancelled")]
    Cancelled,
    /// The background job task itself failed.
    #[error("download task failed")]
    Join(#[source] tokio::task::JoinError),
    /// The job ended without publishing a completion value.
    #[error("download completion channel closed unexpectedly")]
    CompletionClosed,
}

/// A bounded non-blocking event subscription.
pub struct JobEventStream {
    pub(crate) receiver: broadcast::Receiver<JobEvent>,
}

impl JobEventStream {
    /// Receives the next event.
    ///
    /// # Errors
    ///
    /// Returns the broadcast receiver's explicit closed or lagged state.
    pub async fn recv(&mut self) -> Result<JobEvent, broadcast::error::RecvError> {
        self.receiver.recv().await
    }

    /// Tries to receive the next event without waiting.
    ///
    /// # Errors
    ///
    /// Returns the broadcast receiver's explicit empty, closed, or lagged state.
    pub fn try_recv(&mut self) -> Result<JobEvent, broadcast::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

/// Awaitable authoritative job completion.
pub struct CompletionHandle {
    pub(crate) receiver: oneshot::Receiver<Result<DownloadResult, DownloadError>>,
}

impl CompletionHandle {
    /// Waits for verified success or a typed terminal error.
    ///
    /// # Errors
    ///
    /// Returns the job error or a channel-closure error.
    pub async fn wait(self) -> Result<DownloadResult, DownloadError> {
        self.receiver
            .await
            .map_err(|_| DownloadError::CompletionClosed)?
    }
}

pub(crate) const CONTROL_RUNNING: u8 = 0;
pub(crate) const CONTROL_PAUSE: u8 = 1;
pub(crate) const CONTROL_CANCEL: u8 = 2;

/// Explicit non-blocking controls for one engine-owned job.
#[derive(Clone)]
pub struct JobControls {
    pub(crate) state: Arc<AtomicU8>,
    pub(crate) cancellation: CancellationToken,
}

impl fmt::Debug for JobControls {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobControls")
            .field("state", &self.state.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl JobControls {
    /// Requests a resumable pause. Cancellation remains stronger if already requested.
    pub fn pause(&self) {
        let _ignored = self.state.compare_exchange(
            CONTROL_RUNNING,
            CONTROL_PAUSE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.cancellation.cancel();
    }

    /// Requests permanent cancellation and cleanup.
    pub fn cancel(&self) {
        self.state.store(CONTROL_CANCEL, Ordering::Release);
        self.cancellation.cancel();
    }
}

/// Handles returned immediately after an engine job starts.
pub struct StartedDownload {
    /// Correlated job identity.
    pub job_id: JobId,
    /// Non-blocking bounded event stream.
    pub events: JobEventStream,
    /// Authoritative terminal result.
    pub completion: CompletionHandle,
    /// Explicit pause and cancel controls.
    pub controls: JobControls,
}
