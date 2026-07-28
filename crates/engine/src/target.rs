//! Supported release targets and platform-specific naming.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A release target supported by the sidecar supply chain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SupportedTarget {
    /// 64-bit Windows using MSVC.
    #[serde(rename = "x86_64-pc-windows-msvc")]
    WindowsX64,
    /// ARM64 Windows using MSVC.
    #[serde(rename = "aarch64-pc-windows-msvc")]
    WindowsArm64,
    /// Intel macOS.
    #[serde(rename = "x86_64-apple-darwin")]
    MacOsX64,
    /// Apple Silicon macOS.
    #[serde(rename = "aarch64-apple-darwin")]
    MacOsArm64,
    /// 64-bit glibc Linux.
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    LinuxX64,
    /// ARM64 glibc Linux.
    #[serde(rename = "aarch64-unknown-linux-gnu")]
    LinuxArm64,
}

impl SupportedTarget {
    /// Every target shipped by the project.
    pub const ALL: [Self; 6] = [
        Self::WindowsX64,
        Self::WindowsArm64,
        Self::MacOsX64,
        Self::MacOsArm64,
        Self::LinuxX64,
        Self::LinuxArm64,
    ];

    /// Returns the canonical Rust target triple.
    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::WindowsX64 => "x86_64-pc-windows-msvc",
            Self::WindowsArm64 => "aarch64-pc-windows-msvc",
            Self::MacOsX64 => "x86_64-apple-darwin",
            Self::MacOsArm64 => "aarch64-apple-darwin",
            Self::LinuxX64 => "x86_64-unknown-linux-gnu",
            Self::LinuxArm64 => "aarch64-unknown-linux-gnu",
        }
    }

    /// Returns whether executables for this target use the `.exe` suffix.
    #[must_use]
    pub const fn is_windows(self) -> bool {
        matches!(self, Self::WindowsX64 | Self::WindowsArm64)
    }

    /// Identifies the target used to compile the current process.
    ///
    /// # Errors
    ///
    /// Returns [`TargetError::UnsupportedCurrent`] for a platform outside the release matrix.
    pub fn current() -> Result<Self, TargetError> {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            Ok(Self::WindowsX64)
        }
        #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
        {
            Ok(Self::WindowsArm64)
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            Ok(Self::MacOsX64)
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Ok(Self::MacOsArm64)
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Ok(Self::LinuxX64)
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            Ok(Self::LinuxArm64)
        }
        #[cfg(not(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
        )))]
        {
            Err(TargetError::UnsupportedCurrent {
                triple: format!(
                    "{}-unknown-{}",
                    std::env::consts::ARCH,
                    std::env::consts::OS
                ),
            })
        }
    }
}

impl fmt::Display for SupportedTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.triple())
    }
}

impl FromStr for SupportedTarget {
    type Err = TargetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|target| target.triple() == value)
            .ok_or_else(|| TargetError::Unsupported {
                triple: value.to_owned(),
            })
    }
}

/// Failure to identify a supported target.
#[derive(Debug, Error)]
pub enum TargetError {
    /// A supplied target triple is outside the release matrix.
    #[error("unsupported target triple `{triple}`")]
    Unsupported {
        /// The unsupported triple.
        triple: String,
    },
    /// The current compiler target is outside the release matrix.
    #[error("current compiler target `{triple}` is not a supported release target")]
    UnsupportedCurrent {
        /// The unsupported compiler triple.
        triple: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{SupportedTarget, TargetError};

    #[test]
    fn accepts_every_supported_triple() {
        for target in SupportedTarget::ALL {
            let parsed = target.triple().parse::<SupportedTarget>();
            assert_eq!(parsed.ok(), Some(target));
        }
    }

    #[test]
    fn rejects_unsupported_triple() {
        let error = "i686-pc-windows-msvc".parse::<SupportedTarget>();
        assert!(matches!(error, Err(TargetError::Unsupported { .. })));
    }
}
