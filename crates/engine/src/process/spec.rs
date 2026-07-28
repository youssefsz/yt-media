//! Typed process specifications.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

use thiserror::Error;

/// Independent byte and line limits applied to stdout and stderr.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputLimit {
    /// Maximum bytes retained per stream.
    pub max_bytes_per_stream: usize,
    /// Maximum logical lines retained per stream.
    pub max_lines_per_stream: usize,
}

impl OutputLimit {
    /// Creates a non-zero output limit.
    ///
    /// # Errors
    ///
    /// Returns an error when either bound is zero.
    pub fn new(
        max_bytes_per_stream: usize,
        max_lines_per_stream: usize,
    ) -> Result<Self, ProcessSpecError> {
        if max_bytes_per_stream == 0 || max_lines_per_stream == 0 {
            return Err(ProcessSpecError::ZeroOutputLimit);
        }
        Ok(Self {
            max_bytes_per_stream,
            max_lines_per_stream,
        })
    }
}

impl Default for OutputLimit {
    fn default() -> Self {
        Self {
            max_bytes_per_stream: 1024 * 1024,
            max_lines_per_stream: 10_000,
        }
    }
}

/// A shell-free child-process request.
#[derive(Clone, Debug)]
pub struct ProcessSpec {
    pub(crate) executable: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    pub(crate) inherit_environment: bool,
    pub(crate) current_directory: Option<PathBuf>,
    pub(crate) stdin: Option<Vec<u8>>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) output_limit: OutputLimit,
}

impl ProcessSpec {
    /// Creates a process specification for an executable path.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            inherit_environment: true,
            current_directory: None,
            stdin: None,
            timeout: None,
            output_limit: OutputLimit::default(),
        }
    }

    /// Appends one argument without shell interpretation.
    #[must_use]
    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    /// Appends arguments without shell interpretation.
    #[must_use]
    pub fn arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Adds or replaces one child environment variable.
    #[must_use]
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.insert(name.into(), value.into());
        self
    }

    /// Prevents the child from inheriting the parent environment.
    #[must_use]
    pub fn clear_environment(mut self) -> Self {
        self.inherit_environment = false;
        self
    }

    /// Sets the child working directory.
    #[must_use]
    pub fn current_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_directory = Some(path.into());
        self
    }

    /// Supplies exact bytes to the child's standard input.
    #[must_use]
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// Sets the wall-clock timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets independent stdout and stderr retention limits.
    #[must_use]
    pub fn output_limit(mut self, output_limit: OutputLimit) -> Self {
        self.output_limit = output_limit;
        self
    }

    /// Returns the requested executable path.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns the exact argument vector.
    pub fn argument_values(&self) -> impl Iterator<Item = &OsStr> {
        self.arguments.iter().map(OsString::as_os_str)
    }
}

/// An invalid process specification.
#[derive(Debug, Error)]
pub enum ProcessSpecError {
    /// A retention bound was zero.
    #[error("process output byte and line limits must both be non-zero")]
    ZeroOutputLimit,
}
