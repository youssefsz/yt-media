//! Versioned, serializable desktop IPC contracts and TypeScript generation.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use yt_media_engine::{
    analysis::{
        AudioCodecDescriptor, AudioCodecFamily, CompatibilityWork, ContainerDescriptor,
        ContainerFamily, FormatOption, MediaInfo, SourceFormat, VideoCodecDescriptor,
        VideoCodecFamily,
    },
    download::{JobEventKind, JobProgress, JobStage, OutputSelection},
    jobs::{
        EngineSettings, JobErrorClass, JobRecord, JobState, OutputAvailability, QueueEvent,
        UpdatePreference,
    },
    resolver::ToolPathSource,
    tool::Tool,
};

/// Current IPC schema version.
pub const IPC_SCHEMA_VERSION: u16 = 1;

/// Tauri event name carrying [`JobEventEnvelopeDto`].
pub const JOB_EVENT_NAME: &str = "job-event-v1";

/// Command names registered by the native shell and consumed by the typed client.
pub const COMMAND_NAMES: [&str; 16] = [
    "bootstrap",
    "analyze",
    "enqueue",
    "list_jobs",
    "get_job",
    "pause_job",
    "resume_job",
    "cancel_job",
    "retry_job",
    "list_history",
    "delete_history",
    "read_settings",
    "update_settings",
    "choose_destination",
    "reveal_output",
    "tool_status",
];

/// Stable error codes exposed to the webview.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum IpcErrorCodeDto {
    /// Transport input did not satisfy a public contract.
    InvalidRequest,
    /// A job identity was syntactically invalid.
    InvalidJobId,
    /// No job exists for the supplied identity.
    JobNotFound,
    /// The requested operation is invalid for the current job state.
    InvalidJobState,
    /// Required verified tools are unavailable.
    ToolsUnavailable,
    /// Media analysis failed safely.
    AnalysisFailed,
    /// Queue persistence or recovery is unavailable.
    PersistenceUnavailable,
    /// The service is shutting down.
    ShuttingDown,
    /// Native folder selection failed.
    DestinationSelectionFailed,
    /// A completed output is unavailable or cannot be revealed.
    RevealFailed,
    /// An unexpected native failure occurred.
    Internal,
}

/// One bounded structured error detail.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ErrorDetailDto {
    /// Stable detail key.
    pub key: String,
    /// Bounded safe value.
    pub value: String,
}

/// Redacted IPC error safe to display.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct IpcErrorDto {
    /// Stable machine code.
    pub code: IpcErrorCodeDto,
    /// Safe user-facing explanation.
    pub message: String,
    /// Optional structured safe context.
    pub details: Vec<ErrorDetailDto>,
}

impl IpcErrorDto {
    /// Creates an error without exposing a source chain.
    #[must_use]
    pub fn new(code: IpcErrorCodeDto, message: impl Into<String>) -> Self {
        Self {
            code,
            message: bounded(message.into(), 1_024),
            details: Vec::new(),
        }
    }

    /// Adds one bounded structured detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.push(ErrorDetailDto {
            key: bounded(key.into(), 64),
            value: bounded(value.into(), 256),
        });
        self
    }
}

impl std::fmt::Display for IpcErrorDto {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IpcErrorDto {}

/// Bootstrap health after persistence recovery and tool resolution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum BootstrapHealthDto {
    /// Queue and verified media tools are ready.
    Healthy,
    /// Persistence is ready, but media tools require repair.
    Degraded,
    /// Persistence could not be initialized safely.
    Failed,
}

/// Stable external tool identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum ToolNameDto {
    /// yt-dlp extractor.
    YtDlp,
    /// `FFmpeg` processor.
    Ffmpeg,
    /// `FFprobe` verifier.
    Ffprobe,
    /// Deno runtime.
    Deno,
}

impl From<Tool> for ToolNameDto {
    fn from(value: Tool) -> Self {
        match value {
            Tool::YtDlp => Self::YtDlp,
            Tool::Ffmpeg => Self::Ffmpeg,
            Tool::Ffprobe => Self::Ffprobe,
            Tool::Deno => Self::Deno,
        }
    }
}

/// Verified resolution tier without disclosing an executable path.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum ToolSourceDto {
    /// Explicit developer override.
    ExplicitOverride,
    /// Verified managed update.
    ManagedUpdate,
    /// Verified bundled baseline.
    BundledBaseline,
    /// Development-only system path.
    DevelopmentPath,
}

impl From<ToolPathSource> for ToolSourceDto {
    fn from(value: ToolPathSource) -> Self {
        match value {
            ToolPathSource::ExplicitOverride => Self::ExplicitOverride,
            ToolPathSource::ManagedUpdate => Self::ManagedUpdate,
            ToolPathSource::BundledBaseline => Self::BundledBaseline,
            ToolPathSource::DevelopmentPath => Self::DevelopmentPath,
        }
    }
}

/// Safe status for one required tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ToolStatusDto {
    /// Tool identity.
    pub tool: ToolNameDto,
    /// Whether a verified executable is ready.
    pub ready: bool,
    /// Selected verified tier.
    pub source: Option<ToolSourceDto>,
    /// Safe remediation message when unavailable.
    pub message: Option<String>,
}

/// Authoritative startup and reconnect snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
pub struct BootstrapStateDto {
    /// IPC schema version.
    pub schema_version: u16,
    /// Persistence and tool readiness.
    pub health: BootstrapHealthDto,
    /// Highest event sequence covered by this snapshot, encoded losslessly.
    pub last_event_sequence: String,
    /// Durable authoritative jobs.
    pub jobs: Vec<JobDto>,
    /// Persisted engine settings when persistence is ready.
    pub settings: Option<SettingsDto>,
    /// Safe required-tool statuses.
    pub tools: Vec<ToolStatusDto>,
    /// Recoverable startup diagnostic.
    pub diagnostic: Option<IpcErrorDto>,
}

/// Request to analyze one public video URL.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct AnalyzeRequestDto {
    /// Untrusted URL text.
    pub url: String,
}

/// Successful normalized media analysis.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
pub struct AnalyzeResponseDto {
    /// Current IPC schema.
    pub schema_version: u16,
    /// Engine-normalized media information.
    pub media: MediaInfoDto,
}

/// One thumbnail candidate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ThumbnailDto {
    /// Validated HTTP(S) URL.
    pub url: String,
    /// Pixel width when known.
    pub width: Option<u32>,
    /// Pixel height when known.
    pub height: Option<u32>,
}

/// Recognized video codec family.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum VideoCodecFamilyDto {
    /// H.264/AVC.
    H264,
    /// VP9.
    Vp9,
    /// AV1.
    Av1,
    /// Another bounded codec.
    Other,
}

/// Video codec information.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct VideoCodecDto {
    /// Normalized codec name.
    pub name: String,
    /// Recognized family.
    pub family: VideoCodecFamilyDto,
}

/// Recognized audio codec family.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum AudioCodecFamilyDto {
    /// AAC/MPEG-4 Audio.
    Aac,
    /// Opus.
    Opus,
    /// Vorbis.
    Vorbis,
    /// MP3.
    Mp3,
    /// Another bounded codec.
    Other,
}

/// Audio codec information.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct AudioCodecDto {
    /// Normalized codec name.
    pub name: String,
    /// Recognized family.
    pub family: AudioCodecFamilyDto,
}

/// Recognized source container.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ContainerFamilyDto {
    /// MP4.
    Mp4,
    /// M4A.
    M4a,
    /// `WebM`.
    Webm,
    /// Another bounded container.
    Other,
}

/// Source container information.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ContainerDto {
    /// Normalized container name.
    pub name: String,
    /// Recognized family.
    pub family: ContainerFamilyDto,
}

/// One selected source stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct SourceFormatDto {
    /// Extractor format identity.
    pub format_id: String,
    /// Source container.
    pub container: ContainerDto,
    /// Video codec when present.
    pub video_codec: Option<VideoCodecDto>,
    /// Audio codec when present.
    pub audio_codec: Option<AudioCodecDto>,
}

/// Compatibility work needed for MP4 output.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityWorkDto {
    /// No merge or transcode.
    None,
    /// Merge compatible streams.
    Merge,
    /// Transcode video.
    VideoTranscode,
    /// Transcode audio.
    AudioTranscode,
    /// Transcode both streams.
    VideoAndAudioTranscode,
}

/// One engine-normalized format option.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FormatOptionDto {
    /// MP3 bitrate choice.
    Mp3 {
        /// Constant bitrate.
        bitrate_kbps: u16,
        /// Selected source.
        source: SourceFormatDto,
    },
    /// MP4 source-height choice.
    Mp4 {
        /// Source height.
        height: u32,
        /// Source width.
        width: Option<u32>,
        /// Frames per second.
        fps: Option<f64>,
        /// Estimated combined size.
        estimated_size_bytes: Option<u64>,
        /// Selected video-bearing source.
        video_source: SourceFormatDto,
        /// Selected audio-bearing source.
        audio_source: SourceFormatDto,
        /// Required compatibility work.
        compatibility: CompatibilityWorkDto,
    },
}

/// Normalized bounded media information.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
pub struct MediaInfoDto {
    /// Eleven-character video identity.
    pub id: String,
    /// Canonical video URL.
    pub url: String,
    /// Bounded title.
    pub title: String,
    /// Uploader when present.
    pub uploader: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// View count when present.
    pub view_count: Option<u64>,
    /// Upload date when present.
    pub upload_date: Option<String>,
    /// Validated thumbnails.
    pub thumbnails: Vec<ThumbnailDto>,
    /// Engine-normalized choices.
    pub formats: Vec<FormatOptionDto>,
    /// Bounded analyzer warnings.
    pub warnings: Vec<String>,
}

impl From<&MediaInfo> for MediaInfoDto {
    fn from(value: &MediaInfo) -> Self {
        Self {
            id: value.id.as_str().to_owned(),
            url: value.url.as_str().to_owned(),
            title: value.title.clone(),
            uploader: value.uploader.clone(),
            duration_ms: value.duration.as_millis(),
            view_count: value.view_count,
            upload_date: value.upload_date.clone(),
            thumbnails: value
                .thumbnails
                .iter()
                .map(|thumbnail| ThumbnailDto {
                    url: thumbnail.url.clone(),
                    width: thumbnail.width,
                    height: thumbnail.height,
                })
                .collect(),
            formats: value.formats.iter().map(FormatOptionDto::from).collect(),
            warnings: value.warnings.clone(),
        }
    }
}

impl From<&FormatOption> for FormatOptionDto {
    fn from(value: &FormatOption) -> Self {
        match value {
            FormatOption::Mp3 {
                bitrate_kbps,
                source,
            } => Self::Mp3 {
                bitrate_kbps: *bitrate_kbps,
                source: SourceFormatDto::from(source),
            },
            FormatOption::Mp4 {
                height,
                width,
                fps,
                estimated_size_bytes,
                video_source,
                audio_source,
                compatibility,
            } => Self::Mp4 {
                height: *height,
                width: *width,
                fps: *fps,
                estimated_size_bytes: *estimated_size_bytes,
                video_source: SourceFormatDto::from(video_source),
                audio_source: SourceFormatDto::from(audio_source),
                compatibility: CompatibilityWorkDto::from(*compatibility),
            },
        }
    }
}

impl From<&SourceFormat> for SourceFormatDto {
    fn from(value: &SourceFormat) -> Self {
        Self {
            format_id: value.format_id.as_str().to_owned(),
            container: ContainerDto::from(&value.container),
            video_codec: value.video_codec.as_ref().map(VideoCodecDto::from),
            audio_codec: value.audio_codec.as_ref().map(AudioCodecDto::from),
        }
    }
}

impl From<&ContainerDescriptor> for ContainerDto {
    fn from(value: &ContainerDescriptor) -> Self {
        Self {
            name: value.name.clone(),
            family: match value.family {
                ContainerFamily::Mp4 => ContainerFamilyDto::Mp4,
                ContainerFamily::M4a => ContainerFamilyDto::M4a,
                ContainerFamily::Webm => ContainerFamilyDto::Webm,
                ContainerFamily::Other => ContainerFamilyDto::Other,
            },
        }
    }
}

impl From<&VideoCodecDescriptor> for VideoCodecDto {
    fn from(value: &VideoCodecDescriptor) -> Self {
        Self {
            name: value.name.clone(),
            family: match value.family {
                VideoCodecFamily::H264 => VideoCodecFamilyDto::H264,
                VideoCodecFamily::Vp9 => VideoCodecFamilyDto::Vp9,
                VideoCodecFamily::Av1 => VideoCodecFamilyDto::Av1,
                VideoCodecFamily::Other => VideoCodecFamilyDto::Other,
            },
        }
    }
}

impl From<&AudioCodecDescriptor> for AudioCodecDto {
    fn from(value: &AudioCodecDescriptor) -> Self {
        Self {
            name: value.name.clone(),
            family: match value.family {
                AudioCodecFamily::Aac => AudioCodecFamilyDto::Aac,
                AudioCodecFamily::Opus => AudioCodecFamilyDto::Opus,
                AudioCodecFamily::Vorbis => AudioCodecFamilyDto::Vorbis,
                AudioCodecFamily::Mp3 => AudioCodecFamilyDto::Mp3,
                AudioCodecFamily::Other => AudioCodecFamilyDto::Other,
            },
        }
    }
}

impl From<CompatibilityWork> for CompatibilityWorkDto {
    fn from(value: CompatibilityWork) -> Self {
        match value {
            CompatibilityWork::None => Self::None,
            CompatibilityWork::Merge => Self::Merge,
            CompatibilityWork::VideoTranscode => Self::VideoTranscode,
            CompatibilityWork::AudioTranscode => Self::AudioTranscode,
            CompatibilityWork::VideoAndAudioTranscode => Self::VideoAndAudioTranscode,
        }
    }
}

/// Requested output and quality.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "format", content = "quality", rename_all = "lowercase")]
pub enum OutputSelectionDto {
    /// MP3 bitrate in kilobits per second.
    Mp3(u16),
    /// MP4 source height in pixels.
    Mp4(u32),
}

impl From<OutputSelection> for OutputSelectionDto {
    fn from(value: OutputSelection) -> Self {
        match value {
            OutputSelection::Mp3(quality) => Self::Mp3(quality.bitrate_kbps()),
            OutputSelection::Mp4(quality) => Self::Mp4(quality.height()),
        }
    }
}

/// Request to persist and explicitly schedule a job.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct EnqueueRequestDto {
    /// Public single-video URL.
    pub url: String,
    /// Requested normalized output.
    pub output: OutputSelectionDto,
    /// User-facing destination directory.
    pub destination: String,
    /// Optional requested output stem.
    pub name: Option<String>,
}

/// Request containing one job identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct JobIdRequestDto {
    /// `UUIDv7` job identity.
    pub job_id: String,
}

/// Durable lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum JobStateDto {
    /// Waiting for a scheduler slot.
    Queued,
    /// Refreshing metadata.
    Analyzing,
    /// Downloading.
    Downloading,
    /// Merging.
    Merging,
    /// Converting.
    Converting,
    /// Explicitly paused.
    Paused,
    /// Recovered and waiting for explicit resume.
    Interrupted,
    /// Verified output published.
    Completed,
    /// Explicitly cancelled.
    Cancelled,
    /// Failed.
    Failed,
}

impl From<JobState> for JobStateDto {
    fn from(value: JobState) -> Self {
        match value {
            JobState::Queued => Self::Queued,
            JobState::Analyzing => Self::Analyzing,
            JobState::Downloading => Self::Downloading,
            JobState::Merging => Self::Merging,
            JobState::Converting => Self::Converting,
            JobState::Paused => Self::Paused,
            JobState::Interrupted => Self::Interrupted,
            JobState::Completed => Self::Completed,
            JobState::Cancelled => Self::Cancelled,
            JobState::Failed => Self::Failed,
        }
    }
}

/// Active engine stage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum JobStageDto {
    /// Analyzing.
    Analyzing,
    /// Downloading.
    Downloading,
    /// Merging.
    Merging,
    /// Converting.
    Converting,
    /// Verifying and publishing.
    Finalizing,
    /// Completed.
    Completed,
    /// Paused.
    Paused,
    /// Cancelled.
    Cancelled,
    /// Failed.
    Failed,
}

impl From<JobStage> for JobStageDto {
    fn from(value: JobStage) -> Self {
        match value {
            JobStage::Analyzing => Self::Analyzing,
            JobStage::Downloading => Self::Downloading,
            JobStage::Merging => Self::Merging,
            JobStage::Converting => Self::Converting,
            JobStage::Finalizing => Self::Finalizing,
            JobStage::Completed => Self::Completed,
            JobStage::Paused => Self::Paused,
            JobStage::Cancelled => Self::Cancelled,
            JobStage::Failed => Self::Failed,
        }
    }
}

/// Bounded progress snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
pub struct JobProgressDto {
    /// Current stage.
    pub stage: JobStageDto,
    /// Completed work.
    pub completed: u64,
    /// Known total.
    pub total: Option<u64>,
    /// Percentage from zero through one hundred.
    pub percent: Option<f64>,
    /// Transfer speed.
    pub bytes_per_second: Option<u64>,
    /// Estimated seconds remaining.
    pub eta_seconds: Option<u64>,
}

impl From<&JobProgress> for JobProgressDto {
    fn from(value: &JobProgress) -> Self {
        Self {
            stage: value.stage.into(),
            completed: value.completed,
            total: value.total,
            percent: value.percent,
            bytes_per_second: value.bytes_per_second,
            eta_seconds: value.eta_seconds,
        }
    }
}

/// Stable job failure class.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum JobErrorClassDto {
    /// Invalid request.
    InvalidRequest,
    /// Analysis failure.
    Analysis,
    /// Format disappeared.
    FormatUnavailable,
    /// Destination unavailable.
    DestinationUnavailable,
    /// Filesystem failure.
    Filesystem,
    /// Tool process failure.
    Process,
    /// Protocol failure.
    Protocol,
    /// Output verification failure.
    Verification,
    /// Internal orchestration failure.
    Internal,
}

impl From<JobErrorClass> for JobErrorClassDto {
    fn from(value: JobErrorClass) -> Self {
        match value {
            JobErrorClass::InvalidRequest => Self::InvalidRequest,
            JobErrorClass::Analysis => Self::Analysis,
            JobErrorClass::FormatUnavailable => Self::FormatUnavailable,
            JobErrorClass::DestinationUnavailable => Self::DestinationUnavailable,
            JobErrorClass::Filesystem => Self::Filesystem,
            JobErrorClass::Process => Self::Process,
            JobErrorClass::Protocol => Self::Protocol,
            JobErrorClass::Verification => Self::Verification,
            JobErrorClass::Internal => Self::Internal,
        }
    }
}

/// Persisted safe job failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct JobFailureDto {
    /// Stable classification.
    pub class: JobErrorClassDto,
    /// Bounded safe message.
    pub message: String,
}

/// Completed output availability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum OutputAvailabilityDto {
    /// Job has no output.
    NotApplicable,
    /// Output is present.
    Present,
    /// Output is missing.
    Missing,
}

/// User-facing verified final output.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct FinalOutputDto {
    /// Published output path.
    pub path: String,
    /// Published size.
    pub size_bytes: u64,
    /// Requested output.
    pub output: OutputSelectionDto,
}

/// Durable authoritative job snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
pub struct JobDto {
    /// `UUIDv7` job identity.
    pub id: String,
    /// Canonical URL.
    pub canonical_url: String,
    /// Output selection.
    pub output: OutputSelectionDto,
    /// User-facing destination.
    pub destination: String,
    /// Optional requested name.
    pub name: Option<String>,
    /// Current state.
    pub state: JobStateDto,
    /// Current progress.
    pub progress: Option<JobProgressDto>,
    /// Persisted safe failure.
    pub error: Option<JobFailureDto>,
    /// Creation timestamp in epoch milliseconds.
    pub created_at_ms: i64,
    /// Update timestamp in epoch milliseconds.
    pub updated_at_ms: i64,
    /// Completion timestamp in epoch milliseconds.
    pub completed_at_ms: Option<i64>,
    /// Explicitly started attempts.
    pub attempt_count: u32,
    /// Verified output.
    pub final_output: Option<FinalOutputDto>,
    /// Current output availability.
    pub output_availability: OutputAvailabilityDto,
    /// Destination availability.
    pub destination_available: bool,
}

impl From<&JobRecord> for JobDto {
    fn from(value: &JobRecord) -> Self {
        Self {
            id: value.id.as_str().to_owned(),
            canonical_url: value.request.canonical_url.clone(),
            output: value.request.output.into(),
            destination: display_path(&value.request.destination),
            name: value.request.name.clone(),
            state: value.state.into(),
            progress: value.progress.as_ref().map(JobProgressDto::from),
            error: value.error.as_ref().map(|error| JobFailureDto {
                class: error.class.into(),
                message: error.message.clone(),
            }),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
            completed_at_ms: value.completed_at_ms,
            attempt_count: value.attempt_count,
            final_output: value.final_output.as_ref().map(|output| FinalOutputDto {
                path: display_path(&output.path),
                size_bytes: output.size_bytes,
                output: output.output.into(),
            }),
            output_availability: match value.output_availability {
                OutputAvailability::NotApplicable => OutputAvailabilityDto::NotApplicable,
                OutputAvailability::Present => OutputAvailabilityDto::Present,
                OutputAvailability::Missing => OutputAvailabilityDto::Missing,
            },
            destination_available: value.destination_available,
        }
    }
}

/// Optional transient queue activity.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum JobActivityDto {
    /// Stage transition.
    Stage {
        /// New stage.
        stage: JobStageDto,
    },
    /// Progress update.
    Progress {
        /// New progress.
        progress: JobProgressDto,
    },
    /// Safe warning.
    Warning {
        /// Bounded warning.
        message: String,
    },
}

impl From<&JobEventKind> for JobActivityDto {
    fn from(value: &JobEventKind) -> Self {
        match value {
            JobEventKind::Stage { stage } => Self::Stage {
                stage: (*stage).into(),
            },
            JobEventKind::Progress { progress } => Self::Progress {
                progress: JobProgressDto::from(progress),
            },
            JobEventKind::Warning { message } => Self::Warning {
                message: message.clone(),
            },
        }
    }
}

/// Versioned job event emitted by the native shell.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
pub struct JobEventEnvelopeDto {
    /// IPC schema version.
    pub schema_version: u16,
    /// Lossless monotonic sequence.
    pub sequence: String,
    /// `UUIDv7` job identity.
    pub job_id: String,
    /// Authoritative update timestamp in epoch milliseconds.
    pub timestamp_ms: i64,
    /// Authoritative state.
    pub state: JobStateDto,
    /// Latest progress.
    pub progress: Option<JobProgressDto>,
    /// Terminal output when completed.
    pub result: Option<FinalOutputDto>,
    /// Terminal safe failure when failed.
    pub error: Option<JobFailureDto>,
    /// Optional transient activity.
    pub activity: Option<JobActivityDto>,
}

impl From<&QueueEvent> for JobEventEnvelopeDto {
    fn from(value: &QueueEvent) -> Self {
        let job = JobDto::from(&value.job);
        Self {
            schema_version: IPC_SCHEMA_VERSION,
            sequence: value.sequence.to_string(),
            job_id: job.id,
            timestamp_ms: job.updated_at_ms,
            state: job.state,
            progress: job.progress,
            result: job.final_output,
            error: job.error,
            activity: value.activity.as_ref().map(JobActivityDto::from),
        }
    }
}

/// Persisted update preference.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum UpdatePreferenceDto {
    /// Notify.
    Notify,
    /// Activate verified updates automatically.
    Automatic,
    /// Do not check.
    Disabled,
}

impl From<UpdatePreference> for UpdatePreferenceDto {
    fn from(value: UpdatePreference) -> Self {
        match value {
            UpdatePreference::Notify => Self::Notify,
            UpdatePreference::Automatic => Self::Automatic,
            UpdatePreference::Disabled => Self::Disabled,
        }
    }
}

/// Persisted engine settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct SettingsDto {
    /// Default destination.
    pub default_destination: Option<String>,
    /// Shared one-through-four concurrency.
    pub queue_concurrency: u8,
    /// Managed update preference.
    pub update_preference: UpdatePreferenceDto,
    /// Last normalized output.
    pub last_output: OutputSelectionDto,
}

impl From<&EngineSettings> for SettingsDto {
    fn from(value: &EngineSettings) -> Self {
        Self {
            default_destination: value.default_destination.as_deref().map(display_path),
            queue_concurrency: value.queue_concurrency.get(),
            update_preference: value.update_preference.into(),
            last_output: value.last_output.into(),
        }
    }
}

/// Explicit default-destination mutation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "action", content = "value", rename_all = "kebab-case")]
pub enum DefaultDestinationUpdateDto {
    /// Leave the existing value unchanged.
    Unchanged,
    /// Remove the existing value.
    Clear,
    /// Set a user-facing path.
    Set(String),
}

/// Partial settings mutation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct UpdateSettingsRequestDto {
    /// Default destination mutation.
    pub default_destination: DefaultDestinationUpdateDto,
    /// Replacement concurrency.
    pub queue_concurrency: Option<u8>,
    /// Replacement update preference.
    pub update_preference: Option<UpdatePreferenceDto>,
    /// Replacement output default.
    pub last_output: Option<OutputSelectionDto>,
}

/// Native folder selection result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct DestinationSelectionDto {
    /// Selected user-facing path, or `null` when cancelled.
    pub path: Option<String>,
}

/// Successful unit-like native action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct ActionResultDto {
    /// Current IPC schema.
    pub schema_version: u16,
}

/// Returns deterministic checked-in TypeScript declarations.
#[must_use]
pub fn generated_typescript() -> String {
    let config = ts_rs::Config::default();
    let declarations = [
        IpcErrorCodeDto::decl(&config),
        ErrorDetailDto::decl(&config),
        IpcErrorDto::decl(&config),
        BootstrapHealthDto::decl(&config),
        ToolNameDto::decl(&config),
        ToolSourceDto::decl(&config),
        ToolStatusDto::decl(&config),
        ThumbnailDto::decl(&config),
        VideoCodecFamilyDto::decl(&config),
        VideoCodecDto::decl(&config),
        AudioCodecFamilyDto::decl(&config),
        AudioCodecDto::decl(&config),
        ContainerFamilyDto::decl(&config),
        ContainerDto::decl(&config),
        SourceFormatDto::decl(&config),
        CompatibilityWorkDto::decl(&config),
        FormatOptionDto::decl(&config),
        MediaInfoDto::decl(&config),
        AnalyzeRequestDto::decl(&config),
        AnalyzeResponseDto::decl(&config),
        OutputSelectionDto::decl(&config),
        EnqueueRequestDto::decl(&config),
        JobIdRequestDto::decl(&config),
        JobStateDto::decl(&config),
        JobStageDto::decl(&config),
        JobProgressDto::decl(&config),
        JobErrorClassDto::decl(&config),
        JobFailureDto::decl(&config),
        OutputAvailabilityDto::decl(&config),
        FinalOutputDto::decl(&config),
        JobDto::decl(&config),
        JobActivityDto::decl(&config),
        JobEventEnvelopeDto::decl(&config),
        UpdatePreferenceDto::decl(&config),
        SettingsDto::decl(&config),
        DefaultDestinationUpdateDto::decl(&config),
        UpdateSettingsRequestDto::decl(&config),
        DestinationSelectionDto::decl(&config),
        ActionResultDto::decl(&config),
        BootstrapStateDto::decl(&config),
    ];
    let generated = format!(
        "// Generated by `pnpm ipc:generate`. Do not edit.\n\n{}\n",
        declarations
            .into_iter()
            .map(|declaration| format!("export {declaration}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    generated
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Writes or checks the generated TypeScript file.
///
/// # Errors
///
/// Returns an I/O error or a drift diagnostic.
pub fn write_typescript(path: &Path, check: bool) -> Result<(), String> {
    let generated = generated_typescript();
    if check {
        let existing = fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", display_path(path)))?;
        if existing == generated {
            Ok(())
        } else {
            Err(format!(
                "{} is stale; run `pnpm ipc:generate`",
                display_path(path)
            ))
        }
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", display_path(parent)))?;
        }
        fs::write(path, generated)
            .map_err(|error| format!("could not write {}: {error}", display_path(path)))
    }
}

/// Resolves the checked-in generated file relative to this crate.
#[must_use]
pub fn generated_typescript_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/lib/ipc")
        .join("generated.ts")
}

fn display_path(path: &Path) -> String {
    bounded(path.to_string_lossy(), 4_096)
}

fn bounded(value: impl AsRef<str>, maximum: usize) -> String {
    value.as_ref().chars().take(maximum).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        COMMAND_NAMES, IpcErrorCodeDto, IpcErrorDto, generated_typescript,
        generated_typescript_path,
    };

    #[test]
    fn generated_types_are_current() {
        let existing = std::fs::read_to_string(generated_typescript_path());
        assert!(existing.is_ok());
        assert_eq!(
            existing.ok().as_deref(),
            Some(generated_typescript().as_str())
        );
    }

    #[test]
    fn errors_bound_messages_and_details() {
        let error = IpcErrorDto::new(IpcErrorCodeDto::Internal, "x".repeat(2_000))
            .with_detail("job", "y".repeat(400));
        assert_eq!(error.message.chars().count(), 1_024);
        assert_eq!(error.details[0].value.chars().count(), 256);
    }

    #[test]
    fn typed_client_declares_every_registered_command() {
        let client = include_str!("../../src/lib/ipc/client.ts");
        let native_registry = include_str!("lib.rs");
        for command in COMMAND_NAMES {
            assert!(
                client.contains(&format!("'{command}'")),
                "typed client is missing `{command}`"
            );
            assert!(
                native_registry.contains(&format!("commands::{command},")),
                "Tauri handler registry is missing `{command}`"
            );
        }
        let invoke_count = client.matches("invoke<").count();
        assert_eq!(invoke_count, COMMAND_NAMES.len());
    }
}
