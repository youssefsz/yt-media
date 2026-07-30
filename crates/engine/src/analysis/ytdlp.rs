//! Bounded yt-dlp invocation, private response parsing, and deterministic normalization.

use std::{cmp::Ordering, collections::BTreeSet, sync::Arc, time::Duration as StdDuration};

use serde::Deserialize;
use thiserror::Error;

use crate::{
    cancellation::CancellationToken,
    path::ExecutablePath,
    process::{
        OutputLimit, ProcessError, ProcessOutput, ProcessRunner, ProcessSpec, ProcessSpecError,
        TokioProcessRunner,
    },
    resolver::ResolvedTool,
    tool::Tool,
};

use super::{
    AudioCodecDescriptor, AudioCodecFamily, CompatibilityWork, ContainerDescriptor,
    ContainerFamily, Duration, FormatId, FormatOption, MediaId, MediaInfo, MediaUrl, SourceFormat,
    Thumbnail, VideoCodecDescriptor, VideoCodecFamily,
};

const ANALYZE_TIMEOUT: StdDuration = StdDuration::from_mins(2);
const PROCESS_MAX_BYTES_PER_STREAM: usize = 4 * 1024 * 1024;
const PROCESS_MAX_LINES_PER_STREAM: usize = 10_000;
const STDERR_MAX_BYTES: u64 = 256 * 1024;
const STDERR_MAX_LINES: u64 = 256;
const MAX_FORMATS: usize = 512;
const MAX_RAW_THUMBNAILS: usize = 100;
const MAX_PUBLIC_THUMBNAILS: usize = 20;
const MAX_TITLE_CHARS: usize = 512;
const MAX_UPLOADER_CHARS: usize = 256;
const MAX_FORMAT_ID_CHARS: usize = 128;
const MAX_CODEC_CHARS: usize = 128;
const MAX_CONTAINER_CHARS: usize = 32;
const MAX_THUMBNAIL_URL_CHARS: usize = 2_048;
const MAX_WARNING_LINES: usize = 32;
const MAX_WARNING_CHARS: usize = 512;
const MAX_DIAGNOSTIC_CHARS: usize = 4_096;
const MAX_DURATION_SECONDS: f64 = 7.0 * 24.0 * 60.0 * 60.0;
const MAX_DIMENSION: u32 = 16_384;
const MAX_FPS: f64 = 240.0;
const MAX_BITRATE_KBPS: f64 = 1_000_000.0;
const MAX_FILESIZE_BYTES: u64 = 16 * 1024 * 1024 * 1024 * 1024;
const MAX_VIEW_COUNT: u64 = 1_000_000_000_000_000;
const MP3_BITRATES: [u16; 4] = [128, 192, 256, 320];

/// Explicit executable paths already validated or resolved through the engine boundary.
#[derive(Clone, Debug)]
pub struct AnalysisTools {
    yt_dlp: ExecutablePath,
    ffmpeg: ExecutablePath,
    deno: ExecutablePath,
}

impl AnalysisTools {
    /// Creates an analyzer tool set from explicit validated executable paths.
    #[must_use]
    pub const fn new(yt_dlp: ExecutablePath, ffmpeg: ExecutablePath, deno: ExecutablePath) -> Self {
        Self {
            yt_dlp,
            ffmpeg,
            deno,
        }
    }

    /// Creates an analyzer tool set from three engine-resolved tool records.
    ///
    /// # Errors
    ///
    /// Returns a typed error if a record's identity does not match its required position.
    pub fn from_resolved(
        yt_dlp: ResolvedTool,
        ffmpeg: ResolvedTool,
        deno: ResolvedTool,
    ) -> Result<Self, AnalysisToolError> {
        require_tool(&yt_dlp, Tool::YtDlp)?;
        require_tool(&ffmpeg, Tool::Ffmpeg)?;
        require_tool(&deno, Tool::Deno)?;
        Ok(Self::new(yt_dlp.path, ffmpeg.path, deno.path))
    }

    /// Returns the explicit yt-dlp path.
    #[must_use]
    pub fn yt_dlp(&self) -> &ExecutablePath {
        &self.yt_dlp
    }

    /// Returns the explicit `FFmpeg` path supplied to yt-dlp.
    #[must_use]
    pub fn ffmpeg(&self) -> &ExecutablePath {
        &self.ffmpeg
    }

    /// Returns the explicit Deno path supplied to yt-dlp.
    #[must_use]
    pub fn deno(&self) -> &ExecutablePath {
        &self.deno
    }
}

fn require_tool(resolved: &ResolvedTool, expected: Tool) -> Result<(), AnalysisToolError> {
    if resolved.tool == expected {
        Ok(())
    } else {
        Err(AnalysisToolError::UnexpectedTool {
            expected,
            found: resolved.tool,
        })
    }
}

/// Invalid tool composition for analysis.
#[derive(Debug, Error)]
pub enum AnalysisToolError {
    /// A resolved record occupied the wrong role.
    #[error("analysis expected `{expected}` but received `{found}`")]
    UnexpectedTool {
        /// Required identity.
        expected: Tool,
        /// Supplied identity.
        found: Tool,
    },
}

/// The stream whose bounded protocol data was invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalysisStream {
    /// yt-dlp standard output.
    Stdout,
    /// yt-dlp standard error.
    Stderr,
}

/// Content outside the public, on-demand, single-video v1 boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnsupportedContent {
    /// Private video.
    Private,
    /// Premium-only video.
    PremiumOnly,
    /// Subscriber- or member-only video.
    SubscriberOnly,
    /// Content that requires authentication.
    AuthenticationRequired,
    /// Active live stream.
    Live,
    /// Scheduled stream that has not started.
    Upcoming,
    /// Stream whose on-demand recording is not ready.
    PostLive,
    /// Playlist result.
    Playlist,
    /// Another non-video extractor result.
    OtherType(String),
    /// A new availability state not recognized by this engine version.
    OtherAvailability(String),
}

impl std::fmt::Display for UnsupportedContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Private => formatter.write_str("private videos are unsupported"),
            Self::PremiumOnly => formatter.write_str("premium-only videos are unsupported"),
            Self::SubscriberOnly => {
                formatter.write_str("subscriber- or member-only videos are unsupported")
            }
            Self::AuthenticationRequired => {
                formatter.write_str("videos requiring authentication are unsupported")
            }
            Self::Live => formatter.write_str("active live streams are unsupported"),
            Self::Upcoming => formatter.write_str("upcoming live streams are unsupported"),
            Self::PostLive => {
                formatter.write_str("live recordings that are still processing are unsupported")
            }
            Self::Playlist => formatter.write_str("playlist results are unsupported"),
            Self::OtherType(kind) => write!(formatter, "content type `{kind}` is unsupported"),
            Self::OtherAvailability(availability) => {
                write!(
                    formatter,
                    "content availability `{availability}` is unsupported"
                )
            }
        }
    }
}

/// Invalid or unsafe machine-readable analysis data.
#[derive(Debug, Error)]
pub enum AnalysisDataError {
    /// JSON syntax or shape was invalid.
    #[error("yt-dlp did not return one valid JSON document")]
    InvalidJson(#[source] serde_json::Error),
    /// A required field was absent.
    #[error("yt-dlp response is missing required field `{field}`")]
    MissingRequired {
        /// Missing field name.
        field: &'static str,
    },
    /// A collection exceeded its public parser bound.
    #[error("yt-dlp field `{field}` contains {count} records; maximum is {maximum}")]
    TooManyRecords {
        /// Bounded field.
        field: &'static str,
        /// Observed record count.
        count: usize,
        /// Maximum record count.
        maximum: usize,
    },
    /// A string exceeded its field-specific bound.
    #[error("yt-dlp field `{field}` exceeds the maximum of {maximum} characters")]
    StringTooLong {
        /// Bounded field.
        field: &'static str,
        /// Maximum character count.
        maximum: usize,
    },
    /// A field had a nonsensical or unsafe value.
    #[error("yt-dlp field `{field}` is invalid: {reason}")]
    InvalidField {
        /// Invalid field.
        field: &'static str,
        /// Static validation reason.
        reason: &'static str,
    },
    /// Two stream records used the same identity.
    #[error("yt-dlp response repeats format ID `{format_id}`")]
    DuplicateFormatId {
        /// Repeated bounded identity.
        format_id: String,
    },
    /// Extractor identity differed from the validated URL.
    #[error("yt-dlp returned video ID `{found}` for requested ID `{expected}`")]
    MediaIdMismatch {
        /// URL identity.
        expected: String,
        /// Extractor identity.
        found: String,
    },
    /// No usable audio or video choice remained after validation.
    #[error("yt-dlp returned no usable audio or video formats")]
    NoUsableFormats,
}

/// Analysis failure with stable user-facing categories and preserved sources.
#[derive(Debug, Error)]
pub enum AnalyzeError {
    /// Cancellation reached the process owner and reaped the complete tree.
    #[error("analysis was cancelled")]
    Cancelled,
    /// Process creation, timeout, cleanup, or I/O failed.
    #[error("yt-dlp process execution failed")]
    Process(#[source] Box<ProcessError>),
    /// The engine could not construct its bounded process contract.
    #[error("invalid yt-dlp process specification")]
    InvalidProcessSpecification(#[source] ProcessSpecError),
    /// Process output exceeded a protocol bound.
    #[error("yt-dlp {stream:?} exceeded its {kind} limit: observed {observed}, maximum {maximum}")]
    OutputLimitExceeded {
        /// Affected stream.
        stream: AnalysisStream,
        /// Byte or line limit.
        kind: &'static str,
        /// Observed count.
        observed: u64,
        /// Maximum count.
        maximum: u64,
    },
    /// Machine-readable stdout or diagnostic stderr was not UTF-8.
    #[error("yt-dlp {stream:?} was not valid UTF-8")]
    InvalidUtf8 {
        /// Invalid stream.
        stream: AnalysisStream,
        /// UTF-8 decoding source.
        #[source]
        source: std::str::Utf8Error,
    },
    /// yt-dlp reported failure.
    #[error("yt-dlp extraction failed with status {status:?}: {diagnostics}")]
    ExtractionFailed {
        /// Portable exit code when available.
        status: Option<i32>,
        /// Bounded diagnostic text.
        diagnostics: String,
    },
    /// Content is outside the v1 boundary.
    #[error("{0}")]
    UnsupportedContent(UnsupportedContent),
    /// Machine JSON failed validation or normalization.
    #[error(transparent)]
    InvalidData(#[from] AnalysisDataError),
}

/// Asynchronous engine service for one validated media URL.
#[derive(Clone)]
pub struct Analyzer {
    runner: Arc<dyn ProcessRunner>,
    tools: AnalysisTools,
}

impl std::fmt::Debug for Analyzer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Analyzer")
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl Analyzer {
    /// Creates an analyzer with the production Tokio process adapter.
    #[must_use]
    pub fn new(tools: AnalysisTools) -> Self {
        Self {
            runner: Arc::new(TokioProcessRunner),
            tools,
        }
    }

    /// Creates an analyzer with a testable process port.
    #[must_use]
    pub fn with_runner(tools: AnalysisTools, runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner, tools }
    }

    /// Analyzes one previously validated public `YouTube` video.
    ///
    /// # Errors
    ///
    /// Returns a typed cancellation, process, output-bound, encoding, extraction, unsupported
    /// content, or normalization error.
    pub async fn analyze(
        &self,
        url: &MediaUrl,
        cancellation: CancellationToken,
    ) -> Result<MediaInfo, AnalyzeError> {
        let spec = build_process_spec(&self.tools, url)?;
        let output = self
            .runner
            .run(spec, cancellation)
            .await
            .map_err(map_process_error)?;
        parse_process_output(&output, url)
    }
}

fn build_process_spec(tools: &AnalysisTools, url: &MediaUrl) -> Result<ProcessSpec, AnalyzeError> {
    let output_limit = OutputLimit::new(PROCESS_MAX_BYTES_PER_STREAM, PROCESS_MAX_LINES_PER_STREAM)
        .map_err(AnalyzeError::InvalidProcessSpecification)?;
    let runtime = format!("deno:{}", tools.deno.as_path().display());
    Ok(ProcessSpec::new(tools.yt_dlp.as_path())
        .arguments([
            "--ignore-config",
            "--no-config-locations",
            "--no-plugin-dirs",
            "--no-update",
            "--no-playlist",
            "--simulate",
            "--dump-single-json",
            "--no-js-runtimes",
            "--js-runtimes",
        ])
        .argument(runtime)
        .argument("--ffmpeg-location")
        .argument(tools.ffmpeg.as_path().as_os_str())
        .argument("--")
        .argument(url.as_str())
        .timeout(ANALYZE_TIMEOUT)
        .output_limit(output_limit))
}

fn map_process_error(error: ProcessError) -> AnalyzeError {
    if matches!(error, ProcessError::Cancelled { .. }) {
        AnalyzeError::Cancelled
    } else {
        AnalyzeError::Process(Box::new(error))
    }
}

fn parse_process_output(
    output: &ProcessOutput,
    requested_url: &MediaUrl,
) -> Result<MediaInfo, AnalyzeError> {
    enforce_output_bounds(output)?;
    let stderr = std::str::from_utf8(&output.capture.stderr.bytes).map_err(|source| {
        AnalyzeError::InvalidUtf8 {
            stream: AnalysisStream::Stderr,
            source,
        }
    })?;
    if !output.status.success {
        return Err(AnalyzeError::ExtractionFailed {
            status: output.status.code,
            diagnostics: bounded_diagnostics(stderr),
        });
    }
    let stdout = std::str::from_utf8(&output.capture.stdout.bytes).map_err(|source| {
        AnalyzeError::InvalidUtf8 {
            stream: AnalysisStream::Stdout,
            source,
        }
    })?;
    let raw: RawMedia = serde_json::from_str(stdout).map_err(AnalysisDataError::InvalidJson)?;
    normalize_media(raw, requested_url, normalized_warnings(stderr))
}

fn enforce_output_bounds(output: &ProcessOutput) -> Result<(), AnalyzeError> {
    for (stream, capture) in [
        (AnalysisStream::Stdout, &output.capture.stdout),
        (AnalysisStream::Stderr, &output.capture.stderr),
    ] {
        if capture.observed_bytes > u64::try_from(PROCESS_MAX_BYTES_PER_STREAM).unwrap_or(u64::MAX)
        {
            return Err(AnalyzeError::OutputLimitExceeded {
                stream,
                kind: "byte",
                observed: capture.observed_bytes,
                maximum: u64::try_from(PROCESS_MAX_BYTES_PER_STREAM).unwrap_or(u64::MAX),
            });
        }
        if capture.observed_lines > u64::try_from(PROCESS_MAX_LINES_PER_STREAM).unwrap_or(u64::MAX)
        {
            return Err(AnalyzeError::OutputLimitExceeded {
                stream,
                kind: "line",
                observed: capture.observed_lines,
                maximum: u64::try_from(PROCESS_MAX_LINES_PER_STREAM).unwrap_or(u64::MAX),
            });
        }
        if capture.truncated {
            return Err(AnalyzeError::OutputLimitExceeded {
                stream,
                kind: "retention",
                observed: capture.observed_bytes.max(capture.observed_lines),
                maximum: u64::try_from(PROCESS_MAX_BYTES_PER_STREAM).unwrap_or(u64::MAX),
            });
        }
    }
    if output.capture.stderr.observed_bytes > STDERR_MAX_BYTES {
        return Err(AnalyzeError::OutputLimitExceeded {
            stream: AnalysisStream::Stderr,
            kind: "byte",
            observed: output.capture.stderr.observed_bytes,
            maximum: STDERR_MAX_BYTES,
        });
    }
    if output.capture.stderr.observed_lines > STDERR_MAX_LINES {
        return Err(AnalyzeError::OutputLimitExceeded {
            stream: AnalysisStream::Stderr,
            kind: "line",
            observed: output.capture.stderr.observed_lines,
            maximum: STDERR_MAX_LINES,
        });
    }
    Ok(())
}

fn bounded_diagnostics(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "no diagnostic output".to_owned();
    }
    normalized.chars().take(MAX_DIAGNOSTIC_CHARS).collect()
}

fn normalized_warnings(value: &str) -> Vec<String> {
    value
        .lines()
        .filter_map(|line| {
            let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
            (!normalized.is_empty()).then(|| {
                normalized
                    .chars()
                    .take(MAX_WARNING_CHARS)
                    .collect::<String>()
            })
        })
        .take(MAX_WARNING_LINES)
        .collect()
}

#[derive(Debug, Deserialize)]
struct RawMedia {
    id: Option<String>,
    title: Option<String>,
    duration: Option<f64>,
    uploader: Option<String>,
    view_count: Option<u64>,
    upload_date: Option<String>,
    thumbnails: Option<Vec<RawThumbnail>>,
    formats: Option<Vec<RawFormat>>,
    availability: Option<String>,
    live_status: Option<String>,
    is_live: Option<bool>,
    #[serde(rename = "_type")]
    content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawThumbnail {
    url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    format_id: Option<String>,
    ext: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f64>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    tbr: Option<f64>,
    vbr: Option<f64>,
    abr: Option<f64>,
}

#[derive(Clone, Debug)]
struct NormalizedFormat {
    source: SourceFormat,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f64>,
    size: Option<u64>,
    total_bitrate: f64,
    video_bitrate: f64,
    audio_bitrate: f64,
}

impl NormalizedFormat {
    fn has_video(&self) -> bool {
        self.source.video_codec.is_some()
    }

    fn has_audio(&self) -> bool {
        self.source.audio_codec.is_some()
    }

    fn video_is_h264(&self) -> bool {
        self.source
            .video_codec
            .as_ref()
            .is_some_and(|codec| codec.family == VideoCodecFamily::H264)
    }

    fn audio_is_aac(&self) -> bool {
        self.source
            .audio_codec
            .as_ref()
            .is_some_and(|codec| codec.family == AudioCodecFamily::Aac)
    }

    fn container_is_mp4_family(&self) -> bool {
        matches!(
            self.source.container.family,
            ContainerFamily::Mp4 | ContainerFamily::M4a
        )
    }
}

fn normalize_media(
    raw: RawMedia,
    requested_url: &MediaUrl,
    warnings: Vec<String>,
) -> Result<MediaInfo, AnalyzeError> {
    validate_content(&raw)?;
    let raw_id = required_text(raw.id, "id", 64)?;
    let id = validate_response_id(&raw_id)?;
    if id != *requested_url.id() {
        return Err(AnalysisDataError::MediaIdMismatch {
            expected: requested_url.id().as_str().to_owned(),
            found: id.as_str().to_owned(),
        }
        .into());
    }
    let title = required_text(raw.title, "title", MAX_TITLE_CHARS)?;
    let uploader = optional_text(raw.uploader, "uploader", MAX_UPLOADER_CHARS)?;
    let duration = normalize_duration(raw.duration)?;
    if raw.view_count.is_some_and(|count| count > MAX_VIEW_COUNT) {
        return Err(AnalysisDataError::InvalidField {
            field: "view_count",
            reason: "view count exceeds the supported range",
        }
        .into());
    }
    let upload_date = normalize_upload_date(raw.upload_date)?;
    let thumbnails = normalize_thumbnails(raw.thumbnails.unwrap_or_default())?;
    let formats = normalize_formats(raw.formats)?;

    Ok(MediaInfo {
        id,
        url: requested_url.clone(),
        title,
        uploader,
        duration,
        view_count: raw.view_count,
        upload_date,
        thumbnails,
        formats,
        warnings,
    })
}

fn validate_content(raw: &RawMedia) -> Result<(), AnalyzeError> {
    if let Some(content_type) = raw.content_type.as_deref()
        && content_type != "video"
    {
        let kind = bounded_other_value(content_type);
        let unsupported = if matches!(content_type, "playlist" | "multi_video") {
            UnsupportedContent::Playlist
        } else {
            UnsupportedContent::OtherType(kind)
        };
        return Err(AnalyzeError::UnsupportedContent(unsupported));
    }
    if raw.is_live == Some(true) {
        return Err(AnalyzeError::UnsupportedContent(UnsupportedContent::Live));
    }
    match raw.live_status.as_deref() {
        Some("is_live") => {
            return Err(AnalyzeError::UnsupportedContent(UnsupportedContent::Live));
        }
        Some("is_upcoming") => {
            return Err(AnalyzeError::UnsupportedContent(
                UnsupportedContent::Upcoming,
            ));
        }
        Some("post_live") => {
            return Err(AnalyzeError::UnsupportedContent(
                UnsupportedContent::PostLive,
            ));
        }
        Some("not_live" | "was_live") | None => {}
        Some(other) => {
            return Err(AnalyzeError::UnsupportedContent(
                UnsupportedContent::OtherType(bounded_other_value(other)),
            ));
        }
    }
    match raw.availability.as_deref() {
        Some("private") => Err(AnalyzeError::UnsupportedContent(
            UnsupportedContent::Private,
        )),
        Some("premium_only") => Err(AnalyzeError::UnsupportedContent(
            UnsupportedContent::PremiumOnly,
        )),
        Some("subscriber_only") => Err(AnalyzeError::UnsupportedContent(
            UnsupportedContent::SubscriberOnly,
        )),
        Some("needs_auth") => Err(AnalyzeError::UnsupportedContent(
            UnsupportedContent::AuthenticationRequired,
        )),
        Some("public" | "unlisted") | None => Ok(()),
        Some(other) => Err(AnalyzeError::UnsupportedContent(
            UnsupportedContent::OtherAvailability(bounded_other_value(other)),
        )),
    }
}

fn bounded_other_value(value: &str) -> String {
    value.chars().take(128).collect()
}

fn normalize_duration(value: Option<f64>) -> Result<Duration, AnalysisDataError> {
    let value = value.ok_or(AnalysisDataError::MissingRequired { field: "duration" })?;
    if !value.is_finite() || value <= 0.0 || value > MAX_DURATION_SECONDS {
        return Err(AnalysisDataError::InvalidField {
            field: "duration",
            reason: "duration must be positive, finite, and no longer than seven days",
        });
    }
    let standard =
        StdDuration::try_from_secs_f64(value).map_err(|_| AnalysisDataError::InvalidField {
            field: "duration",
            reason: "duration cannot be represented",
        })?;
    let milliseconds =
        u64::try_from(standard.as_millis()).map_err(|_| AnalysisDataError::InvalidField {
            field: "duration",
            reason: "duration cannot be represented in milliseconds",
        })?;
    Ok(Duration(milliseconds))
}

fn validate_response_id(value: &str) -> Result<MediaId, AnalysisDataError> {
    if value.len() != 11
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AnalysisDataError::InvalidField {
            field: "id",
            reason: "video ID must contain eleven safe ASCII characters",
        });
    }
    Ok(MediaId(value.to_owned()))
}

fn required_text(
    value: Option<String>,
    field: &'static str,
    maximum: usize,
) -> Result<String, AnalysisDataError> {
    let value = value.ok_or(AnalysisDataError::MissingRequired { field })?;
    let normalized = normalize_text(&value, field, maximum)?;
    if normalized.is_empty() {
        return Err(AnalysisDataError::InvalidField {
            field,
            reason: "value must not be blank",
        });
    }
    Ok(normalized)
}

fn optional_text(
    value: Option<String>,
    field: &'static str,
    maximum: usize,
) -> Result<Option<String>, AnalysisDataError> {
    value
        .map(|value| normalize_text(&value, field, maximum))
        .transpose()
        .map(|value| value.filter(|text| !text.is_empty()))
}

fn normalize_text(
    value: &str,
    field: &'static str,
    maximum: usize,
) -> Result<String, AnalysisDataError> {
    if value.chars().count() > maximum {
        return Err(AnalysisDataError::StringTooLong { field, maximum });
    }
    if value
        .chars()
        .any(|character| character.is_control() && !character.is_whitespace())
    {
        return Err(AnalysisDataError::InvalidField {
            field,
            reason: "control characters are forbidden",
        });
    }
    Ok(value.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn normalize_upload_date(value: Option<String>) -> Result<Option<String>, AnalysisDataError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AnalysisDataError::InvalidField {
            field: "upload_date",
            reason: "expected YYYYMMDD",
        });
    }
    let year = parse_date_part(&value[0..4], "upload_date")?;
    let month = parse_date_part(&value[4..6], "upload_date")?;
    let day = parse_date_part(&value[6..8], "upload_date")?;
    if !(2005..=2100).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
    {
        return Err(AnalysisDataError::InvalidField {
            field: "upload_date",
            reason: "date does not exist",
        });
    }
    Ok(Some(format!("{year:04}-{month:02}-{day:02}")))
}

fn parse_date_part(value: &str, field: &'static str) -> Result<u32, AnalysisDataError> {
    value
        .parse::<u32>()
        .map_err(|_| AnalysisDataError::InvalidField {
            field,
            reason: "date contains an invalid number",
        })
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

fn normalize_thumbnails(raw: Vec<RawThumbnail>) -> Result<Vec<Thumbnail>, AnalysisDataError> {
    if raw.len() > MAX_RAW_THUMBNAILS {
        return Err(AnalysisDataError::TooManyRecords {
            field: "thumbnails",
            count: raw.len(),
            maximum: MAX_RAW_THUMBNAILS,
        });
    }
    raw.into_iter()
        .take(MAX_PUBLIC_THUMBNAILS)
        .map(normalize_thumbnail)
        .collect()
}

fn normalize_thumbnail(raw: RawThumbnail) -> Result<Thumbnail, AnalysisDataError> {
    let url = required_text(raw.url, "thumbnails.url", MAX_THUMBNAIL_URL_CHARS)?;
    let parsed = url::Url::parse(&url).map_err(|_| AnalysisDataError::InvalidField {
        field: "thumbnails.url",
        reason: "thumbnail URL is malformed",
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(AnalysisDataError::InvalidField {
            field: "thumbnails.url",
            reason: "thumbnail URL must use HTTP or HTTPS and include a host",
        });
    }
    validate_dimension(raw.width, "thumbnails.width")?;
    validate_dimension(raw.height, "thumbnails.height")?;
    Ok(Thumbnail {
        url,
        width: raw.width,
        height: raw.height,
    })
}

fn validate_dimension(value: Option<u32>, field: &'static str) -> Result<(), AnalysisDataError> {
    if value.is_some_and(|dimension| dimension == 0 || dimension > MAX_DIMENSION) {
        return Err(AnalysisDataError::InvalidField {
            field,
            reason: "dimension is outside the supported range",
        });
    }
    Ok(())
}

fn normalize_formats(raw: Option<Vec<RawFormat>>) -> Result<Vec<FormatOption>, AnalysisDataError> {
    let raw = raw.ok_or(AnalysisDataError::MissingRequired { field: "formats" })?;
    if raw.len() > MAX_FORMATS {
        return Err(AnalysisDataError::TooManyRecords {
            field: "formats",
            count: raw.len(),
            maximum: MAX_FORMATS,
        });
    }
    let mut identities = BTreeSet::new();
    let mut normalized = Vec::new();
    for format in raw {
        if let Some(format) = normalize_format(format)? {
            if !identities.insert(format.source.format_id.clone()) {
                return Err(AnalysisDataError::DuplicateFormatId {
                    format_id: format.source.format_id.as_str().to_owned(),
                });
            }
            normalized.push(format);
        }
    }

    let audio_for_mp3 = select_best(&normalized, |format| {
        format.has_audio().then(|| mp3_audio_quality_score(format))
    });
    let mut options = Vec::new();
    if let Some(audio) = audio_for_mp3 {
        for bitrate_kbps in MP3_BITRATES {
            options.push(FormatOption::Mp3 {
                bitrate_kbps,
                source: audio.source.clone(),
            });
        }
    }

    let heights = normalized
        .iter()
        .filter_map(|format| format.has_video().then_some(format.height).flatten())
        .collect::<BTreeSet<_>>();
    for height in heights.into_iter().rev() {
        let video = select_best(&normalized, |format| {
            (format.has_video() && format.height == Some(height))
                .then(|| video_quality_score(format))
        });
        let Some(video) = video else {
            continue;
        };
        let audio = select_audio_for_video(&normalized, video);
        let Some(audio) = audio else {
            continue;
        };
        let same_source = video.source.format_id == audio.source.format_id;
        let compatibility =
            compatibility_work(video.video_is_h264(), audio.audio_is_aac(), same_source);
        let estimated_size_bytes = combined_size(video, audio, same_source);
        options.push(FormatOption::Mp4 {
            height,
            width: video.width,
            fps: video.fps,
            estimated_size_bytes,
            video_source: video.source.clone(),
            audio_source: audio.source.clone(),
            compatibility,
        });
    }
    if options.is_empty() {
        return Err(AnalysisDataError::NoUsableFormats);
    }
    Ok(options)
}

fn normalize_format(raw: RawFormat) -> Result<Option<NormalizedFormat>, AnalysisDataError> {
    let format_id = required_text(raw.format_id, "formats.format_id", MAX_FORMAT_ID_CHARS)?;
    if !format_id
        .chars()
        .all(|character| !character.is_control() && !character.is_whitespace())
    {
        return Err(AnalysisDataError::InvalidField {
            field: "formats.format_id",
            reason: "format ID must not contain whitespace or controls",
        });
    }
    let container_name = required_text(raw.ext, "formats.ext", MAX_CONTAINER_CHARS)?;
    if !container_name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AnalysisDataError::InvalidField {
            field: "formats.ext",
            reason: "container name contains unsafe characters",
        });
    }
    let vcodec = required_text(raw.vcodec, "formats.vcodec", MAX_CODEC_CHARS)?;
    let acodec = required_text(raw.acodec, "formats.acodec", MAX_CODEC_CHARS)?;
    let video_codec =
        (!vcodec.eq_ignore_ascii_case("none")).then(|| video_codec_descriptor(&vcodec));
    let audio_codec =
        (!acodec.eq_ignore_ascii_case("none")).then(|| audio_codec_descriptor(&acodec));
    if video_codec.is_none() && audio_codec.is_none() {
        return Ok(None);
    }
    if video_codec.is_some() && raw.height.is_none() {
        return Err(AnalysisDataError::InvalidField {
            field: "formats.height",
            reason: "a video-bearing format must declare a height",
        });
    }
    validate_dimension(raw.width, "formats.width")?;
    validate_dimension(raw.height, "formats.height")?;
    validate_fps(raw.fps)?;
    validate_size(raw.filesize, "formats.filesize")?;
    validate_size(raw.filesize_approx, "formats.filesize_approx")?;
    validate_bitrate(raw.tbr, "formats.tbr")?;
    validate_bitrate(raw.vbr, "formats.vbr")?;
    validate_bitrate(raw.abr, "formats.abr")?;

    Ok(Some(NormalizedFormat {
        source: SourceFormat {
            format_id: FormatId(format_id),
            container: container_descriptor(&container_name),
            video_codec,
            audio_codec,
        },
        width: raw.width,
        height: raw.height,
        fps: raw.fps,
        size: raw.filesize.or(raw.filesize_approx),
        total_bitrate: raw.tbr.unwrap_or_default(),
        video_bitrate: raw.vbr.unwrap_or_default(),
        audio_bitrate: raw.abr.unwrap_or_default(),
    }))
}

fn validate_fps(value: Option<f64>) -> Result<(), AnalysisDataError> {
    if value.is_some_and(|fps| !fps.is_finite() || fps <= 0.0 || fps > MAX_FPS) {
        return Err(AnalysisDataError::InvalidField {
            field: "formats.fps",
            reason: "frame rate must be positive, finite, and at most 240",
        });
    }
    Ok(())
}

fn validate_size(value: Option<u64>, field: &'static str) -> Result<(), AnalysisDataError> {
    if value.is_some_and(|size| size == 0 || size > MAX_FILESIZE_BYTES) {
        return Err(AnalysisDataError::InvalidField {
            field,
            reason: "file size is outside the supported range",
        });
    }
    Ok(())
}

fn validate_bitrate(value: Option<f64>, field: &'static str) -> Result<(), AnalysisDataError> {
    if value.is_some_and(|bitrate| !(0.0..=MAX_BITRATE_KBPS).contains(&bitrate)) {
        return Err(AnalysisDataError::InvalidField {
            field,
            reason: "bitrate must be non-negative, finite, and bounded",
        });
    }
    Ok(())
}

fn video_codec_descriptor(value: &str) -> VideoCodecDescriptor {
    let name = value.to_ascii_lowercase();
    let family = if name.starts_with("avc1") || name.starts_with("h264") {
        VideoCodecFamily::H264
    } else if name.starts_with("vp9") || name.starts_with("vp09") {
        VideoCodecFamily::Vp9
    } else if name.starts_with("av01") || name.starts_with("av1") {
        VideoCodecFamily::Av1
    } else {
        VideoCodecFamily::Other
    };
    VideoCodecDescriptor { name, family }
}

fn audio_codec_descriptor(value: &str) -> AudioCodecDescriptor {
    let name = value.to_ascii_lowercase();
    let family = if name.starts_with("mp4a") || name.starts_with("aac") {
        AudioCodecFamily::Aac
    } else if name.starts_with("opus") {
        AudioCodecFamily::Opus
    } else if name.starts_with("vorbis") {
        AudioCodecFamily::Vorbis
    } else if name.starts_with("mp3") {
        AudioCodecFamily::Mp3
    } else {
        AudioCodecFamily::Other
    };
    AudioCodecDescriptor { name, family }
}

fn container_descriptor(value: &str) -> ContainerDescriptor {
    let name = value.to_ascii_lowercase();
    let family = match name.as_str() {
        "mp4" => ContainerFamily::Mp4,
        "m4a" => ContainerFamily::M4a,
        "webm" => ContainerFamily::Webm,
        _ => ContainerFamily::Other,
    };
    ContainerDescriptor { name, family }
}

#[derive(Clone, Debug)]
struct SelectionScore {
    primary: u8,
    secondary: u8,
    tertiary: u8,
    bitrate: f64,
    fps: f64,
    size: u64,
    format_id: String,
}

impl SelectionScore {
    fn compare(&self, other: &Self) -> Ordering {
        self.primary
            .cmp(&other.primary)
            .then_with(|| self.secondary.cmp(&other.secondary))
            .then_with(|| self.tertiary.cmp(&other.tertiary))
            .then_with(|| self.bitrate.total_cmp(&other.bitrate))
            .then_with(|| self.fps.total_cmp(&other.fps))
            .then_with(|| self.size.cmp(&other.size))
            .then_with(|| self.format_id.cmp(&other.format_id))
    }
}

fn select_best<F>(formats: &[NormalizedFormat], score: F) -> Option<&NormalizedFormat>
where
    F: Fn(&NormalizedFormat) -> Option<SelectionScore>,
{
    formats
        .iter()
        .filter_map(|format| score(format).map(|score| (format, score)))
        .max_by(|(_, left), (_, right)| left.compare(right))
        .map(|(format, _)| format)
}

fn video_quality_score(format: &NormalizedFormat) -> SelectionScore {
    SelectionScore {
        primary: u8::from(format.video_is_h264()),
        secondary: u8::from(format.container_is_mp4_family()),
        tertiary: u8::from(format.has_audio() && format.audio_is_aac()),
        bitrate: format.video_bitrate.max(format.total_bitrate),
        fps: format.fps.unwrap_or_default(),
        size: format.size.unwrap_or_default(),
        format_id: format.source.format_id.as_str().to_owned(),
    }
}

fn audio_quality_score(format: &NormalizedFormat) -> SelectionScore {
    SelectionScore {
        primary: u8::from(format.audio_is_aac()),
        secondary: u8::from(format.container_is_mp4_family()),
        tertiary: u8::from(!format.has_video()),
        bitrate: format.audio_bitrate.max(format.total_bitrate),
        fps: 0.0,
        size: format.size.unwrap_or_default(),
        format_id: format.source.format_id.as_str().to_owned(),
    }
}

fn mp3_audio_quality_score(format: &NormalizedFormat) -> SelectionScore {
    SelectionScore {
        primary: u8::from(!format.has_video()),
        secondary: 0,
        tertiary: 0,
        bitrate: format.audio_bitrate.max(format.total_bitrate),
        fps: 0.0,
        size: format.size.unwrap_or_default(),
        format_id: format.source.format_id.as_str().to_owned(),
    }
}

fn select_audio_for_video<'a>(
    formats: &'a [NormalizedFormat],
    video: &'a NormalizedFormat,
) -> Option<&'a NormalizedFormat> {
    if video.has_audio() && video.audio_is_aac() {
        return Some(video);
    }
    select_best(formats, |format| {
        format.has_audio().then(|| audio_quality_score(format))
    })
}

fn compatibility_work(
    video_compatible: bool,
    audio_compatible: bool,
    same_source: bool,
) -> CompatibilityWork {
    match (video_compatible, audio_compatible, same_source) {
        (true, true, true) => CompatibilityWork::None,
        (true, true, false) => CompatibilityWork::Merge,
        (false, true, _) => CompatibilityWork::VideoTranscode,
        (true, false, _) => CompatibilityWork::AudioTranscode,
        (false, false, _) => CompatibilityWork::VideoAndAudioTranscode,
    }
}

fn combined_size(
    video: &NormalizedFormat,
    audio: &NormalizedFormat,
    same_source: bool,
) -> Option<u64> {
    if same_source {
        video.size
    } else {
        video.size?.checked_add(audio.size?)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::{
        AnalysisDataError, AnalysisStream, AnalysisTools, AnalyzeError, Analyzer,
        CompatibilityWork, FormatOption, MAX_FORMATS, PROCESS_MAX_BYTES_PER_STREAM, RawMedia,
        normalize_media,
    };
    use crate::{
        analysis::MediaUrl,
        cancellation::CancellationToken,
        path::ExecutablePath,
        process::{
            CapturedOutput, ProcessError, ProcessExitStatus, ProcessOutput, ProcessRunner,
            ProcessSpec, StreamCapture,
        },
    };

    const PROGRESSIVE: &[u8] = include_bytes!("../../tests/fixtures/analysis/progressive.json");
    const ADAPTIVE: &[u8] = include_bytes!("../../tests/fixtures/analysis/adaptive.json");
    const AUDIO_ONLY: &[u8] = include_bytes!("../../tests/fixtures/analysis/audio-only.json");
    const MISSING_SIZE: &[u8] = include_bytes!("../../tests/fixtures/analysis/missing-size.json");
    const HIGH_FPS: &[u8] = include_bytes!("../../tests/fixtures/analysis/high-fps.json");
    const FOUR_K: &[u8] = include_bytes!("../../tests/fixtures/analysis/4k.json");
    const MISSING_METADATA: &[u8] =
        include_bytes!("../../tests/fixtures/analysis/missing-metadata.json");
    const PRIVATE: &[u8] = include_bytes!("../../tests/fixtures/analysis/private.json");
    const LIVE: &[u8] = include_bytes!("../../tests/fixtures/analysis/live.json");
    const MALFORMED: &[u8] = include_bytes!("../../tests/fixtures/analysis/malformed.txt");
    const INCOMPATIBLE: &[u8] = include_bytes!("../../tests/fixtures/analysis/incompatible.json");
    const ZERO_INACTIVE_BITRATES: &[u8] =
        include_bytes!("../../tests/fixtures/analysis/zero-inactive-bitrates.json");

    fn fixture_url() -> Result<MediaUrl, crate::analysis::MediaUrlError> {
        MediaUrl::parse("https://youtu.be/dQw4w9WgXcQ")
    }

    fn parse_fixture(bytes: &[u8]) -> Result<crate::analysis::MediaInfo, AnalyzeError> {
        let raw: RawMedia =
            serde_json::from_slice(bytes).map_err(AnalysisDataError::InvalidJson)?;
        normalize_media(
            raw,
            &fixture_url().map_err(|error| {
                AnalyzeError::InvalidData(AnalysisDataError::InvalidField {
                    field: "test.url",
                    reason: if error.is_unsupported_content() {
                        "unsupported"
                    } else {
                        "invalid"
                    },
                })
            })?,
            Vec::new(),
        )
    }

    #[test]
    fn progressive_fixture_offers_mp3_and_no_work_mp4() -> Result<(), AnalyzeError> {
        let info = parse_fixture(PROGRESSIVE)?;
        let bitrates = info
            .formats
            .iter()
            .filter_map(|option| match option {
                FormatOption::Mp3 { bitrate_kbps, .. } => Some(*bitrate_kbps),
                FormatOption::Mp4 { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(bitrates, [128, 192, 256, 320]);
        assert!(info.formats.iter().any(|option| matches!(
            option,
            FormatOption::Mp4 {
                height: 720,
                compatibility: CompatibilityWork::None,
                ..
            }
        )));
        Ok(())
    }

    #[test]
    fn adaptive_fixture_has_unique_descending_heights_and_merge() -> Result<(), AnalyzeError> {
        let info = parse_fixture(ADAPTIVE)?;
        let videos = info
            .formats
            .iter()
            .filter_map(|option| match option {
                FormatOption::Mp4 {
                    height,
                    compatibility,
                    ..
                } => Some((*height, *compatibility)),
                FormatOption::Mp3 { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            videos,
            [
                (1080, CompatibilityWork::Merge),
                (720, CompatibilityWork::Merge),
                (360, CompatibilityWork::None)
            ]
        );
        let selected_1080 = info.formats.iter().find_map(|option| match option {
            FormatOption::Mp4 {
                height: 1080,
                estimated_size_bytes,
                video_source,
                audio_source,
                ..
            } => Some((
                video_source.format_id.as_str(),
                audio_source.format_id.as_str(),
                *estimated_size_bytes,
            )),
            _ => None,
        });
        assert_eq!(selected_1080, Some(("137", "140", Some(13_500_000))));
        Ok(())
    }

    #[test]
    fn audio_only_fixture_offers_only_mp3() -> Result<(), AnalyzeError> {
        let info = parse_fixture(AUDIO_ONLY)?;
        assert_eq!(info.formats.len(), 4);
        assert!(
            info.formats
                .iter()
                .all(|option| matches!(option, FormatOption::Mp3 { .. }))
        );
        Ok(())
    }

    #[test]
    fn missing_sizes_remain_unknown() -> Result<(), AnalyzeError> {
        let info = parse_fixture(MISSING_SIZE)?;
        assert!(info.formats.iter().any(|option| matches!(
            option,
            FormatOption::Mp4 {
                estimated_size_bytes: None,
                ..
            }
        )));
        Ok(())
    }

    #[test]
    fn zero_inactive_bitrates_are_treated_as_unavailable() -> Result<(), AnalyzeError> {
        let info = parse_fixture(ZERO_INACTIVE_BITRATES)?;
        assert!(info.formats.iter().any(|option| matches!(
            option,
            FormatOption::Mp4 {
                video_source,
                audio_source,
                ..
            } if video_source.format_id.as_str() == "137"
                && audio_source.format_id.as_str() == "140"
        )));
        assert!(info.formats.iter().any(|option| matches!(
            option,
            FormatOption::Mp3 { source, .. } if source.format_id.as_str() == "140"
        )));
        Ok(())
    }

    #[test]
    fn negative_bitrates_remain_invalid() -> Result<(), Box<dyn std::error::Error>> {
        let mut value: serde_json::Value = serde_json::from_slice(ZERO_INACTIVE_BITRATES)?;
        value["formats"][0]["vbr"] = serde_json::Value::from(-1);
        let raw: RawMedia = serde_json::from_value(value)?;
        assert!(matches!(
            normalize_media(raw, &fixture_url()?, Vec::new()),
            Err(AnalyzeError::InvalidData(AnalysisDataError::InvalidField {
                field: "formats.vbr",
                ..
            }))
        ));
        Ok(())
    }

    #[test]
    fn high_fps_and_4k_sources_are_preserved_without_upscaling() -> Result<(), AnalyzeError> {
        let high_fps = parse_fixture(HIGH_FPS)?;
        assert!(high_fps.formats.iter().any(|option| matches!(
            option,
            FormatOption::Mp4 {
                height: 1080,
                fps: Some(value),
                ..
            } if (*value - 60.0).abs() < f64::EPSILON
        )));
        let four_k = parse_fixture(FOUR_K)?;
        let heights = four_k
            .formats
            .iter()
            .filter_map(|option| match option {
                FormatOption::Mp4 { height, .. } => Some(*height),
                FormatOption::Mp3 { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(heights, [2160, 1440, 1080]);
        Ok(())
    }

    #[test]
    fn unknown_json_fields_are_ignored_and_missing_metadata_is_contextual() {
        assert!(parse_fixture(PROGRESSIVE).is_ok());
        assert!(matches!(
            parse_fixture(MISSING_METADATA),
            Err(AnalyzeError::InvalidData(
                AnalysisDataError::MissingRequired { field: "title" }
            ))
        ));
    }

    #[test]
    fn malformed_private_and_live_fixtures_have_distinct_failures() {
        assert!(matches!(
            parse_fixture(MALFORMED),
            Err(AnalyzeError::InvalidData(AnalysisDataError::InvalidJson(_)))
        ));
        assert!(matches!(
            parse_fixture(PRIVATE),
            Err(AnalyzeError::UnsupportedContent(
                super::UnsupportedContent::Private
            ))
        ));
        assert!(matches!(
            parse_fixture(LIVE),
            Err(AnalyzeError::UnsupportedContent(
                super::UnsupportedContent::Live
            ))
        ));
        let playlist = serde_json::from_slice::<serde_json::Value>(PROGRESSIVE);
        assert!(playlist.is_ok());
        let Some(mut playlist) = playlist.ok() else {
            return;
        };
        playlist["_type"] = serde_json::Value::String("playlist".to_owned());
        let raw = serde_json::from_value::<RawMedia>(playlist);
        assert!(raw.is_ok());
        let Some(raw) = raw.ok() else {
            return;
        };
        let url = fixture_url();
        assert!(url.is_ok());
        let Some(url) = url.ok() else {
            return;
        };
        assert!(matches!(
            normalize_media(raw, &url, Vec::new()),
            Err(AnalyzeError::UnsupportedContent(
                super::UnsupportedContent::Playlist
            ))
        ));
    }

    #[test]
    fn compatibility_classification_covers_every_required_work_kind() {
        assert_eq!(
            super::compatibility_work(true, true, true),
            CompatibilityWork::None
        );
        assert_eq!(
            super::compatibility_work(true, true, false),
            CompatibilityWork::Merge
        );
        assert_eq!(
            super::compatibility_work(false, true, false),
            CompatibilityWork::VideoTranscode
        );
        assert_eq!(
            super::compatibility_work(true, false, false),
            CompatibilityWork::AudioTranscode
        );
        assert_eq!(
            super::compatibility_work(false, false, false),
            CompatibilityWork::VideoAndAudioTranscode
        );
        let info = parse_fixture(INCOMPATIBLE);
        assert!(info.is_ok());
        let Some(info) = info.ok() else {
            return;
        };
        assert!(info.formats.iter().any(|option| matches!(
            option,
            FormatOption::Mp4 {
                height: 720,
                compatibility: CompatibilityWork::AudioTranscode,
                ..
            }
        )));
        assert!(info.formats.iter().any(|option| matches!(
            option,
            FormatOption::Mp4 {
                height: 1080,
                compatibility: CompatibilityWork::VideoAndAudioTranscode,
                ..
            }
        )));
    }

    #[test]
    fn metadata_and_collection_bounds_are_enforced() -> Result<(), Box<dyn std::error::Error>> {
        let mut value: serde_json::Value = serde_json::from_slice(PROGRESSIVE)?;
        value["title"] = serde_json::Value::String("x".repeat(513));
        let raw: RawMedia = serde_json::from_value(value)?;
        let result = normalize_media(raw, &fixture_url()?, Vec::new());
        assert!(matches!(
            result,
            Err(AnalyzeError::InvalidData(
                AnalysisDataError::StringTooLong { field: "title", .. }
            ))
        ));

        let mut value: serde_json::Value = serde_json::from_slice(PROGRESSIVE)?;
        value["formats"] = serde_json::Value::Array(
            (0..=MAX_FORMATS)
                .map(|_| {
                    serde_json::json!({
                        "format_id": "fixture",
                        "ext": "m4a",
                        "vcodec": "none",
                        "acodec": "mp4a.40.2"
                    })
                })
                .collect(),
        );
        let raw: RawMedia = serde_json::from_value(value)?;
        assert!(matches!(
            normalize_media(raw, &fixture_url()?, Vec::new()),
            Err(AnalyzeError::InvalidData(
                AnalysisDataError::TooManyRecords {
                    field: "formats",
                    ..
                }
            ))
        ));

        let mut value: serde_json::Value = serde_json::from_slice(PROGRESSIVE)?;
        value["formats"][0]["height"] = serde_json::Value::from(0);
        let raw: RawMedia = serde_json::from_value(value)?;
        assert!(matches!(
            normalize_media(raw, &fixture_url()?, Vec::new()),
            Err(AnalyzeError::InvalidData(AnalysisDataError::InvalidField {
                field: "formats.height",
                ..
            }))
        ));
        Ok(())
    }

    #[derive(Clone)]
    struct RecordingRunner {
        calls: Arc<Mutex<Vec<RecordedSpec>>>,
        output: ProcessOutput,
    }

    struct CancelledRunner;

    #[async_trait]
    impl ProcessRunner for CancelledRunner {
        async fn run(
            &self,
            _spec: ProcessSpec,
            _cancellation: CancellationToken,
        ) -> Result<ProcessOutput, ProcessError> {
            Err(ProcessError::Cancelled {
                output: CapturedOutput::default(),
            })
        }
    }

    #[derive(Clone, Debug)]
    struct RecordedSpec {
        executable: PathBuf,
        arguments: Vec<OsString>,
    }

    #[async_trait]
    impl ProcessRunner for RecordingRunner {
        async fn run(
            &self,
            spec: ProcessSpec,
            _cancellation: CancellationToken,
        ) -> Result<ProcessOutput, ProcessError> {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(RecordedSpec {
                    executable: spec.executable().to_path_buf(),
                    arguments: spec.argument_values().map(OsString::from).collect(),
                });
            }
            Ok(self.output.clone())
        }
    }

    fn successful_output(stdout: Vec<u8>, stderr: Vec<u8>) -> ProcessOutput {
        ProcessOutput {
            status: ProcessExitStatus {
                success: true,
                code: Some(0),
            },
            capture: CapturedOutput {
                stdout: StreamCapture {
                    observed_bytes: u64::try_from(stdout.len()).unwrap_or(u64::MAX),
                    observed_lines: 1,
                    bytes: stdout,
                    truncated: false,
                },
                stderr: StreamCapture {
                    observed_bytes: u64::try_from(stderr.len()).unwrap_or(u64::MAX),
                    observed_lines: u64::from(!stderr.is_empty()),
                    bytes: stderr,
                    truncated: false,
                },
                events: Vec::new(),
            },
        }
    }

    fn make_tools() -> Result<(tempfile::TempDir, AnalysisTools), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let create = |name: &str| -> Result<ExecutablePath, Box<dyn std::error::Error>> {
            let path = directory.path().join(name);
            fs::write(&path, b"fixture")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let mut permissions = fs::metadata(&path)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&path, permissions)?;
            }
            Ok(ExecutablePath::validate(path)?)
        };
        let tools = AnalysisTools::new(create("yt-dlp")?, create("ffmpeg")?, create("deno")?);
        Ok((directory, tools))
    }

    #[tokio::test]
    async fn adapter_uses_exact_isolated_argument_vector() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_directory, tools) = make_tools()?;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = RecordingRunner {
            calls: Arc::clone(&calls),
            output: successful_output(PROGRESSIVE.to_vec(), b"WARNING: fixture warning\n".to_vec()),
        };
        let analyzer = Analyzer::with_runner(tools.clone(), Arc::new(runner));
        let info = analyzer
            .analyze(&fixture_url()?, CancellationToken::new())
            .await?;
        assert_eq!(info.warnings, ["WARNING: fixture warning"]);
        let calls = calls.lock().map_err(|_| "recording mutex was poisoned")?;
        assert_eq!(calls.len(), 1);
        let call = calls.first().ok_or("missing recorded call")?;
        assert_eq!(call.executable, tools.yt_dlp().as_path());
        let expected = [
            OsString::from("--ignore-config"),
            OsString::from("--no-config-locations"),
            OsString::from("--no-plugin-dirs"),
            OsString::from("--no-update"),
            OsString::from("--no-playlist"),
            OsString::from("--simulate"),
            OsString::from("--dump-single-json"),
            OsString::from("--no-js-runtimes"),
            OsString::from("--js-runtimes"),
            OsString::from(format!("deno:{}", tools.deno().as_path().display())),
            OsString::from("--ffmpeg-location"),
            tools.ffmpeg().as_path().as_os_str().to_owned(),
            OsString::from("--"),
            OsString::from("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
        ];
        assert_eq!(call.arguments, expected);
        assert!(
            !call
                .arguments
                .iter()
                .any(|argument| argument.to_string_lossy().contains("cookie"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn adapter_distinguishes_encoding_exit_and_output_limit_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, tools) = make_tools()?;
        let invalid = RecordingRunner {
            calls: Arc::default(),
            output: successful_output(vec![0xff, 0xfe], Vec::new()),
        };
        assert!(matches!(
            Analyzer::with_runner(tools.clone(), Arc::new(invalid))
                .analyze(&fixture_url()?, CancellationToken::new())
                .await,
            Err(AnalyzeError::InvalidUtf8 {
                stream: AnalysisStream::Stdout,
                ..
            })
        ));

        let invalid_stderr = RecordingRunner {
            calls: Arc::default(),
            output: successful_output(PROGRESSIVE.to_vec(), vec![0xff, 0xfe]),
        };
        assert!(matches!(
            Analyzer::with_runner(tools.clone(), Arc::new(invalid_stderr))
                .analyze(&fixture_url()?, CancellationToken::new())
                .await,
            Err(AnalyzeError::InvalidUtf8 {
                stream: AnalysisStream::Stderr,
                ..
            })
        ));

        let mut failed_output = successful_output(Vec::new(), b"bounded failure".to_vec());
        failed_output.status = ProcessExitStatus {
            success: false,
            code: Some(9),
        };
        let failed = RecordingRunner {
            calls: Arc::default(),
            output: failed_output,
        };
        assert!(matches!(
            Analyzer::with_runner(tools.clone(), Arc::new(failed))
                .analyze(&fixture_url()?, CancellationToken::new())
                .await,
            Err(AnalyzeError::ExtractionFailed {
                status: Some(9),
                ..
            })
        ));

        let mut oversized = successful_output(PROGRESSIVE.to_vec(), Vec::new());
        oversized.capture.stdout.truncated = true;
        oversized.capture.stdout.observed_bytes = u64::try_from(PROCESS_MAX_BYTES_PER_STREAM + 1)?;
        let runner = RecordingRunner {
            calls: Arc::default(),
            output: oversized,
        };
        assert!(matches!(
            Analyzer::with_runner(tools.clone(), Arc::new(runner))
                .analyze(&fixture_url()?, CancellationToken::new())
                .await,
            Err(AnalyzeError::OutputLimitExceeded {
                stream: AnalysisStream::Stdout,
                ..
            })
        ));

        let mut excessive_lines = successful_output(PROGRESSIVE.to_vec(), b"warning\n".to_vec());
        excessive_lines.capture.stderr.observed_lines = super::STDERR_MAX_LINES + 1;
        let runner = RecordingRunner {
            calls: Arc::default(),
            output: excessive_lines,
        };
        assert!(matches!(
            Analyzer::with_runner(tools.clone(), Arc::new(runner))
                .analyze(&fixture_url()?, CancellationToken::new())
                .await,
            Err(AnalyzeError::OutputLimitExceeded {
                stream: AnalysisStream::Stderr,
                kind: "line",
                ..
            })
        ));

        assert!(matches!(
            Analyzer::with_runner(tools, Arc::new(CancelledRunner))
                .analyze(&fixture_url()?, CancellationToken::new())
                .await,
            Err(AnalyzeError::Cancelled)
        ));
        Ok(())
    }
}
