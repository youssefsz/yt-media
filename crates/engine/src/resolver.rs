//! Verified tool sets and deterministic runtime resolution precedence.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::{
    cancellation::CancellationToken,
    manifest::{BuildReceipt, Distribution, ManifestError, TargetManifest},
    path::{ExecutablePath, PathValidationError},
    process::{
        OutputLimit, ProcessError, ProcessOutput, ProcessRunner, ProcessSpec, TokioProcessRunner,
    },
    target::{SupportedTarget, TargetError},
    tool::{Tool, ToolIdentityError},
};

const MAX_BUILD_RECEIPT_BYTES: u64 = 1024 * 1024;
const MAX_STAGED_CHECKSUM_BYTES: u64 = 16 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_MAX_BYTES: usize = 64 * 1024;
const PROBE_MAX_LINES: usize = 256;

/// A complete target-specific tool set whose files and identities were verified.
#[derive(Clone, Debug)]
pub struct VerifiedToolSet {
    target: SupportedTarget,
    paths: BTreeMap<Tool, ExecutablePath>,
}

impl VerifiedToolSet {
    /// Verifies hashes, sizes, paths, build receipts, and version identities below a root.
    ///
    /// # Errors
    ///
    /// Returns a typed error for missing or escaped paths, receipt failures, digest mismatches, or
    /// version-probe failures.
    pub async fn verify(
        manifest: &TargetManifest,
        root: impl Into<PathBuf>,
        runner: Arc<dyn ProcessRunner>,
        cancellation: CancellationToken,
    ) -> Result<Self, ToolSetVerificationError> {
        let root = canonical_directory(root.into()).await?;
        let mut receipts = BTreeMap::<String, BuildReceipt>::new();
        let mut paths = BTreeMap::new();

        for tool_manifest in &manifest.tools {
            let expected =
                expected_executable(manifest.target, tool_manifest, &root, &mut receipts).await?;
            let path = confined_path(&root, &expected.relative_path).await?;
            verify_file_digest(tool_manifest.tool, &path, expected.size, &expected.sha256).await?;
            let executable_path = validate_executable_async(path).await?;
            probe_tool(
                runner.as_ref(),
                tool_manifest.tool,
                &tool_manifest.version,
                executable_path.as_path(),
                cancellation.child_token(),
            )
            .await?;
            paths.insert(tool_manifest.tool, executable_path);
        }

        Ok(Self {
            target: manifest.target,
            paths,
        })
    }

    /// Verifies one Tauri-staged tool directory using its exact checksum inventory and identity
    /// probes.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the inventory is malformed, incomplete, escaped, changed, or a
    /// staged executable does not identify as the pinned tool version.
    pub async fn verify_staged(
        target: SupportedTarget,
        root: impl Into<PathBuf>,
        runner: Arc<dyn ProcessRunner>,
        cancellation: CancellationToken,
    ) -> Result<Self, ToolSetVerificationError> {
        let root = canonical_directory(root.into()).await?;
        let checksum_path = root.join("SHA256SUMS");
        let metadata = tokio::fs::metadata(&checksum_path)
            .await
            .map_err(|source| ToolSetVerificationError::Inspect {
                path: checksum_path.clone(),
                source,
            })?;
        if metadata.len() > MAX_STAGED_CHECKSUM_BYTES {
            return Err(ToolSetVerificationError::ChecksumInventoryTooLarge {
                path: checksum_path,
                size: metadata.len(),
            });
        }
        let document = tokio::fs::read_to_string(&checksum_path)
            .await
            .map_err(|source| ToolSetVerificationError::Inspect {
                path: checksum_path,
                source,
            })?;
        let mut checksums = BTreeMap::new();
        for line in document.lines() {
            let Some((digest, name)) = line.split_once("  ") else {
                return Err(ToolSetVerificationError::InvalidChecksumInventory);
            };
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || name.is_empty()
                || name.contains(['/', '\\'])
                || checksums
                    .insert(name.to_owned(), digest.to_owned())
                    .is_some()
            {
                return Err(ToolSetVerificationError::InvalidChecksumInventory);
            }
        }
        if checksums.len() != Tool::ALL.len() {
            return Err(ToolSetVerificationError::InvalidChecksumInventory);
        }

        let mut paths = BTreeMap::new();
        for tool in Tool::ALL {
            let name = tool.staged_name(target);
            let expected_hash = checksums
                .remove(&name)
                .ok_or(ToolSetVerificationError::StagedToolMissing { tool })?;
            let path = confined_path(&root, &name).await?;
            let size = tokio::fs::metadata(&path)
                .await
                .map_err(|source| ToolSetVerificationError::Inspect {
                    path: path.clone(),
                    source,
                })?
                .len();
            verify_file_digest(tool, &path, size, &expected_hash).await?;
            let executable_path = validate_executable_async(path).await?;
            probe_tool(
                runner.as_ref(),
                tool,
                tool.baseline_version(),
                executable_path.as_path(),
                cancellation.child_token(),
            )
            .await?;
            paths.insert(tool, executable_path);
        }
        if !checksums.is_empty() {
            return Err(ToolSetVerificationError::InvalidChecksumInventory);
        }
        Ok(Self { target, paths })
    }

    /// Returns this set's release target.
    #[must_use]
    pub const fn target(&self) -> SupportedTarget {
        self.target
    }

    /// Returns a verified tool path.
    #[must_use]
    pub fn path(&self, tool: Tool) -> Option<&ExecutablePath> {
        self.paths.get(&tool)
    }
}

struct ExpectedExecutable {
    relative_path: String,
    sha256: String,
    size: u64,
}

async fn expected_executable(
    target: SupportedTarget,
    tool_manifest: &crate::manifest::ToolManifest,
    root: &Path,
    receipts: &mut BTreeMap<String, BuildReceipt>,
) -> Result<ExpectedExecutable, ToolSetVerificationError> {
    let executable =
        tool_manifest
            .executables
            .first()
            .ok_or(ToolSetVerificationError::Manifest(
                ManifestError::MissingExecutable {
                    target,
                    tool: tool_manifest.tool,
                },
            ))?;
    if tool_manifest.executables.len() != 1 {
        return Err(ToolSetVerificationError::UnexpectedExecutableCount {
            tool: tool_manifest.tool,
            count: tool_manifest.executables.len(),
        });
    }
    match &tool_manifest.distribution {
        Distribution::UpstreamRelease => Ok(ExpectedExecutable {
            relative_path: executable.archive_path.clone(),
            sha256: executable
                .sha256
                .clone()
                .ok_or(ToolSetVerificationError::Manifest(
                    ManifestError::MissingExecutableDigest {
                        target,
                        tool: tool_manifest.tool,
                    },
                ))?,
            size: executable.size.ok_or(ToolSetVerificationError::Manifest(
                ManifestError::InvalidSize {
                    tool: tool_manifest.tool,
                    field: "executables.size",
                },
            ))?,
        }),
        Distribution::NativeBuild { receipt_file } => {
            if !receipts.contains_key(receipt_file) {
                let receipt = load_build_receipt(root, receipt_file, target, tool_manifest).await?;
                receipts.insert(receipt_file.clone(), receipt);
            }
            let receipt = receipts.get(receipt_file).ok_or_else(|| {
                ToolSetVerificationError::MissingReceipt {
                    path: receipt_file.clone(),
                }
            })?;
            let built = receipt.executable(tool_manifest.tool).ok_or(
                ToolSetVerificationError::ReceiptMissingTool {
                    tool: tool_manifest.tool,
                },
            )?;
            if built.path != executable.archive_path {
                return Err(ToolSetVerificationError::ReceiptPathMismatch {
                    tool: tool_manifest.tool,
                    expected: executable.archive_path.clone(),
                    found: built.path.clone(),
                });
            }
            Ok(ExpectedExecutable {
                relative_path: built.path.clone(),
                sha256: built.sha256.clone(),
                size: built.size,
            })
        }
    }
}

async fn load_build_receipt(
    root: &Path,
    receipt_file: &str,
    target: SupportedTarget,
    tool_manifest: &crate::manifest::ToolManifest,
) -> Result<BuildReceipt, ToolSetVerificationError> {
    let receipt_path = confined_path(root, receipt_file).await?;
    let metadata = tokio::fs::metadata(&receipt_path).await.map_err(|source| {
        ToolSetVerificationError::Inspect {
            path: receipt_path.clone(),
            source,
        }
    })?;
    if metadata.len() > MAX_BUILD_RECEIPT_BYTES {
        return Err(ToolSetVerificationError::ReceiptTooLarge {
            path: receipt_path,
            size: metadata.len(),
        });
    }
    let bytes = tokio::fs::read(&receipt_path).await.map_err(|source| {
        ToolSetVerificationError::Inspect {
            path: receipt_path,
            source,
        }
    })?;
    BuildReceipt::from_json(&bytes, target, &tool_manifest.source.source_ref)
        .map_err(ToolSetVerificationError::Manifest)
}

async fn canonical_directory(path: PathBuf) -> Result<PathBuf, ToolSetVerificationError> {
    let canonical = tokio::fs::canonicalize(&path).await.map_err(|source| {
        ToolSetVerificationError::Inspect {
            path: path.clone(),
            source,
        }
    })?;
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|source| {
        ToolSetVerificationError::Inspect {
            path: canonical.clone(),
            source,
        }
    })?;
    if !metadata.is_dir() {
        return Err(ToolSetVerificationError::NotDirectory { path: canonical });
    }
    Ok(canonical)
}

async fn confined_path(
    canonical_root: &Path,
    relative: &str,
) -> Result<PathBuf, ToolSetVerificationError> {
    let requested = canonical_root.join(relative);
    let canonical = tokio::fs::canonicalize(&requested)
        .await
        .map_err(|source| ToolSetVerificationError::Inspect {
            path: requested,
            source,
        })?;
    if !canonical.starts_with(canonical_root) {
        return Err(ToolSetVerificationError::EscapedRoot {
            root: canonical_root.to_path_buf(),
            path: canonical,
        });
    }
    Ok(canonical)
}

async fn verify_file_digest(
    tool: Tool,
    path: &Path,
    expected_size: u64,
    expected_hash: &str,
) -> Result<(), ToolSetVerificationError> {
    let metadata =
        tokio::fs::metadata(path)
            .await
            .map_err(|source| ToolSetVerificationError::Inspect {
                path: path.to_path_buf(),
                source,
            })?;
    if !metadata.is_file() {
        return Err(ToolSetVerificationError::NotFile {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() != expected_size {
        return Err(ToolSetVerificationError::SizeMismatch {
            tool,
            expected: expected_size,
            found: metadata.len(),
        });
    }
    let mut file =
        tokio::fs::File::open(path)
            .await
            .map_err(|source| ToolSetVerificationError::Inspect {
                path: path.to_path_buf(),
                source,
            })?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let length =
            file.read(&mut buffer)
                .await
                .map_err(|source| ToolSetVerificationError::Inspect {
                    path: path.to_path_buf(),
                    source,
                })?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    let found = format!("{:x}", digest.finalize());
    if found != expected_hash {
        return Err(ToolSetVerificationError::DigestMismatch {
            tool,
            expected: expected_hash.to_owned(),
            found,
        });
    }
    Ok(())
}

async fn validate_executable_async(
    path: PathBuf,
) -> Result<ExecutablePath, ToolSetVerificationError> {
    tokio::task::spawn_blocking(move || ExecutablePath::validate(path))
        .await
        .map_err(ToolSetVerificationError::ValidationTask)?
        .map_err(ToolSetVerificationError::InvalidPath)
}

/// Whether development-only `PATH` discovery is permitted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResolutionMode {
    /// Production mode never consults `PATH`.
    #[default]
    Production,
    /// Development mode may consult an explicitly supplied `PATH` value.
    Development,
}

/// Ordered resolution inputs.
#[derive(Clone, Debug, Default)]
pub struct ToolResolutionConfig {
    /// Exact per-tool developer overrides.
    pub explicit_overrides: BTreeMap<Tool, PathBuf>,
    /// Checksum- and identity-verified managed update.
    pub managed_update: Option<VerifiedToolSet>,
    /// Checksum- and identity-verified bundled baseline.
    pub bundled_baseline: Option<VerifiedToolSet>,
    /// Production or development behavior.
    pub mode: ResolutionMode,
    /// Explicit environment search path, used only in development mode.
    pub path_environment: Option<OsString>,
}

/// The candidate tier selected by resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolPathSource {
    /// User-supplied exact executable override.
    ExplicitOverride,
    /// Verified managed update.
    ManagedUpdate,
    /// Verified application-bundled baseline.
    BundledBaseline,
    /// Development-only `PATH` discovery.
    DevelopmentPath,
}

/// One resolved and identity-checked executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTool {
    /// Tool identity.
    pub tool: Tool,
    /// Canonical executable path.
    pub path: ExecutablePath,
    /// Selected precedence tier.
    pub source: ToolPathSource,
}

/// Deterministic resolver over verified candidate sets.
#[derive(Clone)]
pub struct ToolResolver {
    runner: Arc<dyn ProcessRunner>,
}

impl std::fmt::Debug for ToolResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolResolver")
            .finish_non_exhaustive()
    }
}

impl Default for ToolResolver {
    fn default() -> Self {
        Self {
            runner: Arc::new(TokioProcessRunner),
        }
    }
}

impl ToolResolver {
    /// Creates a resolver with a testable process port.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }

    /// Resolves one current-target tool using the locked precedence order.
    ///
    /// # Errors
    ///
    /// Returns a typed target, candidate, probe, or not-found failure.
    pub async fn resolve(
        &self,
        tool: Tool,
        target: SupportedTarget,
        config: &ToolResolutionConfig,
        cancellation: CancellationToken,
    ) -> Result<ResolvedTool, ToolResolutionError> {
        let current = SupportedTarget::current().map_err(ToolResolutionError::Target)?;
        if target != current {
            return Err(ToolResolutionError::TargetNotRunnable {
                requested: target,
                current,
            });
        }

        if let Some(path) = config.explicit_overrides.get(&tool) {
            return self
                .resolve_unverified(tool, path, ToolPathSource::ExplicitOverride, cancellation)
                .await;
        }
        if let Some(managed) = &config.managed_update {
            if managed.target != target {
                return Err(ToolResolutionError::VerifiedSetTargetMismatch {
                    tier: ToolPathSource::ManagedUpdate,
                    expected: target,
                    found: managed.target,
                });
            }
            if let Some(path) = managed.path(tool) {
                return Ok(ResolvedTool {
                    tool,
                    path: path.clone(),
                    source: ToolPathSource::ManagedUpdate,
                });
            }
        }
        if let Some(bundled) = &config.bundled_baseline {
            if bundled.target != target {
                return Err(ToolResolutionError::VerifiedSetTargetMismatch {
                    tier: ToolPathSource::BundledBaseline,
                    expected: target,
                    found: bundled.target,
                });
            }
            if let Some(path) = bundled.path(tool) {
                return Ok(ResolvedTool {
                    tool,
                    path: path.clone(),
                    source: ToolPathSource::BundledBaseline,
                });
            }
        }
        if config.mode == ResolutionMode::Development
            && let Some(path_environment) = &config.path_environment
        {
            for directory in std::env::split_paths(path_environment) {
                let path = directory.join(tool.executable_name(target));
                if path.is_file() {
                    return self
                        .resolve_unverified(
                            tool,
                            &path,
                            ToolPathSource::DevelopmentPath,
                            cancellation,
                        )
                        .await;
                }
            }
        }
        Err(ToolResolutionError::NotFound {
            tool,
            path_discovery_enabled: config.mode == ResolutionMode::Development,
        })
    }

    async fn resolve_unverified(
        &self,
        tool: Tool,
        path: &Path,
        source: ToolPathSource,
        cancellation: CancellationToken,
    ) -> Result<ResolvedTool, ToolResolutionError> {
        let executable = validate_executable_async(path.to_path_buf())
            .await
            .map_err(|error| ToolResolutionError::Candidate {
                tool,
                tier: source,
                error: Box::new(error),
            })?;
        probe_tool(
            self.runner.as_ref(),
            tool,
            tool.baseline_version(),
            executable.as_path(),
            cancellation,
        )
        .await
        .map_err(|error| ToolResolutionError::Probe {
            tool,
            tier: source,
            error,
        })?;
        Ok(ResolvedTool {
            tool,
            path: executable,
            source,
        })
    }
}

/// Runs and validates a bounded version probe.
///
/// # Errors
///
/// Returns process, exit-status, or identity failures.
pub async fn probe_tool(
    runner: &dyn ProcessRunner,
    tool: Tool,
    expected_version: &str,
    path: &Path,
    cancellation: CancellationToken,
) -> Result<ProcessOutput, ToolProbeError> {
    let output_limit =
        OutputLimit::new(PROBE_MAX_BYTES, PROBE_MAX_LINES).map_err(ToolProbeError::Spec)?;
    let spec = ProcessSpec::new(path)
        .arguments(tool.version_arguments())
        .timeout(PROBE_TIMEOUT)
        .output_limit(output_limit);
    let output = runner
        .run(spec, cancellation)
        .await
        .map_err(ToolProbeError::Process)?;
    if !output.status.success {
        return Err(ToolProbeError::NonZero {
            tool,
            output: Box::new(output),
        });
    }
    tool.validate_version_output(
        expected_version,
        &output.capture.stdout.bytes,
        &output.capture.stderr.bytes,
    )
    .map_err(ToolProbeError::Identity)?;
    Ok(output)
}

/// Complete tool-set verification failure.
#[derive(Debug, Error)]
pub enum ToolSetVerificationError {
    /// The manifest or receipt was invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A filesystem path could not be inspected.
    #[error("could not inspect `{}`", path.display())]
    Inspect {
        /// Affected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The configured root was not a directory.
    #[error("tool-set root `{}` is not a directory", path.display())]
    NotDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// A tool path was not a regular file.
    #[error("tool path `{}` is not a regular file", path.display())]
    NotFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// Canonicalization escaped the selected root.
    #[error("tool path `{}` escapes root `{}`", path.display(), root.display())]
    EscapedRoot {
        /// Selected root.
        root: PathBuf,
        /// Escaped canonical path.
        path: PathBuf,
    },
    /// A receipt exceeded the bounded parser input.
    #[error("native build receipt `{}` is {size} bytes; maximum is {MAX_BUILD_RECEIPT_BYTES}", path.display())]
    ReceiptTooLarge {
        /// Receipt path.
        path: PathBuf,
        /// Observed bytes.
        size: u64,
    },
    /// The staged checksum inventory exceeded its parser bound.
    #[error("staged checksum inventory `{}` is {size} bytes; maximum is {MAX_STAGED_CHECKSUM_BYTES}", path.display())]
    ChecksumInventoryTooLarge {
        /// Inventory path.
        path: PathBuf,
        /// Observed bytes.
        size: u64,
    },
    /// The staged checksum inventory was malformed or contained unexpected entries.
    #[error("staged checksum inventory is invalid")]
    InvalidChecksumInventory,
    /// The staged checksum inventory omitted one required executable.
    #[error("staged checksum inventory omitted `{tool}`")]
    StagedToolMissing {
        /// Missing tool.
        tool: Tool,
    },
    /// Receipt map unexpectedly omitted a loaded path.
    #[error("native build receipt `{path}` was not loaded")]
    MissingReceipt {
        /// Missing receipt path.
        path: String,
    },
    /// Receipt omitted one native tool.
    #[error("native build receipt omitted `{tool}`")]
    ReceiptMissingTool {
        /// Missing tool.
        tool: Tool,
    },
    /// Receipt executable path differs from the baseline manifest.
    #[error("native build receipt path for `{tool}` is `{found}`; expected `{expected}`")]
    ReceiptPathMismatch {
        /// Affected tool.
        tool: Tool,
        /// Manifest path.
        expected: String,
        /// Receipt path.
        found: String,
    },
    /// File size differs from the verified record.
    #[error("`{tool}` size is {found}; expected {expected}")]
    SizeMismatch {
        /// Affected tool.
        tool: Tool,
        /// Expected bytes.
        expected: u64,
        /// Observed bytes.
        found: u64,
    },
    /// File digest differs from the verified record.
    #[error("`{tool}` digest is {found}; expected {expected}")]
    DigestMismatch {
        /// Affected tool.
        tool: Tool,
        /// Expected SHA-256.
        expected: String,
        /// Observed SHA-256.
        found: String,
    },
    /// More than one executable was declared for one tool.
    #[error("`{tool}` declares {count} executables; exactly one is supported")]
    UnexpectedExecutableCount {
        /// Affected tool.
        tool: Tool,
        /// Observed record count.
        count: usize,
    },
    /// Validating an executable path failed.
    #[error(transparent)]
    InvalidPath(#[from] PathValidationError),
    /// The blocking path-validation task failed.
    #[error("executable path validation task failed")]
    ValidationTask(#[source] tokio::task::JoinError),
    /// A tool version probe failed.
    #[error(transparent)]
    Probe(#[from] ToolProbeError),
}

/// One version probe failed.
#[derive(Debug, Error)]
pub enum ToolProbeError {
    /// Probe process specification was invalid.
    #[error("invalid tool probe")]
    Spec(#[source] crate::process::ProcessSpecError),
    /// Probe execution failed.
    #[error("tool probe process failed")]
    Process(#[source] ProcessError),
    /// Tool returned a non-zero status.
    #[error("`{tool}` version probe returned a non-zero status")]
    NonZero {
        /// Tool being probed.
        tool: Tool,
        /// Complete bounded output and exit status.
        output: Box<ProcessOutput>,
    },
    /// Output did not identify the expected tool/version.
    #[error(transparent)]
    Identity(#[from] ToolIdentityError),
}

/// Runtime tool resolution failure.
#[derive(Debug, Error)]
pub enum ToolResolutionError {
    /// Current platform target detection failed.
    #[error(transparent)]
    Target(#[from] TargetError),
    /// A non-native target cannot be executed.
    #[error("cannot run `{requested}` tools from a `{current}` process")]
    TargetNotRunnable {
        /// Requested target.
        requested: SupportedTarget,
        /// Current target.
        current: SupportedTarget,
    },
    /// A verified set was supplied for another target.
    #[error("{tier:?} tool set is for `{found}`; expected `{expected}`")]
    VerifiedSetTargetMismatch {
        /// Candidate tier.
        tier: ToolPathSource,
        /// Expected target.
        expected: SupportedTarget,
        /// Tool-set target.
        found: SupportedTarget,
    },
    /// An override or PATH candidate was invalid.
    #[error("invalid {tier:?} candidate for `{tool}`")]
    Candidate {
        /// Affected tool.
        tool: Tool,
        /// Candidate tier.
        tier: ToolPathSource,
        /// Verification failure.
        #[source]
        error: Box<ToolSetVerificationError>,
    },
    /// An override or PATH candidate failed its identity probe.
    #[error("{tier:?} candidate for `{tool}` failed its identity probe")]
    Probe {
        /// Affected tool.
        tool: Tool,
        /// Candidate tier.
        tier: ToolPathSource,
        /// Probe failure.
        #[source]
        error: ToolProbeError,
    },
    /// No candidate was available.
    #[error("no usable `{tool}` executable was found")]
    NotFound {
        /// Missing tool.
        tool: Tool,
        /// Whether development PATH discovery was enabled.
        path_discovery_enabled: bool,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::{
        ResolutionMode, ToolPathSource, ToolResolutionConfig, ToolResolver, VerifiedToolSet,
    };
    use crate::{
        cancellation::CancellationToken,
        path::ExecutablePath,
        process::{
            CapturedOutput, ProcessError, ProcessExitStatus, ProcessOutput, ProcessRunner,
            ProcessSpec, StreamCapture,
        },
        target::SupportedTarget,
        tool::Tool,
    };

    #[derive(Default)]
    struct ProbeRunner {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ProcessRunner for ProbeRunner {
        async fn run(
            &self,
            spec: ProcessSpec,
            _cancellation: CancellationToken,
        ) -> Result<ProcessOutput, ProcessError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let filename = spec
                .executable()
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default();
            let stdout = if filename.contains("yt-dlp") {
                b"2026.06.09\n".to_vec()
            } else if filename.contains("ffprobe") {
                b"ffprobe version 8.0.1\n".to_vec()
            } else if filename.contains("ffmpeg") {
                b"ffmpeg version 8.0.1\n".to_vec()
            } else if filename.contains("deno") {
                b"deno 2.8.1\n".to_vec()
            } else {
                Vec::new()
            };
            Ok(ProcessOutput {
                status: ProcessExitStatus {
                    success: true,
                    code: Some(0),
                },
                capture: CapturedOutput {
                    stdout: StreamCapture {
                        observed_bytes: u64::try_from(stdout.len()).unwrap_or(u64::MAX),
                        observed_lines: 1,
                        bytes: stdout,
                        truncated: false,
                    },
                    ..CapturedOutput::default()
                },
            })
        }
    }

    fn make_executable(path: &Path) {
        assert!(fs::write(path, b"fixture").is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let metadata = fs::metadata(path);
            assert!(metadata.is_ok());
            if let Ok(metadata) = metadata {
                let mut permissions = metadata.permissions();
                permissions.set_mode(0o755);
                assert!(fs::set_permissions(path, permissions).is_ok());
            }
        }
    }

    fn verified_set(target: SupportedTarget, tool: Tool, path: &Path) -> VerifiedToolSet {
        let validated = ExecutablePath::validate(path);
        assert!(validated.is_ok());
        let mut paths = BTreeMap::new();
        if let Ok(validated) = validated {
            paths.insert(tool, validated);
        }
        VerifiedToolSet { target, paths }
    }

    #[tokio::test]
    async fn explicit_override_precedes_verified_sets() {
        let target = SupportedTarget::current();
        assert!(target.is_ok());
        let Some(target) = target.ok() else {
            return;
        };
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let override_path = directory.path().join(Tool::YtDlp.executable_name(target));
        let managed_path = directory
            .path()
            .join(format!("managed-{}", Tool::YtDlp.executable_name(target)));
        make_executable(&override_path);
        make_executable(&managed_path);
        let mut overrides = BTreeMap::new();
        overrides.insert(Tool::YtDlp, override_path.clone());
        let config = ToolResolutionConfig {
            explicit_overrides: overrides,
            managed_update: Some(verified_set(target, Tool::YtDlp, &managed_path)),
            bundled_baseline: None,
            mode: ResolutionMode::Production,
            path_environment: None,
        };
        let runner = Arc::new(ProbeRunner::default());
        let resolver = ToolResolver::new(runner);
        let resolution = resolver
            .resolve(Tool::YtDlp, target, &config, CancellationToken::new())
            .await;
        assert!(resolution.is_ok());
        if let Ok(resolution) = resolution {
            assert_eq!(resolution.source, ToolPathSource::ExplicitOverride);
            assert_eq!(
                resolution.path.as_path(),
                override_path
                    .canonicalize()
                    .ok()
                    .as_deref()
                    .unwrap_or(&override_path)
            );
        }
    }

    #[tokio::test]
    async fn managed_update_precedes_bundled_baseline() {
        let target = SupportedTarget::current();
        assert!(target.is_ok());
        let Some(target) = target.ok() else {
            return;
        };
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let managed_path = directory.path().join("managed");
        let bundled_path = directory.path().join("bundled");
        make_executable(&managed_path);
        make_executable(&bundled_path);
        let config = ToolResolutionConfig {
            explicit_overrides: BTreeMap::new(),
            managed_update: Some(verified_set(target, Tool::YtDlp, &managed_path)),
            bundled_baseline: Some(verified_set(target, Tool::YtDlp, &bundled_path)),
            mode: ResolutionMode::Development,
            path_environment: Some(directory.path().as_os_str().to_owned()),
        };
        let resolver = ToolResolver::new(Arc::new(ProbeRunner::default()));
        let resolution = resolver
            .resolve(Tool::YtDlp, target, &config, CancellationToken::new())
            .await;
        assert!(matches!(
            resolution,
            Ok(ref resolution) if resolution.source == ToolPathSource::ManagedUpdate
        ));
    }

    #[tokio::test]
    async fn production_mode_never_uses_path() {
        let target = SupportedTarget::current();
        assert!(target.is_ok());
        let Some(target) = target.ok() else {
            return;
        };
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let path = directory.path().join(Tool::YtDlp.executable_name(target));
        make_executable(&path);
        let config = ToolResolutionConfig {
            path_environment: Some(directory.path().as_os_str().to_owned()),
            ..ToolResolutionConfig::default()
        };
        let resolver = ToolResolver::new(Arc::new(ProbeRunner::default()));
        let resolution = resolver
            .resolve(Tool::YtDlp, target, &config, CancellationToken::new())
            .await;
        assert!(resolution.is_err());
    }

    #[tokio::test]
    async fn verifies_exact_tauri_staged_inventory() {
        let target = SupportedTarget::current();
        assert!(target.is_ok());
        let Some(target) = target.ok() else {
            return;
        };
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let mut checksums = Vec::new();
        for tool in Tool::ALL {
            let name = tool.staged_name(target);
            let path = directory.path().join(&name);
            make_executable(&path);
            let bytes = fs::read(&path);
            assert!(bytes.is_ok());
            if let Ok(bytes) = bytes {
                checksums.push(format!("{:x}  {name}", Sha256::digest(bytes)));
            }
        }
        checksums.sort();
        let document = format!("{}\n", checksums.join("\n"));
        assert!(fs::write(directory.path().join("SHA256SUMS"), document).is_ok());
        let runner = Arc::new(ProbeRunner::default());
        let verified = VerifiedToolSet::verify_staged(
            target,
            directory.path(),
            runner.clone(),
            CancellationToken::new(),
        )
        .await;
        assert!(verified.is_ok());
        if let Ok(verified) = verified {
            assert_eq!(verified.target(), target);
            for tool in Tool::ALL {
                assert!(verified.path(tool).is_some());
            }
        }
        assert_eq!(runner.calls.load(Ordering::SeqCst), Tool::ALL.len());
    }
}
