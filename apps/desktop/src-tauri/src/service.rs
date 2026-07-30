//! One managed desktop application service over the engine.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Mutex, OnceCell};
use yt_media_engine::{
    analysis::{AnalysisTools, AnalyzeError, Analyzer, MediaUrl},
    cancellation::CancellationToken,
    download::{
        AudioQuality, Destination, DownloadRequest, DownloadService, DownloadTools, JobId,
        OutputName, OutputSelection, VideoQuality,
    },
    jobs::{
        EngineSettings, JobQueue, QueueConcurrency, QueueError, SettingsPatch, UpdatePreference,
    },
    process::{ProcessRunner, TokioProcessRunner},
    resolver::{ResolutionMode, ResolvedTool, ToolResolutionConfig, ToolResolver, VerifiedToolSet},
    target::SupportedTarget,
    tool::Tool,
};

use crate::ipc::{
    AnalyzeRequestDto, AnalyzeResponseDto, BootstrapHealthDto, BootstrapStateDto,
    DefaultDestinationUpdateDto, EnqueueRequestDto, IPC_SCHEMA_VERSION, IpcErrorCodeDto,
    IpcErrorDto, JobDto, JobEventEnvelopeDto, JobIdRequestDto, OutputSelectionDto, SettingsDto,
    ToolStatusDto, UpdatePreferenceDto, UpdateSettingsRequestDto,
};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Receives safe versioned job events from the application service.
pub(crate) trait JobEventSink: Send + Sync + 'static {
    /// Delivers one event without blocking queue progress.
    fn emit(&self, event: JobEventEnvelopeDto);
}

/// Runtime paths and resolution inputs supplied by the native shell.
#[derive(Clone)]
pub(crate) struct ServiceConfig {
    /// Private application data directory.
    pub(crate) data_directory: PathBuf,
    /// Native operating-system Downloads directory used when no override is persisted.
    pub(crate) system_downloads_directory: Option<PathBuf>,
    /// Directory containing a verified managed tool update, when installed.
    pub(crate) managed_tools_directory: Option<PathBuf>,
    /// Directory containing the bundled baseline sidecars.
    pub(crate) bundled_tools_directory: Option<PathBuf>,
    /// Production or development-only system tool behavior.
    pub(crate) resolution_mode: ResolutionMode,
    /// Explicit system search path used only in development mode.
    pub(crate) path_environment: Option<OsString>,
    /// Exact developer overrides.
    pub(crate) explicit_overrides: BTreeMap<Tool, PathBuf>,
    /// Testable process owner used by resolution, analysis, and downloads.
    pub(crate) runner: Arc<dyn ProcessRunner>,
}

impl std::fmt::Debug for ServiceConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceConfig")
            .field("data_directory", &self.data_directory)
            .field(
                "has_system_downloads_directory",
                &self.system_downloads_directory.is_some(),
            )
            .field("resolution_mode", &self.resolution_mode)
            .field("has_managed_tools", &self.managed_tools_directory.is_some())
            .field("has_bundled_tools", &self.bundled_tools_directory.is_some())
            .field("override_count", &self.explicit_overrides.len())
            .finish_non_exhaustive()
    }
}

impl ServiceConfig {
    /// Creates production defaults for native application paths.
    #[must_use]
    pub(crate) fn production(
        data_directory: PathBuf,
        resource_directory: &Path,
        system_downloads_directory: Option<PathBuf>,
    ) -> Self {
        let target = SupportedTarget::current().ok();
        let managed_tools_directory = target.map(|target| {
            data_directory
                .join("tools")
                .join("active")
                .join(target.triple())
        });
        Self {
            data_directory,
            system_downloads_directory,
            managed_tools_directory,
            bundled_tools_directory: Some(resource_directory.join("sidecars")),
            resolution_mode: if cfg!(debug_assertions) {
                ResolutionMode::Development
            } else {
                ResolutionMode::Production
            },
            path_environment: if cfg!(debug_assertions) {
                std::env::var_os("PATH")
            } else {
                None
            },
            explicit_overrides: BTreeMap::new(),
            runner: Arc::new(TokioProcessRunner),
        }
    }
}

struct InitializedService {
    queue: Option<JobQueue>,
    analyzer: Option<Analyzer>,
    tools: Vec<ToolStatusDto>,
    diagnostic: Option<IpcErrorDto>,
}

#[derive(Clone, Debug)]
struct AnalysisOperation {
    id: u64,
    cancellation: CancellationToken,
}

/// Cloneable single owner for desktop persistence, tools, queue, events, and shutdown.
pub(crate) struct ApplicationService {
    config: ServiceConfig,
    initialized: OnceCell<InitializedService>,
    sink: Arc<dyn JobEventSink>,
    closing: AtomicBool,
    next_analysis_id: AtomicU64,
    active_analysis: Mutex<Option<AnalysisOperation>>,
}

impl std::fmt::Debug for ApplicationService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplicationService")
            .field("config", &self.config)
            .field("initialized", &self.initialized.initialized())
            .field("closing", &self.closing.load(Ordering::Acquire))
            .field(
                "has_active_analysis",
                &self
                    .active_analysis
                    .try_lock()
                    .is_ok_and(|active| active.is_some()),
            )
            .finish_non_exhaustive()
    }
}

impl ApplicationService {
    /// Creates one lazily initialized managed service.
    #[must_use]
    pub(crate) fn new(config: ServiceConfig, sink: Arc<dyn JobEventSink>) -> Self {
        Self {
            config,
            initialized: OnceCell::new(),
            sink,
            closing: AtomicBool::new(false),
            next_analysis_id: AtomicU64::new(0),
            active_analysis: Mutex::new(None),
        }
    }

    async fn initialized(&self) -> &InitializedService {
        self.initialized
            .get_or_init(|| async { self.initialize().await })
            .await
    }

    async fn initialize(&self) -> InitializedService {
        let resolved = resolve_tools(&self.config).await;
        let (analyzer, download_service, tools, tool_diagnostic) = match resolved {
            Ok(resolved) => {
                let tools = resolved
                    .iter()
                    .map(|tool| ToolStatusDto {
                        tool: tool.tool.into(),
                        ready: true,
                        source: Some(tool.source.into()),
                        message: None,
                    })
                    .collect();
                match compose_engine_services(&resolved, Arc::clone(&self.config.runner)) {
                    Ok((analyzer, download_service)) => {
                        (Some(analyzer), Some(download_service), tools, None)
                    }
                    Err(()) => (
                        None,
                        None,
                        unavailable_tool_statuses(),
                        Some(tools_unavailable_error()),
                    ),
                }
            }
            Err(()) => (
                None,
                None,
                unavailable_tool_statuses(),
                Some(tools_unavailable_error()),
            ),
        };

        let queue_result = if let Some(download_service) = download_service {
            JobQueue::open_with_download_service(&self.config.data_directory, download_service)
                .await
        } else {
            JobQueue::open(&self.config.data_directory).await
        };
        match queue_result {
            Ok(queue) => {
                self.start_event_forwarder(&queue);
                InitializedService {
                    queue: Some(queue),
                    analyzer,
                    tools,
                    diagnostic: tool_diagnostic,
                }
            }
            Err(error) => {
                eprintln!("desktop persistence initialization failed: {error:#}");
                InitializedService {
                    queue: None,
                    analyzer: None,
                    tools,
                    diagnostic: Some(IpcErrorDto::new(
                        IpcErrorCodeDto::PersistenceUnavailable,
                        "YT Media could not open its local job database. No work was started; retry after checking application data access.",
                    )),
                }
            }
        }
    }

    fn start_event_forwarder(&self, queue: &JobQueue) {
        let mut subscription = queue.subscribe();
        let sink = Arc::clone(&self.sink);
        tauri::async_runtime::spawn(async move {
            loop {
                match subscription.recv().await {
                    Ok(event) => sink.emit(JobEventEnvelopeDto::from(&event)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Reconnect snapshots are authoritative; a slow consumer never blocks the
                        // queue and is expected to resynchronize.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Returns the authoritative recovered bootstrap snapshot.
    pub(crate) async fn bootstrap(&self) -> BootstrapStateDto {
        let initialized = self.initialized().await;
        let Some(queue) = &initialized.queue else {
            return BootstrapStateDto {
                schema_version: IPC_SCHEMA_VERSION,
                health: BootstrapHealthDto::Failed,
                last_event_sequence: "0".to_owned(),
                jobs: Vec::new(),
                settings: None,
                tools: initialized.tools.clone(),
                diagnostic: initialized.diagnostic.clone(),
            };
        };
        let snapshot = queue.snapshot().await;
        let settings = queue.settings().await;
        match (snapshot, settings) {
            (Ok(snapshot), Ok(settings)) => BootstrapStateDto {
                schema_version: IPC_SCHEMA_VERSION,
                health: if initialized.analyzer.is_some() {
                    BootstrapHealthDto::Healthy
                } else {
                    BootstrapHealthDto::Degraded
                },
                last_event_sequence: snapshot.last_event_sequence.to_string(),
                jobs: snapshot.jobs.iter().map(JobDto::from).collect(),
                settings: Some(self.settings_dto(&settings)),
                tools: initialized.tools.clone(),
                diagnostic: initialized.diagnostic.clone(),
            },
            (snapshot, settings) => {
                if let Err(error) = snapshot {
                    eprintln!("desktop bootstrap snapshot failed: {error:#}");
                }
                if let Err(error) = settings {
                    eprintln!("desktop bootstrap settings failed: {error:#}");
                }
                BootstrapStateDto {
                    schema_version: IPC_SCHEMA_VERSION,
                    health: BootstrapHealthDto::Failed,
                    last_event_sequence: "0".to_owned(),
                    jobs: Vec::new(),
                    settings: None,
                    tools: initialized.tools.clone(),
                    diagnostic: Some(persistence_error()),
                }
            }
        }
    }

    /// Analyzes one URL through verified engine tools.
    pub(crate) async fn analyze(
        &self,
        request: AnalyzeRequestDto,
    ) -> Result<AnalyzeResponseDto, IpcErrorDto> {
        self.ensure_open()?;
        let url = MediaUrl::parse(&request.url).map_err(|_| {
            IpcErrorDto::new(
                IpcErrorCodeDto::InvalidRequest,
                "Enter a valid public YouTube video URL.",
            )
        })?;
        let operation = self.start_analysis().await;
        let result = async {
            let initialized = self.initialized().await;
            let analyzer = initialized
                .analyzer
                .as_ref()
                .ok_or_else(tools_unavailable_error)?;
            analyzer
                .analyze(&url, operation.cancellation.clone())
                .await
                .map(|media| AnalyzeResponseDto {
                    schema_version: IPC_SCHEMA_VERSION,
                    media: (&media).into(),
                })
                .map_err(|error| map_analysis_error(&error))
        }
        .await;
        self.finish_analysis(operation.id).await;
        result
    }

    /// Cancels the active URL analysis, when one exists.
    pub(crate) async fn cancel_analysis(&self) {
        if let Some(operation) = self.active_analysis.lock().await.take() {
            operation.cancellation.cancel();
        }
    }

    /// Persists and explicitly starts one normalized engine job.
    pub(crate) async fn enqueue(&self, request: EnqueueRequestDto) -> Result<JobDto, IpcErrorDto> {
        self.ensure_open()?;
        let request = download_request(request)?;
        let queue = self.queue().await?;
        queue
            .enqueue(request)
            .await
            .map(|record| JobDto::from(&record))
            .map_err(queue_error)
    }

    /// Lists all durable jobs.
    pub(crate) async fn list_jobs(&self) -> Result<Vec<JobDto>, IpcErrorDto> {
        let queue = self.queue().await?;
        queue
            .list()
            .await
            .map(|jobs| jobs.iter().map(JobDto::from).collect())
            .map_err(queue_error)
    }

    /// Reads one job.
    pub(crate) async fn get_job(&self, request: JobIdRequestDto) -> Result<JobDto, IpcErrorDto> {
        let id = job_id(&request)?;
        let queue = self.queue().await?;
        queue
            .get(&id)
            .await
            .map(|record| JobDto::from(&record))
            .map_err(queue_error)
    }

    /// Pauses one queued or active job.
    pub(crate) async fn pause_job(&self, request: JobIdRequestDto) -> Result<JobDto, IpcErrorDto> {
        let id = job_id(&request)?;
        self.queue()
            .await?
            .pause(&id)
            .await
            .map(|record| JobDto::from(&record))
            .map_err(queue_error)
    }

    /// Explicitly resumes one paused or interrupted job.
    pub(crate) async fn resume_job(&self, request: JobIdRequestDto) -> Result<JobDto, IpcErrorDto> {
        self.ensure_open()?;
        let id = job_id(&request)?;
        self.queue()
            .await?
            .resume(&id)
            .await
            .map(|record| JobDto::from(&record))
            .map_err(queue_error)
    }

    /// Cancels one queued, retained, or active job.
    pub(crate) async fn cancel_job(&self, request: JobIdRequestDto) -> Result<JobDto, IpcErrorDto> {
        let id = job_id(&request)?;
        self.queue()
            .await?
            .cancel(&id)
            .await
            .map(|record| JobDto::from(&record))
            .map_err(queue_error)
    }

    /// Explicitly retries one failed or cancelled job.
    pub(crate) async fn retry_job(&self, request: JobIdRequestDto) -> Result<JobDto, IpcErrorDto> {
        self.ensure_open()?;
        let id = job_id(&request)?;
        self.queue()
            .await?
            .retry(&id)
            .await
            .map(|record| JobDto::from(&record))
            .map_err(queue_error)
    }

    /// Lists terminal history newest first.
    pub(crate) async fn list_history(&self) -> Result<Vec<JobDto>, IpcErrorDto> {
        let queue = self.queue().await?;
        queue
            .history()
            .await
            .map(|jobs| jobs.iter().map(JobDto::from).collect())
            .map_err(queue_error)
    }

    /// Deletes one completed history record without touching output.
    pub(crate) async fn delete_history(&self, request: JobIdRequestDto) -> Result<(), IpcErrorDto> {
        let id = job_id(&request)?;
        self.queue()
            .await?
            .remove_completed(&id)
            .await
            .map_err(queue_error)
    }

    /// Reads persisted engine settings.
    pub(crate) async fn read_settings(&self) -> Result<SettingsDto, IpcErrorDto> {
        self.queue()
            .await?
            .settings()
            .await
            .map(|settings| self.settings_dto(&settings))
            .map_err(queue_error)
    }

    /// Validates and persists an engine settings patch.
    pub(crate) async fn update_settings(
        &self,
        request: UpdateSettingsRequestDto,
    ) -> Result<SettingsDto, IpcErrorDto> {
        self.ensure_open()?;
        let patch = settings_patch(request)?;
        self.queue()
            .await?
            .update_settings(patch)
            .await
            .map(|settings| self.settings_dto(&settings))
            .map_err(queue_error)
    }

    /// Returns safe current tool status without rerunning probes.
    pub(crate) async fn tool_status(&self) -> Vec<ToolStatusDto> {
        self.initialized().await.tools.clone()
    }

    /// Validates that a job owns a present completed output and returns its user-facing path.
    pub(crate) async fn revealable_output(
        &self,
        request: JobIdRequestDto,
    ) -> Result<PathBuf, IpcErrorDto> {
        let id = job_id(&request)?;
        let record = self.queue().await?.get(&id).await.map_err(queue_error)?;
        let Some(output) = record.final_output else {
            return Err(IpcErrorDto::new(
                IpcErrorCodeDto::RevealFailed,
                "This job does not have a completed output to reveal.",
            ));
        };
        if !output.path.is_file() {
            return Err(IpcErrorDto::new(
                IpcErrorCodeDto::RevealFailed,
                "The completed output was moved or deleted.",
            ));
        }
        Ok(output.path)
    }

    /// Stops new work and performs bounded engine shutdown.
    pub(crate) async fn shutdown(&self) -> Result<(), IpcErrorDto> {
        self.closing.store(true, Ordering::Release);
        self.cancel_analysis().await;
        let Some(initialized) = self.initialized.get() else {
            return Ok(());
        };
        let Some(queue) = &initialized.queue else {
            return Ok(());
        };
        queue.shutdown(SHUTDOWN_TIMEOUT).await.map_err(|error| {
            eprintln!("desktop bounded shutdown failed: {error:#}");
            IpcErrorDto::new(
                IpcErrorCodeDto::Internal,
                "YT Media could not finish bounded shutdown cleanly. Interrupted work remains recoverable.",
            )
        })
    }

    async fn queue(&self) -> Result<&JobQueue, IpcErrorDto> {
        self.initialized()
            .await
            .queue
            .as_ref()
            .ok_or_else(persistence_error)
    }

    fn ensure_open(&self) -> Result<(), IpcErrorDto> {
        if self.closing.load(Ordering::Acquire) {
            Err(IpcErrorDto::new(
                IpcErrorCodeDto::ShuttingDown,
                "YT Media is shutting down and no longer accepts new work.",
            ))
        } else {
            Ok(())
        }
    }

    fn settings_dto(&self, settings: &EngineSettings) -> SettingsDto {
        let mut dto = SettingsDto::from(settings);
        if dto.default_destination.is_none() {
            dto.default_destination = self
                .config
                .system_downloads_directory
                .as_deref()
                .map(crate::ipc::display_path);
        }
        dto
    }

    async fn start_analysis(&self) -> AnalysisOperation {
        let operation = AnalysisOperation {
            id: self.next_analysis_id.fetch_add(1, Ordering::Relaxed),
            cancellation: CancellationToken::new(),
        };
        let mut active = self.active_analysis.lock().await;
        if let Some(previous) = active.replace(operation.clone()) {
            previous.cancellation.cancel();
        }
        operation
    }

    async fn finish_analysis(&self, id: u64) {
        let mut active = self.active_analysis.lock().await;
        if active.as_ref().is_some_and(|operation| operation.id == id) {
            active.take();
        }
    }
}

fn map_analysis_error(error: &AnalyzeError) -> IpcErrorDto {
    if matches!(error, AnalyzeError::Cancelled) {
        return IpcErrorDto::new(
            IpcErrorCodeDto::AnalysisCancelled,
            "Video analysis was cancelled.",
        );
    }
    eprintln!("desktop analysis failed: {error:#}");
    IpcErrorDto::new(
        IpcErrorCodeDto::AnalysisFailed,
        "The video could not be analyzed. Check that it is public and try again.",
    )
}

async fn resolve_tools(config: &ServiceConfig) -> Result<Vec<ResolvedTool>, ()> {
    let target = SupportedTarget::current().map_err(|error| {
        eprintln!("desktop target resolution failed: {error:#}");
    })?;
    let managed_update = verify_optional_staged(
        config.managed_tools_directory.as_deref(),
        target,
        Arc::clone(&config.runner),
        "managed",
    )
    .await;
    let bundled_baseline = verify_optional_staged(
        config.bundled_tools_directory.as_deref(),
        target,
        Arc::clone(&config.runner),
        "bundled",
    )
    .await;
    let resolution = ToolResolutionConfig {
        explicit_overrides: config.explicit_overrides.clone(),
        managed_update,
        bundled_baseline,
        mode: config.resolution_mode,
        path_environment: config.path_environment.clone(),
    };
    let tool_resolver = ToolResolver::new(Arc::clone(&config.runner));
    let mut resolved_tools = Vec::with_capacity(Tool::ALL.len());
    for tool in Tool::ALL {
        match tool_resolver
            .resolve(tool, target, &resolution, CancellationToken::new())
            .await
        {
            Ok(candidate) => resolved_tools.push(candidate),
            Err(error) => {
                eprintln!("desktop {tool} resolution failed: {error:#}");
                return Err(());
            }
        }
    }
    Ok(resolved_tools)
}

async fn verify_optional_staged(
    directory: Option<&Path>,
    target: SupportedTarget,
    runner: Arc<dyn ProcessRunner>,
    tier: &str,
) -> Option<VerifiedToolSet> {
    let directory = directory.filter(|path| path.is_dir())?;
    match VerifiedToolSet::verify_staged(target, directory, runner, CancellationToken::new()).await
    {
        Ok(verified) => Some(verified),
        Err(error) => {
            eprintln!("desktop {tier} tool verification failed: {error:#}");
            None
        }
    }
}

fn compose_engine_services(
    resolved: &[ResolvedTool],
    runner: Arc<dyn ProcessRunner>,
) -> Result<(Analyzer, DownloadService), ()> {
    let find = |tool| {
        resolved
            .iter()
            .find(|candidate| candidate.tool == tool)
            .cloned()
            .ok_or(())
    };
    let yt_dlp = find(Tool::YtDlp)?;
    let ffmpeg = find(Tool::Ffmpeg)?;
    let ffprobe = find(Tool::Ffprobe)?;
    let deno = find(Tool::Deno)?;
    let analysis_tools = AnalysisTools::from_resolved(yt_dlp.clone(), ffmpeg.clone(), deno.clone())
        .map_err(|error| {
            eprintln!("desktop analysis tool composition failed: {error:#}");
        })?;
    let download_tools =
        DownloadTools::from_resolved(yt_dlp, ffmpeg, ffprobe, deno).map_err(|error| {
            eprintln!("desktop download tool composition failed: {error:#}");
        })?;
    Ok((
        Analyzer::with_runner(analysis_tools, Arc::clone(&runner)),
        DownloadService::with_runner(download_tools, runner),
    ))
}

fn download_request(request: EnqueueRequestDto) -> Result<DownloadRequest, IpcErrorDto> {
    let url = MediaUrl::parse(&request.url).map_err(|_| invalid_request())?;
    let output = output_selection(request.output)?;
    let destination =
        Destination::new(PathBuf::from(request.destination)).map_err(|_| invalid_request())?;
    let name = request
        .name
        .map(OutputName::new)
        .transpose()
        .map_err(|_| invalid_request())?;
    Ok(DownloadRequest {
        url,
        output,
        destination,
        name,
    })
}

fn output_selection(value: OutputSelectionDto) -> Result<OutputSelection, IpcErrorDto> {
    match value {
        OutputSelectionDto::Mp3(quality) => AudioQuality::try_from(quality)
            .map(OutputSelection::Mp3)
            .map_err(|_| invalid_request()),
        OutputSelectionDto::Mp4(quality) => VideoQuality::try_from(quality)
            .map(OutputSelection::Mp4)
            .map_err(|_| invalid_request()),
    }
}

fn settings_patch(request: UpdateSettingsRequestDto) -> Result<SettingsPatch, IpcErrorDto> {
    let default_destination = match request.default_destination {
        DefaultDestinationUpdateDto::Unchanged => None,
        DefaultDestinationUpdateDto::Clear => Some(None),
        DefaultDestinationUpdateDto::Set(value) => Some(Some(PathBuf::from(value))),
    };
    let queue_concurrency = request
        .queue_concurrency
        .map(QueueConcurrency::try_from)
        .transpose()
        .map_err(|_| invalid_request())?;
    let update_preference = request
        .update_preference
        .map(|preference| match preference {
            UpdatePreferenceDto::Notify => UpdatePreference::Notify,
            UpdatePreferenceDto::Automatic => UpdatePreference::Automatic,
            UpdatePreferenceDto::Disabled => UpdatePreference::Disabled,
        });
    let last_output = request.last_output.map(output_selection).transpose()?;
    Ok(SettingsPatch {
        default_destination,
        queue_concurrency,
        update_preference,
        last_output,
    })
}

fn job_id(request: &JobIdRequestDto) -> Result<JobId, IpcErrorDto> {
    JobId::parse(&request.job_id).map_err(|_| {
        IpcErrorDto::new(
            IpcErrorCodeDto::InvalidJobId,
            "The supplied job ID is invalid.",
        )
    })
}

fn queue_error(error: QueueError) -> IpcErrorDto {
    eprintln!("desktop queue command failed: {error:#}");
    match error {
        QueueError::JobNotFound(id) => IpcErrorDto::new(
            IpcErrorCodeDto::JobNotFound,
            "The requested job no longer exists.",
        )
        .with_detail("job_id", id.as_str()),
        QueueError::InvalidState {
            id,
            operation,
            state,
        } => IpcErrorDto::new(
            IpcErrorCodeDto::InvalidJobState,
            "That action is not available for the job in its current state.",
        )
        .with_detail("job_id", id.as_str())
        .with_detail("operation", operation)
        .with_detail("state", state.as_str()),
        QueueError::Closing => IpcErrorDto::new(
            IpcErrorCodeDto::ShuttingDown,
            "YT Media is shutting down and no longer accepts new work.",
        ),
        QueueError::ExecutorUnavailable => tools_unavailable_error(),
        QueueError::InvalidRequest(_) | QueueError::Destination(_) => invalid_request(),
        QueueError::Storage(_)
        | QueueError::Ownership(_)
        | QueueError::Join(_)
        | QueueError::ShutdownTimedOut { .. } => persistence_error(),
    }
}

fn invalid_request() -> IpcErrorDto {
    IpcErrorDto::new(
        IpcErrorCodeDto::InvalidRequest,
        "The request contains an invalid URL, output choice, destination, name, or setting.",
    )
}

fn persistence_error() -> IpcErrorDto {
    IpcErrorDto::new(
        IpcErrorCodeDto::PersistenceUnavailable,
        "The local job database is unavailable. No new work was started.",
    )
}

fn tools_unavailable_error() -> IpcErrorDto {
    IpcErrorDto::new(
        IpcErrorCodeDto::ToolsUnavailable,
        "Required verified media tools are unavailable. Reinstall the application or repair its managed tools.",
    )
}

fn unavailable_tool_statuses() -> Vec<ToolStatusDto> {
    Tool::ALL
        .into_iter()
        .map(|tool| ToolStatusDto {
            tool: tool.into(),
            ready: false,
            source: None,
            message: Some(
                "No verified executable is available; reinstall or repair managed tools."
                    .to_owned(),
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use tempfile::tempdir;
    use yt_media_engine::{
        cancellation::CancellationToken,
        jobs::JobState,
        process::{
            CapturedOutput, OutputStream, ProcessError, ProcessEvent, ProcessExitStatus,
            ProcessOutput, ProcessRunner, ProcessSpec, StreamCapture,
        },
        resolver::ResolutionMode,
        target::SupportedTarget,
        tool::Tool,
    };

    use super::{ApplicationService, JobEventSink, ServiceConfig};
    use crate::ipc::{
        AnalyzeRequestDto, BootstrapHealthDto, DefaultDestinationUpdateDto, EnqueueRequestDto,
        IpcErrorCodeDto, JobEventEnvelopeDto, JobIdRequestDto, OutputSelectionDto,
        UpdatePreferenceDto, UpdateSettingsRequestDto,
    };

    const ANALYSIS: &str =
        include_str!("../../../../crates/engine/tests/fixtures/analysis/progressive.json");
    const PROBE: &str = r#"{"streams":[{"codec_type":"audio","codec_name":"mp3"}],"format":{"format_name":"mp3","duration":"212.400"}}"#;

    #[derive(Default)]
    struct FixtureRunner {
        block_analysis: AtomicBool,
        analysis_started: AtomicBool,
        block_download: AtomicBool,
        fail_analysis: AtomicBool,
    }

    #[async_trait]
    impl ProcessRunner for FixtureRunner {
        async fn run(
            &self,
            spec: ProcessSpec,
            cancellation: CancellationToken,
        ) -> Result<ProcessOutput, ProcessError> {
            let executable = spec
                .executable()
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            let arguments = spec
                .argument_values()
                .map(OsString::from)
                .collect::<Vec<_>>();
            let rendered = arguments
                .iter()
                .map(|value| value.to_string_lossy())
                .collect::<Vec<_>>();
            if rendered.len() == 1 && matches!(rendered[0].as_ref(), "--version" | "-version") {
                let version = if executable.contains("yt-dlp") {
                    "2026.06.09\n"
                } else if executable.contains("ffprobe") {
                    "ffprobe version 8.0.1\n"
                } else if executable.contains("ffmpeg") {
                    "ffmpeg version 8.0.1\n"
                } else {
                    "deno 2.8.1\n"
                };
                return Ok(success_stdout(version.as_bytes()));
            }
            if rendered
                .iter()
                .any(|argument| argument == "--dump-single-json")
            {
                self.analysis_started.store(true, Ordering::Release);
                if self.block_analysis.load(Ordering::Acquire) {
                    cancellation.cancelled().await;
                    return Err(ProcessError::Cancelled {
                        output: CapturedOutput::default(),
                    });
                }
                if self.fail_analysis.load(Ordering::Acquire) {
                    return Err(ProcessError::Write(std::io::Error::other(
                        "secret C:\\private\\tool-output",
                    )));
                }
                return Ok(success_stdout(ANALYSIS.as_bytes()));
            }
            if executable.contains("ffprobe") {
                return Ok(success_stdout(PROBE.as_bytes()));
            }
            if executable.contains("ffmpeg") {
                let Some(target) = arguments.last() else {
                    return Err(ProcessError::Write(std::io::Error::other(
                        "fixture FFmpeg target missing",
                    )));
                };
                fs::write(target, b"encoded desktop fixture").map_err(ProcessError::Write)?;
                return Ok(success_stdout(
                    b"out_time_us=106200000\nout_time_us=212400000\nprogress=end\n",
                ));
            }
            let output_index = rendered
                .iter()
                .position(|argument| argument == "--output")
                .and_then(|index| index.checked_add(1))
                .ok_or_else(|| {
                    ProcessError::Write(std::io::Error::other("fixture output argument missing"))
                })?;
            let Some(target) = arguments.get(output_index).map(PathBuf::from) else {
                return Err(ProcessError::Write(std::io::Error::other(
                    "fixture output value missing",
                )));
            };
            if self.block_download.load(Ordering::Acquire) {
                let partial = target.with_file_name(format!(
                    "{}.part",
                    target
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or_default()
                ));
                fs::write(partial, b"resumable desktop fixture").map_err(ProcessError::Write)?;
                cancellation.cancelled().await;
                return Err(ProcessError::Cancelled {
                    output: CapturedOutput::default(),
                });
            }
            fs::write(target, b"downloaded desktop fixture").map_err(ProcessError::Write)?;
            Ok(success_stderr(
                b"yt-media-progress|downloading|50|100|100|10|5\nyt-media-progress|finished|100|100|100|0|0\n",
            ))
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<JobEventEnvelopeDto>>,
    }

    impl JobEventSink for RecordingSink {
        fn emit(&self, event: JobEventEnvelopeDto) {
            if let Ok(mut events) = self.events.lock() {
                events.push(event);
            }
        }
    }

    fn success_stdout(bytes: &[u8]) -> ProcessOutput {
        success_capture(bytes, &[], OutputStream::Stdout)
    }

    fn success_stderr(bytes: &[u8]) -> ProcessOutput {
        success_capture(&[], bytes, OutputStream::Stderr)
    }

    fn success_capture(stdout: &[u8], stderr: &[u8], stream: OutputStream) -> ProcessOutput {
        let bytes = match stream {
            OutputStream::Stdout => stdout,
            OutputStream::Stderr => stderr,
        };
        ProcessOutput {
            status: ProcessExitStatus {
                success: true,
                code: Some(0),
            },
            capture: CapturedOutput {
                stdout: capture(stdout),
                stderr: capture(stderr),
                events: (!bytes.is_empty())
                    .then(|| ProcessEvent {
                        sequence: 0,
                        stream,
                        bytes: bytes.to_vec(),
                    })
                    .into_iter()
                    .collect(),
            },
        }
    }

    fn capture(bytes: &[u8]) -> StreamCapture {
        StreamCapture {
            bytes: bytes.to_vec(),
            observed_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            observed_lines: u64::try_from(
                bytes
                    .split(|byte| *byte == b'\n')
                    .count()
                    .saturating_sub(usize::from(bytes.last() == Some(&b'\n'))),
            )
            .unwrap_or(u64::MAX),
            truncated: false,
        }
    }

    fn fixture_config(
        root: &Path,
        runner: Arc<dyn ProcessRunner>,
    ) -> Result<ServiceConfig, Box<dyn std::error::Error>> {
        let target = SupportedTarget::current()?;
        let tools = root.join("tools");
        let downloads = root.join("Downloads");
        fs::create_dir_all(&tools)?;
        fs::create_dir_all(&downloads)?;
        let mut explicit_overrides = std::collections::BTreeMap::new();
        for tool in Tool::ALL {
            let path = tools.join(tool.executable_name(target));
            fs::write(&path, b"desktop fixture")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let mut permissions = fs::metadata(&path)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&path, permissions)?;
            }
            explicit_overrides.insert(tool, path);
        }
        Ok(ServiceConfig {
            data_directory: root.join("data"),
            system_downloads_directory: Some(downloads),
            managed_tools_directory: None,
            bundled_tools_directory: None,
            resolution_mode: ResolutionMode::Production,
            path_environment: None,
            explicit_overrides,
            runner,
        })
    }

    fn job_request(destination: &Path) -> EnqueueRequestDto {
        EnqueueRequestDto {
            url: "https://youtu.be/dQw4w9WgXcQ".to_owned(),
            output: OutputSelectionDto::Mp3(192),
            destination: destination.to_string_lossy().into_owned(),
            name: Some("desktop-smoke".to_owned()),
        }
    }

    async fn wait_for_state(
        service: &ApplicationService,
        job_id: &str,
        expected: JobState,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let job = service
                    .get_job(JobIdRequestDto {
                        job_id: job_id.to_owned(),
                    })
                    .await;
                if matches!(job, Ok(ref job) if job.state == expected.into()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        Ok(())
    }

    async fn verify_bootstrap_analysis_and_settings(
        service: &ApplicationService,
        output: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bootstrap = service.bootstrap().await;
        assert_eq!(bootstrap.health, BootstrapHealthDto::Healthy);
        assert!(bootstrap.jobs.is_empty());
        assert_eq!(service.tool_status().await.len(), 4);
        let analyzed = service
            .analyze(AnalyzeRequestDto {
                url: "https://youtu.be/dQw4w9WgXcQ".to_owned(),
            })
            .await?;
        assert_eq!(analyzed.media.id, "dQw4w9WgXcQ");
        assert!(
            service
                .get_job(JobIdRequestDto {
                    job_id: "stale".to_owned(),
                })
                .await
                .is_err()
        );
        let settings = service
            .update_settings(UpdateSettingsRequestDto {
                default_destination: DefaultDestinationUpdateDto::Set(
                    output.to_string_lossy().into_owned(),
                ),
                queue_concurrency: Some(1),
                update_preference: Some(UpdatePreferenceDto::Disabled),
                last_output: Some(OutputSelectionDto::Mp3(256)),
            })
            .await?;
        assert_eq!(settings.queue_concurrency, 1);
        assert_eq!(service.read_settings().await?, settings);
        Ok(())
    }

    async fn exercise_job_lifecycle(
        service: &ApplicationService,
        runner: &FixtureRunner,
        output: &Path,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let job = service.enqueue(job_request(output)).await?;
        wait_for_state(service, &job.id, JobState::Downloading).await?;
        let paused = service
            .pause_job(JobIdRequestDto {
                job_id: job.id.clone(),
            })
            .await?;
        assert_eq!(paused.state, JobState::Paused.into());
        let _resumed = service
            .resume_job(JobIdRequestDto {
                job_id: job.id.clone(),
            })
            .await?;
        wait_for_state(service, &job.id, JobState::Downloading).await?;
        let cancelled = service
            .cancel_job(JobIdRequestDto {
                job_id: job.id.clone(),
            })
            .await?;
        assert_eq!(cancelled.state, JobState::Cancelled.into());
        assert_eq!(service.list_history().await?.len(), 1);
        assert!(
            service
                .delete_history(JobIdRequestDto {
                    job_id: job.id.clone(),
                })
                .await
                .is_err()
        );
        runner.block_download.store(false, Ordering::Release);
        let _retried = service
            .retry_job(JobIdRequestDto {
                job_id: job.id.clone(),
            })
            .await?;
        wait_for_state(service, &job.id, JobState::Completed).await?;
        let revealed = service
            .revealable_output(JobIdRequestDto {
                job_id: job.id.clone(),
            })
            .await?;
        assert!(revealed.is_file());
        Ok(job.id)
    }

    async fn verify_reconnect_order(
        service: &ApplicationService,
        sink: &RecordingSink,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let reconnect = service.bootstrap().await;
        assert_eq!(reconnect.jobs.len(), 1);
        let boundary = reconnect.last_event_sequence.parse::<u64>()?;
        tokio::time::sleep(Duration::from_millis(25)).await;
        let events = sink
            .events
            .lock()
            .map_err(|_| std::io::Error::other("event sink poisoned"))?
            .clone();
        assert!(!events.is_empty());
        let sequences = events
            .iter()
            .map(|event| event.sequence.parse::<u64>())
            .collect::<Result<Vec<_>, _>>()?;
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            sequences
                .last()
                .is_some_and(|sequence| *sequence <= boundary)
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_fixture_smoke_covers_commands_reconnect_persistence_and_shutdown()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let output = root.path().join("output");
        fs::create_dir_all(&output)?;
        let runner = Arc::new(FixtureRunner::default());
        runner.block_download.store(true, Ordering::Release);
        let sink = Arc::new(RecordingSink::default());
        let config = fixture_config(root.path(), runner.clone())?;
        let service = ApplicationService::new(config.clone(), sink.clone());

        verify_bootstrap_analysis_and_settings(&service, &output).await?;
        let job_id = exercise_job_lifecycle(&service, &runner, &output).await?;
        verify_reconnect_order(&service, &sink).await?;
        service.delete_history(JobIdRequestDto { job_id }).await?;
        service.shutdown().await?;
        let closing_error = service.enqueue(job_request(&output)).await;
        assert!(matches!(
            closing_error,
            Err(ref error) if error.code == IpcErrorCodeDto::ShuttingDown
        ));
        drop(service);

        let reopened = ApplicationService::new(config, Arc::new(RecordingSink::default()));
        let persisted = reopened.read_settings().await?;
        assert_eq!(persisted.queue_concurrency, 1);
        assert_eq!(
            persisted.default_destination.as_deref(),
            Some(output.to_string_lossy().as_ref())
        );
        reopened.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn system_downloads_is_the_effective_default_and_clear_restores_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let custom = root.path().join("custom");
        fs::create_dir_all(&custom)?;
        let config = fixture_config(root.path(), Arc::new(FixtureRunner::default()))?;
        let system_downloads = config
            .system_downloads_directory
            .clone()
            .ok_or("fixture system Downloads directory missing")?;
        let service = ApplicationService::new(config, Arc::new(RecordingSink::default()));

        assert_eq!(
            service
                .read_settings()
                .await?
                .default_destination
                .as_deref(),
            Some(system_downloads.to_string_lossy().as_ref())
        );
        let custom_settings = service
            .update_settings(UpdateSettingsRequestDto {
                default_destination: DefaultDestinationUpdateDto::Set(
                    custom.to_string_lossy().into_owned(),
                ),
                queue_concurrency: None,
                update_preference: None,
                last_output: None,
            })
            .await?;
        assert_eq!(
            custom_settings.default_destination.as_deref(),
            Some(custom.to_string_lossy().as_ref())
        );
        let restored = service
            .update_settings(UpdateSettingsRequestDto {
                default_destination: DefaultDestinationUpdateDto::Clear,
                queue_concurrency: None,
                update_preference: None,
                last_output: None,
            })
            .await?;
        assert_eq!(
            restored.default_destination.as_deref(),
            Some(system_downloads.to_string_lossy().as_ref())
        );
        service.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn analysis_errors_are_redacted_from_ipc() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let runner = Arc::new(FixtureRunner::default());
        runner.fail_analysis.store(true, Ordering::Release);
        let config = fixture_config(root.path(), runner)?;
        let service = ApplicationService::new(config, Arc::new(RecordingSink::default()));
        let error = service
            .analyze(AnalyzeRequestDto {
                url: "https://youtu.be/dQw4w9WgXcQ".to_owned(),
            })
            .await;
        assert!(matches!(
            error,
            Err(ref error) if error.code == IpcErrorCodeDto::AnalysisFailed
        ));
        let serialized = serde_json::to_string(&error.err())?;
        assert!(!serialized.contains("private"));
        assert!(!serialized.contains("tool-output"));
        service.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn active_analysis_is_cancelled_and_a_later_analysis_can_succeed()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let runner = Arc::new(FixtureRunner::default());
        runner.block_analysis.store(true, Ordering::Release);
        let config = fixture_config(root.path(), runner.clone())?;
        let service = Arc::new(ApplicationService::new(
            config,
            Arc::new(RecordingSink::default()),
        ));
        let analysis_service = Arc::clone(&service);
        let pending = tokio::spawn(async move {
            analysis_service
                .analyze(AnalyzeRequestDto {
                    url: "https://youtu.be/dQw4w9WgXcQ".to_owned(),
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !runner.analysis_started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await?;

        service.cancel_analysis().await;
        let cancelled = pending.await?;
        assert!(matches!(
            cancelled,
            Err(ref error) if error.code == IpcErrorCodeDto::AnalysisCancelled
        ));

        runner.block_analysis.store(false, Ordering::Release);
        let analyzed = service
            .analyze(AnalyzeRequestDto {
                url: "https://youtu.be/dQw4w9WgXcQ".to_owned(),
            })
            .await?;
        assert_eq!(analyzed.media.id, "dQw4w9WgXcQ");
        service.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_distinguishes_recoverable_tools_from_persistence_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let degraded = ServiceConfig {
            data_directory: root.path().join("degraded-data"),
            system_downloads_directory: Some(root.path().join("Downloads")),
            managed_tools_directory: None,
            bundled_tools_directory: None,
            resolution_mode: ResolutionMode::Production,
            path_environment: None,
            explicit_overrides: std::collections::BTreeMap::new(),
            runner: Arc::new(FixtureRunner::default()),
        };
        let service = ApplicationService::new(degraded, Arc::new(RecordingSink::default()));
        let bootstrap = service.bootstrap().await;
        assert_eq!(bootstrap.health, BootstrapHealthDto::Degraded);
        assert!(bootstrap.settings.is_some());
        assert!(bootstrap.tools.iter().all(|tool| !tool.ready));
        service.shutdown().await?;

        let invalid_data = root.path().join("not-a-directory");
        fs::write(&invalid_data, b"fixture")?;
        let failed = ServiceConfig {
            data_directory: invalid_data,
            system_downloads_directory: Some(root.path().join("Downloads")),
            managed_tools_directory: None,
            bundled_tools_directory: None,
            resolution_mode: ResolutionMode::Production,
            path_environment: None,
            explicit_overrides: std::collections::BTreeMap::new(),
            runner: Arc::new(FixtureRunner::default()),
        };
        let service = ApplicationService::new(failed, Arc::new(RecordingSink::default()));
        let bootstrap = service.bootstrap().await;
        assert_eq!(bootstrap.health, BootstrapHealthDto::Failed);
        assert!(bootstrap.settings.is_none());
        assert!(matches!(
            bootstrap.diagnostic,
            Some(ref error) if error.code == IpcErrorCodeDto::PersistenceUnavailable
        ));
        Ok(())
    }
}
