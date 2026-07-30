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
        AudioQuality, Destination, DownloadError, DownloadRequest, DownloadResult, DownloadService,
        DownloadTools, JobEvent, JobEventKind, JobId, JobStage, OutputName, OutputSelection,
        VideoQuality,
    },
    jobs::{
        EngineSettings, JobErrorClass, JobQueue, JobRecord, JobState, QueueConcurrency, QueueError,
        SettingsPatch, UpdatePreference,
    },
    resolver::{
        ResolutionMode, ToolResolutionConfig, ToolResolutionError, ToolResolver, VerifiedToolSet,
    },
    target::SupportedTarget,
    tool::Tool,
};

const ANALYZE_SCHEMA_VERSION: u32 = 1;
const DOWNLOAD_SCHEMA_VERSION: u32 = 1;
const JOBS_SCHEMA_VERSION: u32 = 1;

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
    /// Durable queue, migration, locking, or recovery failed.
    PersistenceFailure = 10,
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
    /// Override the platform application-data directory for isolation and automation.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,
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
    /// Inspect and control durable jobs.
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
    /// Inspect or remove durable terminal history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Inspect or update persisted engine settings.
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum JobsCommand {
    /// List all durable jobs.
    List {
        /// Emit one stable JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Inspect one durable job.
    Get {
        /// `UUIDv7` job identity.
        id: String,
        /// Emit one stable JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Pause queued or active work after process cleanup.
    Pause {
        /// `UUIDv7` job identity.
        id: String,
        /// Emit one stable JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Cancel work and remove only recorded engine-owned paths.
    Cancel {
        /// `UUIDv7` job identity.
        id: String,
        /// Emit one stable JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Explicitly resume a paused or interrupted job and wait for it.
    Resume {
        /// `UUIDv7` job identity.
        id: String,
        /// Emit schema-versioned progress and result NDJSON.
        #[arg(long)]
        json: bool,
        /// Directory containing exact yt-dlp, `FFmpeg`, `FFprobe`, and Deno executables.
        #[arg(long, value_name = "PATH")]
        tool_dir: Option<PathBuf>,
    },
    /// Explicitly append a failed or cancelled job to retry order and wait for it.
    Retry {
        /// `UUIDv7` job identity.
        id: String,
        /// Emit schema-versioned progress and result NDJSON.
        #[arg(long)]
        json: bool,
        /// Directory containing exact yt-dlp, `FFmpeg`, `FFprobe`, and Deno executables.
        #[arg(long, value_name = "PATH")]
        tool_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum HistoryCommand {
    /// List durable terminal history.
    List {
        /// Emit one stable JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Remove one completed history record without deleting its output.
    Remove {
        /// `UUIDv7` completed job identity.
        id: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliUpdatePreference {
    Notify,
    Automatic,
    Disabled,
}

#[derive(Debug, Subcommand)]
enum SettingsCommand {
    /// Show persisted engine settings.
    Show {
        /// Emit one stable JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Update one or more persisted engine settings.
    Set {
        /// Set the default destination.
        #[arg(long, value_name = "DIR", conflicts_with = "clear_default_destination")]
        default_destination: Option<PathBuf>,
        /// Clear the persisted default destination.
        #[arg(long)]
        clear_default_destination: bool,
        /// Shared download and post-processing concurrency from one through four.
        #[arg(long)]
        concurrency: Option<u8>,
        /// Managed tool update behavior.
        #[arg(long, value_enum)]
        update_preference: Option<CliUpdatePreference>,
        /// Last selected output container.
        #[arg(long, value_enum, requires = "quality")]
        format: Option<CliOutputFormat>,
        /// Last selected MP3 bitrate or MP4 height.
        #[arg(long, requires = "format")]
        quality: Option<u32>,
        /// Emit one stable JSON document.
        #[arg(long)]
        json: bool,
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

struct ExistingJobCommandArgs<'a> {
    raw_id: &'a str,
    resume: bool,
    json: bool,
    tool_directory: Option<&'a Path>,
    data_directory: Option<&'a Path>,
    cancellation: CancellationToken,
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
    Queue(String),
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
            Self::Queue(_) => ExitCode::PersistenceFailure,
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
            | Self::Queue(message)
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

#[derive(Serialize)]
struct JobsDocument<'a> {
    schema_version: u32,
    jobs: &'a [JobRecord],
}

#[derive(Serialize)]
struct JobDocument<'a> {
    schema_version: u32,
    job: &'a JobRecord,
}

#[derive(Serialize)]
struct SettingsDocument<'a> {
    schema_version: u32,
    settings: &'a EngineSettings,
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
        let operation = execute(
            cli.command,
            cli.data_dir.as_deref(),
            cancellation.clone(),
            stdout,
            stderr,
        );
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
    data_directory: Option<&Path>,
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
                data_directory,
                cancellation,
                stdout,
                stderr,
            )
            .await?;
            Ok(SuccessOutput::Written)
        }
        Command::Jobs { command } => {
            execute_jobs(command, data_directory, cancellation, stdout, stderr).await
        }
        Command::History { command } => execute_history(command, data_directory).await,
        Command::Settings { command } => execute_settings(command, data_directory).await,
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
    data_directory: Option<&Path>,
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
    let queue = open_queue(data_directory, Some(DownloadService::new(tools))).await?;
    let mut subscription = queue.subscribe();
    let record = queue.enqueue(request).await.map_err(map_queue_error)?;
    drive_queued_job(
        &queue,
        &mut subscription,
        &record.id,
        arguments.json,
        cancellation,
        stdout,
        stderr,
    )
    .await
}

async fn execute_jobs(
    command: JobsCommand,
    data_directory: Option<&Path>,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<SuccessOutput, CommandFailure> {
    match command {
        JobsCommand::List { json } => {
            let queue = open_queue(data_directory, None).await?;
            let jobs = queue.list().await.map_err(map_queue_error)?;
            render_jobs_output(&jobs, json)
        }
        JobsCommand::Get { id, json } => {
            let id = parse_job_id(&id)?;
            let queue = open_queue(data_directory, None).await?;
            let job = queue.get(&id).await.map_err(map_queue_error)?;
            render_job_output(&job, json)
        }
        JobsCommand::Pause { id, json } => {
            let id = parse_job_id(&id)?;
            let queue = open_queue(data_directory, None).await?;
            let job = queue.pause(&id).await.map_err(map_queue_error)?;
            render_job_output(&job, json)
        }
        JobsCommand::Cancel { id, json } => {
            let id = parse_job_id(&id)?;
            let queue = open_queue(data_directory, None).await?;
            let job = queue.cancel(&id).await.map_err(map_queue_error)?;
            render_job_output(&job, json)
        }
        JobsCommand::Resume { id, json, tool_dir } => {
            execute_existing_job(
                ExistingJobCommandArgs {
                    raw_id: &id,
                    resume: true,
                    json,
                    tool_directory: tool_dir.as_deref(),
                    data_directory,
                    cancellation,
                },
                stdout,
                stderr,
            )
            .await?;
            Ok(SuccessOutput::Written)
        }
        JobsCommand::Retry { id, json, tool_dir } => {
            execute_existing_job(
                ExistingJobCommandArgs {
                    raw_id: &id,
                    resume: false,
                    json,
                    tool_directory: tool_dir.as_deref(),
                    data_directory,
                    cancellation,
                },
                stdout,
                stderr,
            )
            .await?;
            Ok(SuccessOutput::Written)
        }
    }
}

async fn execute_existing_job(
    arguments: ExistingJobCommandArgs<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CommandFailure> {
    let id = parse_job_id(arguments.raw_id)?;
    let tools = resolve_download_tools(
        arguments.tool_directory,
        arguments.cancellation.child_token(),
    )
    .await?;
    let queue = open_queue(arguments.data_directory, Some(DownloadService::new(tools))).await?;
    let mut subscription = queue.subscribe();
    if arguments.resume {
        queue.resume(&id).await.map_err(map_queue_error)?;
    } else {
        queue.retry(&id).await.map_err(map_queue_error)?;
    }
    drive_queued_job(
        &queue,
        &mut subscription,
        &id,
        arguments.json,
        arguments.cancellation,
        stdout,
        stderr,
    )
    .await
}

async fn execute_history(
    command: HistoryCommand,
    data_directory: Option<&Path>,
) -> Result<SuccessOutput, CommandFailure> {
    let queue = open_queue(data_directory, None).await?;
    match command {
        HistoryCommand::List { json } => {
            let jobs = queue.history().await.map_err(map_queue_error)?;
            render_jobs_output(&jobs, json)
        }
        HistoryCommand::Remove { id } => {
            let id = parse_job_id(&id)?;
            queue.remove_completed(&id).await.map_err(map_queue_error)?;
            Ok(SuccessOutput::Human {
                rendered: format!("removed completed history {id}\n"),
                warnings: Vec::new(),
            })
        }
    }
}

async fn execute_settings(
    command: SettingsCommand,
    data_directory: Option<&Path>,
) -> Result<SuccessOutput, CommandFailure> {
    let queue = open_queue(data_directory, None).await?;
    match command {
        SettingsCommand::Show { json } => {
            let settings = queue.settings().await.map_err(map_queue_error)?;
            render_settings_output(&settings, json)
        }
        SettingsCommand::Set {
            default_destination,
            clear_default_destination,
            concurrency,
            update_preference,
            format,
            quality,
            json,
        } => {
            let queue_concurrency = concurrency
                .map(QueueConcurrency::try_from)
                .transpose()
                .map_err(|error| CommandFailure::InvalidInput(error.to_string()))?;
            let update_preference = update_preference.map(|preference| match preference {
                CliUpdatePreference::Notify => UpdatePreference::Notify,
                CliUpdatePreference::Automatic => UpdatePreference::Automatic,
                CliUpdatePreference::Disabled => UpdatePreference::Disabled,
            });
            let last_output = format
                .zip(quality)
                .map(|(format, quality)| build_output_selection(format, quality))
                .transpose()?;
            let default_destination = if clear_default_destination {
                Some(None)
            } else {
                default_destination.map(Some)
            };
            let settings = queue
                .update_settings(SettingsPatch {
                    default_destination,
                    queue_concurrency,
                    update_preference,
                    last_output,
                })
                .await
                .map_err(map_queue_error)?;
            render_settings_output(&settings, json)
        }
    }
}

async fn open_queue(
    data_directory: Option<&Path>,
    service: Option<DownloadService>,
) -> Result<JobQueue, CommandFailure> {
    let directory = match data_directory {
        Some(path) => path.to_path_buf(),
        None => JobQueue::platform_data_directory().map_err(map_queue_error)?,
    };
    match service {
        Some(service) => JobQueue::open_with_download_service(directory, service)
            .await
            .map_err(map_queue_error),
        None => JobQueue::open(directory).await.map_err(map_queue_error),
    }
}

fn parse_job_id(value: &str) -> Result<JobId, CommandFailure> {
    JobId::parse(value).map_err(|error| CommandFailure::InvalidInput(error.to_string()))
}

fn map_queue_error(error: QueueError) -> CommandFailure {
    match error {
        QueueError::InvalidRequest(message) => CommandFailure::InvalidInput(message),
        QueueError::Destination(message) => CommandFailure::Output(message),
        QueueError::Ownership(source) => map_download_error(source),
        other => CommandFailure::Queue(other.to_string()),
    }
}

fn render_jobs_output(jobs: &[JobRecord], json: bool) -> Result<SuccessOutput, CommandFailure> {
    if json {
        serialize_json_document(&JobsDocument {
            schema_version: JOBS_SCHEMA_VERSION,
            jobs,
        })
    } else {
        let rendered = if jobs.is_empty() {
            "no jobs\n".to_owned()
        } else {
            jobs.iter()
                .map(render_job_summary)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        };
        Ok(SuccessOutput::Human {
            rendered,
            warnings: Vec::new(),
        })
    }
}

fn render_job_output(job: &JobRecord, json: bool) -> Result<SuccessOutput, CommandFailure> {
    if json {
        serialize_json_document(&JobDocument {
            schema_version: JOBS_SCHEMA_VERSION,
            job,
        })
    } else {
        Ok(SuccessOutput::Human {
            rendered: format!("{}\n", render_job_summary(job)),
            warnings: Vec::new(),
        })
    }
}

fn render_job_summary(job: &JobRecord) -> String {
    format!(
        "{}\t{}\tattempt {}\t{}",
        job.id, job.state, job.attempt_count, job.request.canonical_url
    )
}

fn render_settings_output(
    settings: &EngineSettings,
    json: bool,
) -> Result<SuccessOutput, CommandFailure> {
    if json {
        serialize_json_document(&SettingsDocument {
            schema_version: JOBS_SCHEMA_VERSION,
            settings,
        })
    } else {
        Ok(SuccessOutput::Human {
            rendered: format!(
                "default destination: {}\nqueue concurrency: {}\nupdate preference: {:?}\nlast output: {:?}\n",
                settings
                    .default_destination
                    .as_ref()
                    .map_or_else(|| "(none)".to_owned(), |path| path.display().to_string()),
                settings.queue_concurrency.get(),
                settings.update_preference,
                settings.last_output,
            ),
            warnings: Vec::new(),
        })
    }
}

fn serialize_json_document(document: &impl Serialize) -> Result<SuccessOutput, CommandFailure> {
    let mut bytes = serde_json::to_vec(document).map_err(|error| {
        CommandFailure::Internal(format!("could not serialize command JSON: {error}"))
    })?;
    bytes.push(b'\n');
    Ok(SuccessOutput::Json(bytes))
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
    let selection = build_output_selection(format, quality)?;
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

fn build_output_selection(
    format: CliOutputFormat,
    quality: u32,
) -> Result<OutputSelection, CommandFailure> {
    match format {
        CliOutputFormat::Mp3 => {
            let bitrate = u16::try_from(quality).map_err(|_| {
                CommandFailure::InvalidInput(format!(
                    "invalid MP3 quality `{quality}`; expected 128, 192, 256, or 320"
                ))
            })?;
            Ok(OutputSelection::Mp3(
                AudioQuality::try_from(bitrate)
                    .map_err(|error| CommandFailure::InvalidInput(error.to_string()))?,
            ))
        }
        CliOutputFormat::Mp4 => Ok(OutputSelection::Mp4(
            VideoQuality::try_from(quality)
                .map_err(|error| CommandFailure::InvalidInput(error.to_string()))?,
        )),
    }
}

async fn drive_queued_job(
    queue: &JobQueue,
    events: &mut yt_media_engine::jobs::QueueSubscription,
    job_id: &JobId,
    json: bool,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CommandFailure> {
    let mut cancellation_forwarded = false;

    let final_record = loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) if event.job.id == *job_id => {
                        if let Some(kind) = event.activity {
                            let rendered = render_event_for_mode(
                                json,
                                stdout,
                                stderr,
                                &JobEvent {
                                    job_id: job_id.clone(),
                                    sequence: event.sequence,
                                    kind,
                                },
                            );
                            if let Err(error) = rendered {
                                let _ignored = queue.cancel(job_id).await;
                                return Err(CommandFailure::Internal(format!(
                                    "could not write download progress: {error}"
                                )));
                            }
                        }
                        if event.job.state.is_terminal()
                            || matches!(event.job.state, JobState::Paused | JobState::Interrupted)
                        {
                            break event.job;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        let rendered = if json {
                            write_json_line(
                                stdout,
                                &DownloadLagDocument {
                                    schema_version: DOWNLOAD_SCHEMA_VERSION,
                                    event: "stream-lagged",
                                    job_id,
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
                            let _ignored = queue.cancel(job_id).await;
                            return Err(CommandFailure::Internal(format!(
                                "could not write progress lag notice: {error}"
                            )));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(CommandFailure::Internal(
                            "queue event stream closed before job completion".to_owned(),
                        ));
                    }
                }
            }
            () = cancellation.cancelled(), if !cancellation_forwarded => {
                cancellation_forwarded = true;
                let record = queue.cancel(job_id).await.map_err(map_queue_error)?;
                if record.state.is_terminal() {
                    break record;
                }
            }
        }
    };
    render_job_completion(final_record, json, stdout)
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

fn render_job_completion(
    record: JobRecord,
    json: bool,
    stdout: &mut dyn Write,
) -> Result<(), CommandFailure> {
    if record.state == JobState::Completed {
        let output = record.final_output.ok_or_else(|| {
            CommandFailure::Internal("completed job had no final output metadata".to_owned())
        })?;
        let result = DownloadResult {
            job_id: record.id,
            path: output.path,
            size_bytes: output.size_bytes,
            output: output.output,
        };
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
                CommandFailure::Internal(format!("could not write final download result: {error}"))
            })?;
        } else {
            writeln!(stdout, "{}", result.path.display()).map_err(|error| {
                CommandFailure::Internal(format!("could not write final output path: {error}"))
            })?;
        }
        return Ok(());
    }
    let failure = match record.state {
        JobState::Cancelled => CommandFailure::Cancelled,
        JobState::Paused | JobState::Interrupted => CommandFailure::Paused,
        JobState::Failed => {
            let message = record.error.as_ref().map_or_else(
                || "job failed without a diagnostic".to_owned(),
                |error| error.message.clone(),
            );
            match record.error.as_ref().map(|error| error.class) {
                Some(JobErrorClass::InvalidRequest) => CommandFailure::InvalidInput(message),
                Some(JobErrorClass::DestinationUnavailable | JobErrorClass::Filesystem) => {
                    CommandFailure::Output(message)
                }
                Some(JobErrorClass::Internal) => CommandFailure::Queue(message),
                _ => CommandFailure::Download(message),
            }
        }
        state => CommandFailure::Internal(format!(
            "job ended output processing in unexpected `{state}` state"
        )),
    };
    if json {
        write_json_line(
            stdout,
            &DownloadErrorDocument {
                schema_version: DOWNLOAD_SCHEMA_VERSION,
                event: "result",
                job_id: &record.id,
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
        config.bundled_baseline = bundled_cli_tools(target, cancellation.child_token()).await?;
        if config.bundled_baseline.is_none() {
            config.mode = ResolutionMode::Development;
            config.path_environment = std::env::var_os("PATH");
        }
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
        config.bundled_baseline = bundled_cli_tools(target, cancellation.child_token()).await?;
        if config.bundled_baseline.is_none() {
            config.mode = ResolutionMode::Development;
            config.path_environment = std::env::var_os("PATH");
        }
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

async fn bundled_cli_tools(
    target: SupportedTarget,
    cancellation: CancellationToken,
) -> Result<Option<VerifiedToolSet>, CommandFailure> {
    let executable = std::env::current_exe()
        .map_err(|error| CommandFailure::Internal(format!("could not locate CLI: {error}")))?;
    let parent = executable.parent().ok_or_else(|| {
        CommandFailure::Internal("the CLI executable path has no parent directory".to_owned())
    })?;
    let directory = parent.join("sidecars");
    if !directory.is_dir() {
        return Ok(None);
    }
    VerifiedToolSet::verify_staged(
        target,
        directory,
        std::sync::Arc::new(yt_media_engine::process::TokioProcessRunner),
        cancellation,
    )
    .await
    .map(Some)
    .map_err(|error| {
        CommandFailure::UnavailableTools(format!(
            "the bundled CLI tool set failed verification: {error}"
        ))
    })
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
        assert_eq!(ExitCode::PersistenceFailure.value(), 10);
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
