//! Engine-owned download, conversion, naming, progress, and lifecycle orchestration.

mod model;
mod name;
mod progress;
mod service;

pub use model::{
    AudioQuality, CompletionHandle, Destination, DownloadError, DownloadRequest,
    DownloadRequestError, DownloadResult, JobControls, JobEvent, JobEventKind, JobEventStream,
    JobId, JobIdError, JobProgress, JobStage, OutputName, OutputSelection, StartedDownload,
    VideoQuality,
};
pub use name::sanitize_output_stem;
pub use service::{DownloadService, DownloadToolError, DownloadTools};
pub(crate) use service::{reconcile_owned_paths, remove_recorded_owned_paths};
