//! Public media-analysis contracts and the private yt-dlp adapter.

mod model;
mod url;
mod ytdlp;

pub use model::{
    AudioCodecDescriptor, AudioCodecFamily, CompatibilityWork, ContainerDescriptor,
    ContainerFamily, Duration, FormatId, FormatOption, MediaId, MediaInfo, OutputKind,
    SourceFormat, Thumbnail, VideoCodecDescriptor, VideoCodecFamily,
};
pub use url::{MediaUrl, MediaUrlError};
pub use ytdlp::{
    AnalysisDataError, AnalysisStream, AnalysisToolError, AnalysisTools, AnalyzeError, Analyzer,
    UnsupportedContent,
};
