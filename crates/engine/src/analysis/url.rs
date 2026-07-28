//! Deterministic `YouTube` URL validation without network redirects.

use std::{fmt, str::FromStr};

use serde::{Serialize, Serializer};
use thiserror::Error;
use url::Url;

use super::MediaId;

const MAX_MEDIA_URL_BYTES: usize = 2_048;
const VIDEO_ID_LENGTH: usize = 11;
const CANONICAL_ORIGIN: &str = "https://www.youtube.com/watch?v=";

/// A validated, canonical public `YouTube` video URL.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MediaUrl {
    canonical: String,
    id: MediaId,
}

impl MediaUrl {
    /// Validates one URL and canonicalizes it to a single-video watch URL.
    ///
    /// Validation is entirely local and never follows redirects.
    ///
    /// # Errors
    ///
    /// Returns a typed error for oversized, malformed, authenticated, non-HTTP(S), unsupported,
    /// playlist-only, live-route, or invalid-identity input.
    pub fn parse(input: &str) -> Result<Self, MediaUrlError> {
        if input.len() > MAX_MEDIA_URL_BYTES {
            return Err(MediaUrlError::InputTooLong {
                bytes: input.len(),
                maximum: MAX_MEDIA_URL_BYTES,
            });
        }
        let parsed = Url::parse(input).map_err(MediaUrlError::Malformed)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(MediaUrlError::UnsupportedScheme {
                scheme: parsed.scheme().to_owned(),
            });
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(MediaUrlError::CredentialsForbidden);
        }
        if parsed.port().is_some() {
            return Err(MediaUrlError::UnsupportedPort);
        }
        reject_authentication_query(&parsed)?;

        let host = parsed
            .host_str()
            .ok_or(MediaUrlError::MissingHost)?
            .to_ascii_lowercase();
        let id = match host.as_str() {
            "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => {
                id_from_youtube_url(&parsed)?
            }
            "youtu.be" | "www.youtu.be" => id_from_short_url(&parsed)?,
            _ => return Err(MediaUrlError::UnsupportedHost { host }),
        };
        let canonical = format!("{CANONICAL_ORIGIN}{}", id.as_str());
        Ok(Self { canonical, id })
    }

    /// Returns the canonical single-video URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// Returns the validated video identity.
    #[must_use]
    pub const fn id(&self) -> &MediaId {
        &self.id
    }
}

impl fmt::Display for MediaUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MediaUrl {
    type Err = MediaUrlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for MediaUrl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

fn id_from_youtube_url(url: &Url) -> Result<MediaId, MediaUrlError> {
    let normalized_path = normalized_path(url.path());
    match normalized_path {
        "/watch" => {
            let video_values = query_values(url, "v");
            if video_values.is_empty() {
                if !query_values(url, "list").is_empty() {
                    return Err(MediaUrlError::PlaylistOnly);
                }
                return Err(MediaUrlError::MissingVideoId);
            }
            if video_values.len() != 1 {
                return Err(MediaUrlError::AmbiguousVideoId);
            }
            validate_video_id(&video_values[0])
        }
        "/playlist" => Err(MediaUrlError::PlaylistOnly),
        path if path == "/live" || path.starts_with("/live/") => Err(MediaUrlError::LiveRoute),
        "/shorts" => Err(MediaUrlError::MissingVideoId),
        path if path.starts_with("/shorts/") => {
            let remainder = &path["/shorts/".len()..];
            if remainder.is_empty() || remainder.contains('/') {
                return Err(MediaUrlError::MissingVideoId);
            }
            validate_video_id(remainder)
        }
        _ => Err(MediaUrlError::UnsupportedPath {
            path: url.path().to_owned(),
        }),
    }
}

fn id_from_short_url(url: &Url) -> Result<MediaId, MediaUrlError> {
    let path = url.path().trim_matches('/');
    if path.is_empty() {
        return Err(MediaUrlError::MissingVideoId);
    }
    if path.contains('/') {
        return Err(MediaUrlError::UnsupportedPath {
            path: url.path().to_owned(),
        });
    }
    validate_video_id(path)
}

fn normalized_path(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

fn query_values(url: &Url, name: &str) -> Vec<String> {
    url.query_pairs()
        .filter_map(|(key, value)| key.eq_ignore_ascii_case(name).then(|| value.into_owned()))
        .collect()
}

fn reject_authentication_query(url: &Url) -> Result<(), MediaUrlError> {
    const FORBIDDEN: [&str; 8] = [
        "cookie",
        "cookies",
        "cookies-from-browser",
        "username",
        "password",
        "user",
        "login",
        "auth",
    ];
    for (key, _) in url.query_pairs() {
        if key.starts_with("--")
            || FORBIDDEN
                .iter()
                .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
        {
            return Err(MediaUrlError::AuthenticationOptionForbidden {
                name: key.into_owned(),
            });
        }
    }
    Ok(())
}

fn validate_video_id(value: &str) -> Result<MediaId, MediaUrlError> {
    if value.len() != VIDEO_ID_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(MediaUrlError::InvalidVideoId);
    }
    Ok(MediaId(value.to_owned()))
}

/// A rejected media URL.
#[derive(Debug, Error)]
pub enum MediaUrlError {
    /// Input exceeded the pre-parser bound.
    #[error("media URL is {bytes} bytes; maximum is {maximum}")]
    InputTooLong {
        /// Observed byte count.
        bytes: usize,
        /// Maximum accepted bytes.
        maximum: usize,
    },
    /// URL syntax was malformed.
    #[error("media URL is malformed")]
    Malformed(#[source] url::ParseError),
    /// Scheme was not HTTP or HTTPS.
    #[error("URL scheme `{scheme}` is unsupported; use HTTP or HTTPS")]
    UnsupportedScheme {
        /// Rejected scheme.
        scheme: String,
    },
    /// URL contained user information.
    #[error("credentials in media URLs are forbidden")]
    CredentialsForbidden,
    /// URL contained a non-default port.
    #[error("custom ports are unsupported for media URLs")]
    UnsupportedPort,
    /// URL had no host.
    #[error("media URL has no host")]
    MissingHost,
    /// Host was outside the exact `YouTube` allowlist.
    #[error("host `{host}` is unsupported")]
    UnsupportedHost {
        /// Rejected normalized host.
        host: String,
    },
    /// Query attempted to express authentication or cookie behavior.
    #[error("authentication or cookie query option `{name}` is forbidden")]
    AuthenticationOptionForbidden {
        /// Rejected query name.
        name: String,
    },
    /// URL identified a playlist without one video.
    #[error("playlist-only URLs are unsupported; provide one video URL")]
    PlaylistOnly,
    /// URL used `YouTube`'s active-live route.
    #[error("active live-stream URLs are unsupported")]
    LiveRoute,
    /// URL used a `YouTube` route outside the v1 boundary.
    #[error("YouTube path `{path}` is unsupported")]
    UnsupportedPath {
        /// Rejected path.
        path: String,
    },
    /// No video identity was supplied.
    #[error("media URL is missing a video ID")]
    MissingVideoId,
    /// More than one video identity was supplied.
    #[error("media URL contains more than one video ID")]
    AmbiguousVideoId,
    /// Video identity was not exactly eleven safe ASCII characters.
    #[error("media URL contains an invalid video ID")]
    InvalidVideoId,
}

impl MediaUrlError {
    /// Returns whether the URL is well-formed but names content outside the v1 content boundary.
    #[must_use]
    pub const fn is_unsupported_content(&self) -> bool {
        matches!(self, Self::PlaylistOnly | Self::LiveRoute)
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaUrl, MediaUrlError};

    const ID: &str = "dQw4w9WgXcQ";
    const CANONICAL: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

    #[test]
    fn accepts_and_canonicalizes_watch_urls() -> Result<(), MediaUrlError> {
        let media = MediaUrl::parse("http://m.youtube.com/watch?feature=share&v=dQw4w9WgXcQ")?;
        assert_eq!(media.as_str(), CANONICAL);
        assert_eq!(media.id().as_str(), ID);
        Ok(())
    }

    #[test]
    fn accepts_short_and_shorts_urls() -> Result<(), MediaUrlError> {
        for input in [
            "https://youtu.be/dQw4w9WgXcQ?t=1",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ/",
        ] {
            assert_eq!(MediaUrl::parse(input)?.as_str(), CANONICAL);
        }
        Ok(())
    }

    #[test]
    fn treats_video_plus_playlist_as_one_video() -> Result<(), MediaUrlError> {
        let media =
            MediaUrl::parse("https://youtube.com/watch?list=PL_fixture&index=2&v=dQw4w9WgXcQ")?;
        assert_eq!(media.as_str(), CANONICAL);
        Ok(())
    }

    #[test]
    fn accepts_decoded_query_identity_but_not_encoded_path_identity() {
        assert!(
            MediaUrl::parse("https://youtube.com/watch?v=dQw4w9WgXcQ%20").is_err(),
            "decoded whitespace must not enter an identity"
        );
        assert!(
            MediaUrl::parse("https://youtube.com/shorts/dQw4w9WgX%63Q").is_err(),
            "percent-encoded path bytes are not canonical identities"
        );
    }

    #[test]
    fn rejects_playlist_only_and_live_routes() {
        assert!(matches!(
            MediaUrl::parse("https://youtube.com/playlist?list=PL_fixture"),
            Err(MediaUrlError::PlaylistOnly)
        ));
        assert!(matches!(
            MediaUrl::parse("https://youtube.com/watch?list=PL_fixture"),
            Err(MediaUrlError::PlaylistOnly)
        ));
        assert!(matches!(
            MediaUrl::parse("https://youtube.com/live/dQw4w9WgXcQ"),
            Err(MediaUrlError::LiveRoute)
        ));
    }

    #[test]
    fn rejects_malformed_deceptive_and_unsupported_hosts() {
        assert!(matches!(
            MediaUrl::parse("not a URL"),
            Err(MediaUrlError::Malformed(_))
        ));
        for input in [
            "https://youtube.com.attacker.example/watch?v=dQw4w9WgXcQ",
            "https://notyoutube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.example/watch?v=dQw4w9WgXcQ",
        ] {
            assert!(matches!(
                MediaUrl::parse(input),
                Err(MediaUrlError::UnsupportedHost { .. })
            ));
        }
    }

    #[test]
    fn rejects_non_http_credentials_ports_and_authentication_queries() {
        assert!(matches!(
            MediaUrl::parse("ftp://youtube.com/watch?v=dQw4w9WgXcQ"),
            Err(MediaUrlError::UnsupportedScheme { .. })
        ));
        assert!(matches!(
            MediaUrl::parse("https://user:secret@youtube.com/watch?v=dQw4w9WgXcQ"),
            Err(MediaUrlError::CredentialsForbidden)
        ));
        assert!(matches!(
            MediaUrl::parse("https://youtube.com:8443/watch?v=dQw4w9WgXcQ"),
            Err(MediaUrlError::UnsupportedPort)
        ));
        assert!(matches!(
            MediaUrl::parse("https://youtube.com/watch?v=dQw4w9WgXcQ&cookies=browser"),
            Err(MediaUrlError::AuthenticationOptionForbidden { .. })
        ));
    }

    #[test]
    fn rejects_missing_ambiguous_and_invalid_identities() {
        for input in [
            "https://youtube.com/watch",
            "https://youtu.be/",
            "https://youtube.com/shorts/",
        ] {
            assert!(matches!(
                MediaUrl::parse(input),
                Err(MediaUrlError::MissingVideoId)
            ));
        }
        assert!(matches!(
            MediaUrl::parse("https://youtube.com/watch?v=dQw4w9WgXcQ&v=aaaaaaaaaaa"),
            Err(MediaUrlError::AmbiguousVideoId)
        ));
        for value in ["short", "dQw4w9WgXc!", "dQw4w9WgXcQx"] {
            let input = format!("https://youtu.be/{value}");
            assert!(matches!(
                MediaUrl::parse(&input),
                Err(MediaUrlError::InvalidVideoId)
            ));
        }
    }

    #[test]
    fn rejects_unsupported_youtube_paths_and_oversized_input() {
        assert!(matches!(
            MediaUrl::parse("https://youtube.com/channel/dQw4w9WgXcQ"),
            Err(MediaUrlError::UnsupportedPath { .. })
        ));
        let oversized = format!(
            "https://youtube.com/watch?v={ID}&padding={}",
            "a".repeat(2_048)
        );
        assert!(matches!(
            MediaUrl::parse(&oversized),
            Err(MediaUrlError::InputTooLong { .. })
        ));
    }
}
