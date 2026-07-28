//! Validated executable paths at the external-process boundary.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use thiserror::Error;

/// A canonical absolute path proven to reference an executable file.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ExecutablePath(PathBuf);

impl ExecutablePath {
    /// Validates and canonicalizes an executable path.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the path cannot be inspected, is not a regular file, or lacks
    /// executable permission on Unix.
    pub fn validate(path: impl AsRef<Path>) -> Result<Self, PathValidationError> {
        let requested = path.as_ref();
        let canonical =
            std::fs::canonicalize(requested).map_err(|source| PathValidationError::Inspect {
                path: requested.to_path_buf(),
                source,
            })?;
        let metadata =
            std::fs::metadata(&canonical).map_err(|source| PathValidationError::Inspect {
                path: canonical.clone(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(PathValidationError::NotFile { path: canonical });
        }
        if !is_executable(&metadata) {
            return Err(PathValidationError::NotExecutable { path: canonical });
        }
        Ok(Self(canonical))
    }

    /// Returns the canonical path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consumes the wrapper and returns the canonical path.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for ExecutablePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for ExecutablePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Failure to validate an executable path.
#[derive(Debug, Error)]
pub enum PathValidationError {
    /// Filesystem metadata or canonicalization failed.
    #[error("could not inspect executable path `{}`", path.display())]
    Inspect {
        /// Path that could not be inspected.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The path does not reference a regular file.
    #[error("executable path `{}` is not a regular file", path.display())]
    NotFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// The file is not executable on the current platform.
    #[error("file `{}` does not have executable permission", path.display())]
    NotExecutable {
        /// Rejected path.
        path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{ExecutablePath, PathValidationError};

    #[test]
    fn rejects_a_directory() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let result = ExecutablePath::validate(directory.path());
        assert!(matches!(result, Err(PathValidationError::NotFile { .. })));
    }

    #[test]
    fn accepts_a_regular_file_on_windows() {
        if !cfg!(windows) {
            return;
        }
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let path = directory.path().join("fixture.exe");
        assert!(fs::write(&path, b"fixture").is_ok());
        assert!(ExecutablePath::validate(path).is_ok());
    }
}
