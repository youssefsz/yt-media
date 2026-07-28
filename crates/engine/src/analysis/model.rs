//! Bounded public media and format contracts.

use serde::Serialize;

use super::MediaUrl;

/// A validated `YouTube` video identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MediaId(pub(crate) String);

impl MediaId {
    /// Returns the eleven-character `YouTube` identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated yt-dlp format identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct FormatId(pub(crate) String);

impl FormatId {
    /// Returns the extractor-provided identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A positive, bounded media duration in milliseconds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Duration(pub(crate) u64);

impl Duration {
    /// Returns the duration in milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }
}

/// One bounded thumbnail candidate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Thumbnail {
    /// Validated HTTP(S) thumbnail URL.
    pub url: String,
    /// Pixel width, when supplied.
    pub width: Option<u32>,
    /// Pixel height, when supplied.
    pub height: Option<u32>,
}

/// The output family represented by a normalized format option.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputKind {
    /// MP3 audio output.
    Mp3,
    /// MP4 video output.
    Mp4,
}

/// Recognized video codec family.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoCodecFamily {
    /// H.264/AVC, the v1 MP4 compatibility target.
    H264,
    /// VP9.
    Vp9,
    /// AV1.
    Av1,
    /// Another bounded extractor codec name.
    Other,
}

/// A bounded video codec descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VideoCodecDescriptor {
    /// Normalized extractor codec name.
    pub name: String,
    /// Recognized codec family.
    pub family: VideoCodecFamily,
}

/// Recognized audio codec family.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioCodecFamily {
    /// AAC or MPEG-4 Audio, the v1 MP4 compatibility target.
    Aac,
    /// Opus.
    Opus,
    /// Vorbis.
    Vorbis,
    /// MP3.
    Mp3,
    /// Another bounded extractor codec name.
    Other,
}

/// A bounded audio codec descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AudioCodecDescriptor {
    /// Normalized extractor codec name.
    pub name: String,
    /// Recognized codec family.
    pub family: AudioCodecFamily,
}

/// Recognized source container family.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerFamily {
    /// MP4.
    Mp4,
    /// M4A.
    M4a,
    /// `WebM`.
    Webm,
    /// Another bounded extractor container name.
    Other,
}

/// A bounded source container descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerDescriptor {
    /// Normalized extractor extension or container name.
    pub name: String,
    /// Recognized container family.
    pub family: ContainerFamily,
}

/// One selected source stream or progressive source format.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceFormat {
    /// yt-dlp format identity.
    pub format_id: FormatId,
    /// Source container.
    pub container: ContainerDescriptor,
    /// Video codec when the source carries video.
    pub video_codec: Option<VideoCodecDescriptor>,
    /// Audio codec when the source carries audio.
    pub audio_codec: Option<AudioCodecDescriptor>,
}

/// Work needed to produce the v1 H.264/AAC MP4 compatibility target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityWork {
    /// One progressive H.264/AAC source needs no merge or transcode.
    None,
    /// Compatible H.264 and AAC sources must be merged.
    Merge,
    /// The video source must be transcoded to H.264.
    VideoTranscode,
    /// The audio source must be transcoded to AAC.
    AudioTranscode,
    /// Both video and audio sources must be transcoded.
    VideoAndAudioTranscode,
}

/// One normalized output choice.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FormatOption {
    /// An MP3 bitrate choice backed by a selected audio-bearing source.
    Mp3 {
        /// Target constant bitrate in kilobits per second.
        bitrate_kbps: u16,
        /// Source retained for Plan 03 conversion.
        source: SourceFormat,
    },
    /// An MP4 source-height choice.
    Mp4 {
        /// Source height in pixels.
        height: u32,
        /// Source width in pixels, when known.
        width: Option<u32>,
        /// Source frames per second, when known.
        fps: Option<f64>,
        /// Combined selected-source bytes when every component size is available.
        estimated_size_bytes: Option<u64>,
        /// Selected video-bearing source.
        video_source: SourceFormat,
        /// Selected audio-bearing source.
        audio_source: SourceFormat,
        /// Compatibility work Plan 03 will need to perform.
        compatibility: CompatibilityWork,
    },
}

impl FormatOption {
    /// Returns the represented output family.
    #[must_use]
    pub const fn output_kind(&self) -> OutputKind {
        match self {
            Self::Mp3 { .. } => OutputKind::Mp3,
            Self::Mp4 { .. } => OutputKind::Mp4,
        }
    }
}

/// Normalized, bounded information for one public on-demand video.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MediaInfo {
    /// Validated media identity.
    pub id: MediaId,
    /// Canonical single-video URL.
    pub url: MediaUrl,
    /// Bounded title.
    pub title: String,
    /// Bounded uploader name, when present.
    pub uploader: Option<String>,
    /// Positive bounded duration.
    pub duration: Duration,
    /// Non-negative view count, when present.
    pub view_count: Option<u64>,
    /// Valid `YYYY-MM-DD` upload date, when present.
    pub upload_date: Option<String>,
    /// At most twenty validated thumbnails.
    pub thumbnails: Vec<Thumbnail>,
    /// MP3 choices followed by descending distinct MP4 source heights.
    pub formats: Vec<FormatOption>,
    /// Bounded yt-dlp warnings retained as diagnostics.
    pub warnings: Vec<String>,
}
