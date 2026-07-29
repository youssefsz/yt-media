//! Durable engine-owned queue, settings, history, migration, and recovery.

mod model;
mod queue;
mod storage;

pub use model::{
    EngineSettings, FinalOutput, JobErrorClass, JobFailure, JobRecord, JobRequest, JobState,
    JobStateParseError, JobTransitionError, OutputAvailability, QueueConcurrency,
    QueueConcurrencyError, QueueEvent, QueueSnapshot, QueueSubscription, SettingsPatch,
    SettingsValueError, UpdatePreference,
};
pub use queue::{JobQueue, QueueError};
pub use storage::StorageError;
