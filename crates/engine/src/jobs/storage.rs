//! Single-writer `SQLite` persistence and forward-only migrations.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use thiserror::Error;

use crate::{
    download::{AudioQuality, JobId, JobProgress, OutputSelection, VideoQuality},
    jobs::{
        EngineSettings, FinalOutput, JobErrorClass, JobFailure, JobRecord, JobRequest, JobState,
        OutputAvailability, QueueConcurrency, SettingsPatch, SettingsValueError, UpdatePreference,
        model::bounded_path,
    },
};

const DATABASE_FILENAME: &str = "jobs.sqlite3";
const LOCK_FILENAME: &str = "jobs.sqlite3.lock";
const CURRENT_SCHEMA_VERSION: i64 = 2;
const MAX_ERROR_CHARS: usize = 4_096;

static OPEN_DATABASES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

const MIGRATION_1: &str = r"
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at_ms INTEGER NOT NULL
);
CREATE TABLE queue_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_sequence INTEGER NOT NULL CHECK (next_sequence > 0)
);
INSERT INTO queue_meta(singleton, next_sequence) VALUES (1, 1);
CREATE TABLE settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    default_destination TEXT,
    queue_concurrency INTEGER NOT NULL CHECK (queue_concurrency BETWEEN 1 AND 4),
    update_preference TEXT NOT NULL,
    last_output_format TEXT NOT NULL,
    last_output_quality INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
INSERT INTO settings(
    singleton,
    default_destination,
    queue_concurrency,
    update_preference,
    last_output_format,
    last_output_quality,
    updated_at_ms
) VALUES (1, NULL, 2, 'notify', 'mp3', 192, 0);
CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    canonical_url TEXT NOT NULL,
    output_format TEXT NOT NULL,
    output_quality INTEGER NOT NULL,
    destination TEXT NOT NULL,
    output_name TEXT,
    state TEXT NOT NULL,
    progress_json TEXT,
    error_class TEXT,
    error_message TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    attempt_count INTEGER NOT NULL CHECK (attempt_count >= 1),
    queue_sequence INTEGER NOT NULL,
    final_path TEXT,
    final_size_bytes INTEGER,
    CHECK (
        (final_path IS NULL AND final_size_bytes IS NULL)
        OR (final_path IS NOT NULL AND final_size_bytes > 0)
    )
);
CREATE UNIQUE INDEX jobs_queue_sequence ON jobs(queue_sequence);
CREATE INDEX jobs_state_queue ON jobs(state, queue_sequence);
";

const MIGRATION_2: &str = r"
ALTER TABLE jobs ADD COLUMN request_json TEXT;
CREATE TABLE owned_paths (
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    kind TEXT NOT NULL,
    PRIMARY KEY(job_id, path)
);
CREATE INDEX owned_paths_job ON owned_paths(job_id);
";

const MIGRATIONS: [Migration; 2] = [
    Migration {
        version: 1,
        destructive: false,
        sql: MIGRATION_1,
    },
    Migration {
        version: 2,
        destructive: false,
        sql: MIGRATION_2,
    },
];

struct Migration {
    version: i64,
    destructive: bool,
    sql: &'static str,
}

/// Actionable durable storage failures.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The data directory could not be prepared.
    #[error("could not prepare queue data directory `{path}`")]
    DataDirectory {
        /// Bounded affected path.
        path: String,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Another process owns the writable database lock.
    #[error(
        "job database `{path}` is already open for mutation by another process; close the other YT Media instance and retry"
    )]
    Locked {
        /// Bounded database path.
        path: String,
    },
    /// The lock file could not be opened.
    #[error("could not open job database lock `{path}`")]
    LockFile {
        /// Bounded lock path.
        path: String,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The process-local lock registry was poisoned.
    #[error("could not inspect the process-local job database lock registry")]
    LockRegistry,
    /// `SQLite` could not perform an operation.
    #[error("job database operation `{operation}` failed for `{path}`")]
    Database {
        /// Stable operation name.
        operation: &'static str,
        /// Bounded database path.
        path: String,
        /// Original `SQLite` failure.
        #[source]
        source: rusqlite::Error,
    },
    /// Integrity checking detected corruption.
    #[error(
        "job database `{path}` is corrupt or unreadable: {reason}; the file was left unchanged"
    )]
    Corrupt {
        /// Bounded database path.
        path: String,
        /// Bounded integrity diagnostic.
        reason: String,
    },
    /// The database belongs to a newer application schema.
    #[error(
        "job database `{path}` uses unsupported schema version {found}; this build supports through version {supported}"
    )]
    FutureSchema {
        /// Bounded database path.
        path: String,
        /// Observed version.
        found: i64,
        /// Maximum supported version.
        supported: i64,
    },
    /// A destructive migration backup failed.
    #[error("could not back up job database `{path}` before schema migration")]
    Backup {
        /// Bounded database path.
        path: String,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Persisted data violated the engine contract.
    #[error("job database `{path}` contains invalid data: {reason}")]
    InvalidData {
        /// Bounded database path.
        path: String,
        /// Bounded reason.
        reason: String,
    },
    /// A blocking database task could not be joined.
    #[error("job database blocking task failed")]
    Join(#[source] tokio::task::JoinError),
}

struct StoreInner {
    database_path: PathBuf,
    _lock_file: File,
    _process_reservation: ProcessReservation,
    operation_lock: tokio::sync::Mutex<()>,
}

struct ProcessReservation {
    database_path: PathBuf,
}

impl ProcessReservation {
    fn acquire(database_path: &Path) -> Result<Self, StorageError> {
        let mut open = OPEN_DATABASES
            .lock()
            .map_err(|_| StorageError::LockRegistry)?;
        if !open.insert(database_path.to_path_buf()) {
            return Err(StorageError::Locked {
                path: bounded_path(database_path),
            });
        }
        Ok(Self {
            database_path: database_path.to_path_buf(),
        })
    }
}

impl Drop for ProcessReservation {
    fn drop(&mut self) {
        if let Ok(mut open) = OPEN_DATABASES.lock() {
            open.remove(&self.database_path);
        }
    }
}

/// Cloneable handle to one exclusively locked writable database.
#[derive(Clone)]
pub(crate) struct SqliteStore {
    inner: Arc<StoreInner>,
}

#[derive(Clone, Copy)]
pub(crate) enum RequeueKind {
    Resume,
    Retry,
}

impl SqliteStore {
    pub(crate) async fn open(data_directory: PathBuf) -> Result<Self, StorageError> {
        tokio::task::spawn_blocking(move || Self::open_blocking(&data_directory))
            .await
            .map_err(StorageError::Join)?
    }

    fn open_blocking(data_directory: &Path) -> Result<Self, StorageError> {
        fs::create_dir_all(data_directory).map_err(|source| StorageError::DataDirectory {
            path: bounded_path(data_directory),
            source,
        })?;
        let canonical =
            fs::canonicalize(data_directory).map_err(|source| StorageError::DataDirectory {
                path: bounded_path(data_directory),
                source,
            })?;
        let database_path = canonical.join(DATABASE_FILENAME);
        let lock_path = canonical.join(LOCK_FILENAME);
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| StorageError::LockFile {
                path: bounded_path(&lock_path),
                source,
            })?;
        if lock_file.try_lock_exclusive().is_err() {
            return Err(StorageError::Locked {
                path: bounded_path(&database_path),
            });
        }
        let process_reservation = ProcessReservation::acquire(&database_path)?;

        let mut connection = open_connection(&database_path)?;
        migrate(&mut connection, &database_path)?;
        Ok(Self {
            inner: Arc::new(StoreInner {
                database_path,
                _lock_file: lock_file,
                _process_reservation: process_reservation,
                operation_lock: tokio::sync::Mutex::new(()),
            }),
        })
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.inner.database_path
    }

    async fn run<T, F>(&self, _operation: &'static str, work: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &Path) -> Result<T, StorageError> + Send + 'static,
    {
        let _operation_guard = self.inner.operation_lock.lock().await;
        let path = self.inner.database_path.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = open_connection(&path)?;
            work(&mut connection, &path)
        })
        .await
        .map_err(StorageError::Join)?
    }

    pub(crate) async fn insert_job(
        &self,
        id: JobId,
        request: JobRequest,
    ) -> Result<JobRecord, StorageError> {
        self.run("insert-job", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-insert-job", path, source))?;
            let sequence = take_queue_sequence(&transaction, path)?;
            let now = now_ms();
            let (format, quality) = output_parts(request.output);
            let request_json = serde_json::to_string(&request).map_err(|source| {
                StorageError::InvalidData {
                    path: bounded_path(path),
                    reason: format!("could not serialize normalized request: {source}"),
                }
            })?;
            transaction
                .execute(
                    "INSERT INTO jobs(
                        id, canonical_url, output_format, output_quality, destination, output_name,
                        state, progress_json, error_class, error_message, created_at_ms,
                        updated_at_ms, completed_at_ms, attempt_count, queue_sequence, final_path,
                        final_size_bytes, request_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', NULL, NULL, NULL, ?7, ?7, NULL, 1, ?8, NULL, NULL, ?9)",
                    params![
                        id.as_str(),
                        request.canonical_url,
                        format,
                        quality,
                        path_text(&request.destination, path)?,
                        request.name,
                        now,
                        sequence,
                        request_json,
                    ],
                )
                .map_err(|source| database_error("insert-job", path, source))?;
            transaction
                .commit()
                .map_err(|source| database_error("commit-insert-job", path, source))?;
            load_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn get_job(&self, id: JobId) -> Result<Option<JobRecord>, StorageError> {
        self.run("get-job", move |connection, path| {
            load_optional_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn list_jobs(
        &self,
        history_only: bool,
    ) -> Result<Vec<JobRecord>, StorageError> {
        self.run("list-jobs", move |connection, path| {
            let sql = if history_only {
                "SELECT id FROM jobs WHERE state IN ('completed', 'cancelled', 'failed') ORDER BY created_at_ms DESC, id DESC"
            } else {
                "SELECT id FROM jobs ORDER BY created_at_ms ASC, id ASC"
            };
            let mut statement = connection
                .prepare(sql)
                .map_err(|source| database_error("prepare-list-jobs", path, source))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|source| database_error("query-list-jobs", path, source))?;
            let mut ids = Vec::new();
            for row in rows {
                let value =
                    row.map_err(|source| database_error("read-list-job-id", path, source))?;
                ids.push(parse_job_id(&value, path)?);
            }
            drop(statement);
            ids.into_iter()
                .map(|id| load_job(connection, path, &id))
                .collect()
        })
        .await
    }

    pub(crate) async fn claim_next(&self) -> Result<Option<JobRecord>, StorageError> {
        self.run("claim-next-job", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-claim-job", path, source))?;
            let id = transaction
                .query_row(
                    "SELECT id FROM jobs WHERE state = 'queued' ORDER BY queue_sequence ASC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|source| database_error("select-next-job", path, source))?;
            let Some(id) = id else {
                transaction
                    .commit()
                    .map_err(|source| database_error("commit-empty-claim", path, source))?;
                return Ok(None);
            };
            let id = parse_job_id(&id, path)?;
            transition_in_transaction(
                &transaction,
                path,
                &id,
                JobState::Analyzing,
                None,
                None,
                None,
            )?;
            transaction
                .commit()
                .map_err(|source| database_error("commit-claim-job", path, source))?;
            load_optional_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn save_progress(
        &self,
        id: JobId,
        state: Option<JobState>,
        progress: Option<JobProgress>,
    ) -> Result<JobRecord, StorageError> {
        self.run("save-job-progress", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-save-progress", path, source))?;
            let current = load_state(&transaction, path, &id)?;
            if let Some(next) = state
                && current != next
            {
                current.validate_transition(next).map_err(|source| {
                    StorageError::InvalidData {
                        path: bounded_path(path),
                        reason: source.to_string(),
                    }
                })?;
            }
            let next = state.unwrap_or(current);
            let progress_json = progress
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|source| StorageError::InvalidData {
                    path: bounded_path(path),
                    reason: format!("could not serialize progress: {source}"),
                })?;
            transaction
                .execute(
                    "UPDATE jobs SET state = ?2, progress_json = COALESCE(?3, progress_json), updated_at_ms = ?4 WHERE id = ?1",
                    params![id.as_str(), next.as_str(), progress_json, now_ms()],
                )
                .map_err(|source| database_error("update-job-progress", path, source))?;
            transaction
                .commit()
                .map_err(|source| database_error("commit-job-progress", path, source))?;
            load_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn finish_job(
        &self,
        id: JobId,
        state: JobState,
        failure: Option<JobFailure>,
        output: Option<FinalOutput>,
    ) -> Result<JobRecord, StorageError> {
        self.run("finish-job", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-finish-job", path, source))?;
            let current = load_state(&transaction, path, &id)?;
            if current != state {
                current.validate_transition(state).map_err(|source| {
                    StorageError::InvalidData {
                        path: bounded_path(path),
                        reason: source.to_string(),
                    }
                })?;
            }
            let now = now_ms();
            let completed = state.is_terminal().then_some(now);
            let (error_class, error_message) = failure.map_or((None, None), |failure| {
                (
                    Some(failure.class.as_str()),
                    Some(bound_text(&failure.message, MAX_ERROR_CHARS)),
                )
            });
            let (final_path, final_size) = match output {
                Some(output) => (
                    Some(path_text(&output.path, path)?),
                    Some(i64::try_from(output.size_bytes).map_err(|_| {
                        StorageError::InvalidData {
                            path: bounded_path(path),
                            reason: "final output size exceeds SQLite integer range".to_owned(),
                        }
                    })?),
                ),
                None => (None, None),
            };
            transaction
                .execute(
                    "UPDATE jobs SET state = ?2, error_class = ?3, error_message = ?4,
                     updated_at_ms = ?5, completed_at_ms = ?6, final_path = ?7,
                     final_size_bytes = ?8, progress_json = CASE WHEN ?2 = 'completed' THEN progress_json ELSE progress_json END
                     WHERE id = ?1",
                    params![
                        id.as_str(),
                        state.as_str(),
                        error_class,
                        error_message,
                        now,
                        completed,
                        final_path,
                        final_size,
                    ],
                )
                .map_err(|source| database_error("update-finished-job", path, source))?;
            if matches!(state, JobState::Completed | JobState::Cancelled) {
                transaction
                    .execute("DELETE FROM owned_paths WHERE job_id = ?1", [id.as_str()])
                    .map_err(|source| database_error("clear-finished-owned-paths", path, source))?;
            }
            transaction
                .commit()
                .map_err(|source| database_error("commit-finished-job", path, source))?;
            load_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn transition_stopped(
        &self,
        id: JobId,
        state: JobState,
    ) -> Result<JobRecord, StorageError> {
        self.run("transition-stopped-job", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-transition-stopped", path, source))?;
            transition_in_transaction(&transaction, path, &id, state, None, None, None)?;
            transaction
                .commit()
                .map_err(|source| database_error("commit-transition-stopped", path, source))?;
            load_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn requeue(
        &self,
        id: JobId,
        kind: RequeueKind,
    ) -> Result<JobRecord, StorageError> {
        self.run("requeue-job", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-requeue-job", path, source))?;
            let current = load_state(&transaction, path, &id)?;
            let allowed = match kind {
                RequeueKind::Resume => {
                    matches!(current, JobState::Paused | JobState::Interrupted)
                }
                RequeueKind::Retry => {
                    matches!(current, JobState::Failed | JobState::Cancelled)
                }
            };
            if !allowed {
                return Err(StorageError::InvalidData {
                    path: bounded_path(path),
                    reason: format!("cannot requeue a job in `{current}` state"),
                });
            }
            current
                .validate_transition(JobState::Queued)
                .map_err(|source| StorageError::InvalidData {
                    path: bounded_path(path),
                    reason: source.to_string(),
                })?;
            let sequence = take_queue_sequence(&transaction, path)?;
            transaction
                .execute(
                    "UPDATE jobs SET state = 'queued', updated_at_ms = ?2, completed_at_ms = NULL,
                     attempt_count = attempt_count + 1, queue_sequence = ?3, error_class = NULL,
                     error_message = NULL, final_path = NULL, final_size_bytes = NULL
                     WHERE id = ?1",
                    params![id.as_str(), now_ms(), sequence],
                )
                .map_err(|source| database_error("update-requeued-job", path, source))?;
            transaction
                .commit()
                .map_err(|source| database_error("commit-requeued-job", path, source))?;
            load_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn recover_active(&self) -> Result<Vec<JobRecord>, StorageError> {
        self.run("recover-active-jobs", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-recover-active", path, source))?;
            let mut statement = transaction
                .prepare(
                    "SELECT id FROM jobs WHERE state IN ('analyzing', 'downloading', 'merging', 'converting') ORDER BY queue_sequence",
                )
                .map_err(|source| database_error("prepare-recover-active", path, source))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|source| database_error("query-recover-active", path, source))?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(parse_job_id(
                    &row.map_err(|source| database_error("read-recover-id", path, source))?,
                    path,
                )?);
            }
            drop(statement);
            for id in &ids {
                transaction
                    .execute(
                        "UPDATE jobs SET state = 'interrupted', updated_at_ms = ?2,
                         error_class = CASE WHEN EXISTS(
                             SELECT 1 FROM jobs AS checked WHERE checked.id = ?1
                         ) THEN 'internal' ELSE error_class END,
                         error_message = 'previous process ended before the job reached a terminal state; explicit resume is required'
                         WHERE id = ?1",
                        params![id.as_str(), now_ms()],
                    )
                    .map_err(|source| database_error("mark-job-interrupted", path, source))?;
            }
            transaction
                .commit()
                .map_err(|source| database_error("commit-recover-active", path, source))?;
            ids.into_iter()
                .map(|id| load_job(connection, path, &id))
                .collect()
        })
        .await
    }

    pub(crate) async fn set_recovery_failure(
        &self,
        id: JobId,
        failure: JobFailure,
    ) -> Result<JobRecord, StorageError> {
        self.run("set-recovery-failure", move |connection, path| {
            let state = connection
                .query_row(
                    "SELECT state FROM jobs WHERE id = ?1",
                    [id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|source| database_error("read-recovery-state", path, source))?;
            let state = JobState::parse(&state).map_err(|source| invalid_data(path, source))?;
            if !matches!(state, JobState::Paused | JobState::Interrupted) {
                return Err(StorageError::InvalidData {
                    path: bounded_path(path),
                    reason: format!(
                        "cannot attach a recovery diagnostic to job `{id}` in `{state}` state"
                    ),
                });
            }
            connection
                .execute(
                    "UPDATE jobs SET error_class = ?2, error_message = ?3, updated_at_ms = ?4
                     WHERE id = ?1",
                    params![
                        id.as_str(),
                        failure.class.as_str(),
                        bound_text(&failure.message, MAX_ERROR_CHARS),
                        now_ms(),
                    ],
                )
                .map_err(|source| database_error("write-recovery-diagnostic", path, source))?;
            load_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn replace_owned_paths(
        &self,
        id: JobId,
        paths: Vec<PathBuf>,
    ) -> Result<JobRecord, StorageError> {
        self.run("replace-owned-paths", move |connection, path| {
            let transaction = connection
                .transaction()
                .map_err(|source| database_error("begin-owned-paths", path, source))?;
            transaction
                .execute("DELETE FROM owned_paths WHERE job_id = ?1", [id.as_str()])
                .map_err(|source| database_error("clear-owned-paths", path, source))?;
            for owned in paths {
                transaction
                    .execute(
                        "INSERT INTO owned_paths(job_id, path, kind) VALUES (?1, ?2, 'partial')",
                        params![id.as_str(), path_text(&owned, path)?],
                    )
                    .map_err(|source| database_error("insert-owned-path", path, source))?;
            }
            transaction
                .commit()
                .map_err(|source| database_error("commit-owned-paths", path, source))?;
            load_job(connection, path, &id)
        })
        .await
    }

    pub(crate) async fn delete_completed(&self, id: JobId) -> Result<bool, StorageError> {
        self.run("delete-completed-history", move |connection, path| {
            let changed = connection
                .execute(
                    "DELETE FROM jobs WHERE id = ?1 AND state = 'completed'",
                    [id.as_str()],
                )
                .map_err(|source| database_error("delete-completed-history", path, source))?;
            Ok(changed == 1)
        })
        .await
    }

    pub(crate) async fn settings(&self) -> Result<EngineSettings, StorageError> {
        self.run("read-settings", |connection, path| {
            load_settings(connection, path)
        })
        .await
    }

    pub(crate) async fn update_settings(
        &self,
        patch: SettingsPatch,
    ) -> Result<EngineSettings, StorageError> {
        self.run("update-settings", move |connection, path| {
            let mut settings = load_settings(connection, path)?;
            if let Some(destination) = patch.default_destination {
                settings.default_destination = destination;
            }
            if let Some(concurrency) = patch.queue_concurrency {
                settings.queue_concurrency = concurrency;
            }
            if let Some(preference) = patch.update_preference {
                settings.update_preference = preference;
            }
            if let Some(output) = patch.last_output {
                settings.last_output = output;
            }
            let destination = settings
                .default_destination
                .as_ref()
                .map(|value| path_text(value, path))
                .transpose()?;
            let (format, quality) = output_parts(settings.last_output);
            connection
                .execute(
                    "UPDATE settings SET default_destination = ?1, queue_concurrency = ?2,
                     update_preference = ?3, last_output_format = ?4,
                     last_output_quality = ?5, updated_at_ms = ?6 WHERE singleton = 1",
                    params![
                        destination,
                        settings.queue_concurrency.get(),
                        settings.update_preference.as_str(),
                        format,
                        quality,
                        now_ms(),
                    ],
                )
                .map_err(|source| database_error("write-settings", path, source))?;
            Ok(settings)
        })
        .await
    }
}

fn open_connection(path: &Path) -> Result<Connection, StorageError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
    .map_err(|source| database_error("open", path, source))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|source| database_error("configure-busy-timeout", path, source))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|source| database_error("enable-foreign-keys", path, source))?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|source| database_error("configure-synchronous", path, source))?;
    let mut journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(|source| database_error("read-journal-mode", path, source))?;
    if !journal.eq_ignore_ascii_case("wal") {
        journal = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .map_err(|source| database_error("configure-wal", path, source))?;
    }
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(StorageError::InvalidData {
            path: bounded_path(path),
            reason: format!("SQLite refused WAL journaling and returned `{journal}`"),
        });
    }
    let integrity = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .map_err(|source| StorageError::Corrupt {
            path: bounded_path(path),
            reason: bound_text(&source.to_string(), 512),
        })?;
    if integrity != "ok" {
        return Err(StorageError::Corrupt {
            path: bounded_path(path),
            reason: bound_text(&integrity, 512),
        });
    }
    Ok(connection)
}

fn migrate(connection: &mut Connection, path: &Path) -> Result<(), StorageError> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|source| database_error("read-schema-version", path, source))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(StorageError::FutureSchema {
            path: bounded_path(path),
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    for migration in MIGRATIONS.iter().filter(|item| item.version > version) {
        if migration.destructive && path.exists() {
            backup_database(path, migration.version)?;
        }
        apply_migration(connection, path, migration)?;
    }
    Ok(())
}

fn backup_database(path: &Path, target_version: i64) -> Result<(), StorageError> {
    let backup = path.with_extension(format!("sqlite3.backup-before-v{target_version}"));
    fs::copy(path, backup).map_err(|source| StorageError::Backup {
        path: bounded_path(path),
        source,
    })?;
    Ok(())
}

fn apply_migration(
    connection: &mut Connection,
    path: &Path,
    migration: &Migration,
) -> Result<(), StorageError> {
    let transaction = connection
        .transaction()
        .map_err(|source| database_error("begin-migration", path, source))?;
    transaction
        .execute_batch(migration.sql)
        .map_err(|source| database_error("apply-migration", path, source))?;
    transaction
        .execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) VALUES (?1, ?2)",
            params![migration.version, now_ms()],
        )
        .map_err(|source| database_error("record-migration", path, source))?;
    transaction
        .pragma_update(None, "user_version", migration.version)
        .map_err(|source| database_error("write-schema-version", path, source))?;
    transaction
        .commit()
        .map_err(|source| database_error("commit-migration", path, source))
}

fn load_optional_job(
    connection: &Connection,
    path: &Path,
    id: &JobId,
) -> Result<Option<JobRecord>, StorageError> {
    let raw = connection
        .query_row(
            "SELECT id, canonical_url, output_format, output_quality, destination, output_name,
             state, progress_json, error_class, error_message, created_at_ms, updated_at_ms,
             completed_at_ms, attempt_count, final_path, final_size_bytes
             FROM jobs WHERE id = ?1",
            [id.as_str()],
            raw_job_from_row,
        )
        .optional()
        .map_err(|source| database_error("read-job", path, source))?;
    raw.map(|raw| materialize_job(connection, path, raw))
        .transpose()
}

fn load_job(connection: &Connection, path: &Path, id: &JobId) -> Result<JobRecord, StorageError> {
    load_optional_job(connection, path, id)?.ok_or_else(|| StorageError::InvalidData {
        path: bounded_path(path),
        reason: format!("job `{id}` disappeared during a database transaction"),
    })
}

struct RawJob {
    id: String,
    canonical_url: String,
    output_format: String,
    output_quality: i64,
    destination: String,
    output_name: Option<String>,
    state: String,
    progress_json: Option<String>,
    error_class: Option<String>,
    error_message: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
    completed_at_ms: Option<i64>,
    attempt_count: i64,
    final_path: Option<String>,
    final_size_bytes: Option<i64>,
}

fn raw_job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawJob> {
    Ok(RawJob {
        id: row.get(0)?,
        canonical_url: row.get(1)?,
        output_format: row.get(2)?,
        output_quality: row.get(3)?,
        destination: row.get(4)?,
        output_name: row.get(5)?,
        state: row.get(6)?,
        progress_json: row.get(7)?,
        error_class: row.get(8)?,
        error_message: row.get(9)?,
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
        completed_at_ms: row.get(12)?,
        attempt_count: row.get(13)?,
        final_path: row.get(14)?,
        final_size_bytes: row.get(15)?,
    })
}

fn materialize_job(
    connection: &Connection,
    path: &Path,
    raw: RawJob,
) -> Result<JobRecord, StorageError> {
    let id = parse_job_id(&raw.id, path)?;
    let output = parse_output(&raw.output_format, raw.output_quality, path)?;
    let state = JobState::parse(&raw.state).map_err(|source| invalid_data(path, source))?;
    let progress = raw
        .progress_json
        .map(|value| serde_json::from_str::<JobProgress>(&value))
        .transpose()
        .map_err(|source| invalid_data(path, source))?;
    let error = match (raw.error_class, raw.error_message) {
        (Some(class), Some(message)) => Some(JobFailure {
            class: JobErrorClass::parse(&class).map_err(|source| invalid_data(path, source))?,
            message,
        }),
        (None, None) => None,
        _ => {
            return Err(StorageError::InvalidData {
                path: bounded_path(path),
                reason: format!("job `{id}` has an incomplete persisted error"),
            });
        }
    };
    let final_output = match (raw.final_path, raw.final_size_bytes) {
        (Some(final_path), Some(size)) => Some(FinalOutput {
            path: PathBuf::from(final_path),
            size_bytes: u64::try_from(size).map_err(|_| StorageError::InvalidData {
                path: bounded_path(path),
                reason: format!("job `{id}` has an invalid final output size"),
            })?,
            output,
        }),
        (None, None) => None,
        _ => {
            return Err(StorageError::InvalidData {
                path: bounded_path(path),
                reason: format!("job `{id}` has incomplete final output metadata"),
            });
        }
    };
    let attempt_count =
        u32::try_from(raw.attempt_count).map_err(|_| StorageError::InvalidData {
            path: bounded_path(path),
            reason: format!("job `{id}` has an invalid attempt count"),
        })?;
    let mut statement = connection
        .prepare("SELECT path FROM owned_paths WHERE job_id = ?1 ORDER BY path")
        .map_err(|source| database_error("prepare-owned-paths", path, source))?;
    let rows = statement
        .query_map([id.as_str()], |row| row.get::<_, String>(0))
        .map_err(|source| database_error("query-owned-paths", path, source))?;
    let mut owned_partial_paths = Vec::new();
    for row in rows {
        owned_partial_paths.push(PathBuf::from(
            row.map_err(|source| database_error("read-owned-path", path, source))?,
        ));
    }
    let mut record = JobRecord {
        id,
        request: JobRequest {
            canonical_url: raw.canonical_url,
            output,
            destination: PathBuf::from(raw.destination),
            name: raw.output_name,
        },
        state,
        progress,
        error,
        created_at_ms: raw.created_at_ms,
        updated_at_ms: raw.updated_at_ms,
        completed_at_ms: raw.completed_at_ms,
        attempt_count,
        final_output,
        output_availability: OutputAvailability::NotApplicable,
        owned_partial_paths,
        destination_available: false,
    };
    record.refresh_filesystem_status();
    Ok(record)
}

fn load_state(
    transaction: &Transaction<'_>,
    path: &Path,
    id: &JobId,
) -> Result<JobState, StorageError> {
    let state = transaction
        .query_row(
            "SELECT state FROM jobs WHERE id = ?1",
            [id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|source| database_error("read-job-state", path, source))?
        .ok_or_else(|| StorageError::InvalidData {
            path: bounded_path(path),
            reason: format!("job `{id}` does not exist"),
        })?;
    JobState::parse(&state).map_err(|source| invalid_data(path, source))
}

fn transition_in_transaction(
    transaction: &Transaction<'_>,
    path: &Path,
    id: &JobId,
    next: JobState,
    progress: Option<&JobProgress>,
    failure: Option<&JobFailure>,
    completed_at_ms: Option<i64>,
) -> Result<(), StorageError> {
    let current = load_state(transaction, path, id)?;
    if current != next {
        current
            .validate_transition(next)
            .map_err(|source| invalid_data(path, source))?;
    }
    let progress = progress
        .map(serde_json::to_string)
        .transpose()
        .map_err(|source| invalid_data(path, source))?;
    let (error_class, error_message) = failure.map_or((None, None), |failure| {
        (
            Some(failure.class.as_str()),
            Some(bound_text(&failure.message, MAX_ERROR_CHARS)),
        )
    });
    transaction
        .execute(
            "UPDATE jobs SET state = ?2, progress_json = COALESCE(?3, progress_json),
             error_class = COALESCE(?4, error_class), error_message = COALESCE(?5, error_message),
             updated_at_ms = ?6, completed_at_ms = ?7 WHERE id = ?1",
            params![
                id.as_str(),
                next.as_str(),
                progress,
                error_class,
                error_message,
                now_ms(),
                completed_at_ms,
            ],
        )
        .map_err(|source| database_error("transition-job", path, source))?;
    Ok(())
}

fn take_queue_sequence(transaction: &Transaction<'_>, path: &Path) -> Result<i64, StorageError> {
    let sequence = transaction
        .query_row(
            "SELECT next_sequence FROM queue_meta WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|source| database_error("read-queue-sequence", path, source))?;
    let next = sequence
        .checked_add(1)
        .ok_or_else(|| StorageError::InvalidData {
            path: bounded_path(path),
            reason: "queue sequence exhausted the SQLite integer range".to_owned(),
        })?;
    transaction
        .execute(
            "UPDATE queue_meta SET next_sequence = ?1 WHERE singleton = 1",
            [next],
        )
        .map_err(|source| database_error("advance-queue-sequence", path, source))?;
    Ok(sequence)
}

fn load_settings(connection: &Connection, path: &Path) -> Result<EngineSettings, StorageError> {
    let raw = connection
        .query_row(
            "SELECT default_destination, queue_concurrency, update_preference,
             last_output_format, last_output_quality FROM settings WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .map_err(|source| database_error("read-settings", path, source))?;
    let concurrency_value = u8::try_from(raw.1).map_err(|_| StorageError::InvalidData {
        path: bounded_path(path),
        reason: format!("invalid persisted queue concurrency `{}`", raw.1),
    })?;
    Ok(EngineSettings {
        default_destination: raw.0.map(PathBuf::from),
        queue_concurrency: QueueConcurrency::try_from(concurrency_value)
            .map_err(|source| invalid_data(path, SettingsValueError::from(source)))?,
        update_preference: UpdatePreference::parse(&raw.2)
            .map_err(|source| invalid_data(path, source))?,
        last_output: parse_output(&raw.3, raw.4, path)?,
    })
}

fn output_parts(output: OutputSelection) -> (&'static str, i64) {
    match output {
        OutputSelection::Mp3(quality) => ("mp3", i64::from(quality.bitrate_kbps())),
        OutputSelection::Mp4(quality) => ("mp4", i64::from(quality.height())),
    }
}

fn parse_output(format: &str, quality: i64, path: &Path) -> Result<OutputSelection, StorageError> {
    match format {
        "mp3" => {
            let quality = u16::try_from(quality)
                .map_err(|_| invalid_data(path, SettingsValueError::OutputQuality(quality)))?;
            AudioQuality::try_from(quality)
                .map(OutputSelection::Mp3)
                .map_err(|_| {
                    invalid_data(path, SettingsValueError::OutputQuality(i64::from(quality)))
                })
        }
        "mp4" => {
            let quality = u32::try_from(quality)
                .map_err(|_| invalid_data(path, SettingsValueError::OutputQuality(quality)))?;
            VideoQuality::try_from(quality)
                .map(OutputSelection::Mp4)
                .map_err(|_| {
                    invalid_data(path, SettingsValueError::OutputQuality(i64::from(quality)))
                })
        }
        other => Err(invalid_data(
            path,
            SettingsValueError::OutputFormat(other.chars().take(64).collect()),
        )),
    }
}

fn parse_job_id(value: &str, path: &Path) -> Result<JobId, StorageError> {
    JobId::parse(value).map_err(|source| invalid_data(path, source))
}

fn path_text(value: &Path, database_path: &Path) -> Result<String, StorageError> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| StorageError::InvalidData {
            path: bounded_path(database_path),
            reason: format!(
                "path `{}` is not valid Unicode and cannot enter the stable database contract",
                bounded_path(value)
            ),
        })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn bound_text(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn invalid_data(path: &Path, source: impl std::fmt::Display) -> StorageError {
    StorageError::InvalidData {
        path: bounded_path(path),
        reason: bound_text(&source.to_string(), 1_024),
    }
}

fn database_error(operation: &'static str, path: &Path, source: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::SqliteFailure(error, detail) = &source
        && matches!(
            error.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        )
    {
        return StorageError::Corrupt {
            path: bounded_path(path),
            reason: detail
                .as_deref()
                .map_or_else(|| source.to_string(), str::to_owned)
                .chars()
                .take(512)
                .collect(),
        };
    }
    StorageError::Database {
        operation,
        path: bounded_path(path),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::{Connection, OptionalExtension};
    use tempfile::tempdir;

    use super::{
        CURRENT_SCHEMA_VERSION, MIGRATION_1, Migration, SqliteStore, StorageError, apply_migration,
    };
    use crate::{
        download::{AudioQuality, JobId, OutputSelection},
        jobs::{JobRequest, JobState},
    };

    #[tokio::test]
    async fn fresh_database_runs_every_numbered_migration() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempdir()?;
        let store = SqliteStore::open(directory.path().to_path_buf()).await?;
        let connection = Connection::open(store.database_path())?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        let migrations: i64 =
            connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })?;
        assert_eq!(migrations, CURRENT_SCHEMA_VERSION);
        Ok(())
    }

    #[tokio::test]
    async fn historical_v1_fixture_migrates_without_losing_jobs()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let database = directory.path().join("jobs.sqlite3");
        let connection = Connection::open(&database)?;
        connection.execute_batch(MIGRATION_1)?;
        connection.execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) VALUES (1, 1)",
            [],
        )?;
        connection.pragma_update(None, "user_version", 1_i64)?;
        connection.execute(
            "INSERT INTO jobs(
                id, canonical_url, output_format, output_quality, destination, output_name, state,
                created_at_ms, updated_at_ms, attempt_count, queue_sequence
            ) VALUES (
                '018f22d4-5c32-7cc0-8000-000000000001',
                'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
                'mp3', 192, 'fixture', NULL, 'queued', 1, 1, 1, 1
            )",
            [],
        )?;
        drop(connection);
        let store = SqliteStore::open(directory.path().to_path_buf()).await?;
        let jobs = store.list_jobs(false).await?;
        assert_eq!(jobs.len(), 1);
        let migrated = Connection::open(store.database_path())?;
        let version: i64 = migrated.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn interrupted_migration_rolls_back_schema_and_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let database = directory.path().join("rollback.sqlite3");
        let mut connection = Connection::open(&database)?;
        let migration = Migration {
            version: 1,
            destructive: false,
            sql: "CREATE TABLE should_rollback(value INTEGER); INVALID SQL;",
        };
        assert!(apply_migration(&mut connection, &database, &migration).is_err());
        let table: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE name = 'should_rollback'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        assert!(table.is_none());
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 0);
        Ok(())
    }

    #[tokio::test]
    async fn future_corrupt_and_locked_databases_fail_actionably()
    -> Result<(), Box<dyn std::error::Error>> {
        let locked_directory = tempdir()?;
        let store = SqliteStore::open(locked_directory.path().to_path_buf()).await?;
        let second = SqliteStore::open(locked_directory.path().to_path_buf()).await;
        assert!(matches!(second, Err(StorageError::Locked { .. })));
        drop(store);

        let future_directory = tempdir()?;
        let future_database = future_directory.path().join("jobs.sqlite3");
        let connection = Connection::open(&future_database)?;
        connection.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)?;
        drop(connection);
        let future = SqliteStore::open(future_directory.path().to_path_buf()).await;
        assert!(matches!(future, Err(StorageError::FutureSchema { .. })));

        let corrupt_directory = tempdir()?;
        fs::write(corrupt_directory.path().join("jobs.sqlite3"), b"not sqlite")?;
        let corrupt = SqliteStore::open(corrupt_directory.path().to_path_buf()).await;
        assert!(matches!(corrupt, Err(StorageError::Corrupt { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn startup_recovery_interrupts_every_process_active_stage_and_leaves_queued_work_passive()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let output = directory.path().join("output");
        fs::create_dir_all(&output)?;
        let store = SqliteStore::open(directory.path().join("data")).await?;
        let states = [
            JobState::Queued,
            JobState::Analyzing,
            JobState::Downloading,
            JobState::Merging,
            JobState::Converting,
        ];
        let mut ids = Vec::new();
        for state in states {
            let id = JobId::new_v7();
            store
                .insert_job(
                    id.clone(),
                    JobRequest {
                        canonical_url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
                        output: OutputSelection::Mp3(AudioQuality::try_from(192)?),
                        destination: output.clone(),
                        name: None,
                    },
                )
                .await?;
            let connection = Connection::open(store.database_path())?;
            connection.execute(
                "UPDATE jobs SET state = ?2 WHERE id = ?1",
                rusqlite::params![id.as_str(), state.as_str()],
            )?;
            ids.push((id, state));
        }
        let recovered = store.recover_active().await?;
        assert_eq!(recovered.len(), 4);
        for (id, previous) in ids {
            let state = store
                .get_job(id)
                .await?
                .map(|record| record.state)
                .ok_or("fixture job disappeared")?;
            if previous == JobState::Queued {
                assert_eq!(state, JobState::Queued);
            } else {
                assert_eq!(state, JobState::Interrupted);
            }
        }
        Ok(())
    }
}
