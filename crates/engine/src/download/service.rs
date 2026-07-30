//! Download adapters and engine-owned orchestration.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration as StdDuration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    analysis::{
        AnalysisTools, AnalyzeError, Analyzer, AudioCodecFamily, CompatibilityWork, FormatOption,
        MediaInfo, SourceFormat, VideoCodecFamily,
    },
    cancellation::CancellationToken,
    path::ExecutablePath,
    process::{
        OutputLimit, OutputStream, ProcessError, ProcessOutput, ProcessRunner, ProcessSpec,
        TokioProcessRunner,
    },
    resolver::ResolvedTool,
    tool::Tool,
};

use super::{
    CompletionHandle, DownloadError, DownloadRequest, DownloadResult, JobControls, JobEvent,
    JobEventKind, JobEventStream, JobId, JobProgress, JobStage, OutputSelection, StartedDownload,
    model::{CONTROL_CANCEL, CONTROL_PAUSE, CONTROL_RUNNING},
    name::{OutputReservation, bounded_path, reserve_output, sanitize_output_stem},
    progress::{DownloadProgressAggregator, FfmpegProgress, ProtocolLines, parse_ytdlp_progress},
};

const JOB_EVENT_CAPACITY: usize = 128;
const NO_STAGE: u8 = u8::MAX;
const PROCESS_EVENT_CAPACITY: usize = 128;
const PROCESS_MAX_BYTES: usize = 1024 * 1024;
const PROCESS_MAX_LINES: usize = 20_000;
const MAX_DIAGNOSTIC_CHARS: usize = 4_096;
const MAX_WARNING_LINES: usize = 32;
const MAX_WARNING_CHARS: usize = 512;
const OWNER_MANIFEST_VERSION: u32 = 1;
const DOWNLOAD_TIMEOUT: StdDuration = StdDuration::from_hours(24);
const FFMPEG_TIMEOUT: StdDuration = StdDuration::from_hours(24);
const FFPROBE_TIMEOUT: StdDuration = StdDuration::from_mins(2);
const YTDLP_PROGRESS_TEMPLATE: &str = "download:yt-media-progress|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s";

/// Explicit verified tools required by download and final validation.
#[derive(Clone, Debug)]
pub struct DownloadTools {
    yt_dlp: ExecutablePath,
    ffmpeg: ExecutablePath,
    ffprobe: ExecutablePath,
    deno: ExecutablePath,
}

impl DownloadTools {
    /// Creates a tool set from already validated explicit executable paths.
    #[must_use]
    pub const fn new(
        yt_dlp: ExecutablePath,
        ffmpeg: ExecutablePath,
        ffprobe: ExecutablePath,
        deno: ExecutablePath,
    ) -> Self {
        Self {
            yt_dlp,
            ffmpeg,
            ffprobe,
            deno,
        }
    }

    /// Creates a tool set from four engine-resolved records.
    ///
    /// # Errors
    ///
    /// Returns a typed error if a record occupies the wrong role.
    pub fn from_resolved(
        yt_dlp: ResolvedTool,
        ffmpeg: ResolvedTool,
        ffprobe: ResolvedTool,
        deno: ResolvedTool,
    ) -> Result<Self, DownloadToolError> {
        require_tool(&yt_dlp, Tool::YtDlp)?;
        require_tool(&ffmpeg, Tool::Ffmpeg)?;
        require_tool(&ffprobe, Tool::Ffprobe)?;
        require_tool(&deno, Tool::Deno)?;
        Ok(Self::new(yt_dlp.path, ffmpeg.path, ffprobe.path, deno.path))
    }

    fn analysis_tools(&self) -> AnalysisTools {
        AnalysisTools::new(self.yt_dlp.clone(), self.ffmpeg.clone(), self.deno.clone())
    }
}

fn require_tool(resolved: &ResolvedTool, expected: Tool) -> Result<(), DownloadToolError> {
    if resolved.tool == expected {
        Ok(())
    } else {
        Err(DownloadToolError::UnexpectedTool {
            expected,
            found: resolved.tool,
        })
    }
}

/// Invalid tool composition for download.
#[derive(Debug, Error)]
pub enum DownloadToolError {
    /// A resolved record occupied the wrong role.
    #[error("download expected `{expected}` but received `{found}`")]
    UnexpectedTool {
        /// Required identity.
        expected: Tool,
        /// Supplied identity.
        found: Tool,
    },
}

/// Cloneable asynchronous download service.
#[derive(Clone)]
pub struct DownloadService {
    runner: Arc<dyn ProcessRunner>,
    tools: DownloadTools,
}

struct JobRuntime<'a> {
    emitter: &'a EventEmitter,
    control: &'a Arc<AtomicU8>,
}

struct SourceDownload<'a> {
    request: &'a DownloadRequest,
    source: &'a SourceFormat,
    path: &'a Path,
    source_index: usize,
    aggregator: &'a mut DownloadProgressAggregator,
}

struct OutputWork<'a> {
    media: &'a MediaInfo,
    selected: &'a FormatOption,
    output: OutputSelection,
    workspace: &'a Workspace,
}

impl std::fmt::Debug for DownloadService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DownloadService")
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl DownloadService {
    /// Creates a service backed by the production process owner.
    #[must_use]
    pub fn new(tools: DownloadTools) -> Self {
        Self {
            runner: Arc::new(TokioProcessRunner),
            tools,
        }
    }

    /// Creates a service backed by a test process port.
    #[must_use]
    pub fn with_runner(tools: DownloadTools, runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner, tools }
    }

    /// Starts one download and immediately returns events, completion, and controls.
    #[must_use]
    pub fn start(&self, request: DownloadRequest) -> StartedDownload {
        self.start_with_id(request, JobId::new_v7())
    }

    pub(crate) fn start_with_id(&self, request: DownloadRequest, job_id: JobId) -> StartedDownload {
        let cancellation = CancellationToken::new();
        let state = Arc::new(AtomicU8::new(CONTROL_RUNNING));
        let controls = JobControls {
            state: Arc::clone(&state),
            cancellation: cancellation.clone(),
        };
        let (event_sender, event_receiver) = broadcast::channel(JOB_EVENT_CAPACITY);
        let (completion_sender, completion_receiver) = oneshot::channel();
        let service = self.clone();
        let task_job_id = job_id.clone();
        tokio::spawn(async move {
            let emitter = EventEmitter::new(task_job_id.clone(), event_sender);
            let result = service
                .run_job(request, task_job_id, emitter.clone(), state, cancellation)
                .await;
            match &result {
                Ok(_) => emitter.stage(JobStage::Completed),
                Err(DownloadError::Paused) => emitter.stage(JobStage::Paused),
                Err(DownloadError::Cancelled) => emitter.stage(JobStage::Cancelled),
                Err(_) => emitter.stage(JobStage::Failed),
            }
            let _ignored = completion_sender.send(result);
        });
        StartedDownload {
            job_id,
            events: JobEventStream {
                receiver: event_receiver,
            },
            completion: CompletionHandle {
                receiver: completion_receiver,
            },
            controls,
        }
    }

    async fn run_job(
        &self,
        request: DownloadRequest,
        job_id: JobId,
        emitter: EventEmitter,
        control: Arc<AtomicU8>,
        cancellation: CancellationToken,
    ) -> Result<DownloadResult, DownloadError> {
        ensure_running(&control)?;
        let destination = validate_destination(request.destination.as_path()).await?;
        emitter.stage(JobStage::Analyzing);
        let analyzer = Analyzer::with_runner(self.tools.analysis_tools(), Arc::clone(&self.runner));
        let media = analyzer
            .analyze(&request.url, cancellation.child_token())
            .await
            .map_err(|error| map_analysis_error(error, &control))?;
        ensure_running(&control)?;

        let selected = resolve_format(&media, request.output)?.clone();
        let raw_stem = request
            .name
            .as_ref()
            .map_or(media.title.as_str(), |name| name.as_str());
        let stripped = strip_internal_extension(raw_stem, request.output.extension());
        let stem = sanitize_output_stem(stripped);
        let extension = request.output.extension();
        let reservation = reserve_async(destination.clone(), stem, extension.to_owned()).await?;
        let mut workspace = Workspace::new(
            destination,
            reservation.path(),
            reservation.lock_path(),
            &selected,
            request.output,
            &job_id,
        )?;

        self.download_sources(
            &request,
            &selected,
            &mut workspace,
            &emitter,
            &control,
            cancellation.child_token(),
        )
        .await?;
        self.produce_work_output(
            OutputWork {
                media: &media,
                selected: &selected,
                output: request.output,
                workspace: &workspace,
            },
            JobRuntime {
                emitter: &emitter,
                control: &control,
            },
            cancellation.child_token(),
        )
        .await?;
        ensure_running(&control)?;

        let work_path = workspace.work_path().to_path_buf();
        emitter.stage(JobStage::Finalizing);
        self.verify_output(
            &work_path,
            request.output,
            media.duration.as_millis(),
            cancellation.child_token(),
            &control,
        )
        .await?;
        let metadata =
            tokio::fs::metadata(&work_path)
                .await
                .map_err(|source| DownloadError::Filesystem {
                    operation: "read-verified-temporary-metadata",
                    path: bounded_path(&work_path),
                    source,
                })?;
        if metadata.len() == 0 {
            return Err(DownloadError::Verification(
                "verified temporary output was empty".to_owned(),
            ));
        }
        let published = publish_async(reservation, work_path).await?;
        workspace.mark_published();
        Ok(DownloadResult {
            job_id,
            path: published,
            size_bytes: metadata.len(),
            output: request.output,
        })
    }

    async fn download_sources(
        &self,
        request: &DownloadRequest,
        selected: &FormatOption,
        workspace: &mut Workspace,
        emitter: &EventEmitter,
        control: &Arc<AtomicU8>,
        cancellation: CancellationToken,
    ) -> Result<(), DownloadError> {
        emitter.stage(JobStage::Downloading);
        let source_count = if same_source(selected) { 1 } else { 2 };
        let estimated_total = match selected {
            FormatOption::Mp4 {
                estimated_size_bytes,
                ..
            } => *estimated_size_bytes,
            FormatOption::Mp3 { .. } => None,
        };
        let mut aggregator = DownloadProgressAggregator::new(source_count, estimated_total);
        for source_index in 0..source_count {
            ensure_running(control)?;
            let (source, path) = workspace.source(source_index)?;
            let outcome = self
                .download_source(
                    SourceDownload {
                        request,
                        source,
                        path,
                        source_index,
                        aggregator: &mut aggregator,
                    },
                    JobRuntime { emitter, control },
                    cancellation.child_token(),
                )
                .await;
            if matches!(
                outcome,
                Err(ref error) if !matches!(error, DownloadError::Cancelled)
            ) {
                workspace.preserve_resumable_partials();
            }
            outcome?;
        }
        workspace.remove_resumable_sidecars();
        ensure_running(control)
    }

    async fn produce_work_output(
        &self,
        work: OutputWork<'_>,
        runtime: JobRuntime<'_>,
        cancellation: CancellationToken,
    ) -> Result<(), DownloadError> {
        let work_path = work.workspace.work_path().to_path_buf();
        match (work.selected, work.output) {
            (FormatOption::Mp3 { .. }, OutputSelection::Mp3(quality)) => {
                runtime.emitter.stage(JobStage::Converting);
                let metadata = SafeMetadata::from_media(work.media);
                let arguments = mp3_arguments(
                    work.workspace.source(0)?.1,
                    &work_path,
                    quality.bitrate_kbps(),
                    &metadata,
                );
                self.run_ffmpeg(
                    arguments,
                    JobStage::Converting,
                    work.media.duration.as_millis(),
                    runtime.emitter,
                    runtime.control,
                    cancellation,
                )
                .await
            }
            (
                FormatOption::Mp4 {
                    compatibility: CompatibilityWork::None,
                    ..
                },
                OutputSelection::Mp4(_),
            ) => copy_or_link(work.workspace.source(0)?.1.to_path_buf(), work_path).await,
            (
                FormatOption::Mp4 {
                    compatibility,
                    video_source,
                    audio_source,
                    ..
                },
                OutputSelection::Mp4(_),
            ) => {
                let stage = if *compatibility == CompatibilityWork::Merge {
                    JobStage::Merging
                } else {
                    JobStage::Converting
                };
                runtime.emitter.stage(stage);
                let arguments = mp4_arguments(
                    work.workspace.source(0)?.1,
                    if same_source(work.selected) {
                        None
                    } else {
                        Some(work.workspace.source(1)?.1)
                    },
                    &work_path,
                    video_source,
                    audio_source,
                    *compatibility,
                );
                self.run_ffmpeg(
                    arguments,
                    stage,
                    work.media.duration.as_millis(),
                    runtime.emitter,
                    runtime.control,
                    cancellation,
                )
                .await
            }
            _ => Err(DownloadError::Protocol {
                protocol: "selection",
                reason: "resolved format family differed from the request".to_owned(),
            }),
        }
    }

    async fn download_source(
        &self,
        source_download: SourceDownload<'_>,
        runtime: JobRuntime<'_>,
        cancellation: CancellationToken,
    ) -> Result<(), DownloadError> {
        let output_limit = process_output_limit()?;
        let runtime_argument = format!("deno:{}", self.tools.deno.as_path().display());
        let spec = ProcessSpec::new(self.tools.yt_dlp.as_path())
            .arguments([
                "--ignore-config",
                "--no-config-locations",
                "--no-plugin-dirs",
                "--no-playlist",
                "--no-js-runtimes",
                "--js-runtimes",
            ])
            .argument(runtime_argument)
            .argument("--ffmpeg-location")
            .argument(self.tools.ffmpeg.as_path().as_os_str())
            .arguments([
                "--newline",
                "--progress-delta",
                "1",
                "--progress-template",
                YTDLP_PROGRESS_TEMPLATE,
                "--continue",
                "--no-overwrites",
                "--format",
            ])
            .argument(source_download.source.format_id.as_str())
            .argument("--output")
            .argument(source_download.path.as_os_str())
            .argument("--")
            .argument(source_download.request.url.as_str())
            .timeout(DOWNLOAD_TIMEOUT)
            .output_limit(output_limit);
        let (observer_sender, mut observer_receiver) = mpsc::channel(PROCESS_EVENT_CAPACITY);
        let process = self
            .runner
            .run_streaming(spec, cancellation, observer_sender);
        tokio::pin!(process);
        let mut lines = ProtocolLines::new();
        let output = loop {
            tokio::select! {
                result = &mut process => break result,
                event = observer_receiver.recv() => {
                    if let Some(event) = event {
                        apply_ytdlp_event(
                            &event,
                            source_download.source_index,
                            &mut lines,
                            &mut *source_download.aggregator,
                            runtime.emitter,
                        );
                    }
                }
            }
        };
        while let Ok(event) = observer_receiver.try_recv() {
            apply_ytdlp_event(
                &event,
                source_download.source_index,
                &mut lines,
                &mut *source_download.aggregator,
                runtime.emitter,
            );
        }
        if let Some(reason) = lines.invalid_reason() {
            return Err(DownloadError::Protocol {
                protocol: "yt-dlp progress",
                reason: reason.to_owned(),
            });
        }
        let output = output.map_err(|error| map_process_error("yt-dlp", error, runtime.control))?;
        validate_process_output("yt-dlp", &output)?;
        emit_warnings(&output, runtime.emitter);
        if !source_download.path.is_file() {
            return Err(DownloadError::Verification(format!(
                "yt-dlp did not create selected source `{}`",
                bounded_path(source_download.path)
            )));
        }
        Ok(())
    }

    async fn run_ffmpeg(
        &self,
        arguments: Vec<OsString>,
        stage: JobStage,
        duration_millis: u64,
        emitter: &EventEmitter,
        control: &Arc<AtomicU8>,
        cancellation: CancellationToken,
    ) -> Result<(), DownloadError> {
        let spec = ProcessSpec::new(self.tools.ffmpeg.as_path())
            .arguments(arguments)
            .timeout(FFMPEG_TIMEOUT)
            .output_limit(process_output_limit()?);
        let (observer_sender, mut observer_receiver) = mpsc::channel(PROCESS_EVENT_CAPACITY);
        let process = self
            .runner
            .run_streaming(spec, cancellation, observer_sender);
        tokio::pin!(process);
        let mut lines = ProtocolLines::new();
        let mut parser = FfmpegProgress::new(duration_millis);
        let output = loop {
            tokio::select! {
                result = &mut process => break result,
                event = observer_receiver.recv() => {
                    if let Some(event) = event {
                        apply_ffmpeg_event(&event, stage, &mut lines, &mut parser, emitter);
                    }
                }
            }
        };
        while let Ok(event) = observer_receiver.try_recv() {
            apply_ffmpeg_event(&event, stage, &mut lines, &mut parser, emitter);
        }
        if let Some(reason) = lines.invalid_reason() {
            return Err(DownloadError::Protocol {
                protocol: "FFmpeg progress",
                reason: reason.to_owned(),
            });
        }
        let output = output.map_err(|error| map_process_error("FFmpeg", error, control))?;
        validate_process_output("FFmpeg", &output)?;
        emit_warnings(&output, emitter);
        Ok(())
    }

    async fn verify_output(
        &self,
        path: &Path,
        selection: OutputSelection,
        expected_duration_millis: u64,
        cancellation: CancellationToken,
        control: &Arc<AtomicU8>,
    ) -> Result<(), DownloadError> {
        let spec = ProcessSpec::new(self.tools.ffprobe.as_path())
            .arguments([
                "-v",
                "error",
                "-show_entries",
                "format=format_name,duration:stream=codec_type,codec_name,width,height,pix_fmt",
                "-of",
                "json",
                "--",
            ])
            .argument(path.as_os_str())
            .timeout(FFPROBE_TIMEOUT)
            .output_limit(process_output_limit()?);
        let output = self
            .runner
            .run(spec, cancellation)
            .await
            .map_err(|error| map_process_error("FFprobe", error, control))?;
        validate_process_output("FFprobe", &output)?;
        let stdout = std::str::from_utf8(&output.capture.stdout.bytes).map_err(|_| {
            DownloadError::Protocol {
                protocol: "FFprobe JSON",
                reason: "stdout was not valid UTF-8".to_owned(),
            }
        })?;
        let probe: ProbeDocument =
            serde_json::from_str(stdout).map_err(|error| DownloadError::Protocol {
                protocol: "FFprobe JSON",
                reason: bounded_text(&error.to_string(), MAX_DIAGNOSTIC_CHARS),
            })?;
        validate_probe(&probe, selection, expected_duration_millis)
    }
}

#[derive(Clone)]
struct EventEmitter {
    job_id: JobId,
    sender: broadcast::Sender<JobEvent>,
    sequence: Arc<AtomicU64>,
    stage_state: Arc<AtomicU8>,
}

impl EventEmitter {
    fn new(job_id: JobId, sender: broadcast::Sender<JobEvent>) -> Self {
        Self {
            job_id,
            sender,
            sequence: Arc::new(AtomicU64::new(0)),
            stage_state: Arc::new(AtomicU8::new(NO_STAGE)),
        }
    }

    fn stage(&self, stage: JobStage) {
        let previous_code = self.stage_state.load(Ordering::Acquire);
        let previous = stage_from_code(previous_code);
        if !valid_stage_transition(previous, stage) {
            self.warning(format!(
                "engine suppressed invalid stage transition from {previous:?} to {stage:?}"
            ));
            return;
        }
        self.stage_state.store(stage_code(stage), Ordering::Release);
        self.emit(JobEventKind::Stage { stage });
    }

    fn progress(&self, progress: JobProgress) {
        self.emit(JobEventKind::Progress { progress });
    }

    fn warning(&self, message: String) {
        self.emit(JobEventKind::Warning { message });
    }

    fn emit(&self, kind: JobEventKind) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let _ignored = self.sender.send(JobEvent {
            job_id: self.job_id.clone(),
            sequence,
            kind,
        });
    }
}

const fn stage_code(stage: JobStage) -> u8 {
    match stage {
        JobStage::Analyzing => 0,
        JobStage::Downloading => 1,
        JobStage::Merging => 2,
        JobStage::Converting => 3,
        JobStage::Finalizing => 4,
        JobStage::Completed => 5,
        JobStage::Paused => 6,
        JobStage::Cancelled => 7,
        JobStage::Failed => 8,
    }
}

const fn stage_from_code(code: u8) -> Option<JobStage> {
    match code {
        0 => Some(JobStage::Analyzing),
        1 => Some(JobStage::Downloading),
        2 => Some(JobStage::Merging),
        3 => Some(JobStage::Converting),
        4 => Some(JobStage::Finalizing),
        5 => Some(JobStage::Completed),
        6 => Some(JobStage::Paused),
        7 => Some(JobStage::Cancelled),
        8 => Some(JobStage::Failed),
        _ => None,
    }
}

const fn valid_stage_transition(previous: Option<JobStage>, next: JobStage) -> bool {
    match previous {
        None => matches!(
            next,
            JobStage::Analyzing | JobStage::Paused | JobStage::Cancelled | JobStage::Failed
        ),
        Some(JobStage::Analyzing) => matches!(
            next,
            JobStage::Downloading | JobStage::Paused | JobStage::Cancelled | JobStage::Failed
        ),
        Some(JobStage::Downloading) => matches!(
            next,
            JobStage::Merging
                | JobStage::Converting
                | JobStage::Finalizing
                | JobStage::Paused
                | JobStage::Cancelled
                | JobStage::Failed
        ),
        Some(JobStage::Merging | JobStage::Converting) => matches!(
            next,
            JobStage::Finalizing | JobStage::Paused | JobStage::Cancelled | JobStage::Failed
        ),
        Some(JobStage::Finalizing) => matches!(
            next,
            JobStage::Completed | JobStage::Paused | JobStage::Cancelled | JobStage::Failed
        ),
        Some(JobStage::Completed | JobStage::Paused | JobStage::Cancelled | JobStage::Failed) => {
            false
        }
    }
}

fn ensure_running(control: &AtomicU8) -> Result<(), DownloadError> {
    match control.load(Ordering::Acquire) {
        CONTROL_PAUSE => Err(DownloadError::Paused),
        CONTROL_CANCEL => Err(DownloadError::Cancelled),
        _ => Ok(()),
    }
}

fn map_analysis_error(error: AnalyzeError, control: &AtomicU8) -> DownloadError {
    match control.load(Ordering::Acquire) {
        CONTROL_PAUSE => DownloadError::Paused,
        CONTROL_CANCEL => DownloadError::Cancelled,
        _ => DownloadError::Analysis(error),
    }
}

fn map_process_error(tool: &'static str, error: ProcessError, control: &AtomicU8) -> DownloadError {
    if matches!(error, ProcessError::Cancelled { .. }) {
        match control.load(Ordering::Acquire) {
            CONTROL_PAUSE => return DownloadError::Paused,
            CONTROL_CANCEL => return DownloadError::Cancelled,
            _ => {}
        }
    }
    DownloadError::Process {
        tool,
        source: Box::new(error),
    }
}

async fn validate_destination(path: &Path) -> Result<PathBuf, DownloadError> {
    let requested = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let metadata = fs::metadata(&requested).map_err(|source| DownloadError::Destination {
            path: bounded_path(&requested),
            reason: bounded_text(&source.to_string(), 512),
        })?;
        if !metadata.is_dir() {
            return Err(DownloadError::Destination {
                path: bounded_path(&requested),
                reason: "path is not a directory".to_owned(),
            });
        }
        let canonical =
            fs::canonicalize(&requested).map_err(|source| DownloadError::Destination {
                path: bounded_path(&requested),
                reason: bounded_text(&source.to_string(), 512),
            })?;
        let Some(canonical_text) = canonical.to_str() else {
            return Err(DownloadError::Destination {
                path: bounded_path(&requested),
                reason: "canonical destination was not valid Unicode".to_owned(),
            });
        };
        if canonical_text.chars().count() > 4_096 {
            return Err(DownloadError::Destination {
                path: bounded_path(&requested),
                reason: "canonical destination exceeded the 4096-character limit".to_owned(),
            });
        }
        Ok(canonical)
    })
    .await
    .map_err(DownloadError::Join)?
}

async fn reserve_async(
    directory: PathBuf,
    stem: String,
    extension: String,
) -> Result<OutputReservation, DownloadError> {
    tokio::task::spawn_blocking(move || reserve_output(&directory, &stem, &extension))
        .await
        .map_err(DownloadError::Join)?
}

async fn publish_async(
    reservation: OutputReservation,
    temporary: PathBuf,
) -> Result<PathBuf, DownloadError> {
    tokio::task::spawn_blocking(move || reservation.publish(&temporary))
        .await
        .map_err(DownloadError::Join)?
}

async fn copy_or_link(source: PathBuf, target: PathBuf) -> Result<(), DownloadError> {
    tokio::task::spawn_blocking(move || match fs::hard_link(&source, &target) {
        Ok(()) => Ok(()),
        Err(_) => fs::copy(&source, &target)
            .map(|_| ())
            .map_err(|source_error| DownloadError::Filesystem {
                operation: "prepare-direct-mp4",
                path: bounded_path(&target),
                source: source_error,
            }),
    })
    .await
    .map_err(DownloadError::Join)?
}

fn resolve_format(
    media: &MediaInfo,
    selection: OutputSelection,
) -> Result<&FormatOption, DownloadError> {
    let selected = media
        .formats
        .iter()
        .find(|option| match (option, selection) {
            (FormatOption::Mp3 { bitrate_kbps, .. }, OutputSelection::Mp3(quality)) => {
                *bitrate_kbps == quality.bitrate_kbps()
            }
            (FormatOption::Mp4 { height, .. }, OutputSelection::Mp4(quality)) => {
                *height == quality.height()
            }
            _ => false,
        });
    selected.ok_or_else(|| DownloadError::FormatUnavailable {
        requested: match selection {
            OutputSelection::Mp3(quality) => format!("MP3 {} kbps", quality.bitrate_kbps()),
            OutputSelection::Mp4(quality) => format!("MP4 {}p", quality.height()),
        },
        available: available_formats(media),
    })
}

fn available_formats(media: &MediaInfo) -> String {
    let mut values = media
        .formats
        .iter()
        .take(64)
        .map(|option| match option {
            FormatOption::Mp3 { bitrate_kbps, .. } => format!("mp3:{bitrate_kbps}"),
            FormatOption::Mp4 { height, .. } => format!("mp4:{height}"),
        })
        .collect::<Vec<_>>();
    if media.formats.len() > values.len() {
        values.push("…".to_owned());
    }
    values.join(", ")
}

fn strip_internal_extension<'a>(name: &'a str, extension: &str) -> &'a str {
    let suffix = format!(".{extension}");
    name.get(name.len().saturating_sub(suffix.len())..)
        .filter(|tail| tail.eq_ignore_ascii_case(&suffix))
        .and_then(|_| name.get(..name.len().saturating_sub(suffix.len())))
        .unwrap_or(name)
}

fn same_source(option: &FormatOption) -> bool {
    match option {
        FormatOption::Mp3 { .. } => true,
        FormatOption::Mp4 {
            video_source,
            audio_source,
            ..
        } => video_source.format_id == audio_source.format_id,
    }
}

fn source_extension(source: &SourceFormat) -> String {
    let value = source
        .container
        .name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect::<String>();
    if value.is_empty() {
        "media".to_owned()
    } else {
        value
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OwnerManifest {
    version: u32,
    job_id: String,
    final_name: String,
    exact_paths: Vec<PathBuf>,
    source_paths: Vec<PathBuf>,
}

fn owner_manifest_path(directory: &Path, job_id: &JobId) -> PathBuf {
    directory.join(format!(".yt-media-{}.owner.json", job_id.as_str()))
}

fn safe_format_component(value: &str) -> String {
    let token = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(32)
        .collect::<String>();
    if token.is_empty() {
        "format".to_owned()
    } else {
        token
    }
}

fn write_owner_manifest(path: &Path, manifest: &OwnerManifest) -> Result<(), DownloadError> {
    let bytes = serde_json::to_vec(manifest).map_err(|error| DownloadError::Protocol {
        protocol: "partial-owner-manifest",
        reason: format!("could not serialize ownership data: {error}"),
    })?;
    let mut owner = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| DownloadError::Filesystem {
            operation: "claim-workspace-owner",
            path: bounded_path(path),
            source,
        })?;
    if let Err(source) = owner.write_all(&bytes).and_then(|()| owner.sync_all()) {
        drop(owner);
        let _ignored = fs::remove_file(path);
        return Err(DownloadError::Filesystem {
            operation: "write-workspace-owner",
            path: bounded_path(path),
            source,
        });
    }
    Ok(())
}

fn read_owner_manifest(path: &Path, job_id: &JobId) -> Result<OwnerManifest, DownloadError> {
    let metadata = fs::metadata(path).map_err(|source| DownloadError::Filesystem {
        operation: "read-workspace-owner-metadata",
        path: bounded_path(path),
        source,
    })?;
    if metadata.len() > 64 * 1024 {
        return Err(workspace_conflict(path));
    }
    let bytes = fs::read(path).map_err(|source| DownloadError::Filesystem {
        operation: "read-workspace-owner",
        path: bounded_path(path),
        source,
    })?;
    let manifest =
        serde_json::from_slice::<OwnerManifest>(&bytes).map_err(|_| workspace_conflict(path))?;
    if manifest.version != OWNER_MANIFEST_VERSION
        || manifest.job_id != job_id.as_str()
        || manifest.final_name.chars().count() > 255
        || manifest.exact_paths.len() > 8
        || manifest.source_paths.len() > 2
    {
        return Err(workspace_conflict(path));
    }
    let Some(directory) = path.parent() else {
        return Err(workspace_conflict(path));
    };
    if manifest
        .exact_paths
        .iter()
        .chain(&manifest.source_paths)
        .any(|owned| owned.parent() != Some(directory))
    {
        return Err(workspace_conflict(path));
    }
    Ok(manifest)
}

fn discover_manifest_paths(
    directory: &Path,
    owner_path: &Path,
    manifest: &OwnerManifest,
) -> Result<Vec<PathBuf>, DownloadError> {
    let mut paths = Vec::new();
    if owner_path.is_file() {
        paths.push(owner_path.to_path_buf());
    }
    paths.extend(
        manifest
            .exact_paths
            .iter()
            .filter(|path| path.is_file())
            .cloned(),
    );
    let entries = fs::read_dir(directory).map_err(|source| DownloadError::Filesystem {
        operation: "discover-owned-partials",
        path: bounded_path(directory),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| DownloadError::Filesystem {
            operation: "read-owned-partial-entry",
            path: bounded_path(directory),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if manifest.source_paths.iter().any(|source| {
            source
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|source_name| {
                    name.starts_with(&format!("{source_name}.part"))
                        || name == format!("{source_name}.ytdl")
                })
        }) {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn remove_manifest_paths(
    directory: &Path,
    owner_path: &Path,
    manifest: &OwnerManifest,
) -> Result<(), DownloadError> {
    let paths = discover_manifest_paths(directory, owner_path, manifest)?;
    for path in paths.iter().filter(|path| path.as_path() != owner_path) {
        remove_exact_if_exists(path)?;
    }
    remove_exact_if_exists(owner_path)
}

pub(crate) fn reconcile_owned_paths(
    directory: &Path,
    job_id: &JobId,
    retain_resumable_only: bool,
) -> Result<Vec<PathBuf>, DownloadError> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let owner_path = owner_manifest_path(directory, job_id);
    if !owner_path.is_file() {
        return Ok(Vec::new());
    }
    let manifest = read_owner_manifest(&owner_path, job_id)?;
    let discovered = discover_manifest_paths(directory, &owner_path, &manifest)?;
    if !retain_resumable_only {
        return Ok(discovered);
    }
    for path in &manifest.exact_paths {
        remove_exact_if_exists(path)?;
    }
    discover_manifest_paths(directory, &owner_path, &manifest)
}

pub(crate) fn remove_recorded_owned_paths(
    directory: &Path,
    job_id: &JobId,
    recorded: &[PathBuf],
) -> Result<(), DownloadError> {
    if recorded.is_empty() {
        return Ok(());
    }
    let owner_path = owner_manifest_path(directory, job_id);
    let manifest = read_owner_manifest(&owner_path, job_id)?;
    let discovered = discover_manifest_paths(directory, &owner_path, &manifest)?;
    if recorded
        .iter()
        .any(|path| !discovered.iter().any(|owned| owned == path))
    {
        return Err(workspace_conflict(&owner_path));
    }
    for path in recorded.iter().filter(|path| path.as_path() != owner_path) {
        remove_exact_if_exists(path)?;
    }
    if recorded.iter().any(|path| path == &owner_path) {
        remove_exact_if_exists(&owner_path)?;
    }
    Ok(())
}

struct Workspace {
    directory: PathBuf,
    owner_path: PathBuf,
    sources: Vec<SourceFormat>,
    source_paths: Vec<PathBuf>,
    work_path: PathBuf,
    preserve_partials: bool,
}

impl Workspace {
    fn new(
        directory: PathBuf,
        final_path: &Path,
        reservation_path: &Path,
        selected: &FormatOption,
        output: OutputSelection,
        job_id: &JobId,
    ) -> Result<Self, DownloadError> {
        let final_name = final_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                DownloadError::Verification("final filename was not UTF-8".to_owned())
            })?;
        let mut sources = Vec::new();
        match selected {
            FormatOption::Mp3 { source, .. } => sources.push(source.clone()),
            FormatOption::Mp4 {
                video_source,
                audio_source,
                ..
            } => {
                sources.push(video_source.clone());
                if video_source.format_id != audio_source.format_id {
                    sources.push(audio_source.clone());
                }
            }
        }
        let source_paths = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                directory.join(format!(
                    ".yt-media-{}.source-{}-{}.{}",
                    job_id.as_str(),
                    index + 1,
                    safe_format_component(source.format_id.as_str()),
                    source_extension(source)
                ))
            })
            .collect::<Vec<_>>();
        let work_path = directory.join(format!(
            ".yt-media-{}.work.{}",
            job_id.as_str(),
            output.extension()
        ));
        let owner_path = owner_manifest_path(&directory, job_id);
        claim_workspace(
            &directory,
            &owner_path,
            &source_paths,
            &work_path,
            reservation_path,
            job_id,
            final_name,
        )?;
        remove_exact_if_exists(&work_path)?;
        for path in &source_paths {
            if path.is_file() {
                remove_exact_if_exists(path)?;
            }
        }
        Ok(Self {
            directory,
            owner_path,
            sources,
            source_paths,
            work_path,
            preserve_partials: false,
        })
    }

    fn source(&self, index: usize) -> Result<(&SourceFormat, &Path), DownloadError> {
        let source = self
            .sources
            .get(index)
            .ok_or_else(|| DownloadError::Protocol {
                protocol: "workspace",
                reason: format!("selected source index {index} was unavailable"),
            })?;
        let path = self
            .source_paths
            .get(index)
            .ok_or_else(|| DownloadError::Protocol {
                protocol: "workspace",
                reason: format!("selected source path index {index} was unavailable"),
            })?;
        Ok((source, path))
    }

    fn work_path(&self) -> &Path {
        &self.work_path
    }

    fn preserve_resumable_partials(&mut self) {
        self.preserve_partials = true;
    }

    fn remove_resumable_sidecars(&mut self) {
        self.preserve_partials = false;
    }

    fn mark_published(&mut self) {
        self.preserve_partials = false;
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ignored = fs::remove_file(&self.work_path);
        for source in &self.source_paths {
            let _ignored = fs::remove_file(source);
        }
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let owned_by_source = self.source_paths.iter().any(|source| {
                let Some(source_name) = source.file_name().and_then(|value| value.to_str()) else {
                    return false;
                };
                let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                    return false;
                };
                name.starts_with(&format!("{source_name}.part"))
                    || name == format!("{source_name}.ytdl")
            });
            if owned_by_source && !self.preserve_partials {
                let _ignored = fs::remove_file(path);
            }
        }
        if !self.preserve_partials {
            let _ignored = fs::remove_file(&self.owner_path);
        }
    }
}

fn claim_workspace(
    directory: &Path,
    owner_path: &Path,
    source_paths: &[PathBuf],
    work_path: &Path,
    reservation_path: &Path,
    job_id: &JobId,
    final_name: &str,
) -> Result<(), DownloadError> {
    let manifest = OwnerManifest {
        version: OWNER_MANIFEST_VERSION,
        job_id: job_id.as_str().to_owned(),
        final_name: final_name.to_owned(),
        exact_paths: source_paths
            .iter()
            .cloned()
            .chain([work_path.to_path_buf(), reservation_path.to_path_buf()])
            .collect(),
        source_paths: source_paths.to_vec(),
    };
    if owner_path.exists() {
        let existing = read_owner_manifest(owner_path, job_id)?;
        if existing != manifest {
            remove_manifest_paths(directory, owner_path, &existing)?;
            write_owner_manifest(owner_path, &manifest)?;
        }
        return Ok(());
    }
    if workspace_paths_exist(directory, source_paths, work_path)? {
        return Err(workspace_conflict(owner_path));
    }
    write_owner_manifest(owner_path, &manifest)
}

fn workspace_paths_exist(
    directory: &Path,
    source_paths: &[PathBuf],
    work_path: &Path,
) -> Result<bool, DownloadError> {
    if work_path.exists() || source_paths.iter().any(|source| source.exists()) {
        return Ok(true);
    }
    let entries = fs::read_dir(directory).map_err(|source| DownloadError::Filesystem {
        operation: "inspect-workspace-conflicts",
        path: bounded_path(directory),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| DownloadError::Filesystem {
            operation: "inspect-workspace-entry",
            path: bounded_path(directory),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if source_paths.iter().any(|source| {
            source
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|source_name| {
                    name.starts_with(&format!("{source_name}.part"))
                        || name == format!("{source_name}.ytdl")
                })
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn workspace_conflict(path: &Path) -> DownloadError {
    DownloadError::Destination {
        path: bounded_path(path),
        reason: "unowned or invalid temporary workspace files conflict with this output name"
            .to_owned(),
    }
}

fn remove_exact_if_exists(path: &Path) -> Result<(), DownloadError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(DownloadError::Filesystem {
            operation: "remove-stale-owned-file",
            path: bounded_path(path),
            source,
        }),
    }
}

fn process_output_limit() -> Result<OutputLimit, DownloadError> {
    OutputLimit::new(PROCESS_MAX_BYTES, PROCESS_MAX_LINES)
        .map_err(DownloadError::ProcessSpecification)
}

fn validate_process_output(
    tool: &'static str,
    output: &ProcessOutput,
) -> Result<(), DownloadError> {
    for (name, capture) in [
        ("stdout", &output.capture.stdout),
        ("stderr", &output.capture.stderr),
    ] {
        if capture.truncated {
            return Err(DownloadError::Protocol {
                protocol: tool,
                reason: format!("{name} exceeded its bounded capture"),
            });
        }
        if std::str::from_utf8(&capture.bytes).is_err() {
            return Err(DownloadError::Protocol {
                protocol: tool,
                reason: format!("{name} was not valid UTF-8"),
            });
        }
    }
    if output.status.success {
        Ok(())
    } else {
        Err(DownloadError::NonZero {
            tool,
            status: output.status.code,
            diagnostics: diagnostics(output),
        })
    }
}

fn diagnostics(output: &ProcessOutput) -> String {
    let bytes = if output.capture.stderr.bytes.is_empty() {
        &output.capture.stdout.bytes
    } else {
        &output.capture.stderr.bytes
    };
    bounded_text(&String::from_utf8_lossy(bytes), MAX_DIAGNOSTIC_CHARS)
}

fn emit_warnings(output: &ProcessOutput, emitter: &EventEmitter) {
    let text = String::from_utf8_lossy(&output.capture.stderr.bytes);
    for line in text.lines().take(MAX_WARNING_LINES) {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with("yt-media-progress|") {
            emitter.warning(bounded_text(line, MAX_WARNING_CHARS));
        }
    }
}

fn bounded_text(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn apply_ytdlp_event(
    event: &crate::process::ProcessEvent,
    source_index: usize,
    lines: &mut ProtocolLines,
    aggregator: &mut DownloadProgressAggregator,
    emitter: &EventEmitter,
) {
    for line in lines.push(event) {
        if let Some(progress) = parse_ytdlp_progress(&line) {
            let source = u8::try_from(source_index).unwrap_or(u8::MAX);
            emitter.progress(aggregator.update(source, progress));
        }
    }
}

fn apply_ffmpeg_event(
    event: &crate::process::ProcessEvent,
    stage: JobStage,
    lines: &mut ProtocolLines,
    parser: &mut FfmpegProgress,
    emitter: &EventEmitter,
) {
    if event.stream != OutputStream::Stdout {
        return;
    }
    for line in lines.push(event) {
        if let Some(progress) = parser.parse_line(&line, stage) {
            emitter.progress(progress);
        }
    }
}

struct SafeMetadata {
    title: String,
    artist: Option<String>,
}

impl SafeMetadata {
    fn from_media(media: &MediaInfo) -> Self {
        Self {
            title: sanitize_metadata(&media.title, 512),
            artist: media
                .uploader
                .as_deref()
                .map(|value| sanitize_metadata(value, 256))
                .filter(|value| !value.is_empty()),
        }
    }
}

fn sanitize_metadata(value: &str, maximum: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(maximum)
        .collect()
}

fn mp3_arguments(
    source: &Path,
    target: &Path,
    bitrate_kbps: u16,
    metadata: &SafeMetadata,
) -> Vec<OsString> {
    let mut arguments = strings([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "warning",
        "-n",
        "-i",
    ]);
    arguments.push(source.as_os_str().to_owned());
    arguments.extend(strings([
        "-map",
        "0:a:0",
        "-vn",
        "-c:a",
        "libmp3lame",
        "-b:a",
        &format!("{bitrate_kbps}k"),
        "-id3v2_version",
        "3",
        "-metadata",
        &format!("title={}", metadata.title),
    ]));
    if let Some(artist) = &metadata.artist {
        arguments.extend(strings(["-metadata", &format!("artist={artist}")]));
    }
    arguments.extend(strings(["-progress", "pipe:1", "-nostats"]));
    arguments.push(target.as_os_str().to_owned());
    arguments
}

fn mp4_arguments(
    video_path: &Path,
    audio_path: Option<&Path>,
    target: &Path,
    video_source: &SourceFormat,
    audio_source: &SourceFormat,
    compatibility: CompatibilityWork,
) -> Vec<OsString> {
    let mut arguments = strings([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "warning",
        "-n",
        "-i",
    ]);
    arguments.push(video_path.as_os_str().to_owned());
    if let Some(audio_path) = audio_path {
        arguments.push(OsString::from("-i"));
        arguments.push(audio_path.as_os_str().to_owned());
    }
    arguments.extend(strings(["-map", "0:v:0"]));
    arguments.extend(strings([
        "-map",
        if audio_path.is_some() {
            "1:a:0"
        } else {
            "0:a:0"
        },
    ]));
    if video_source
        .video_codec
        .as_ref()
        .is_some_and(|codec| codec.family == VideoCodecFamily::H264)
        && !matches!(
            compatibility,
            CompatibilityWork::VideoTranscode | CompatibilityWork::VideoAndAudioTranscode
        )
    {
        arguments.extend(strings(["-c:v", "copy"]));
    } else {
        arguments.extend(strings([
            "-c:v", "libx264", "-preset", "fast", "-crf", "20", "-pix_fmt", "yuv420p",
        ]));
    }
    if audio_source
        .audio_codec
        .as_ref()
        .is_some_and(|codec| codec.family == AudioCodecFamily::Aac)
        && !matches!(
            compatibility,
            CompatibilityWork::AudioTranscode | CompatibilityWork::VideoAndAudioTranscode
        )
    {
        arguments.extend(strings(["-c:a", "copy"]));
    } else {
        arguments.extend(strings(["-c:a", "aac", "-b:a", "192k"]));
    }
    arguments.extend(strings([
        "-movflags",
        "+faststart",
        "-progress",
        "pipe:1",
        "-nostats",
    ]));
    arguments.push(target.as_os_str().to_owned());
    arguments
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

#[derive(Deserialize)]
struct ProbeDocument {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

fn validate_probe(
    probe: &ProbeDocument,
    selection: OutputSelection,
    expected_duration_millis: u64,
) -> Result<(), DownloadError> {
    if probe.streams.len() > 32 {
        return Err(DownloadError::Verification(
            "FFprobe returned too many streams".to_owned(),
        ));
    }
    let format = probe
        .format
        .as_ref()
        .and_then(|format| format.format_name.as_deref())
        .unwrap_or_default();
    let duration_millis = probe
        .format
        .as_ref()
        .and_then(|format| format.duration.as_deref())
        .and_then(parse_duration_millis)
        .ok_or_else(|| {
            DownloadError::Verification("FFprobe returned no positive duration".to_owned())
        })?;
    let tolerance = expected_duration_millis
        .saturating_div(100)
        .clamp(1_000, 5_000);
    if duration_millis.abs_diff(expected_duration_millis) > tolerance {
        return Err(DownloadError::Verification(format!(
            "duration differed by more than {tolerance} ms"
        )));
    }
    let audio = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"));
    match selection {
        OutputSelection::Mp3(_) => {
            if !format.split(',').any(|name| name == "mp3")
                || audio.and_then(|stream| stream.codec_name.as_deref()) != Some("mp3")
            {
                return Err(DownloadError::Verification(
                    "output was not an MP3 stream in an MP3 container".to_owned(),
                ));
            }
        }
        OutputSelection::Mp4(quality) => {
            let video = probe
                .streams
                .iter()
                .find(|stream| stream.codec_type.as_deref() == Some("video"))
                .ok_or_else(|| {
                    DownloadError::Verification("MP4 output had no video stream".to_owned())
                })?;
            if !format
                .split(',')
                .any(|name| matches!(name, "mov" | "mp4" | "m4a" | "3gp"))
                || video.codec_name.as_deref() != Some("h264")
                || audio.and_then(|stream| stream.codec_name.as_deref()) != Some("aac")
            {
                return Err(DownloadError::Verification(
                    "output was not an H.264/AAC MP4".to_owned(),
                ));
            }
            let height = video.height.ok_or_else(|| {
                DownloadError::Verification("MP4 output had no video height".to_owned())
            })?;
            if height > quality.height() {
                return Err(DownloadError::Verification(format!(
                    "output height {height} exceeded requested source height {}",
                    quality.height()
                )));
            }
            if video.width.is_none() {
                return Err(DownloadError::Verification(
                    "MP4 output had no video width".to_owned(),
                ));
            }
            if video.pix_fmt.as_deref() != Some("yuv420p") {
                return Err(DownloadError::Verification(
                    "MP4 output was not yuv420p".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn parse_duration_millis(value: &str) -> Option<u64> {
    if value.len() > 32 {
        return None;
    }
    let (seconds, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = seconds.parse::<u64>().ok()?;
    if whole > 7 * 24 * 60 * 60 {
        return None;
    }
    let mut fraction_digits = fraction.bytes().take(3).collect::<Vec<_>>();
    if fraction_digits.iter().any(|digit| !digit.is_ascii_digit()) {
        return None;
    }
    while fraction_digits.len() < 3 {
        fraction_digits.push(b'0');
    }
    let millis = std::str::from_utf8(&fraction_digits)
        .ok()?
        .parse::<u64>()
        .ok()?;
    let total = whole.checked_mul(1_000)?.checked_add(millis)?;
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::{
        ProbeDocument, SafeMetadata, mp3_arguments, mp4_arguments, sanitize_metadata, strings,
        strip_internal_extension, valid_stage_transition, validate_probe,
    };
    use crate::{
        analysis::{
            AudioCodecDescriptor, AudioCodecFamily, CompatibilityWork, ContainerDescriptor,
            ContainerFamily, FormatId, SourceFormat, VideoCodecDescriptor, VideoCodecFamily,
        },
        download::{AudioQuality, JobStage, OutputSelection, VideoQuality},
    };
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        process::Command,
    };
    use tempfile::tempdir;

    fn source(id: &str, video: bool, h264: bool, aac: bool) -> SourceFormat {
        SourceFormat {
            format_id: FormatId(id.to_owned()),
            container: ContainerDescriptor {
                name: "mp4".to_owned(),
                family: ContainerFamily::Mp4,
            },
            video_codec: video.then(|| VideoCodecDescriptor {
                name: if h264 { "avc1" } else { "vp9" }.to_owned(),
                family: if h264 {
                    VideoCodecFamily::H264
                } else {
                    VideoCodecFamily::Vp9
                },
            }),
            audio_codec: aac.then(|| AudioCodecDescriptor {
                name: "aac".to_owned(),
                family: AudioCodecFamily::Aac,
            }),
        }
    }

    #[test]
    fn mp3_arguments_lock_constant_bitrate_and_metadata() {
        let args = mp3_arguments(
            Path::new("source.m4a"),
            Path::new("target.mp3"),
            320,
            &SafeMetadata {
                title: "Title".to_owned(),
                artist: Some("Artist".to_owned()),
            },
        );
        let args = args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "libmp3lame"]));
        assert!(args.windows(2).any(|pair| pair == ["-b:a", "320k"]));
        assert!(args.windows(2).any(|pair| pair == ["-progress", "pipe:1"]));
    }

    #[test]
    fn mp4_arguments_copy_compatible_streams() {
        let video = source("v", true, true, false);
        let audio = source("a", false, false, true);
        let args = mp4_arguments(
            Path::new("video.mp4"),
            Some(Path::new("audio.m4a")),
            Path::new("target.mp4"),
            &video,
            &audio,
            CompatibilityWork::Merge,
        );
        let args = args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "copy"]));
        assert!(!args.iter().any(|value| value == "-r"));
    }

    #[test]
    fn mp4_arguments_transcode_with_locked_compatibility_settings() {
        let video = source("v", true, false, false);
        let audio = source("a", false, false, false);
        let args = mp4_arguments(
            Path::new("video.webm"),
            Some(Path::new("audio.webm")),
            Path::new("target.mp4"),
            &video,
            &audio,
            CompatibilityWork::VideoAndAudioTranscode,
        );
        let args = args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        for pair in [
            ["-c:v", "libx264"],
            ["-preset", "fast"],
            ["-crf", "20"],
            ["-pix_fmt", "yuv420p"],
            ["-c:a", "aac"],
            ["-b:a", "192k"],
            ["-movflags", "+faststart"],
        ] {
            assert!(args.windows(2).any(|values| values == pair));
        }
        assert!(!args.iter().any(|value| value == "-r"));
        assert!(!args.iter().any(|value| value == "-s"));
    }

    #[test]
    fn metadata_removes_control_characters_and_is_bounded() {
        let value = sanitize_metadata(&format!("a\0b{}", "x".repeat(600)), 512);
        assert!(!value.contains('\0'));
        assert_eq!(value.chars().count(), 512);
    }

    #[test]
    fn internal_extension_is_not_duplicated() {
        assert_eq!(strip_internal_extension("song.MP3", "mp3"), "song");
        assert_eq!(strip_internal_extension("song.wav", "mp3"), "song.wav");
    }

    #[test]
    fn probe_validation_rejects_upscaling() {
        let probe: Result<ProbeDocument, _> = serde_json::from_str(
            r#"{"streams":[{"codec_type":"video","codec_name":"h264","height":1080},{"codec_type":"audio","codec_name":"aac"}],"format":{"format_name":"mov,mp4","duration":"10.0"}}"#,
        );
        assert!(probe.is_ok());
        if let Ok(probe) = probe {
            let quality = VideoQuality::try_from(720);
            assert!(quality.is_ok());
            if let Ok(quality) = quality {
                assert!(validate_probe(&probe, OutputSelection::Mp4(quality), 10_000).is_err());
            }
        }
    }

    #[test]
    fn audio_quality_is_limited_to_locked_bitrates() {
        assert!(AudioQuality::try_from(128).is_ok());
        assert!(AudioQuality::try_from(320).is_ok());
        assert!(AudioQuality::try_from(129).is_err());
    }

    #[test]
    fn stage_transitions_allow_only_authoritative_lifecycle_paths() {
        assert!(valid_stage_transition(None, JobStage::Analyzing));
        assert!(valid_stage_transition(
            Some(JobStage::Analyzing),
            JobStage::Downloading
        ));
        assert!(valid_stage_transition(
            Some(JobStage::Downloading),
            JobStage::Merging
        ));
        assert!(valid_stage_transition(
            Some(JobStage::Merging),
            JobStage::Finalizing
        ));
        assert!(valid_stage_transition(
            Some(JobStage::Finalizing),
            JobStage::Completed
        ));
        assert!(!valid_stage_transition(
            Some(JobStage::Completed),
            JobStage::Downloading
        ));
        assert!(!valid_stage_transition(
            Some(JobStage::Analyzing),
            JobStage::Completed
        ));
    }

    #[test]
    fn real_media_policies_when_explicit_ffmpeg_is_supplied()
    -> Result<(), Box<dyn std::error::Error>> {
        let Some((ffmpeg, ffprobe)) = explicit_media_tools()? else {
            return Ok(());
        };
        let directory = tempdir()?;
        verify_real_mp3(&ffmpeg, &ffprobe, directory.path())?;
        verify_real_merge(&ffmpeg, &ffprobe, directory.path())?;
        verify_real_transcode(&ffmpeg, &ffprobe, directory.path())?;
        Ok(())
    }

    fn explicit_media_tools() -> Result<Option<(PathBuf, PathBuf)>, Box<dyn std::error::Error>> {
        let Some(ffmpeg) = std::env::var_os("YT_MEDIA_TEST_FFMPEG").map(PathBuf::from) else {
            return Ok(None);
        };
        let Some(ffprobe) = std::env::var_os("YT_MEDIA_TEST_FFPROBE").map(PathBuf::from) else {
            return Ok(None);
        };
        for (path, identity) in [(&ffmpeg, "ffmpeg"), (&ffprobe, "ffprobe")] {
            let output = Command::new(path).arg("-version").output()?;
            let first = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            if !output.status.success() || !first.starts_with(&format!("{identity} version 8.0.1"))
            {
                return Err(format!(
                    "explicit {identity} did not identify the pinned 8.0.1 source family"
                )
                .into());
            }
        }
        Ok(Some((ffmpeg, ffprobe)))
    }

    fn verify_real_mp3(
        ffmpeg: &Path,
        ffprobe: &Path,
        directory: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = directory.join("source.wav");
        run_real(
            ffmpeg,
            strings([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:a",
                "pcm_s16le",
            ])
            .into_iter()
            .chain([source.as_os_str().to_owned()])
            .collect(),
        )?;
        let output = directory.join("output.mp3");
        run_real(
            ffmpeg,
            mp3_arguments(
                &source,
                &output,
                192,
                &SafeMetadata {
                    title: "Fixture title".to_owned(),
                    artist: Some("Fixture artist".to_owned()),
                },
            ),
        )?;
        let probe = real_probe(ffprobe, &output)?;
        validate_probe(
            &probe,
            OutputSelection::Mp3(AudioQuality::try_from(192)?),
            1_000,
        )?;
        let tags = Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format_tags=title,artist",
                "-of",
                "json",
                "--",
            ])
            .arg(&output)
            .output()?;
        let tags = String::from_utf8(tags.stdout)?;
        assert!(tags.contains("Fixture title"));
        assert!(tags.contains("Fixture artist"));
        let metadata = fs::metadata(output)?;
        assert!(metadata.len() > 0);
        Ok(())
    }

    fn verify_real_merge(
        ffmpeg: &Path,
        ffprobe: &Path,
        directory: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let video_path = directory.join("compatible-video.mp4");
        generate_video(ffmpeg, &video_path, "libx264")?;
        let audio_path = directory.join("compatible-audio.m4a");
        generate_audio(ffmpeg, &audio_path, "aac")?;
        let output = directory.join("merged.mp4");
        let video = source("v", true, true, false);
        let audio = source("a", false, false, true);
        run_real(
            ffmpeg,
            mp4_arguments(
                &video_path,
                Some(&audio_path),
                &output,
                &video,
                &audio,
                CompatibilityWork::Merge,
            ),
        )?;
        let probe = real_probe(ffprobe, &output)?;
        validate_probe(
            &probe,
            OutputSelection::Mp4(VideoQuality::try_from(90)?),
            1_000,
        )?;
        assert!(fs::metadata(output)?.len() > 0);
        Ok(())
    }

    fn verify_real_transcode(
        ffmpeg: &Path,
        ffprobe: &Path,
        directory: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let video_path = directory.join("incompatible-video.webm");
        generate_video(ffmpeg, &video_path, "libvpx-vp9")?;
        let audio_path = directory.join("incompatible-audio.webm");
        generate_audio(ffmpeg, &audio_path, "libopus")?;
        let output = directory.join("transcoded.mp4");
        let video = source("v", true, false, false);
        let audio = SourceFormat {
            format_id: FormatId("a".to_owned()),
            container: ContainerDescriptor {
                name: "webm".to_owned(),
                family: ContainerFamily::Webm,
            },
            video_codec: None,
            audio_codec: Some(AudioCodecDescriptor {
                name: "opus".to_owned(),
                family: AudioCodecFamily::Opus,
            }),
        };
        run_real(
            ffmpeg,
            mp4_arguments(
                &video_path,
                Some(&audio_path),
                &output,
                &video,
                &audio,
                CompatibilityWork::VideoAndAudioTranscode,
            ),
        )?;
        let probe = real_probe(ffprobe, &output)?;
        validate_probe(
            &probe,
            OutputSelection::Mp4(VideoQuality::try_from(90)?),
            1_000,
        )?;
        assert!(fs::metadata(output)?.len() > 0);
        Ok(())
    }

    fn generate_video(
        ffmpeg: &Path,
        output: &Path,
        codec: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut arguments = strings([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=24:duration=1",
            "-an",
            "-c:v",
            codec,
            "-pix_fmt",
            "yuv420p",
        ]);
        arguments.push(output.as_os_str().to_owned());
        run_real(ffmpeg, arguments)
    }

    fn generate_audio(
        ffmpeg: &Path,
        output: &Path,
        codec: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut arguments = strings([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-vn",
            "-c:a",
            codec,
        ]);
        arguments.push(output.as_os_str().to_owned());
        run_real(ffmpeg, arguments)
    }

    fn run_real(
        executable: &Path,
        arguments: Vec<OsString>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = Command::new(executable).args(arguments).output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "media fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into())
        }
    }

    fn real_probe(
        ffprobe: &Path,
        path: &Path,
    ) -> Result<ProbeDocument, Box<dyn std::error::Error>> {
        let output = Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=format_name,duration:stream=codec_type,codec_name,width,height,pix_fmt",
                "-of",
                "json",
                "--",
            ])
            .arg(path)
            .output()?;
        if !output.status.success() {
            return Err("real FFprobe fixture command failed".into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }
}
