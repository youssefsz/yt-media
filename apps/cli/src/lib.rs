//! Terminal adapter for the reusable YT Media analysis engine.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind};
use serde::Serialize;
use yt_media_engine::{
    analysis::{
        AnalysisTools, AnalyzeError, Analyzer, CompatibilityWork, FormatOption, MediaInfo, MediaUrl,
    },
    cancellation::CancellationToken,
    download::{
        AudioQuality, Destination, DownloadError, DownloadRequest, DownloadService, DownloadTools,
        JobEvent, JobEventKind, JobStage, OutputName, OutputSelection, VideoQuality,
    },
    resolver::{ResolutionMode, ToolResolutionConfig, ToolResolutionError, ToolResolver},
    target::SupportedTarget,
    tool::Tool,
};

const ANALYZE_SCHEMA_VERSION: u32 = 1;
const DOWNLOAD_SCHEMA_VERSION: u32 = 1;

/// Stable process exit codes for the CLI contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum ExitCode {
    /// The command completed successfully.
    Success = 0,
    /// Arguments or the media URL were invalid.
    InvalidInput = 2,
    /// The URL or extractor result named unsupported content.
    UnsupportedContent = 3,
    /// Required tools were absent, invalid, or had unexpected identities.
    UnavailableTools = 4,
    /// Extraction or analysis failed.
    AnalysisFailure = 5,
    /// Ctrl+C cancelled the operation after process-tree cleanup.
    Cancelled = 6,
    /// Download, conversion, or output verification failed.
    DownloadFailure = 7,
    /// The destination, collision reservation, or final publication failed.
    OutputFailure = 8,
    /// An API caller paused the job after process-tree cleanup.
    Paused = 9,
    /// The CLI adapter or runtime failed unexpectedly.
    InternalFailure = 70,
}

impl ExitCode {
    /// Returns the portable integer process status.
    #[must_use]
    pub const fn value(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "yt-media",
    version,
    about = "Local-first media analysis",
    color = clap::ColorChoice::Never
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Analyze one public, on-demand `YouTube` video.
    #[command(about = "Analyze one public, on-demand YouTube video")]
    Analyze {
        /// Standard watch, youtu.be, or Shorts URL.
        url: String,
        /// Emit exactly one schema-versioned JSON document.
        #[arg(long)]
        json: bool,
        /// Directory containing exact yt-dlp, `FFmpeg`, and Deno executables.
        #[arg(long, value_name = "PATH")]
        tool_dir: Option<PathBuf>,
    },
    /// Download and produce one verified MP3 or MP4 file.
    #[command(about = "Download one public, on-demand YouTube video")]
    Download {
        /// Standard watch, youtu.be, or Shorts URL.
        url: String,
        /// Output container.
        #[arg(long, value_enum)]
        format: CliOutputFormat,
        /// MP3 bitrate (128, 192, 256, 320) or an available MP4 source height.
        #[arg(long)]
        quality: u32,
        /// Existing writable output directory.
        #[arg(long, value_name = "DIR")]
        output: PathBuf,
        /// Optional output stem; the extension is engine-owned.
        #[arg(long)]
        name: Option<String>,
        /// Emit schema-versioned NDJSON events and a final result.
        #[arg(long)]
        json: bool,
        /// Directory containing exact yt-dlp, `FFmpeg`, `FFprobe`, and Deno executables.
        #[arg(long, value_name = "PATH")]
        tool_dir: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliOutputFormat {
    Mp3,
    Mp4,
}

struct DownloadCommandArgs {
    url: String,
    format: CliOutputFormat,
    quality: u32,
    output: PathBuf,
    name: Option<String>,
    json: bool,
    tool_dir: Option<PathBuf>,
}

#[derive(Debug)]
enum CommandFailure {
    InvalidInput(String),
    UnsupportedContent(String),
    UnavailableTools(String),
    Analysis(String),
    Download(String),
    Output(String),
    Cancelled,
    Paused,
    Internal(String),
}

impl CommandFailure {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::InvalidInput(_) => ExitCode::InvalidInput,
            Self::UnsupportedContent(_) => ExitCode::UnsupportedContent,
            Self::UnavailableTools(_) => ExitCode::UnavailableTools,
            Self::Analysis(_) => ExitCode::AnalysisFailure,
            Self::Download(_) => ExitCode::DownloadFailure,
            Self::Output(_) => ExitCode::OutputFailure,
            Self::Cancelled => ExitCode::Cancelled,
            Self::Paused => ExitCode::Paused,
            Self::Internal(_) => ExitCode::InternalFailure,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::InvalidInput(message)
            | Self::UnsupportedContent(message)
            | Self::UnavailableTools(message)
            | Self::Analysis(message)
            | Self::Download(message)
            | Self::Output(message)
            | Self::Internal(message) => message,
            Self::Cancelled => "operation was cancelled",
            Self::Paused => "download was paused",
        }
    }
}

#[derive(Serialize)]
struct AnalyzeDocument<'a> {
    schema_version: u32,
    media: &'a MediaInfo,
}

#[derive(Serialize)]
struct DownloadEventDocument<'a> {
    schema_version: u32,
    #[serde(flatten)]
    event: &'a JobEvent,
}

#[derive(Serialize)]
struct DownloadResultDocument<'a> {
    schema_version: u32,
    event: &'static str,
    result: &'a yt_media_engine::download::DownloadResult,
}

#[derive(Serialize)]
struct DownloadErrorDocument<'a> {
    schema_version: u32,
    event: &'static str,
    job_id: &'a yt_media_engine::download::JobId,
    error: DownloadErrorBody<'a>,
}

#[derive(Serialize)]
struct DownloadErrorBody<'a> {
    code: i32,
    message: &'a str,
}

#[derive(Serialize)]
struct DownloadLagDocument<'a> {
    schema_version: u32,
    event: &'static str,
    job_id: &'a yt_media_engine::download::JobId,
    dropped_events: u64,
}

/// Parses process arguments, executes the requested command, and writes the stable terminal
/// contract to the real standard streams.
pub async fn run_environment() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let stdout = io::stdout();
    let stderr = io::stderr();
    run(arguments, &mut stdout.lock(), &mut stderr.lock()).await
}

async fn run(arguments: Vec<OsString>, stdout: &mut dyn Write, stderr: &mut dyn Write) -> ExitCode {
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) => return render_clap_error(&error, stdout, stderr),
    };
    let cancellation = CancellationToken::new();
    let result = {
        let operation = execute(cli.command, cancellation.clone(), stdout, stderr);
        tokio::pin!(operation);
        tokio::select! {
            result = &mut operation => result,
            signal = tokio::signal::ctrl_c() => {
                match signal {
                    Ok(()) => {
                        cancellation.cancel();
                        operation.await
                    }
                    Err(error) => Err(CommandFailure::Internal(format!(
                        "could not listen for Ctrl+C: {error}"
                    ))),
                }
            }
        }
    };
    match result {
        Ok(output) => match write_success(stdout, &output) {
            Ok(()) => {
                let _ignored = write_success_diagnostics(stderr, &output);
                ExitCode::Success
            }
            Err(error) => {
                let _ignored =
                    write_diagnostic(stderr, &format!("could not write command output: {error}"));
                ExitCode::InternalFailure
            }
        },
        Err(failure) => {
            let exit_code = failure.exit_code();
            let _ignored = write_diagnostic(stderr, failure.message());
            exit_code
        }
    }
}

fn render_clap_error(
    error: &clap::Error,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let rendered = error.to_string();
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        if stdout.write_all(rendered.as_bytes()).is_ok() {
            ExitCode::Success
        } else {
            ExitCode::InternalFailure
        }
    } else {
        let _ignored = stderr.write_all(rendered.as_bytes());
        ExitCode::InvalidInput
    }
}

enum SuccessOutput {
    Human {
        rendered: String,
        warnings: Vec<String>,
    },
    Json(Vec<u8>),
    Written,
}

async fn execute(
    command: Command,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<SuccessOutput, CommandFailure> {
    match command {
        Command::Analyze {
            url,
            json,
            tool_dir,
        } => execute_analyze(&url, json, tool_dir.as_deref(), cancellation).await,
        Command::Download {
            url,
            format,
            quality,
            output,
            name,
            json,
            tool_dir,
        } => {
            execute_download(
                DownloadCommandArgs {
                    url,
                    format,
                    quality,
                    output,
                    name,
                    json,
                    tool_dir,
                },
                cancellation,
                stdout,
                stderr,
            )
            .await?;
            Ok(SuccessOutput::Written)
        }
    }
}

async fn execute_analyze(
    raw_url: &str,
    json: bool,
    tool_directory: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<SuccessOutput, CommandFailure> {
    let media_url = MediaUrl::parse(raw_url).map_err(|error| {
        if error.is_unsupported_content() {
            CommandFailure::UnsupportedContent(error.to_string())
        } else {
            CommandFailure::InvalidInput(error.to_string())
        }
    })?;
    let tools = resolve_analysis_tools(tool_directory, cancellation.child_token()).await?;
    let analyzer = Analyzer::new(tools);
    let info = analyzer
        .analyze(&media_url, cancellation.clone())
        .await
        .map_err(|error| map_analyze_error(error, cancellation.is_cancelled()))?;

    if json {
        let document = AnalyzeDocument {
            schema_version: ANALYZE_SCHEMA_VERSION,
            media: &info,
        };
        let mut bytes = serde_json::to_vec(&document).map_err(|error| {
            CommandFailure::Internal(format!("could not serialize analysis JSON: {error}"))
        })?;
        bytes.push(b'\n');
        Ok(SuccessOutput::Json(bytes))
    } else {
        Ok(SuccessOutput::Human {
            rendered: render_human(&info),
            warnings: info.warnings,
        })
    }
}

async fn execute_download(
    arguments: DownloadCommandArgs,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CommandFailure> {
    let request = build_download_request(
        &arguments.url,
        arguments.format,
        arguments.quality,
        arguments.output,
        arguments.name,
    )?;
    let tools =
        resolve_download_tools(arguments.tool_dir.as_deref(), cancellation.child_token()).await?;
    let started = DownloadService::new(tools).start(request);
    drive_started_download(started, arguments.json, cancellation, stdout, stderr).await
}

fn build_download_request(
    raw_url: &str,
    format: CliOutputFormat,
    quality: u32,
    output: PathBuf,
    name: Option<String>,
) -> Result<DownloadRequest, CommandFailure> {
    let media_url = MediaUrl::parse(raw_url).map_err(|error| {
        if error.is_unsupported_content() {
            CommandFailure::UnsupportedContent(error.to_string())
        } else {
            CommandFailure::InvalidInput(error.to_string())
        }
    })?;
    let selection = match format {
        CliOutputFormat::Mp3 => {
            let bitrate = u16::try_from(quality).map_err(|_| {
                CommandFailure::InvalidInput(format!(
                    "invalid MP3 quality `{quality}`; expected 128, 192, 256, or 320"
                ))
            })?;
            OutputSelection::Mp3(
                AudioQuality::try_from(bitrate)
                    .map_err(|error| CommandFailure::InvalidInput(error.to_string()))?,
            )
        }
        CliOutputFormat::Mp4 => OutputSelection::Mp4(
            VideoQuality::try_from(quality)
                .map_err(|error| CommandFailure::InvalidInput(error.to_string()))?,
        ),
    };
    let destination = Destination::new(output)
        .map_err(|error| CommandFailure::InvalidInput(error.to_string()))?;
    let name = name
        .map(OutputName::new)
        .transpose()
        .map_err(|error| CommandFailure::InvalidInput(error.to_string()))?;
    Ok(DownloadRequest {
        url: media_url,
        output: selection,
        destination,
        name,
    })
}

async fn drive_started_download(
    started: yt_media_engine::download::StartedDownload,
    json: bool,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CommandFailure> {
    let job_id = started.job_id;
    let mut events = started.events;
    let controls = started.controls;
    let completion = started.completion.wait();
    tokio::pin!(completion);
    let mut cancellation_forwarded = false;

    let result = loop {
        tokio::select! {
            result = &mut completion => break result,
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        let rendered =
                            render_event_for_mode(json, stdout, stderr, &event);
                        if let Err(error) = rendered {
                            controls.cancel();
                            let _ignored = (&mut completion).await;
                            return Err(CommandFailure::Internal(format!(
                                "could not write download progress: {error}"
                            )));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        let rendered = if json {
                            write_json_line(
                                stdout,
                                &DownloadLagDocument {
                                    schema_version: DOWNLOAD_SCHEMA_VERSION,
                                    event: "stream-lagged",
                                    job_id: &job_id,
                                    dropped_events: count,
                                },
                            )
                        } else {
                            writeln!(
                                stderr,
                                "warning: progress renderer dropped {count} coalescible events"
                            )
                        };
                        if let Err(error) = rendered {
                            controls.cancel();
                            let _ignored = (&mut completion).await;
                            return Err(CommandFailure::Internal(format!(
                                "could not write progress lag notice: {error}"
                            )));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                }
            }
            () = cancellation.cancelled(), if !cancellation_forwarded => {
                cancellation_forwarded = true;
                controls.cancel();
            }
        }
    };

    while let Ok(event) = events.try_recv() {
        render_event_for_mode(json, stdout, stderr, &event).map_err(|error| {
            CommandFailure::Internal(format!("could not write final download event: {error}"))
        })?;
    }
    render_download_completion(result, &job_id, json, stdout)
}

fn render_event_for_mode(
    json: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    event: &JobEvent,
) -> io::Result<()> {
    if json {
        write_json_line(
            stdout,
            &DownloadEventDocument {
                schema_version: DOWNLOAD_SCHEMA_VERSION,
                event,
            },
        )
    } else {
        render_download_event(stderr, event)
    }
}

fn render_download_completion(
    result: Result<yt_media_engine::download::DownloadResult, DownloadError>,
    job_id: &yt_media_engine::download::JobId,
    json: bool,
    stdout: &mut dyn Write,
) -> Result<(), CommandFailure> {
    match result {
        Ok(result) => {
            if json {
                write_json_line(
                    stdout,
                    &DownloadResultDocument {
                        schema_version: DOWNLOAD_SCHEMA_VERSION,
                        event: "result",
                        result: &result,
                    },
                )
                .map_err(|error| {
                    CommandFailure::Internal(format!(
                        "could not write final download result: {error}"
                    ))
                })?;
            } else {
                writeln!(stdout, "{}", result.path.display()).map_err(|error| {
                    CommandFailure::Internal(format!("could not write final output path: {error}"))
                })?;
            }
            Ok(())
        }
        Err(error) => {
            let failure = map_download_error(error);
            if json {
                write_json_line(
                    stdout,
                    &DownloadErrorDocument {
                        schema_version: DOWNLOAD_SCHEMA_VERSION,
                        event: "result",
                        job_id,
                        error: DownloadErrorBody {
                            code: failure.exit_code().value(),
                            message: failure.message(),
                        },
                    },
                )
                .map_err(|write_error| {
                    CommandFailure::Internal(format!(
                        "could not write final download error: {write_error}"
                    ))
                })?;
            }
            Err(failure)
        }
    }
}

fn render_download_event(writer: &mut dyn Write, event: &JobEvent) -> io::Result<()> {
    match &event.kind {
        JobEventKind::Stage { stage } => writeln!(writer, "{}", render_stage(*stage)),
        JobEventKind::Progress { progress } => {
            if let Some(percent) = progress.percent {
                writeln!(writer, "{}: {percent:.1}%", render_stage(progress.stage))
            } else {
                writeln!(
                    writer,
                    "{}: {} bytes",
                    render_stage(progress.stage),
                    progress.completed
                )
            }
        }
        JobEventKind::Warning { message } => writeln!(writer, "warning: {message}"),
    }
}

fn render_stage(stage: JobStage) -> &'static str {
    match stage {
        JobStage::Analyzing => "analyzing",
        JobStage::Downloading => "downloading",
        JobStage::Merging => "merging",
        JobStage::Converting => "converting",
        JobStage::Finalizing => "finalizing",
        JobStage::Completed => "completed",
        JobStage::Paused => "paused",
        JobStage::Cancelled => "cancelled",
        JobStage::Failed => "failed",
    }
}

fn write_json_line(writer: &mut dyn Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn map_download_error(error: DownloadError) -> CommandFailure {
    match error {
        DownloadError::InvalidRequest(error) => CommandFailure::InvalidInput(error.to_string()),
        DownloadError::Analysis(error) => map_analyze_error(error, false),
        DownloadError::FormatUnavailable { .. } => CommandFailure::InvalidInput(error.to_string()),
        DownloadError::Destination { .. }
        | DownloadError::Filesystem { .. }
        | DownloadError::CollisionLimit => CommandFailure::Output(format_error_chain(&error)),
        DownloadError::Paused => CommandFailure::Paused,
        DownloadError::Cancelled => CommandFailure::Cancelled,
        DownloadError::Join(_) | DownloadError::CompletionClosed => {
            CommandFailure::Internal(format_error_chain(&error))
        }
        DownloadError::ProcessSpecification(_)
        | DownloadError::Process { .. }
        | DownloadError::NonZero { .. }
        | DownloadError::Protocol { .. }
        | DownloadError::Verification(_) => CommandFailure::Download(format_error_chain(&error)),
    }
}

async fn resolve_analysis_tools(
    tool_directory: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<AnalysisTools, CommandFailure> {
    let target =
        SupportedTarget::current().map_err(|error| CommandFailure::Internal(error.to_string()))?;
    let mut config = ToolResolutionConfig::default();
    if let Some(directory) = tool_directory {
        config.explicit_overrides = analysis_tool_paths(directory, target);
    } else {
        config.mode = ResolutionMode::Development;
        config.path_environment = std::env::var_os("PATH");
    }
    let resolver = ToolResolver::default();
    let yt_dlp = resolve_one(
        &resolver,
        Tool::YtDlp,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    let ffmpeg = resolve_one(
        &resolver,
        Tool::Ffmpeg,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    let deno = resolve_one(
        &resolver,
        Tool::Deno,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    AnalysisTools::from_resolved(yt_dlp, ffmpeg, deno)
        .map_err(|error| CommandFailure::Internal(error.to_string()))
}

async fn resolve_download_tools(
    tool_directory: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<DownloadTools, CommandFailure> {
    let target =
        SupportedTarget::current().map_err(|error| CommandFailure::Internal(error.to_string()))?;
    let mut config = ToolResolutionConfig::default();
    if let Some(directory) = tool_directory {
        config.explicit_overrides = Tool::ALL
            .into_iter()
            .map(|tool| (tool, directory.join(tool.executable_name(target))))
            .collect();
    } else {
        config.mode = ResolutionMode::Development;
        config.path_environment = std::env::var_os("PATH");
    }
    let resolver = ToolResolver::default();
    let yt_dlp = resolve_one(
        &resolver,
        Tool::YtDlp,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    let ffmpeg = resolve_one(
        &resolver,
        Tool::Ffmpeg,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    let ffprobe = resolve_one(
        &resolver,
        Tool::Ffprobe,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    let deno = resolve_one(
        &resolver,
        Tool::Deno,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    DownloadTools::from_resolved(yt_dlp, ffmpeg, ffprobe, deno)
        .map_err(|error| CommandFailure::Internal(error.to_string()))
}

async fn resolve_one(
    resolver: &ToolResolver,
    tool: Tool,
    target: SupportedTarget,
    config: &ToolResolutionConfig,
    cancellation: CancellationToken,
) -> Result<yt_media_engine::resolver::ResolvedTool, CommandFailure> {
    resolver
        .resolve(tool, target, config, cancellation.clone())
        .await
        .map_err(|error| map_tool_error(&error, cancellation.is_cancelled()))
}

fn analysis_tool_paths(directory: &Path, target: SupportedTarget) -> BTreeMap<Tool, PathBuf> {
    [Tool::YtDlp, Tool::Ffmpeg, Tool::Deno]
        .into_iter()
        .map(|tool| (tool, directory.join(tool.executable_name(target))))
        .collect()
}

fn map_tool_error(error: &ToolResolutionError, cancelled: bool) -> CommandFailure {
    if cancelled {
        CommandFailure::Cancelled
    } else {
        CommandFailure::UnavailableTools(format_error_chain(error))
    }
}

fn map_analyze_error(error: AnalyzeError, cancellation_requested: bool) -> CommandFailure {
    match error {
        AnalyzeError::Cancelled => CommandFailure::Cancelled,
        unsupported @ AnalyzeError::UnsupportedContent(_) => {
            CommandFailure::UnsupportedContent(unsupported.to_string())
        }
        _ if cancellation_requested => CommandFailure::Cancelled,
        other => CommandFailure::Analysis(format_error_chain(&other)),
    }
}

fn format_error_chain(error: &(dyn Error + 'static)) -> String {
    let mut messages = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        let message = error.to_string();
        if messages.last() != Some(&message) {
            messages.push(message);
        }
        source = error.source();
    }
    messages.join(": ")
}

fn render_human(info: &MediaInfo) -> String {
    let duration_millis = info.duration.as_millis();
    let minutes = duration_millis / 60_000;
    let seconds = (duration_millis % 60_000) / 1_000;
    let mut lines = vec![
        format!("Title: {}", info.title),
        format!("URL: {}", info.url),
        format!("Duration: {minutes}:{seconds:02}"),
    ];
    if let Some(uploader) = &info.uploader {
        lines.push(format!("Uploader: {uploader}"));
    }
    lines.push("Formats:".to_owned());
    for option in &info.formats {
        lines.push(render_format(option));
    }
    lines.join("\n") + "\n"
}

fn render_format(option: &FormatOption) -> String {
    match option {
        FormatOption::Mp3 {
            bitrate_kbps,
            source,
        } => format!(
            "  MP3 {bitrate_kbps} kbps (source {})",
            source.format_id.as_str()
        ),
        FormatOption::Mp4 {
            height,
            fps,
            estimated_size_bytes,
            video_source,
            audio_source,
            compatibility,
            ..
        } => {
            let fps = fps.map_or_else(|| "unknown fps".to_owned(), |value| format!("{value} fps"));
            let size = estimated_size_bytes.map_or_else(
                || "size unknown".to_owned(),
                |bytes| format!("{bytes} bytes"),
            );
            format!(
                "  MP4 {height}p, {fps}, {size} (video {}, audio {}, {})",
                video_source.format_id.as_str(),
                audio_source.format_id.as_str(),
                render_compatibility(*compatibility)
            )
        }
    }
}

fn render_compatibility(work: CompatibilityWork) -> &'static str {
    match work {
        CompatibilityWork::None => "no merge/transcode",
        CompatibilityWork::Merge => "merge",
        CompatibilityWork::VideoTranscode => "video transcode",
        CompatibilityWork::AudioTranscode => "audio transcode",
        CompatibilityWork::VideoAndAudioTranscode => "video and audio transcode",
    }
}

fn write_success(stdout: &mut dyn Write, output: &SuccessOutput) -> io::Result<()> {
    match output {
        SuccessOutput::Human { rendered, .. } => stdout.write_all(rendered.as_bytes()),
        SuccessOutput::Json(bytes) => stdout.write_all(bytes),
        SuccessOutput::Written => Ok(()),
    }
}

fn write_success_diagnostics(stderr: &mut dyn Write, output: &SuccessOutput) -> io::Result<()> {
    if let SuccessOutput::Human { warnings, .. } = output {
        for warning in warnings {
            writeln!(stderr, "warning: {warning}")?;
        }
    }
    Ok(())
}

fn write_diagnostic(stderr: &mut dyn Write, message: &str) -> io::Result<()> {
    writeln!(stderr, "error: {message}")
}

#[cfg(test)]
mod tests {
    use super::{ExitCode, render_clap_error};
    use clap::Parser;

    use super::Cli;

    #[test]
    fn stable_exit_code_values_do_not_drift() {
        assert_eq!(ExitCode::Success.value(), 0);
        assert_eq!(ExitCode::InvalidInput.value(), 2);
        assert_eq!(ExitCode::UnsupportedContent.value(), 3);
        assert_eq!(ExitCode::UnavailableTools.value(), 4);
        assert_eq!(ExitCode::AnalysisFailure.value(), 5);
        assert_eq!(ExitCode::Cancelled.value(), 6);
        assert_eq!(ExitCode::DownloadFailure.value(), 7);
        assert_eq!(ExitCode::OutputFailure.value(), 8);
        assert_eq!(ExitCode::Paused.value(), 9);
        assert_eq!(ExitCode::InternalFailure.value(), 70);
    }

    #[test]
    fn help_is_stdout_success() {
        let error = Cli::try_parse_from(["yt-media", "--help"]);
        assert!(error.is_err());
        let Some(error) = error.err() else {
            return;
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            render_clap_error(&error, &mut stdout, &mut stderr),
            ExitCode::Success
        );
        assert!(!stdout.is_empty());
        assert!(stderr.is_empty());
    }
}
