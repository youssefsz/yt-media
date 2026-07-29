//! External tool identities, versions, and probe contracts.

use std::{ffi::OsString, fmt};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::target::SupportedTarget;

/// An external executable owned by the engine boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    /// The yt-dlp extractor.
    YtDlp,
    /// The `FFmpeg` media processor.
    Ffmpeg,
    /// The `FFprobe` metadata probe.
    Ffprobe,
    /// The Deno JavaScript runtime.
    Deno,
}

impl Tool {
    /// Every tool required by a complete target manifest.
    pub const ALL: [Self; 4] = [Self::YtDlp, Self::Ffmpeg, Self::Ffprobe, Self::Deno];

    /// Returns the stable tool identifier.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::YtDlp => "yt-dlp",
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
            Self::Deno => "deno",
        }
    }

    /// Returns the Plan 01 bundled baseline version.
    #[must_use]
    pub const fn baseline_version(self) -> &'static str {
        match self {
            Self::YtDlp => "2026.06.09",
            Self::Ffmpeg | Self::Ffprobe => "8.0.1",
            Self::Deno => "2.8.1",
        }
    }

    /// Returns the executable filename for a target.
    #[must_use]
    pub fn executable_name(self, target: SupportedTarget) -> String {
        if target.is_windows() {
            format!("{}.exe", self.name())
        } else {
            self.name().to_owned()
        }
    }

    /// Returns the Tauri sidecar filename for a target.
    #[must_use]
    pub fn staged_name(self, target: SupportedTarget) -> String {
        if target.is_windows() {
            format!("{}-{}.exe", self.name(), target.triple())
        } else {
            format!("{}-{}", self.name(), target.triple())
        }
    }

    /// Returns arguments for the stable machine-oriented version probe.
    #[must_use]
    pub fn version_arguments(self) -> Vec<OsString> {
        match self {
            Self::YtDlp | Self::Deno => vec![OsString::from("--version")],
            Self::Ffmpeg | Self::Ffprobe => vec![OsString::from("-version")],
        }
    }

    /// Validates a version probe without accepting output from a different tool.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid UTF-8, an unexpected identity, or a version mismatch.
    pub fn validate_version_output(
        self,
        expected_version: &str,
        stdout: &[u8],
        stderr: &[u8],
    ) -> Result<(), ToolIdentityError> {
        let output = if stdout.is_empty() { stderr } else { stdout };
        let text = std::str::from_utf8(output)
            .map_err(|source| ToolIdentityError::InvalidUtf8 { tool: self, source })?;
        let first_line = text.lines().next().unwrap_or_default().trim();
        let matches = match self {
            Self::YtDlp => first_line == expected_version,
            Self::Deno => {
                first_line
                    .strip_prefix("deno ")
                    .and_then(|remainder| remainder.split_whitespace().next())
                    == Some(expected_version)
            }
            Self::Ffmpeg => versioned_prefix_matches(first_line, "ffmpeg", expected_version),
            Self::Ffprobe => versioned_prefix_matches(first_line, "ffprobe", expected_version),
        };

        if matches {
            Ok(())
        } else {
            Err(ToolIdentityError::UnexpectedOutput {
                tool: self,
                expected_version: expected_version.to_owned(),
                output: output.to_vec(),
            })
        }
    }
}

fn versioned_prefix_matches(line: &str, identity: &str, version: &str) -> bool {
    let Some(remainder) = line
        .strip_prefix(identity)
        .and_then(|value| value.strip_prefix(" version "))
    else {
        return false;
    };
    let reported = remainder.split_whitespace().next().unwrap_or_default();
    reported == version || reported == format!("n{version}")
}

impl fmt::Display for Tool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A version probe did not identify the expected executable.
#[derive(Debug, Error)]
pub enum ToolIdentityError {
    /// Probe output was not valid UTF-8.
    #[error("{tool} version output was not valid UTF-8")]
    InvalidUtf8 {
        /// Tool being probed.
        tool: Tool,
        /// UTF-8 decoding failure.
        #[source]
        source: std::str::Utf8Error,
    },
    /// Probe output did not match the expected identity and version.
    #[error("{tool} did not report expected version {expected_version}")]
    UnexpectedOutput {
        /// Tool being probed.
        tool: Tool,
        /// Required version.
        expected_version: String,
        /// Complete bounded probe output for diagnostics.
        output: Vec<u8>,
    },
}

#[cfg(test)]
mod tests {
    use super::{Tool, ToolIdentityError};

    #[test]
    fn rejects_another_tool_with_the_same_version() {
        let result = Tool::Ffmpeg.validate_version_output("8.0.1", b"ffprobe version 8.0.1\n", b"");
        assert!(matches!(
            result,
            Err(ToolIdentityError::UnexpectedOutput { .. })
        ));
    }

    #[test]
    fn accepts_ffmpeg_release_tag_spelling() {
        let result = Tool::Ffmpeg.validate_version_output("8.0.1", b"ffmpeg version n8.0.1\n", b"");
        assert!(result.is_ok());
    }

    #[test]
    fn staged_names_cover_every_release_target_without_ambiguity() {
        for target in crate::target::SupportedTarget::ALL {
            for tool in Tool::ALL {
                let name = tool.staged_name(target);
                assert!(name.contains(tool.name()));
                assert!(name.contains(target.triple()));
                assert_eq!(
                    std::path::Path::new(&name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
                    target.is_windows()
                );
            }
        }
    }
}
