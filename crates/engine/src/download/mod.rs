//! Engine-owned download, conversion, naming, progress, and lifecycle orchestration.

mod model;
mod name;
mod progress;
mod service;

pub use model::{
    AudioQuality, CompletionHandle, Destination, DownloadError, DownloadRequest,
    DownloadRequestError, DownloadResult, JobControls, JobEvent, JobEventKind, JobEventStream,
    JobId, JobProgress, JobStage, OutputName, OutputSelection, StartedDownload, VideoQuality,
};
pub use name::sanitize_output_stem;
pub use service::{DownloadService, DownloadToolError, DownloadTools};
