//! Durable queue domain types and lifecycle invariants.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast;

use crate::download::{JobEventKind, JobId, JobProgress, OutputSelection};

/// Every durable lifecycle state understood by the engine queue.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    /// Waiting for an explicitly started scheduler slot.
    Queued,
    /// Refreshing media metadata and format availability.
    Analyzing,
    /// Transferring one or more source streams.
    Downloading,
    /// Combining compatible source streams.
    Merging,
    /// Encoding one or more streams.
    Converting,
    /// Explicitly stopped with compatible partials retained.
    Paused,
    /// Previously active work recovered without starting it.
    Interrupted,
    /// Verified output was published.
    Completed,
    /// Explicitly cancelled with owned temporary paths removed.
    Cancelled,
    /// Work ended in a classified failure.
    Failed,
}

impl JobState {
    /// All states, in stable protocol order.
    pub const ALL: [Self; 10] = [
        Self::Queued,
        Self::Analyzing,
        Self::Downloading,
        Self::Merging,
        Self::Converting,
        Self::Paused,
        Self::Interrupted,
        Self::Completed,
        Self::Cancelled,
        Self::Failed,
    ];

    /// Returns the stable storage and transport value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Analyzing => "analyzing",
            Self::Downloading => "downloading",
            Self::Merging => "merging",
            Self::Converting => "converting",
            Self::Paused => "paused",
            Self::Interrupted => "interrupted",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    /// Parses one exact stable storage value.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown values instead of guessing a lifecycle state.
    pub fn parse(value: &str) -> Result<Self, JobStateParseError> {
        match value {
            "queued" => Ok(Self::Queued),
            "analyzing" => Ok(Self::Analyzing),
            "downloading" => Ok(Self::Downloading),
            "merging" => Ok(Self::Merging),
            "converting" => Ok(Self::Converting),
            "paused" => Ok(Self::Paused),
            "interrupted" => Ok(Self::Interrupted),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(JobStateParseError(value.chars().take(64).collect())),
        }
    }

    /// Returns whether this state owns a potentially active process tree.
    #[must_use]
    pub const fn is_process_active(self) -> bool {
        matches!(
            self,
            Self::Analyzing | Self::Downloading | Self::Merging | Self::Converting
        )
    }

    /// Returns whether this state is terminal until an explicit retry.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    /// Returns whether the requested lifecycle transition is valid.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Queued => matches!(
                next,
                Self::Analyzing | Self::Paused | Self::Cancelled | Self::Failed
            ),
            Self::Analyzing => matches!(
                next,
                Self::Downloading
                    | Self::Paused
                    | Self::Interrupted
                    | Self::Cancelled
                    | Self::Failed
            ),
            Self::Downloading => matches!(
                next,
                Self::Merging
                    | Self::Converting
                    | Self::Completed
                    | Self::Paused
                    | Self::Interrupted
                    | Self::Cancelled
                    | Self::Failed
            ),
            Self::Merging => matches!(
                next,
                Self::Converting
                    | Self::Completed
                    | Self::Paused
                    | Self::Interrupted
                    | Self::Cancelled
                    | Self::Failed
            ),
            Self::Converting => matches!(
                next,
                Self::Completed | Self::Paused | Self::Interrupted | Self::Cancelled | Self::Failed
            ),
            Self::Paused | Self::Interrupted => {
                matches!(next, Self::Queued | Self::Cancelled)
            }
            Self::Cancelled | Self::Failed => matches!(next, Self::Queued),
            Self::Completed => false,
        }
    }

    /// Validates one lifecycle transition.
    ///
    /// # Errors
    ///
    /// Returns the exact invalid state pair.
    pub fn validate_transition(self, next: Self) -> Result<(), JobTransitionError> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(JobTransitionError {
                previous: self,
                next,
            })
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An unknown persisted lifecycle state.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("unknown persisted job state `{0}`")]
pub struct JobStateParseError(String);

/// An invalid durable lifecycle transition.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("job state cannot transition from `{previous}` to `{next}`")]
pub struct JobTransitionError {
    /// Existing authoritative state.
    pub previous: JobState,
    /// Rejected requested state.
    pub next: JobState,
}

/// Configured queue concurrency, bounded from one through four.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct QueueConcurrency(u8);

impl QueueConcurrency {
    /// Plan 04's default number of active jobs.
    pub const DEFAULT: Self = Self(2);

    /// Returns the configured slot count.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for QueueConcurrency {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u8> for QueueConcurrency {
    type Error = QueueConcurrencyError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if (1..=4).contains(&value) {
            Ok(Self(value))
        } else {
            Err(QueueConcurrencyError(value))
        }
    }
}

/// A queue concurrency value outside the supported bound.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("queue concurrency `{0}` is invalid; expected a value from 1 through 4")]
pub struct QueueConcurrencyError(pub u8);

/// Engine update behavior retained across launches.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdatePreference {
    /// Inform the user when a verified update is available.
    Notify,
    /// Automatically activate a verified healthy update.
    Automatic,
    /// Do not check for managed updates.
    Disabled,
}

impl UpdatePreference {
    /// Returns the stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Automatic => "automatic",
            Self::Disabled => "disabled",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, SettingsValueError> {
        match value {
            "notify" => Ok(Self::Notify),
            "automatic" => Ok(Self::Automatic),
            "disabled" => Ok(Self::Disabled),
            _ => Err(SettingsValueError::UpdatePreference(
                value.chars().take(64).collect(),
            )),
        }
    }
}

/// Persisted engine settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EngineSettings {
    /// Default destination used when an application does not supply one.
    pub default_destination: Option<PathBuf>,
    /// Shared download and post-processing concurrency.
    pub queue_concurrency: QueueConcurrency,
    /// Managed tool update behavior.
    pub update_preference: UpdatePreference,
    /// Last normalized output choice.
    pub last_output: OutputSelection,
}

/// Partial settings update. `Some(None)` clears the default destination.
#[derive(Clone, Debug, Default)]
pub struct SettingsPatch {
    /// Replacement or explicit removal for the default destination.
    pub default_destination: Option<Option<PathBuf>>,
    /// Replacement queue concurrency.
    pub queue_concurrency: Option<QueueConcurrency>,
    /// Replacement managed update behavior.
    pub update_preference: Option<UpdatePreference>,
    /// Replacement normalized output default.
    pub last_output: Option<OutputSelection>,
}

/// A malformed persisted settings value.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SettingsValueError {
    /// Update preference was unknown.
    #[error("unknown persisted update preference `{0}`")]
    UpdatePreference(String),
    /// Output format was unknown.
    #[error("unknown persisted output format `{0}`")]
    OutputFormat(String),
    /// Numeric output quality was invalid.
    #[error("invalid persisted output quality `{0}`")]
    OutputQuality(i64),
    /// Queue concurrency was outside one through four.
    #[error(transparent)]
    QueueConcurrency(#[from] QueueConcurrencyError),
}

/// One normalized durable download request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct JobRequest {
    /// Canonical public single-video URL.
    pub canonical_url: String,
    /// Normalized output and quality.
    pub output: OutputSelection,
    /// Caller-selected destination.
    pub destination: PathBuf,
    /// Optional untrusted output stem retained for retry.
    pub name: Option<String>,
}

/// Stable failure classes safe for persistence and transport.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobErrorClass {
    /// Request reconstruction or validation failed.
    InvalidRequest,
    /// Fresh media analysis failed.
    Analysis,
    /// The requested normalized format disappeared.
    FormatUnavailable,
    /// The destination is missing or unusable.
    DestinationUnavailable,
    /// A filesystem operation failed.
    Filesystem,
    /// A required external tool failed.
    Process,
    /// A bounded adapter protocol was invalid.
    Protocol,
    /// Produced media did not satisfy the output contract.
    Verification,
    /// Queue storage or orchestration failed.
    Internal,
}

impl JobErrorClass {
    /// Returns the stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid-request",
            Self::Analysis => "analysis",
            Self::FormatUnavailable => "format-unavailable",
            Self::DestinationUnavailable => "destination-unavailable",
            Self::Filesystem => "filesystem",
            Self::Process => "process",
            Self::Protocol => "protocol",
            Self::Verification => "verification",
            Self::Internal => "internal",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, JobFailureValueError> {
        match value {
            "invalid-request" => Ok(Self::InvalidRequest),
            "analysis" => Ok(Self::Analysis),
            "format-unavailable" => Ok(Self::FormatUnavailable),
            "destination-unavailable" => Ok(Self::DestinationUnavailable),
            "filesystem" => Ok(Self::Filesystem),
            "process" => Ok(Self::Process),
            "protocol" => Ok(Self::Protocol),
            "verification" => Ok(Self::Verification),
            "internal" => Ok(Self::Internal),
            _ => Err(JobFailureValueError(value.chars().take(64).collect())),
        }
    }
}

/// A bounded persisted job failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct JobFailure {
    /// Stable classification for application behavior.
    pub class: JobErrorClass,
    /// Safe bounded explanation.
    pub message: String,
}

/// An unknown persisted failure classification.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("unknown persisted job failure class `{0}`")]
pub struct JobFailureValueError(String);

/// Verified published output metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FinalOutput {
    /// Canonical path recorded at publication.
    pub path: PathBuf,
    /// Non-zero size observed before publication.
    pub size_bytes: u64,
    /// Requested normalized output.
    pub output: OutputSelection,
}

/// Current filesystem availability for a completed output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputAvailability {
    /// The job has no final output.
    NotApplicable,
    /// The recorded final output currently exists as a file.
    Present,
    /// History remains, but the output was moved or deleted externally.
    Missing,
}

/// One durable authoritative job snapshot.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JobRecord {
    /// Stable `UUIDv7` identity.
    pub id: JobId,
    /// Normalized request used by first run, resume, and retry.
    pub request: JobRequest,
    /// Current validated lifecycle state.
    pub state: JobState,
    /// Latest bounded progress snapshot.
    pub progress: Option<JobProgress>,
    /// Latest classified failure or recovery diagnostic.
    pub error: Option<JobFailure>,
    /// Creation time as Unix epoch milliseconds.
    pub created_at_ms: i64,
    /// Last mutation time as Unix epoch milliseconds.
    pub updated_at_ms: i64,
    /// Terminal completion time as Unix epoch milliseconds.
    pub completed_at_ms: Option<i64>,
    /// Number of explicitly started attempts, including the initial run.
    pub attempt_count: u32,
    /// Verified final output metadata when completed.
    pub final_output: Option<FinalOutput>,
    /// Derived current output presence.
    pub output_availability: OutputAvailability,
    /// Exact persisted engine-owned paths currently retained for this job.
    pub owned_partial_paths: Vec<PathBuf>,
    /// Whether the requested destination currently exists as a directory.
    pub destination_available: bool,
}

impl JobRecord {
    pub(crate) fn refresh_filesystem_status(&mut self) {
        self.destination_available = self.request.destination.is_dir();
        self.output_availability = match &self.final_output {
            Some(output) if output.path.is_file() => OutputAvailability::Present,
            Some(_) => OutputAvailability::Missing,
            None => OutputAvailability::NotApplicable,
        };
    }
}

/// A monotonically sequenced durable queue event.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueueEvent {
    /// Process-local monotonic event sequence.
    pub sequence: u64,
    /// Authoritative snapshot after the mutation.
    pub job: JobRecord,
    /// Optional transient download activity associated with this snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<JobEventKind>,
}

/// Bounded queue subscription that never blocks scheduler progress.
pub struct QueueSubscription {
    pub(crate) receiver: broadcast::Receiver<QueueEvent>,
}

impl QueueSubscription {
    /// Receives the next queue event.
    ///
    /// # Errors
    ///
    /// Returns explicit closed or lagged subscription state.
    pub async fn recv(&mut self) -> Result<QueueEvent, broadcast::error::RecvError> {
        self.receiver.recv().await
    }
}

/// Returns a bounded display path suitable for database errors.
pub(crate) fn bounded_path(path: &Path) -> String {
    path.to_string_lossy().chars().take(1_024).collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{JobState, QueueConcurrency};

    #[test]
    fn every_state_pair_has_the_expected_transition_contract() {
        for previous in JobState::ALL {
            for next in JobState::ALL {
                let expected = match previous {
                    JobState::Queued => matches!(
                        next,
                        JobState::Analyzing
                            | JobState::Paused
                            | JobState::Cancelled
                            | JobState::Failed
                    ),
                    JobState::Analyzing => matches!(
                        next,
                        JobState::Downloading
                            | JobState::Paused
                            | JobState::Interrupted
                            | JobState::Cancelled
                            | JobState::Failed
                    ),
                    JobState::Downloading => matches!(
                        next,
                        JobState::Merging
                            | JobState::Converting
                            | JobState::Completed
                            | JobState::Paused
                            | JobState::Interrupted
                            | JobState::Cancelled
                            | JobState::Failed
                    ),
                    JobState::Merging => matches!(
                        next,
                        JobState::Converting
                            | JobState::Completed
                            | JobState::Paused
                            | JobState::Interrupted
                            | JobState::Cancelled
                            | JobState::Failed
                    ),
                    JobState::Converting => matches!(
                        next,
                        JobState::Completed
                            | JobState::Paused
                            | JobState::Interrupted
                            | JobState::Cancelled
                            | JobState::Failed
                    ),
                    JobState::Paused | JobState::Interrupted => {
                        matches!(next, JobState::Queued | JobState::Cancelled)
                    }
                    JobState::Cancelled | JobState::Failed => next == JobState::Queued,
                    JobState::Completed => false,
                };
                assert_eq!(
                    previous.can_transition_to(next),
                    expected,
                    "{previous} -> {next}"
                );
                assert_eq!(
                    previous.validate_transition(next).is_ok(),
                    expected,
                    "{previous} -> {next}"
                );
            }
        }
    }

    #[test]
    fn queue_concurrency_is_bounded() {
        assert!(QueueConcurrency::try_from(0).is_err());
        for value in 1..=4 {
            assert_eq!(
                QueueConcurrency::try_from(value).map(QueueConcurrency::get),
                Ok(value)
            );
        }
        assert!(QueueConcurrency::try_from(5).is_err());
        assert_eq!(QueueConcurrency::default().get(), 2);
    }

    proptest! {
        #[test]
        fn command_sequences_never_implicitly_restart_terminal_jobs_or_exceed_concurrency(
            concurrency in 1_u8..=4,
            commands in proptest::collection::vec((0_usize..8, 0_u8..9), 0..256),
        ) {
            let mut states = [JobState::Queued; 8];
            for (index, command) in commands {
                let previous = states[index];
                let active = states
                    .iter()
                    .filter(|state| state.is_process_active())
                    .count();
                let next = match command {
                    0 if previous == JobState::Queued && active < usize::from(concurrency) => {
                        Some(JobState::Analyzing)
                    }
                    1 if previous == JobState::Analyzing => Some(JobState::Downloading),
                    2 if previous == JobState::Downloading => Some(JobState::Converting),
                    3 if previous.is_process_active() || previous == JobState::Queued => {
                        Some(JobState::Cancelled)
                    }
                    4 if previous.is_process_active() => Some(JobState::Interrupted),
                    5 if matches!(previous, JobState::Paused | JobState::Interrupted) => {
                        Some(JobState::Queued)
                    }
                    6 if matches!(previous, JobState::Failed | JobState::Cancelled) => {
                        Some(JobState::Queued)
                    }
                    7 if matches!(
                        previous,
                        JobState::Downloading | JobState::Merging | JobState::Converting
                    ) => Some(JobState::Completed),
                    8 if previous.is_process_active() => Some(JobState::Failed),
                    _ => None,
                };
                if let Some(next) = next {
                    prop_assert!(previous.can_transition_to(next));
                    if previous.is_terminal() && next == JobState::Queued {
                        prop_assert_eq!(command, 6, "only explicit retry may restart a terminal job");
                    }
                    states[index] = next;
                }
                prop_assert!(
                    states
                        .iter()
                        .filter(|state| state.is_process_active())
                        .count()
                        <= usize::from(concurrency)
                );
            }
        }
    }
}
