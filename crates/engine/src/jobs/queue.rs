//! Engine-owned durable scheduler, controls, recovery, and shutdown.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use directories::ProjectDirs;
use thiserror::Error;
use tokio::sync::{Mutex, Notify, broadcast};

use crate::{
    analysis::MediaUrl,
    download::{
        Destination, DownloadError, DownloadRequest, DownloadResult, DownloadService, JobControls,
        JobEventKind, JobId, JobStage, OutputName, reconcile_owned_paths,
        remove_recorded_owned_paths,
    },
};

use super::{
    EngineSettings, FinalOutput, JobErrorClass, JobFailure, JobRecord, JobRequest, JobState,
    QueueEvent, QueueSnapshot, QueueSubscription, SettingsPatch, StorageError,
    storage::{RequeueKind, SqliteStore},
};

const QUEUE_EVENT_CAPACITY: usize = 256;
const MAX_FAILURE_CHARS: usize = 4_096;

/// Durable queue operation failures.
#[derive(Debug, Error)]
pub enum QueueError {
    /// `SQLite` persistence or locking failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The supplied job identity was not found.
    #[error("job `{0}` does not exist")]
    JobNotFound(JobId),
    /// This queue was opened without a download executor.
    #[error(
        "this queue was opened for inspection only; configure verified download tools before starting work"
    )]
    ExecutorUnavailable,
    /// The queue is closing and accepts no new work.
    #[error("the job queue is shutting down and no longer accepts new work")]
    Closing,
    /// A normalized request could not be reconstructed safely.
    #[error("persisted job request is invalid: {0}")]
    InvalidRequest(String),
    /// A requested media destination is missing or unusable.
    #[error("invalid output destination: {0}")]
    Destination(String),
    /// A queue command is invalid in the current lifecycle state.
    #[error("job `{id}` cannot perform `{operation}` while it is `{state}`")]
    InvalidState {
        /// Affected job.
        id: JobId,
        /// Stable operation name.
        operation: &'static str,
        /// Current state.
        state: JobState,
    },
    /// Partial ownership validation or cleanup failed.
    #[error("could not reconcile engine-owned partial files")]
    Ownership(#[source] DownloadError),
    /// Filesystem validation ran on a blocking task that failed to join.
    #[error("queue filesystem task failed")]
    Join(#[source] tokio::task::JoinError),
    /// Bounded shutdown did not finish in time.
    #[error("job queue did not stop all active process trees within {seconds} seconds")]
    ShutdownTimedOut {
        /// Requested timeout.
        seconds: u64,
    },
}

struct QueueInner {
    store: SqliteStore,
    service: Option<DownloadService>,
    active: Mutex<HashMap<JobId, JobControls>>,
    events: broadcast::Sender<QueueEvent>,
    event_sequence: AtomicU64,
    scheduler_running: AtomicBool,
    closing: AtomicBool,
    shutdown_interruption: AtomicBool,
    scheduler_notify: Notify,
    inactive_notify: Notify,
}

/// Cloneable engine-owned durable job queue.
#[derive(Clone)]
pub struct JobQueue {
    inner: Arc<QueueInner>,
}

impl std::fmt::Debug for JobQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobQueue")
            .field("database_path", &self.inner.store.database_path())
            .field("has_executor", &self.inner.service.is_some())
            .finish_non_exhaustive()
    }
}

impl JobQueue {
    /// Opens an inspection-only queue and performs passive startup recovery.
    ///
    /// Opening never starts queued or interrupted work.
    ///
    /// # Errors
    ///
    /// Returns actionable directory, lock, migration, future-schema, or corruption errors.
    pub async fn open(data_directory: impl Into<PathBuf>) -> Result<Self, QueueError> {
        Self::open_inner(data_directory.into(), None).await
    }

    /// Opens a queue with a verified download executor and performs passive startup recovery.
    ///
    /// Opening never starts queued or interrupted work.
    ///
    /// # Errors
    ///
    /// Returns actionable directory, lock, migration, future-schema, corruption, or ownership
    /// errors.
    pub async fn open_with_download_service(
        data_directory: impl Into<PathBuf>,
        service: DownloadService,
    ) -> Result<Self, QueueError> {
        Self::open_inner(data_directory.into(), Some(service)).await
    }

    async fn open_inner(
        data_directory: PathBuf,
        service: Option<DownloadService>,
    ) -> Result<Self, QueueError> {
        let store = SqliteStore::open(data_directory).await?;
        let (events, _) = broadcast::channel(QUEUE_EVENT_CAPACITY);
        let queue = Self {
            inner: Arc::new(QueueInner {
                store,
                service,
                active: Mutex::new(HashMap::new()),
                events,
                event_sequence: AtomicU64::new(1),
                scheduler_running: AtomicBool::new(false),
                closing: AtomicBool::new(false),
                shutdown_interruption: AtomicBool::new(false),
                scheduler_notify: Notify::new(),
                inactive_notify: Notify::new(),
            }),
        };
        queue.recover_startup().await?;
        Ok(queue)
    }

    /// Returns the platform-local application data directory used by the CLI by default.
    ///
    /// # Errors
    ///
    /// Returns an error when the current platform exposes no application data location.
    pub fn platform_data_directory() -> Result<PathBuf, QueueError> {
        ProjectDirs::from("com", "YT Media", "YT Media")
            .map(|directories| directories.data_local_dir().to_path_buf())
            .ok_or_else(|| {
                QueueError::InvalidRequest(
                    "the operating system did not provide an application data directory".to_owned(),
                )
            })
    }

    /// Returns the `SQLite` database path for diagnostics.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        self.inner.store.database_path()
    }

    /// Subscribes to bounded authoritative snapshots.
    #[must_use]
    pub fn subscribe(&self) -> QueueSubscription {
        QueueSubscription {
            receiver: self.inner.events.subscribe(),
        }
    }

    /// Persists and explicitly schedules one new job.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue is closing, has no executor, the destination is invalid, or
    /// persistence fails.
    pub async fn enqueue(&self, request: DownloadRequest) -> Result<JobRecord, QueueError> {
        self.ensure_start_allowed()?;
        let request = normalize_request(request).await?;
        let record = self
            .inner
            .store
            .insert_job(JobId::new_v7(), request)
            .await?;
        self.emit(record.clone());
        self.kick_scheduler();
        Ok(record)
    }

    /// Lists every job in creation order.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the snapshot cannot be read safely.
    pub async fn list(&self) -> Result<Vec<JobRecord>, QueueError> {
        Ok(self.inner.store.list_jobs(false).await?)
    }

    /// Reads an authoritative job snapshot and a stable event boundary for reconnecting clients.
    ///
    /// A caller should subscribe before requesting this snapshot, discard buffered events through
    /// `last_event_sequence`, then consume later events. The retry loop prevents a queue mutation
    /// from being split across the snapshot and its boundary.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the snapshot cannot be read safely.
    pub async fn snapshot(&self) -> Result<QueueSnapshot, QueueError> {
        loop {
            let next_before = self.inner.event_sequence.load(Ordering::Acquire);
            let jobs = self.list().await?;
            let next_after = self.inner.event_sequence.load(Ordering::Acquire);
            if next_before == next_after {
                return Ok(QueueSnapshot {
                    last_event_sequence: next_after.saturating_sub(1),
                    jobs,
                });
            }
        }
    }

    /// Lists persisted completed, cancelled, and failed history newest first.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the snapshot cannot be read safely.
    pub async fn history(&self) -> Result<Vec<JobRecord>, QueueError> {
        Ok(self.inner.store.list_jobs(true).await?)
    }

    /// Reads one authoritative job snapshot.
    ///
    /// # Errors
    ///
    /// Returns a storage error or `JobNotFound`.
    pub async fn get(&self, id: &JobId) -> Result<JobRecord, QueueError> {
        self.inner
            .store
            .get_job(id.clone())
            .await?
            .ok_or_else(|| QueueError::JobNotFound(id.clone()))
    }

    /// Pauses queued or active work after process-tree cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error for missing jobs or states that cannot be paused.
    pub async fn pause(&self, id: &JobId) -> Result<JobRecord, QueueError> {
        let record = self.get(id).await?;
        match record.state {
            JobState::Queued => {
                let updated = self
                    .inner
                    .store
                    .transition_stopped(id.clone(), JobState::Paused)
                    .await?;
                self.emit(updated.clone());
                Ok(updated)
            }
            state if state.is_process_active() => {
                let control = self.inner.active.lock().await.get(id).cloned();
                let Some(control) = control else {
                    return Err(QueueError::InvalidState {
                        id: id.clone(),
                        operation: "pause",
                        state,
                    });
                };
                control.pause();
                self.wait_until_stopped(id).await
            }
            state => Err(QueueError::InvalidState {
                id: id.clone(),
                operation: "pause",
                state,
            }),
        }
    }

    /// Explicitly resumes paused or startup-interrupted work under the same identity.
    ///
    /// The attempt count is incremented and the scheduler is explicitly activated.
    ///
    /// # Errors
    ///
    /// Returns an error for missing jobs, unavailable execution, or invalid states.
    pub async fn resume(&self, id: &JobId) -> Result<JobRecord, QueueError> {
        self.ensure_start_allowed()?;
        let current = self.get(id).await?;
        if !matches!(current.state, JobState::Paused | JobState::Interrupted) {
            return Err(QueueError::InvalidState {
                id: id.clone(),
                operation: "resume",
                state: current.state,
            });
        }
        let record = self
            .inner
            .store
            .requeue(id.clone(), RequeueKind::Resume)
            .await?;
        self.emit(record.clone());
        self.kick_scheduler();
        Ok(record)
    }

    /// Cancels queued, retained, or active work and removes only validated recorded owned paths.
    ///
    /// # Errors
    ///
    /// Returns an error for missing jobs, invalid states, or ownership validation failures.
    pub async fn cancel(&self, id: &JobId) -> Result<JobRecord, QueueError> {
        let record = self.get(id).await?;
        if record.state.is_process_active() {
            let control = self.inner.active.lock().await.get(id).cloned();
            let Some(control) = control else {
                return Err(QueueError::InvalidState {
                    id: id.clone(),
                    operation: "cancel",
                    state: record.state,
                });
            };
            control.cancel();
            return self.wait_until_stopped(id).await;
        }
        if !matches!(
            record.state,
            JobState::Queued | JobState::Paused | JobState::Interrupted
        ) {
            return Err(QueueError::InvalidState {
                id: id.clone(),
                operation: "cancel",
                state: record.state,
            });
        }
        if !record.destination_available && !record.owned_partial_paths.is_empty() {
            return Err(QueueError::Destination(
                "the job destination is unavailable; retained owned files were left unchanged"
                    .to_owned(),
            ));
        }
        let recorded = self.reconcile_ownership(&record, false).await?;
        if !recorded.is_empty() {
            let directory = record.request.destination.clone();
            let job_id = id.clone();
            let deletion_paths = recorded.clone();
            tokio::task::spawn_blocking(move || {
                remove_recorded_owned_paths(&directory, &job_id, &deletion_paths)
            })
            .await
            .map_err(QueueError::Join)?
            .map_err(QueueError::Ownership)?;
        }
        let updated = self
            .inner
            .store
            .finish_job(id.clone(), JobState::Cancelled, None, None)
            .await?;
        self.emit(updated.clone());
        Ok(updated)
    }

    /// Explicitly appends a failed or cancelled job to FIFO retry order.
    ///
    /// # Errors
    ///
    /// Returns an error for missing jobs, unavailable execution, or invalid states.
    pub async fn retry(&self, id: &JobId) -> Result<JobRecord, QueueError> {
        self.ensure_start_allowed()?;
        let current = self.get(id).await?;
        if !matches!(current.state, JobState::Failed | JobState::Cancelled) {
            return Err(QueueError::InvalidState {
                id: id.clone(),
                operation: "retry",
                state: current.state,
            });
        }
        let record = self
            .inner
            .store
            .requeue(id.clone(), RequeueKind::Retry)
            .await?;
        self.emit(record.clone());
        self.kick_scheduler();
        Ok(record)
    }

    /// Deletes one completed history record without touching the user output.
    ///
    /// # Errors
    ///
    /// Returns an error for missing/non-completed jobs or storage failures.
    pub async fn remove_completed(&self, id: &JobId) -> Result<(), QueueError> {
        let current = self.get(id).await?;
        if current.state != JobState::Completed {
            return Err(QueueError::InvalidState {
                id: id.clone(),
                operation: "remove-completed",
                state: current.state,
            });
        }
        if self.inner.store.delete_completed(id.clone()).await? {
            Ok(())
        } else {
            Err(QueueError::JobNotFound(id.clone()))
        }
    }

    /// Reads persisted engine settings.
    ///
    /// # Errors
    ///
    /// Returns a storage or persisted-value error.
    pub async fn settings(&self) -> Result<EngineSettings, QueueError> {
        Ok(self.inner.store.settings().await?)
    }

    /// Applies and persists an engine settings patch.
    ///
    /// Raising concurrency explicitly wakes the scheduler; lowering it never interrupts running
    /// work and applies before the next claim.
    ///
    /// # Errors
    ///
    /// Returns a storage or path-contract error.
    pub async fn update_settings(
        &self,
        patch: SettingsPatch,
    ) -> Result<EngineSettings, QueueError> {
        if let Some(Some(destination)) = &patch.default_destination
            && destination.to_str().is_none()
        {
            return Err(QueueError::InvalidRequest(
                "default destination must be valid Unicode".to_owned(),
            ));
        }
        let settings = self.inner.store.update_settings(patch).await?;
        self.inner.scheduler_notify.notify_waiters();
        self.kick_scheduler();
        Ok(settings)
    }

    /// Waits until one job reaches a terminal, paused, or interrupted state.
    ///
    /// # Errors
    ///
    /// Returns a storage, missing-job, or closed-subscription error.
    pub async fn wait_until_stopped(&self, id: &JobId) -> Result<JobRecord, QueueError> {
        let mut subscription = self.subscribe();
        loop {
            let record = self.get(id).await?;
            if record.state.is_terminal()
                || matches!(record.state, JobState::Paused | JobState::Interrupted)
            {
                return Ok(record);
            }
            match subscription.recv().await {
                Ok(event) if event.job.id == *id => {
                    if event.job.state.is_terminal()
                        || matches!(event.job.state, JobState::Paused | JobState::Interrupted)
                    {
                        return Ok(event.job);
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(QueueError::InvalidRequest(
                        "queue event stream closed before the job stopped".to_owned(),
                    ));
                }
            }
        }
    }

    /// Requests bounded shutdown, retains resumable partials, and records active work as
    /// interrupted.
    ///
    /// # Errors
    ///
    /// Returns `ShutdownTimedOut` if owned process trees do not stop within the supplied bound.
    pub async fn shutdown(&self, timeout: Duration) -> Result<(), QueueError> {
        self.inner.closing.store(true, Ordering::Release);
        self.inner
            .shutdown_interruption
            .store(true, Ordering::Release);
        self.inner.scheduler_notify.notify_waiters();
        let controls = self
            .inner
            .active
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for control in controls {
            control.pause();
        }
        let wait = async {
            loop {
                if self.inner.active.lock().await.is_empty()
                    && !self.inner.scheduler_running.load(Ordering::Acquire)
                {
                    return;
                }
                self.inner.inactive_notify.notified().await;
            }
        };
        if tokio::time::timeout(timeout, wait).await.is_err() {
            return Err(QueueError::ShutdownTimedOut {
                seconds: timeout.as_secs(),
            });
        }
        Ok(())
    }

    async fn recover_startup(&self) -> Result<(), QueueError> {
        let recovered = self.inner.store.recover_active().await?;
        let mut candidates = self.inner.store.list_jobs(false).await?;
        candidates.retain(|record| {
            matches!(record.state, JobState::Paused | JobState::Interrupted)
                || recovered.iter().any(|item| item.id == record.id)
        });
        for record in candidates {
            if !record.destination_available {
                let updated = self
                    .inner
                    .store
                    .set_recovery_failure(
                        record.id.clone(),
                        JobFailure {
                            class: JobErrorClass::DestinationUnavailable,
                            message: format!(
                                "destination `{}` is unavailable; explicit resume will revalidate it",
                                record.request.destination.display()
                            ),
                        },
                    )
                    .await?;
                self.emit(updated);
                continue;
            }
            let retained = self.reconcile_ownership(&record, true).await?;
            let updated = self
                .inner
                .store
                .replace_owned_paths(record.id.clone(), retained)
                .await?;
            self.emit(updated);
        }
        Ok(())
    }

    async fn reconcile_ownership(
        &self,
        record: &JobRecord,
        retain_resumable_only: bool,
    ) -> Result<Vec<PathBuf>, QueueError> {
        let directory = record.request.destination.clone();
        let id = record.id.clone();
        tokio::task::spawn_blocking(move || {
            reconcile_owned_paths(&directory, &id, retain_resumable_only)
        })
        .await
        .map_err(QueueError::Join)?
        .map_err(QueueError::Ownership)
    }

    fn ensure_start_allowed(&self) -> Result<(), QueueError> {
        if self.inner.closing.load(Ordering::Acquire) {
            Err(QueueError::Closing)
        } else if self.inner.service.is_none() {
            Err(QueueError::ExecutorUnavailable)
        } else {
            Ok(())
        }
    }

    fn emit(&self, job: JobRecord) {
        self.emit_with_activity(job, None);
    }

    fn emit_with_activity(&self, job: JobRecord, activity: Option<JobEventKind>) {
        let sequence = self.inner.event_sequence.fetch_add(1, Ordering::Relaxed);
        let _ignored = self.inner.events.send(QueueEvent {
            sequence,
            job,
            activity,
        });
    }

    fn kick_scheduler(&self) {
        self.inner.scheduler_notify.notify_waiters();
        if self
            .inner
            .scheduler_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let queue = self.clone();
            tokio::spawn(async move {
                queue.scheduler_loop().await;
            });
        }
    }

    async fn scheduler_loop(self) {
        loop {
            if self.inner.closing.load(Ordering::Acquire) {
                break;
            }
            let limit = match self.inner.store.settings().await {
                Ok(settings) => usize::from(settings.queue_concurrency.get()),
                Err(_) => break,
            };
            let active = self.inner.active.lock().await.len();
            if active >= limit {
                self.inner.scheduler_notify.notified().await;
                continue;
            }
            let record = match self.inner.store.claim_next().await {
                Ok(Some(record)) => record,
                Ok(None) => {
                    if active == 0 {
                        break;
                    }
                    self.inner.scheduler_notify.notified().await;
                    continue;
                }
                Err(_) => break,
            };
            self.emit(record.clone());
            self.start_claimed(record).await;
        }
        self.inner.scheduler_running.store(false, Ordering::Release);
        self.inner.inactive_notify.notify_waiters();
    }

    async fn start_claimed(&self, record: JobRecord) {
        let Some(service) = self.inner.service.clone() else {
            return;
        };
        let request = match restore_request(&record.request) {
            Ok(request) => request,
            Err(error) => {
                let failure = JobFailure {
                    class: JobErrorClass::InvalidRequest,
                    message: error.to_string(),
                };
                if let Ok(updated) = self
                    .inner
                    .store
                    .finish_job(record.id.clone(), JobState::Failed, Some(failure), None)
                    .await
                {
                    self.emit(updated);
                }
                self.inner.scheduler_notify.notify_waiters();
                return;
            }
        };
        let started = service.start_with_id(request, record.id.clone());
        self.inner
            .active
            .lock()
            .await
            .insert(record.id.clone(), started.controls.clone());
        let queue = self.clone();
        tokio::spawn(async move {
            queue.drive_job(started).await;
        });
    }

    async fn drive_job(&self, started: crate::download::StartedDownload) {
        let id = started.job_id.clone();
        let controls = started.controls.clone();
        let mut events = started.events;
        let completion = started.completion.wait();
        tokio::pin!(completion);
        let result = loop {
            tokio::select! {
                result = &mut completion => break result,
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            if self
                                .persist_activity(&id, event.kind, &controls)
                                .await
                                .is_err()
                            {
                                controls.cancel();
                            }
                        }
                        Err(
                            broadcast::error::RecvError::Lagged(_)
                            | broadcast::error::RecvError::Closed,
                        ) => {}
                    }
                }
            }
        };
        while let Ok(event) = events.try_recv() {
            let _ignored = self.persist_activity(&id, event.kind, &controls).await;
        }
        self.finish_runtime(id.clone(), result).await;
        self.inner.active.lock().await.remove(&id);
        self.inner.inactive_notify.notify_waiters();
        self.inner.scheduler_notify.notify_waiters();
    }

    async fn persist_activity(
        &self,
        id: &JobId,
        activity: JobEventKind,
        controls: &JobControls,
    ) -> Result<(), QueueError> {
        let persisted = match &activity {
            JobEventKind::Stage { stage } => {
                if let Some(state) = stage_state(*stage) {
                    self.inner
                        .store
                        .save_progress(id.clone(), Some(state), None)
                        .await?
                } else {
                    self.get(id).await?
                }
            }
            JobEventKind::Progress { progress } => {
                self.inner
                    .store
                    .save_progress(
                        id.clone(),
                        stage_state(progress.stage),
                        Some(progress.clone()),
                    )
                    .await?
            }
            JobEventKind::Warning { .. } => self.get(id).await?,
        };
        self.emit_with_activity(persisted, Some(activity));
        if self.inner.closing.load(Ordering::Acquire) {
            controls.pause();
        }
        Ok(())
    }

    async fn finish_runtime(&self, id: JobId, result: Result<DownloadResult, DownloadError>) {
        let persisted = match result {
            Ok(result) => {
                let output = FinalOutput {
                    path: result.path,
                    size_bytes: result.size_bytes,
                    output: result.output,
                };
                self.inner
                    .store
                    .finish_job(id.clone(), JobState::Completed, None, Some(output))
                    .await
            }
            Err(DownloadError::Paused) => {
                let state = if self.inner.shutdown_interruption.load(Ordering::Acquire) {
                    JobState::Interrupted
                } else {
                    JobState::Paused
                };
                let Ok(current) = self.get(&id).await else {
                    return;
                };
                let owned = match self.reconcile_ownership(&current, false).await {
                    Ok(paths) => paths,
                    Err(error) => {
                        let failure = JobFailure {
                            class: JobErrorClass::Filesystem,
                            message: error.to_string(),
                        };
                        if let Ok(record) = self
                            .inner
                            .store
                            .finish_job(id.clone(), JobState::Failed, Some(failure), None)
                            .await
                        {
                            self.emit(record);
                        }
                        return;
                    }
                };
                let _ignored = self
                    .inner
                    .store
                    .replace_owned_paths(id.clone(), owned)
                    .await;
                self.inner
                    .store
                    .finish_job(id.clone(), state, None, None)
                    .await
            }
            Err(DownloadError::Cancelled) => {
                self.inner
                    .store
                    .finish_job(id.clone(), JobState::Cancelled, None, None)
                    .await
            }
            Err(error) => {
                let failure = classify_download_error(&error);
                self.inner
                    .store
                    .finish_job(id.clone(), JobState::Failed, Some(failure), None)
                    .await
            }
        };
        if let Ok(record) = persisted {
            self.emit(record);
        }
    }
}

async fn normalize_request(request: DownloadRequest) -> Result<JobRequest, QueueError> {
    let destination = request.destination.as_path().to_path_buf();
    let canonical_destination = tokio::task::spawn_blocking(move || {
        if destination.exists() {
            return fs::canonicalize(&destination).map_err(|source| {
                QueueError::Destination(format!(
                    "destination `{}` could not be canonicalized: {source}",
                    destination.display()
                ))
            });
        }
        if destination.is_absolute() {
            Ok(destination)
        } else {
            std::env::current_dir()
                .map(|current| current.join(&destination))
                .map_err(|source| {
                    QueueError::Destination(format!(
                        "relative destination `{}` could not be resolved: {source}",
                        destination.display()
                    ))
                })
        }
    })
    .await
    .map_err(QueueError::Join)??;
    Ok(JobRequest {
        canonical_url: request.url.as_str().to_owned(),
        output: request.output,
        destination: canonical_destination,
        name: request.name.map(|name| name.as_str().to_owned()),
    })
}

fn restore_request(request: &JobRequest) -> Result<DownloadRequest, QueueError> {
    Ok(DownloadRequest {
        url: MediaUrl::parse(&request.canonical_url)
            .map_err(|error| QueueError::InvalidRequest(error.to_string()))?,
        output: request.output,
        destination: Destination::new(&request.destination)
            .map_err(|error| QueueError::InvalidRequest(error.to_string()))?,
        name: request
            .name
            .as_ref()
            .map(|name| OutputName::new(name.clone()))
            .transpose()
            .map_err(|error| QueueError::InvalidRequest(error.to_string()))?,
    })
}

const fn stage_state(stage: JobStage) -> Option<JobState> {
    match stage {
        JobStage::Analyzing => Some(JobState::Analyzing),
        JobStage::Downloading => Some(JobState::Downloading),
        JobStage::Merging => Some(JobState::Merging),
        JobStage::Converting => Some(JobState::Converting),
        JobStage::Finalizing
        | JobStage::Completed
        | JobStage::Paused
        | JobStage::Cancelled
        | JobStage::Failed => None,
    }
}

fn classify_download_error(error: &DownloadError) -> JobFailure {
    let class = match error {
        DownloadError::InvalidRequest(_) => JobErrorClass::InvalidRequest,
        DownloadError::Analysis(_) => JobErrorClass::Analysis,
        DownloadError::FormatUnavailable { .. } => JobErrorClass::FormatUnavailable,
        DownloadError::Destination { .. } => JobErrorClass::DestinationUnavailable,
        DownloadError::Filesystem { .. } | DownloadError::CollisionLimit => {
            JobErrorClass::Filesystem
        }
        DownloadError::ProcessSpecification(_)
        | DownloadError::Process { .. }
        | DownloadError::NonZero { .. } => JobErrorClass::Process,
        DownloadError::Protocol { .. } => JobErrorClass::Protocol,
        DownloadError::Verification(_) => JobErrorClass::Verification,
        DownloadError::Join(_)
        | DownloadError::CompletionClosed
        | DownloadError::Paused
        | DownloadError::Cancelled => JobErrorClass::Internal,
    };
    JobFailure {
        class,
        message: error.to_string().chars().take(MAX_FAILURE_CHARS).collect(),
    }
}
