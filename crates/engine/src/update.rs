//! Signed managed-tool updates, confined installation, atomic activation, and rollback.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use ed25519_dalek::{Signature, VerifyingKey};
use fs2::FileExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    cancellation::CancellationToken,
    process::{OutputLimit, ProcessError, ProcessRunner, ProcessSpec},
    resolver::{ToolSetVerificationError, VerifiedToolEntry, VerifiedToolSet},
    target::SupportedTarget,
    tool::Tool,
};

/// Signed update-manifest schema understood by this engine version.
pub const UPDATE_MANIFEST_SCHEMA_VERSION: u32 = 1;
/// Hard download limit for one complete tool-set archive.
pub const MAX_UPDATE_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
/// Repeated managed-set startup failures that trigger automatic rollback.
pub const STARTUP_FAILURE_ROLLBACK_THRESHOLD: u32 = 2;
/// Minimum interval between automatic background checks.
pub const BACKGROUND_UPDATE_INTERVAL_SECS: u64 = 24 * 60 * 60;

const MANIFEST_FILE: &str = "update-manifest.v1.json";
const SIGNATURE_FILE: &str = "update-manifest.v1.sig";
const CHECK_STATE_FILE: &str = "update-check.v1.json";
const FAILURE_FILE: &str = "startup-failures.v1.json";
const ACTIVATION_JOURNAL: &str = "activation-journal.v1.json";
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_MAX_BYTES: usize = 4 * 1024 * 1024;
const PROBE_MAX_LINES: usize = 100_000;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 1024;

/// Why a signed update check was requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateCheckMode {
    /// Opportunistic application-start check, rate limited to once every 24 hours.
    Background,
    /// Explicit user action, which is never suppressed by the background interval.
    Manual,
}

/// Whether a verified newer set may be activated by this check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateInstallPolicy {
    /// Report a trusted version without downloading or activating it.
    CheckOnly,
    /// Download, verify, probe, and atomically activate a newer set.
    Install,
}

/// Result of checking the signed update channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateCheckOutcome {
    /// A background check was suppressed because a recent attempt was already recorded.
    SkippedRecently,
    /// The active managed set is at least as new as the signed channel manifest.
    Current {
        /// Trusted active or channel version.
        version: Version,
    },
    /// A newer signed compatible set exists but was not installed.
    Available {
        /// Trusted available version.
        version: Version,
    },
    /// A newly verified complete set was installed and will be selected on the next resolution.
    Installed {
        /// Trusted installed version.
        version: Version,
    },
}

/// Bounded HTTP/file transport port used by the update use case.
#[async_trait]
pub trait UpdateTransport: Send + Sync {
    /// Fetches a small document while enforcing the supplied byte limit.
    async fn fetch_bytes(
        &self,
        url: &str,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>, UpdateTransportError>;

    /// Streams a large response into a newly created application-owned file.
    async fn download_file(
        &self,
        url: &str,
        destination: &Path,
        maximum_bytes: u64,
    ) -> Result<u64, UpdateTransportError>;
}

/// Failure returned by an update transport adapter.
#[derive(Debug, Error)]
pub enum UpdateTransportError {
    /// The remote endpoint returned a non-success status.
    #[error("update endpoint returned HTTP status {status}")]
    HttpStatus {
        /// Numeric HTTP status.
        status: u16,
    },
    /// The response exceeded its pre-verification bound.
    #[error("update response exceeded the {maximum_bytes}-byte limit")]
    ResponseTooLarge {
        /// Enforced maximum.
        maximum_bytes: u64,
    },
    /// The transport or destination write failed.
    #[error("update transport failed")]
    Adapter(#[source] Box<dyn StdError + Send + Sync>),
}

impl UpdateTransportError {
    /// Wraps a transport-specific source without exposing it as product policy.
    pub fn adapter(source: impl StdError + Send + Sync + 'static) -> Self {
        Self::Adapter(Box::new(source))
    }
}

/// Canonical signed release description for one immutable target archive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateManifest {
    /// Manifest contract version.
    pub schema_version: u32,
    /// Update channel, such as `stable`.
    pub channel: String,
    /// Application/tool-set release version.
    pub release_version: String,
    /// Exact release target.
    pub target: SupportedTarget,
    /// Oldest application version allowed to install this set.
    pub minimum_app_version: String,
    /// UTC RFC 3339 creation timestamp.
    pub created_at: String,
    /// Immutable archive record.
    pub archive: UpdateArchive,
    /// Complete four-tool executable inventory.
    pub tools: Vec<UpdateTool>,
}

/// Immutable archive identity covered by the signed manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateArchive {
    /// Immutable HTTPS release-asset URL.
    pub url: String,
    /// Portable archive filename.
    pub filename: String,
    /// Exact archive byte length.
    pub size: u64,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
}

/// One executable inside a signed update archive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTool {
    /// Stable tool identity.
    pub tool: Tool,
    /// Exact version expected from its machine-oriented probe.
    pub version: String,
    /// Safe relative ZIP path.
    pub archive_path: String,
    /// Exact executable byte length.
    pub size: u64,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
}

impl UpdateManifest {
    /// Returns RFC 8785 canonical bytes for signing.
    ///
    /// # Errors
    ///
    /// Returns a canonicalization error if the manifest cannot be represented as JCS.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, UpdateError> {
        serde_json_canonicalizer::to_vec(self).map_err(UpdateError::Canonicalize)
    }

    fn validate(
        &self,
        expected_target: SupportedTarget,
        application_version: &Version,
    ) -> Result<(), UpdateError> {
        if self.schema_version != UPDATE_MANIFEST_SCHEMA_VERSION {
            return Err(UpdateError::UnsupportedSchema {
                found: self.schema_version,
            });
        }
        if self.target != expected_target {
            return Err(UpdateError::WrongTarget {
                expected: expected_target,
                found: self.target,
            });
        }
        validate_channel(&self.channel)?;
        let release_version = Version::parse(&self.release_version).map_err(|source| {
            UpdateError::InvalidVersion {
                field: "release_version",
                source,
            }
        })?;
        let minimum = Version::parse(&self.minimum_app_version).map_err(|source| {
            UpdateError::InvalidVersion {
                field: "minimum_app_version",
                source,
            }
        })?;
        if application_version < &minimum {
            return Err(UpdateError::ApplicationTooOld {
                installed: application_version.clone(),
                minimum,
            });
        }
        if release_version < minimum {
            return Err(UpdateError::ReleaseBeforeMinimum {
                release: release_version,
                minimum,
            });
        }
        if !valid_utc_timestamp(&self.created_at) {
            return Err(UpdateError::InvalidCreationTime);
        }
        validate_archive(&self.archive)?;

        let mut identities = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for record in &self.tools {
            if !identities.insert(record.tool) {
                return Err(UpdateError::DuplicateTool { tool: record.tool });
            }
            if record.version.trim().is_empty() || record.version.len() > 128 {
                return Err(UpdateError::InvalidToolVersion { tool: record.tool });
            }
            validate_relative_path(&record.archive_path)?;
            let key = record.archive_path.replace('\\', "/").to_ascii_lowercase();
            if !paths.insert(key) {
                return Err(UpdateError::DuplicateArchivePath {
                    path: record.archive_path.clone(),
                });
            }
            if record.size == 0 || record.size > MAX_UPDATE_ARCHIVE_BYTES {
                return Err(UpdateError::InvalidToolSize {
                    tool: record.tool,
                    size: record.size,
                });
            }
            validate_sha256(&record.sha256)?;
        }
        if identities.len() != Tool::ALL.len()
            || Tool::ALL.iter().any(|tool| !identities.contains(tool))
        {
            return Err(UpdateError::IncompleteToolSet);
        }
        Ok(())
    }

    fn verification_entries(&self) -> Vec<VerifiedToolEntry> {
        self.tools
            .iter()
            .map(|record| VerifiedToolEntry {
                tool: record.tool,
                version: record.version.clone(),
                relative_path: record.archive_path.clone(),
                size: record.size,
                sha256: record.sha256.clone(),
            })
            .collect()
    }
}

/// A manifest whose canonical bytes and Ed25519 signature were verified.
#[derive(Clone, Debug)]
pub struct VerifiedUpdateManifest {
    manifest: UpdateManifest,
    canonical_bytes: Vec<u8>,
    signature_hex: String,
}

impl VerifiedUpdateManifest {
    /// Verifies canonical form, Ed25519 signature, schema, target, and app compatibility.
    ///
    /// # Errors
    ///
    /// Returns a typed failure without trusting unsigned manifest fields.
    pub fn verify(
        manifest_bytes: &[u8],
        signature_hex: &str,
        public_key_hex: &str,
        expected_target: SupportedTarget,
        application_version: &Version,
    ) -> Result<Self, UpdateError> {
        if manifest_bytes.len() > 256 * 1024 {
            return Err(UpdateError::ManifestTooLarge {
                size: manifest_bytes.len(),
            });
        }
        let value: serde_json::Value =
            serde_json::from_slice(manifest_bytes).map_err(UpdateError::InvalidJson)?;
        let canonical =
            serde_json_canonicalizer::to_vec(&value).map_err(UpdateError::Canonicalize)?;
        if canonical != manifest_bytes {
            return Err(UpdateError::NonCanonicalManifest);
        }
        let key_bytes = decode_fixed::<32>(public_key_hex, "public key")?;
        let verifying_key =
            VerifyingKey::from_bytes(&key_bytes).map_err(UpdateError::InvalidPublicKey)?;
        if verifying_key.is_weak() {
            return Err(UpdateError::WeakPublicKey);
        }
        let signature_bytes = decode_fixed::<64>(signature_hex.trim(), "signature")?;
        let signature = Signature::from_bytes(&signature_bytes);
        verifying_key
            .verify_strict(manifest_bytes, &signature)
            .map_err(UpdateError::InvalidSignature)?;

        let manifest: UpdateManifest =
            serde_json::from_value(value).map_err(UpdateError::InvalidJson)?;
        manifest.validate(expected_target, application_version)?;
        Ok(Self {
            manifest,
            canonical_bytes: canonical,
            signature_hex: signature_hex.trim().to_ascii_lowercase(),
        })
    }

    /// Returns the trusted manifest.
    #[must_use]
    pub const fn manifest(&self) -> &UpdateManifest {
        &self.manifest
    }
}

/// Application-owned managed-tool directory with activation and rollback behavior.
#[derive(Clone)]
pub struct UpdateManager {
    root: PathBuf,
    target: SupportedTarget,
    application_version: Version,
    public_key_hex: String,
    runner: Arc<dyn ProcessRunner>,
}

impl std::fmt::Debug for UpdateManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateManager")
            .field("root", &self.root)
            .field("target", &self.target)
            .field("application_version", &self.application_version)
            .finish_non_exhaustive()
    }
}

impl UpdateManager {
    /// Creates a manager. The caller supplies the compile-time embedded public key.
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        target: SupportedTarget,
        application_version: Version,
        public_key_hex: impl Into<String>,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        Self {
            root: root.into(),
            target,
            application_version,
            public_key_hex: public_key_hex.into(),
            runner,
        }
    }

    /// Checks the signed channel and installs a newer complete tool set.
    ///
    /// Background attempts are durably rate limited before network access. Manual attempts always
    /// run. A transport or verification failure does not change the active or bundled set.
    ///
    /// # Errors
    ///
    /// Returns a typed endpoint, signature, download, extraction, health, or activation failure.
    pub async fn check_and_install(
        &self,
        transport: &dyn UpdateTransport,
        manifest_url: &str,
        mode: UpdateCheckMode,
        install_policy: UpdateInstallPolicy,
        now_epoch_seconds: u64,
        cancellation: CancellationToken,
    ) -> Result<UpdateCheckOutcome, UpdateError> {
        validate_manifest_endpoint(manifest_url)?;
        if !self.claim_check(mode, now_epoch_seconds).await? {
            return Ok(UpdateCheckOutcome::SkippedRecently);
        }

        let signature_url = manifest_url
            .strip_suffix(".manifest.json")
            .map(|prefix| format!("{prefix}.manifest.sig"))
            .ok_or(UpdateError::InvalidManifestEndpoint)?;
        let manifest_bytes = transport
            .fetch_bytes(manifest_url, MAX_MANIFEST_BYTES)
            .await
            .map_err(|error| UpdateError::Transport(Box::new(error)))?;
        let signature_bytes = transport
            .fetch_bytes(&signature_url, MAX_SIGNATURE_BYTES)
            .await
            .map_err(|error| UpdateError::Transport(Box::new(error)))?;
        let signature = String::from_utf8(signature_bytes).map_err(UpdateError::SignatureUtf8)?;
        let verified = VerifiedUpdateManifest::verify(
            &manifest_bytes,
            &signature,
            &self.public_key_hex,
            self.target,
            &self.application_version,
        )?;
        let offered_version =
            Version::parse(&verified.manifest.release_version).map_err(|source| {
                UpdateError::InvalidVersion {
                    field: "release_version",
                    source,
                }
            })?;
        if self
            .active_release_version()
            .await?
            .is_some_and(|active| active >= offered_version)
        {
            return Ok(UpdateCheckOutcome::Current {
                version: offered_version,
            });
        }
        if install_policy == UpdateInstallPolicy::CheckOnly {
            return Ok(UpdateCheckOutcome::Available {
                version: offered_version,
            });
        }

        let download_directory = self.root.join("downloads").join(self.target.triple());
        tokio::fs::create_dir_all(&download_directory)
            .await
            .map_err(|source| UpdateError::Io {
                action: "create managed update download directory",
                path: download_directory.clone(),
                source,
            })?;
        let download = download_directory.join(&verified.manifest.archive.filename);
        remove_file_if_present(&download).await?;
        let download_result = transport
            .download_file(
                &verified.manifest.archive.url,
                &download,
                verified.manifest.archive.size,
            )
            .await;
        let result = match download_result {
            Ok(downloaded) if downloaded == verified.manifest.archive.size => self
                .install(&manifest_bytes, &signature, &download, cancellation)
                .await
                .map(|_| UpdateCheckOutcome::Installed {
                    version: offered_version,
                }),
            Ok(downloaded) => Err(UpdateError::ArchiveSizeMismatch {
                expected: verified.manifest.archive.size,
                found: downloaded,
            }),
            Err(error) => Err(UpdateError::Transport(Box::new(error))),
        };
        let _cleanup_result = remove_file_if_present(&download).await;
        result
    }

    async fn claim_check(
        &self,
        mode: UpdateCheckMode,
        now_epoch_seconds: u64,
    ) -> Result<bool, UpdateError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || claim_update_check(&root, mode, now_epoch_seconds))
            .await
            .map_err(UpdateError::BlockingTask)?
    }

    async fn active_release_version(&self) -> Result<Option<Version>, UpdateError> {
        let active = self.active_directory();
        let manifest_path = active.join(MANIFEST_FILE);
        let manifest_bytes = match tokio::fs::read(&manifest_path).await {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(UpdateError::Io {
                    action: "read active update manifest",
                    path: manifest_path,
                    source,
                });
            }
        };
        let signature_path = active.join(SIGNATURE_FILE);
        let signature = tokio::fs::read_to_string(&signature_path)
            .await
            .map_err(|source| UpdateError::Io {
                action: "read active update signature",
                path: signature_path,
                source,
            })?;
        let verified = VerifiedUpdateManifest::verify(
            &manifest_bytes,
            &signature,
            &self.public_key_hex,
            self.target,
            &self.application_version,
        )?;
        Version::parse(&verified.manifest.release_version)
            .map(Some)
            .map_err(|source| UpdateError::InvalidVersion {
                field: "release_version",
                source,
            })
    }

    /// Installs a fully downloaded archive after signature, size, digest, extraction, and health
    /// verification, then activates the complete set as one directory transaction.
    ///
    /// # Errors
    ///
    /// Returns a typed failure while leaving the current active set untouched.
    pub async fn install(
        &self,
        manifest_bytes: &[u8],
        signature_hex: &str,
        archive_path: impl AsRef<Path>,
        cancellation: CancellationToken,
    ) -> Result<VerifiedToolSet, UpdateError> {
        let verified_manifest = VerifiedUpdateManifest::verify(
            manifest_bytes,
            signature_hex,
            &self.public_key_hex,
            self.target,
            &self.application_version,
        )?;
        let archive_path = archive_path.as_ref().to_path_buf();
        let staging = self.staging_directory(&verified_manifest.manifest.release_version);
        let root = self.root.clone();
        let manifest_for_stage = verified_manifest.clone();
        let staging_for_stage = staging.clone();
        tokio::task::spawn_blocking(move || {
            stage_verified_archive(
                &root,
                &staging_for_stage,
                &archive_path,
                &manifest_for_stage,
            )
        })
        .await
        .map_err(UpdateError::BlockingTask)??;

        let entries = verified_manifest.manifest.verification_entries();
        let tool_set = VerifiedToolSet::verify_entries(
            self.target,
            &staging,
            &entries,
            Arc::clone(&self.runner),
            cancellation.child_token(),
        )
        .await
        .map_err(|error| UpdateError::ToolSet(Box::new(error)))?;
        probe_capabilities(&tool_set, self.runner.as_ref(), cancellation.child_token()).await?;

        let root = self.root.clone();
        let target = self.target;
        tokio::task::spawn_blocking(move || activate_staged(&root, target, &staging))
            .await
            .map_err(UpdateError::BlockingTask)??;
        self.load_active(cancellation).await
    }

    /// Loads and re-verifies the current managed set.
    ///
    /// # Errors
    ///
    /// Returns a typed signature, filesystem, digest, or health failure.
    pub async fn load_active(
        &self,
        cancellation: CancellationToken,
    ) -> Result<VerifiedToolSet, UpdateError> {
        self.recover_activation().await?;
        let active = self.active_directory();
        let manifest_bytes =
            tokio::fs::read(active.join(MANIFEST_FILE))
                .await
                .map_err(|source| UpdateError::Io {
                    action: "read active update manifest",
                    path: active.join(MANIFEST_FILE),
                    source,
                })?;
        let signature = tokio::fs::read_to_string(active.join(SIGNATURE_FILE))
            .await
            .map_err(|source| UpdateError::Io {
                action: "read active update signature",
                path: active.join(SIGNATURE_FILE),
                source,
            })?;
        let verified = VerifiedUpdateManifest::verify(
            &manifest_bytes,
            &signature,
            &self.public_key_hex,
            self.target,
            &self.application_version,
        )?;
        let entries = verified.manifest.verification_entries();
        let tool_set = VerifiedToolSet::verify_entries(
            self.target,
            active,
            &entries,
            Arc::clone(&self.runner),
            cancellation.child_token(),
        )
        .await
        .map_err(|error| UpdateError::ToolSet(Box::new(error)))?;
        probe_capabilities(&tool_set, self.runner.as_ref(), cancellation).await?;
        Ok(tool_set)
    }

    /// Records a successful managed-set startup and resets the rollback counter.
    ///
    /// # Errors
    ///
    /// Returns an application-data write failure.
    pub async fn record_startup_success(&self) -> Result<(), UpdateError> {
        let path = self.root.join(FAILURE_FILE);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(UpdateError::Io {
                action: "clear managed startup failures",
                path,
                source,
            }),
        }
    }

    /// Records a managed-set startup failure and rolls back after the fixed threshold.
    ///
    /// Returns `true` when a rollback occurred.
    ///
    /// # Errors
    ///
    /// Returns an application-data or rollback failure.
    pub async fn record_startup_failure(&self) -> Result<bool, UpdateError> {
        let root = self.root.clone();
        let target = self.target;
        tokio::task::spawn_blocking(move || record_failure_and_maybe_rollback(&root, target))
            .await
            .map_err(UpdateError::BlockingTask)?
    }

    /// Immediately replaces the active set with the last-known-good set.
    ///
    /// # Errors
    ///
    /// Returns a confined filesystem failure. If no last-known-good set exists, the active
    /// managed set is removed so resolution falls back to the immutable bundle.
    pub async fn rollback(&self) -> Result<(), UpdateError> {
        let root = self.root.clone();
        let target = self.target;
        tokio::task::spawn_blocking(move || rollback_managed(&root, target))
            .await
            .map_err(UpdateError::BlockingTask)?
    }

    /// Removes managed active, last-known-good, and staging state without touching bundled tools.
    ///
    /// # Errors
    ///
    /// Returns a confined filesystem failure.
    pub async fn reset_to_bundled(&self) -> Result<(), UpdateError> {
        let root = self.root.clone();
        let target = self.target;
        tokio::task::spawn_blocking(move || reset_managed(&root, target))
            .await
            .map_err(UpdateError::BlockingTask)?
    }

    async fn recover_activation(&self) -> Result<(), UpdateError> {
        let root = self.root.clone();
        let target = self.target;
        tokio::task::spawn_blocking(move || recover_interrupted_activation(&root, target))
            .await
            .map_err(UpdateError::BlockingTask)?
    }

    fn active_directory(&self) -> PathBuf {
        active_directory(&self.root, self.target)
    }

    fn staging_directory(&self, release_version: &str) -> PathBuf {
        self.root
            .join("staging")
            .join(format!("{}-{release_version}", self.target.triple()))
    }
}

async fn probe_capabilities(
    tools: &VerifiedToolSet,
    runner: &dyn ProcessRunner,
    cancellation: CancellationToken,
) -> Result<(), UpdateError> {
    let ffmpeg = required_tool(tools, Tool::Ffmpeg)?;
    let yt_dlp = required_tool(tools, Tool::YtDlp)?;
    let deno = required_tool(tools, Tool::Deno)?;
    let encoders = run_health_probe(
        runner,
        ProcessSpec::new(ffmpeg).arguments(["-hide_banner", "-encoders"]),
        cancellation.child_token(),
    )
    .await?;
    let encoder_text = combined_probe_text(&encoders)?;
    for capability in ["libx264", " aac ", "libmp3lame"] {
        if !encoder_text.contains(capability) {
            return Err(UpdateError::MissingCapability {
                tool: Tool::Ffmpeg,
                capability: capability.to_owned(),
            });
        }
    }
    let muxers = run_health_probe(
        runner,
        ProcessSpec::new(ffmpeg).arguments(["-hide_banner", "-muxers"]),
        cancellation.child_token(),
    )
    .await?;
    if !combined_probe_text(&muxers)?
        .lines()
        .any(|line| line.contains(" mp4 ") || line.trim_end().ends_with(" mp4"))
    {
        return Err(UpdateError::MissingCapability {
            tool: Tool::Ffmpeg,
            capability: "MP4 muxer".to_owned(),
        });
    }
    let deno_output = run_health_probe(
        runner,
        ProcessSpec::new(deno).arguments([
            "eval",
            "--no-config",
            "--no-remote",
            "console.log(6*7)",
        ]),
        cancellation.child_token(),
    )
    .await?;
    if combined_probe_text(&deno_output)?.trim() != "42" {
        return Err(UpdateError::MissingCapability {
            tool: Tool::Deno,
            capability: "restricted JavaScript execution".to_owned(),
        });
    }
    let runtime = format!("deno:{}", deno.display());
    let _pairing = run_health_probe(
        runner,
        ProcessSpec::new(yt_dlp).arguments([
            "--ignore-config",
            "--no-update",
            "--no-js-runtimes",
            "--js-runtimes",
            &runtime,
            "--list-extractors",
        ]),
        cancellation,
    )
    .await?;
    Ok(())
}

fn required_tool(tools: &VerifiedToolSet, tool: Tool) -> Result<&Path, UpdateError> {
    tools
        .path(tool)
        .map(crate::path::ExecutablePath::as_path)
        .ok_or(UpdateError::MissingVerifiedTool { tool })
}

async fn run_health_probe(
    runner: &dyn ProcessRunner,
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> Result<crate::process::ProcessOutput, UpdateError> {
    let limit =
        OutputLimit::new(PROBE_MAX_BYTES, PROBE_MAX_LINES).map_err(UpdateError::ProcessSpec)?;
    let output = runner
        .run(
            spec.timeout(PROBE_TIMEOUT).output_limit(limit),
            cancellation,
        )
        .await
        .map_err(|error| UpdateError::ProbeProcess(Box::new(error)))?;
    if !output.status.success {
        return Err(UpdateError::ProbeNonZero);
    }
    Ok(output)
}

fn combined_probe_text(output: &crate::process::ProcessOutput) -> Result<String, UpdateError> {
    let mut bytes = output.capture.stdout.bytes.clone();
    bytes.extend_from_slice(&output.capture.stderr.bytes);
    String::from_utf8(bytes).map_err(UpdateError::ProbeUtf8)
}

fn stage_verified_archive(
    root: &Path,
    staging: &Path,
    archive_path: &Path,
    signed: &VerifiedUpdateManifest,
) -> Result<(), UpdateError> {
    fs::create_dir_all(root).map_err(|source| UpdateError::Io {
        action: "create managed tool root",
        path: root.to_path_buf(),
        source,
    })?;
    let lock_file = open_lock(root)?;
    FileExt::lock_exclusive(&lock_file).map_err(|source| UpdateError::Io {
        action: "lock managed tool root",
        path: root.join(".update.lock"),
        source,
    })?;
    verify_archive_file(archive_path, &signed.manifest.archive)?;
    remove_directory_if_present(staging, "clear update staging directory")?;
    fs::create_dir_all(staging).map_err(|source| UpdateError::Io {
        action: "create update staging directory",
        path: staging.to_path_buf(),
        source,
    })?;
    let extraction = extract_update_zip(archive_path, staging, &signed.manifest.tools);
    if let Err(error) = extraction {
        let _ignored = fs::remove_dir_all(staging);
        return Err(error);
    }
    write_new(
        &staging.join(MANIFEST_FILE),
        &signed.canonical_bytes,
        "write staged update manifest",
    )?;
    write_new(
        &staging.join(SIGNATURE_FILE),
        signed.signature_hex.as_bytes(),
        "write staged update signature",
    )?;
    sync_tree(staging)?;
    Ok(())
}

fn verify_archive_file(path: &Path, archive: &UpdateArchive) -> Result<(), UpdateError> {
    let metadata = fs::metadata(path).map_err(|source| UpdateError::Io {
        action: "inspect downloaded update archive",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(UpdateError::ArchiveNotFile {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() != archive.size {
        return Err(UpdateError::ArchiveSizeMismatch {
            expected: archive.size,
            found: metadata.len(),
        });
    }
    let found = sha256_file(path)?;
    if found != archive.sha256 {
        return Err(UpdateError::ArchiveDigestMismatch {
            expected: archive.sha256.clone(),
            found,
        });
    }
    Ok(())
}

fn extract_update_zip(
    archive_path: &Path,
    destination: &Path,
    tools: &[UpdateTool],
) -> Result<(), UpdateError> {
    let file = File::open(archive_path).map_err(|source| UpdateError::Io {
        action: "open update archive",
        path: archive_path.to_path_buf(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(UpdateError::InvalidZip)?;
    let expected = tools
        .iter()
        .map(|tool| {
            (
                tool.archive_path.replace('\\', "/"),
                (tool.size, tool.sha256.as_str()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut extracted = BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(UpdateError::InvalidZip)?;
        if entry.is_dir() || !entry.is_file() {
            return Err(UpdateError::UnexpectedArchiveEntry {
                path: entry.name().to_owned(),
            });
        }
        reject_link(&entry)?;
        validate_relative_path(entry.name())?;
        let normalized = entry.name().replace('\\', "/");
        let Some((expected_size, expected_digest)) = expected.get(&normalized) else {
            return Err(UpdateError::UnexpectedArchiveEntry { path: normalized });
        };
        if !extracted.insert(normalized.clone()) {
            return Err(UpdateError::DuplicateArchivePath { path: normalized });
        }
        if entry.size() != *expected_size {
            return Err(UpdateError::ToolSizeMismatch {
                path: normalized,
                expected: *expected_size,
                found: entry.size(),
            });
        }
        total = total
            .checked_add(entry.size())
            .ok_or(UpdateError::ExpandedSizeLimit)?;
        if total > MAX_UPDATE_ARCHIVE_BYTES {
            return Err(UpdateError::ExpandedSizeLimit);
        }
        let output = destination.join(&normalized);
        let parent = output.parent().ok_or_else(|| UpdateError::NoParent {
            path: output.clone(),
        })?;
        fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
            action: "create extracted tool parent",
            path: parent.to_path_buf(),
            source,
        })?;
        let mut target = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|source| UpdateError::Io {
                action: "create extracted tool",
                path: output.clone(),
                source,
            })?;
        let mut digest = Sha256::new();
        {
            let mut hashing = HashingReader {
                inner: &mut entry,
                digest: &mut digest,
            };
            io::copy(&mut hashing, &mut target).map_err(|source| UpdateError::Io {
                action: "extract managed tool",
                path: output.clone(),
                source,
            })?;
        }
        target.sync_all().map_err(|source| UpdateError::Io {
            action: "sync extracted tool",
            path: output.clone(),
            source,
        })?;
        let found = format!("{:x}", digest.finalize());
        if found != *expected_digest {
            return Err(UpdateError::ToolDigestMismatch {
                path: normalized,
                expected: (*expected_digest).to_owned(),
                found,
            });
        }
        set_executable(&output)?;
    }
    if extracted.len() != expected.len() {
        return Err(UpdateError::IncompleteArchive);
    }
    Ok(())
}

struct HashingReader<'a, R> {
    inner: &'a mut R,
    digest: &'a mut Sha256,
}

impl<R: Read> Read for HashingReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = self.inner.read(buffer)?;
        self.digest.update(&buffer[..length]);
        Ok(length)
    }
}

fn reject_link(entry: &zip::read::ZipFile<'_, File>) -> Result<(), UpdateError> {
    if let Some(mode) = entry.unix_mode() {
        let file_type = mode & 0o170_000;
        if file_type != 0 && file_type != 0o100_000 {
            return Err(UpdateError::UnsafeArchiveEntry {
                path: entry.name().to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|source| UpdateError::Io {
        action: "set managed tool executable permission",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_executable(path: &Path) -> Result<(), UpdateError> {
    fs::metadata(path)
        .map(|_| ())
        .map_err(|source| UpdateError::Io {
            action: "confirm managed tool executable",
            path: path.to_path_buf(),
            source,
        })
}

fn activate_staged(
    root: &Path,
    target: SupportedTarget,
    staging: &Path,
) -> Result<(), UpdateError> {
    let lock_file = open_lock(root)?;
    FileExt::lock_exclusive(&lock_file).map_err(|source| UpdateError::Io {
        action: "lock managed tool root",
        path: root.join(".update.lock"),
        source,
    })?;
    let active = active_directory(root, target);
    let last_good = last_good_directory(root, target);
    let journal = root.join(ACTIVATION_JOURNAL);
    write_replace(
        &journal,
        format!("{}\n", target.triple()).as_bytes(),
        "write activation journal",
    )?;
    remove_directory_if_present(&last_good, "remove previous last-known-good set")?;
    if active.exists() {
        ensure_parent(&last_good)?;
        fs::rename(&active, &last_good).map_err(|source| UpdateError::Io {
            action: "preserve active set as last-known-good",
            path: active.clone(),
            source,
        })?;
    }
    ensure_parent(&active)?;
    if let Err(source) = fs::rename(staging, &active) {
        if last_good.exists() && !active.exists() {
            let _ignored = fs::rename(&last_good, &active);
        }
        return Err(UpdateError::Io {
            action: "activate verified tool set",
            path: staging.to_path_buf(),
            source,
        });
    }
    fs::remove_file(&journal).map_err(|source| UpdateError::Io {
        action: "clear activation journal",
        path: journal,
        source,
    })?;
    sync_directory(root)?;
    Ok(())
}

fn recover_interrupted_activation(root: &Path, target: SupportedTarget) -> Result<(), UpdateError> {
    let journal = root.join(ACTIVATION_JOURNAL);
    if !journal.exists() {
        return Ok(());
    }
    let active = active_directory(root, target);
    let last_good = last_good_directory(root, target);
    if !active.exists() && last_good.exists() {
        ensure_parent(&active)?;
        fs::rename(&last_good, &active).map_err(|source| UpdateError::Io {
            action: "recover interrupted activation",
            path: last_good,
            source,
        })?;
    }
    fs::remove_file(&journal).map_err(|source| UpdateError::Io {
        action: "clear recovered activation journal",
        path: journal,
        source,
    })
}

fn record_failure_and_maybe_rollback(
    root: &Path,
    target: SupportedTarget,
) -> Result<bool, UpdateError> {
    fs::create_dir_all(root).map_err(|source| UpdateError::Io {
        action: "create managed tool root",
        path: root.to_path_buf(),
        source,
    })?;
    let path = root.join(FAILURE_FILE);
    let failures = match fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice::<StartupFailures>(&bytes).map_or(0, |state| state.failures)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => 0,
        Err(source) => {
            return Err(UpdateError::Io {
                action: "read startup failure state",
                path,
                source,
            });
        }
    }
    .saturating_add(1);
    if failures >= STARTUP_FAILURE_ROLLBACK_THRESHOLD {
        rollback_managed(root, target)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(UpdateError::Io {
                    action: "clear startup failure state after rollback",
                    path,
                    source,
                });
            }
        }
        return Ok(true);
    }
    let bytes =
        serde_json::to_vec(&StartupFailures { failures }).map_err(UpdateError::SerializeState)?;
    write_replace(&path, &bytes, "record managed startup failure")?;
    Ok(false)
}

fn claim_update_check(
    root: &Path,
    mode: UpdateCheckMode,
    now_epoch_seconds: u64,
) -> Result<bool, UpdateError> {
    let lock_file = open_lock(root)?;
    FileExt::lock_exclusive(&lock_file).map_err(|source| UpdateError::Io {
        action: "lock managed tool root",
        path: root.join(".update.lock"),
        source,
    })?;
    let path = root.join(CHECK_STATE_FILE);
    let previous = match fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice::<UpdateCheckState>(&bytes)
                .map_err(UpdateError::DeserializeState)?
                .last_attempt_epoch_seconds
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(UpdateError::Io {
                action: "read managed update check state",
                path,
                source,
            });
        }
    };
    if mode == UpdateCheckMode::Background
        && previous.is_some_and(|last| {
            now_epoch_seconds <= last
                || now_epoch_seconds.saturating_sub(last) < BACKGROUND_UPDATE_INTERVAL_SECS
        })
    {
        return Ok(false);
    }
    let bytes = serde_json::to_vec(&UpdateCheckState {
        last_attempt_epoch_seconds: Some(now_epoch_seconds),
    })
    .map_err(UpdateError::SerializeState)?;
    write_replace(&path, &bytes, "record managed update check attempt")?;
    Ok(true)
}

fn rollback_managed(root: &Path, target: SupportedTarget) -> Result<(), UpdateError> {
    let lock_file = open_lock(root)?;
    FileExt::lock_exclusive(&lock_file).map_err(|source| UpdateError::Io {
        action: "lock managed tool root",
        path: root.join(".update.lock"),
        source,
    })?;
    let active = active_directory(root, target);
    let last_good = last_good_directory(root, target);
    remove_directory_if_present(&active, "remove unhealthy active managed set")?;
    if last_good.exists() {
        ensure_parent(&active)?;
        fs::rename(&last_good, &active).map_err(|source| UpdateError::Io {
            action: "restore last-known-good managed set",
            path: last_good,
            source,
        })?;
    }
    Ok(())
}

fn reset_managed(root: &Path, target: SupportedTarget) -> Result<(), UpdateError> {
    let lock_file = open_lock(root)?;
    FileExt::lock_exclusive(&lock_file).map_err(|source| UpdateError::Io {
        action: "lock managed tool root",
        path: root.join(".update.lock"),
        source,
    })?;
    for path in [
        active_directory(root, target),
        last_good_directory(root, target),
        root.join("staging").join(target.triple()),
        root.join("downloads").join(target.triple()),
    ] {
        remove_directory_if_present(&path, "remove managed tool state")?;
    }
    for path in [root.join(FAILURE_FILE), root.join(ACTIVATION_JOURNAL)] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(UpdateError::Io {
                    action: "remove managed tool state file",
                    path,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn active_directory(root: &Path, target: SupportedTarget) -> PathBuf {
    root.join("active").join(target.triple())
}

fn last_good_directory(root: &Path, target: SupportedTarget) -> PathBuf {
    root.join("last-good").join(target.triple())
}

fn open_lock(root: &Path) -> Result<File, UpdateError> {
    fs::create_dir_all(root).map_err(|source| UpdateError::Io {
        action: "create managed tool root",
        path: root.to_path_buf(),
        source,
    })?;
    let path = root.join(".update.lock");
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| UpdateError::Io {
            action: "open managed tool lock",
            path,
            source,
        })
}

fn validate_archive(archive: &UpdateArchive) -> Result<(), UpdateError> {
    if !archive
        .url
        .starts_with("https://github.com/youssefsz/yt-media/releases/download/")
        || archive.url.contains(['?', '#'])
    {
        return Err(UpdateError::NonImmutableArchiveUrl);
    }
    validate_relative_path(&archive.filename)?;
    if Path::new(&archive.filename).components().count() != 1
        || !Path::new(&archive.filename)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err(UpdateError::InvalidArchiveFilename);
    }
    if archive.size == 0 || archive.size > MAX_UPDATE_ARCHIVE_BYTES {
        return Err(UpdateError::InvalidArchiveSize { size: archive.size });
    }
    validate_sha256(&archive.sha256)
}

fn validate_manifest_endpoint(value: &str) -> Result<(), UpdateError> {
    let endpoint = url::Url::parse(value).map_err(|_| UpdateError::InvalidManifestEndpoint)?;
    let path = endpoint.path();
    if endpoint.scheme() != "https"
        || endpoint.host_str() != Some("github.com")
        || endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || !path.starts_with("/youssefsz/yt-media/releases/")
        || !path.ends_with(".manifest.json")
    {
        return Err(UpdateError::InvalidManifestEndpoint);
    }
    Ok(())
}

fn validate_channel(channel: &str) -> Result<(), UpdateError> {
    if channel.is_empty()
        || channel.len() > 32
        || !channel
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(UpdateError::InvalidChannel);
    }
    Ok(())
}

fn valid_utc_timestamp(value: &str) -> bool {
    value.ends_with('Z')
        && time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .is_ok_and(|timestamp| timestamp.offset().is_utc())
}

fn validate_relative_path(value: &str) -> Result<(), UpdateError> {
    if value.is_empty()
        || value.contains('\0')
        || value.starts_with(['/', '\\'])
        || value.contains('\\')
    {
        return Err(UpdateError::UnsafeRelativePath {
            path: value.to_owned(),
        });
    }
    let path = Path::new(value);
    if path.components().count() == 0
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(UpdateError::UnsafeRelativePath {
            path: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), UpdateError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(UpdateError::InvalidSha256);
    }
    Ok(())
}

fn decode_fixed<const N: usize>(value: &str, field: &'static str) -> Result<[u8; N], UpdateError> {
    let decoded = hex::decode(value).map_err(|source| UpdateError::InvalidHex { field, source })?;
    decoded
        .try_into()
        .map_err(|bytes: Vec<u8>| UpdateError::InvalidHexLength {
            field,
            expected: N,
            found: bytes.len(),
        })
}

fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    let mut file = File::open(path).map_err(|source| UpdateError::Io {
        action: "open file for hashing",
        path: path.to_path_buf(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer).map_err(|source| UpdateError::Io {
            action: "hash file",
            path: path.to_path_buf(),
            source,
        })?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn write_new(path: &Path, bytes: &[u8], action: &'static str) -> Result<(), UpdateError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|source| UpdateError::Io {
            action,
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| UpdateError::Io {
        action,
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| UpdateError::Io {
        action,
        path: path.to_path_buf(),
        source,
    })
}

fn write_replace(path: &Path, bytes: &[u8], action: &'static str) -> Result<(), UpdateError> {
    let parent = path.parent().ok_or_else(|| UpdateError::NoParent {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
        action,
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = path.with_extension("tmp");
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(UpdateError::Io {
                action,
                path: temporary,
                source,
            });
        }
    }
    write_new(&temporary, bytes, action)?;
    if path.exists() {
        fs::remove_file(path).map_err(|source| UpdateError::Io {
            action,
            path: path.to_path_buf(),
            source,
        })?;
    }
    fs::rename(&temporary, path).map_err(|source| UpdateError::Io {
        action,
        path: path.to_path_buf(),
        source,
    })
}

fn ensure_parent(path: &Path) -> Result<(), UpdateError> {
    let parent = path.parent().ok_or_else(|| UpdateError::NoParent {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
        action: "create managed tool parent",
        path: parent.to_path_buf(),
        source,
    })
}

fn remove_directory_if_present(path: &Path, action: &'static str) -> Result<(), UpdateError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(UpdateError::Io {
            action,
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn sync_tree(root: &Path) -> Result<(), UpdateError> {
    for entry in fs::read_dir(root).map_err(|source| UpdateError::Io {
        action: "inspect staged update",
        path: root.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| UpdateError::Io {
            action: "inspect staged update",
            path: root.to_path_buf(),
            source,
        })?;
        if entry
            .file_type()
            .map_err(|source| UpdateError::Io {
                action: "inspect staged update entry",
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            sync_tree(&entry.path())?;
        }
    }
    sync_directory(root)
}

fn sync_directory(path: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| UpdateError::Io {
                action: "sync managed tool directory",
                path: path.to_path_buf(),
                source,
            })
    }
    #[cfg(not(unix))]
    {
        fs::metadata(path)
            .map(|_| ())
            .map_err(|source| UpdateError::Io {
                action: "confirm managed tool directory",
                path: path.to_path_buf(),
                source,
            })
    }
}

async fn remove_file_if_present(path: &Path) -> Result<(), UpdateError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(UpdateError::Io {
            action: "remove managed update download",
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StartupFailures {
    failures: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateCheckState {
    last_attempt_epoch_seconds: Option<u64>,
}

/// Signed managed-update failure.
#[derive(Debug, Error)]
pub enum UpdateError {
    /// The configured channel endpoint was not the repository's HTTPS release-asset path.
    #[error("update manifest endpoint is invalid")]
    InvalidManifestEndpoint,
    /// A bounded update request failed.
    #[error("managed update request failed")]
    Transport(#[source] Box<UpdateTransportError>),
    /// The detached signature response was not UTF-8 hexadecimal text.
    #[error("update manifest signature response was not UTF-8")]
    SignatureUtf8(#[source] std::string::FromUtf8Error),
    /// Manifest JSON was invalid.
    #[error("update manifest JSON is invalid")]
    InvalidJson(#[source] serde_json::Error),
    /// Manifest exceeded its bounded parser input.
    #[error("update manifest is {size} bytes; maximum is 262144")]
    ManifestTooLarge {
        /// Observed bytes.
        size: usize,
    },
    /// Manifest bytes were not RFC 8785 canonical JSON.
    #[error("update manifest is not RFC 8785 canonical JSON")]
    NonCanonicalManifest,
    /// RFC 8785 canonicalization failed.
    #[error("could not canonicalize update manifest")]
    Canonicalize(#[source] serde_json::Error),
    /// Hex encoding was invalid.
    #[error("update {field} is not valid hexadecimal")]
    InvalidHex {
        /// Affected field.
        field: &'static str,
        /// Decoder failure.
        #[source]
        source: hex::FromHexError,
    },
    /// Hex field had the wrong byte length.
    #[error("update {field} is {found} bytes; expected {expected}")]
    InvalidHexLength {
        /// Affected field.
        field: &'static str,
        /// Required bytes.
        expected: usize,
        /// Observed bytes.
        found: usize,
    },
    /// Embedded public key encoding was invalid.
    #[error("embedded update public key is invalid")]
    InvalidPublicKey(#[source] ed25519_dalek::SignatureError),
    /// Weak Ed25519 public keys are never admitted.
    #[error("embedded update public key is weak")]
    WeakPublicKey,
    /// Detached signature did not authenticate the canonical manifest.
    #[error("update manifest signature is invalid")]
    InvalidSignature(#[source] ed25519_dalek::SignatureError),
    /// Manifest schema is not supported.
    #[error("unsupported update manifest schema {found}")]
    UnsupportedSchema {
        /// Observed schema.
        found: u32,
    },
    /// Manifest target differs from the current application target.
    #[error("update target is {found}; expected {expected}")]
    WrongTarget {
        /// Expected target.
        expected: SupportedTarget,
        /// Signed target.
        found: SupportedTarget,
    },
    /// Channel spelling was unsafe or unsupported.
    #[error("update channel is invalid")]
    InvalidChannel,
    /// Semantic version field was invalid.
    #[error("update {field} is not a semantic version")]
    InvalidVersion {
        /// Affected field.
        field: &'static str,
        /// Parser failure.
        #[source]
        source: semver::Error,
    },
    /// Installed application is older than the signed minimum.
    #[error("application {installed} is older than required version {minimum}")]
    ApplicationTooOld {
        /// Installed version.
        installed: Version,
        /// Signed minimum.
        minimum: Version,
    },
    /// Release version was internally inconsistent.
    #[error("release {release} predates its own minimum app version {minimum}")]
    ReleaseBeforeMinimum {
        /// Signed release.
        release: Version,
        /// Signed minimum.
        minimum: Version,
    },
    /// Creation timestamp was not UTC RFC 3339 form.
    #[error("update creation time is not UTC RFC 3339")]
    InvalidCreationTime,
    /// Archive URL was not an immutable HTTPS release asset.
    #[error("update archive URL is not an immutable HTTPS release asset")]
    NonImmutableArchiveUrl,
    /// Archive filename was unsafe or not ZIP.
    #[error("update archive filename is invalid")]
    InvalidArchiveFilename,
    /// Archive size was zero or above the hard limit.
    #[error("update archive size {size} is outside the allowed range")]
    InvalidArchiveSize {
        /// Signed size.
        size: u64,
    },
    /// A SHA-256 field was not lowercase 64-character hexadecimal.
    #[error("update SHA-256 is invalid")]
    InvalidSha256,
    /// Tool inventory repeated an identity.
    #[error("update inventory repeats `{tool}`")]
    DuplicateTool {
        /// Repeated tool.
        tool: Tool,
    },
    /// Tool inventory did not contain all four tools.
    #[error("update inventory must contain exactly yt-dlp, FFmpeg, FFprobe, and Deno")]
    IncompleteToolSet,
    /// Tool version was empty or unreasonably long.
    #[error("update version for `{tool}` is invalid")]
    InvalidToolVersion {
        /// Affected tool.
        tool: Tool,
    },
    /// Tool size was zero or above the hard limit.
    #[error("update size for `{tool}` is invalid: {size}")]
    InvalidToolSize {
        /// Affected tool.
        tool: Tool,
        /// Signed size.
        size: u64,
    },
    /// A signed or archived relative path was unsafe.
    #[error("update path `{path}` is unsafe")]
    UnsafeRelativePath {
        /// Rejected path.
        path: String,
    },
    /// Two records or entries map to one portable path.
    #[error("update repeats archive path `{path}`")]
    DuplicateArchivePath {
        /// Repeated path.
        path: String,
    },
    /// Download path was not a regular file.
    #[error("downloaded update archive `{}` is not a file", path.display())]
    ArchiveNotFile {
        /// Affected path.
        path: PathBuf,
    },
    /// Downloaded size differed from the signed record.
    #[error("downloaded update size is {found}; expected {expected}")]
    ArchiveSizeMismatch {
        /// Signed size.
        expected: u64,
        /// Downloaded size.
        found: u64,
    },
    /// Downloaded digest differed from the signed record.
    #[error("downloaded update digest is {found}; expected {expected}")]
    ArchiveDigestMismatch {
        /// Signed digest.
        expected: String,
        /// Downloaded digest.
        found: String,
    },
    /// ZIP container was invalid.
    #[error("update archive is not a valid ZIP")]
    InvalidZip(#[source] zip::result::ZipError),
    /// ZIP entry was absent from the signed inventory.
    #[error("update archive contains unexpected entry `{path}`")]
    UnexpectedArchiveEntry {
        /// Rejected entry.
        path: String,
    },
    /// ZIP link or special file was forbidden.
    #[error("update archive entry `{path}` is a link or special file")]
    UnsafeArchiveEntry {
        /// Rejected entry.
        path: String,
    },
    /// Extracted size exceeded the hard limit.
    #[error("update archive expands beyond the hard size limit")]
    ExpandedSizeLimit,
    /// An expected tool was absent from the ZIP.
    #[error("update archive is incomplete")]
    IncompleteArchive,
    /// ZIP entry size differed from the signed record.
    #[error("update entry `{path}` is {found} bytes; expected {expected}")]
    ToolSizeMismatch {
        /// Affected entry.
        path: String,
        /// Signed size.
        expected: u64,
        /// ZIP size.
        found: u64,
    },
    /// Extracted digest differed from the signed record.
    #[error("update entry `{path}` digest is {found}; expected {expected}")]
    ToolDigestMismatch {
        /// Affected entry.
        path: String,
        /// Signed digest.
        expected: String,
        /// Extracted digest.
        found: String,
    },
    /// A path unexpectedly lacked a parent.
    #[error("managed update path `{}` has no parent", path.display())]
    NoParent {
        /// Affected path.
        path: PathBuf,
    },
    /// Managed set hash, path, or identity verification failed.
    #[error("managed tool-set verification failed")]
    ToolSet(#[source] Box<ToolSetVerificationError>),
    /// A required verified executable was absent.
    #[error("verified managed set omitted `{tool}`")]
    MissingVerifiedTool {
        /// Missing tool.
        tool: Tool,
    },
    /// Health probe process failed.
    #[error("managed tool health probe failed")]
    ProbeProcess(#[source] Box<ProcessError>),
    /// Health probe process specification was invalid.
    #[error("managed tool health probe specification was invalid")]
    ProcessSpec(#[source] crate::process::ProcessSpecError),
    /// Health probe exited unsuccessfully.
    #[error("managed tool health probe exited unsuccessfully")]
    ProbeNonZero,
    /// Health probe output was not UTF-8.
    #[error("managed tool health probe output was not UTF-8")]
    ProbeUtf8(#[source] std::string::FromUtf8Error),
    /// Required codec, muxer, or JavaScript behavior was absent.
    #[error("`{tool}` health probe omitted {capability}")]
    MissingCapability {
        /// Affected tool.
        tool: Tool,
        /// Required behavior.
        capability: String,
    },
    /// A filesystem operation failed.
    #[error("{action} failed for `{}`", path.display())]
    Io {
        /// Stable operation context.
        action: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Filesystem source.
        #[source]
        source: io::Error,
    },
    /// Blocking filesystem task could not be joined.
    #[error("managed update filesystem task failed")]
    BlockingTask(#[source] tokio::task::JoinError),
    /// Durable state serialization failed.
    #[error("could not serialize managed update state")]
    SerializeState(#[source] serde_json::Error),
    /// Durable state could not be parsed safely.
    #[error("could not parse managed update state")]
    DeserializeState(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs::{self, File},
        io::Write,
        path::Path,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use ed25519_dalek::{Signer, SigningKey};
    use semver::Version;
    use sha2::Digest;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    use crate::{
        cancellation::CancellationToken,
        process::{
            CapturedOutput, ProcessError, ProcessExitStatus, ProcessOutput, ProcessRunner,
            ProcessSpec, StreamCapture,
        },
        target::SupportedTarget,
        tool::Tool,
    };

    use super::{
        BACKGROUND_UPDATE_INTERVAL_SECS, MAX_UPDATE_ARCHIVE_BYTES, UpdateArchive, UpdateCheckMode,
        UpdateError, UpdateManager, UpdateManifest, UpdateTool, VerifiedUpdateManifest,
    };

    #[derive(Default)]
    struct HealthRunner {
        unhealthy: bool,
        calls: Mutex<Vec<Vec<OsString>>>,
    }

    #[async_trait]
    impl ProcessRunner for HealthRunner {
        async fn run(
            &self,
            spec: ProcessSpec,
            _cancellation: CancellationToken,
        ) -> Result<ProcessOutput, ProcessError> {
            let arguments = spec
                .argument_values()
                .map(OsString::from)
                .collect::<Vec<_>>();
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(arguments.clone());
            }
            let executable = spec.executable().to_string_lossy();
            let rendered = arguments
                .iter()
                .map(|value| value.to_string_lossy())
                .collect::<Vec<_>>();
            let output = if rendered == ["--version"] {
                if executable.contains("yt-dlp") {
                    "2026.07.01\n"
                } else {
                    "deno 2.9.0\n"
                }
            } else if rendered == ["-version"] {
                if executable.contains("ffprobe") {
                    "ffprobe version 8.1\n"
                } else {
                    "ffmpeg version 8.1\n"
                }
            } else if rendered.iter().any(|argument| argument == "-encoders") {
                if self.unhealthy {
                    " V..... libx264\n A..... aac \n"
                } else {
                    " V..... libx264\n A..... aac \n A..... libmp3lame\n"
                }
            } else if rendered.iter().any(|argument| argument == "-muxers") {
                " E mp4             MP4\n"
            } else if rendered.iter().any(|argument| argument == "eval") {
                "42\n"
            } else {
                "youtube\n"
            };
            Ok(ProcessOutput {
                status: ProcessExitStatus {
                    success: true,
                    code: Some(0),
                },
                capture: CapturedOutput {
                    stdout: StreamCapture {
                        bytes: output.as_bytes().to_vec(),
                        ..StreamCapture::default()
                    },
                    ..CapturedOutput::default()
                },
            })
        }
    }

    fn target() -> SupportedTarget {
        SupportedTarget::current().unwrap_or(SupportedTarget::WindowsX64)
    }

    fn tool_bytes(tool: Tool) -> Vec<u8> {
        format!("fixture-{}", tool.name()).into_bytes()
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", sha2::Sha256::digest(bytes))
    }

    fn manifest(archive_size: u64, archive_digest: String) -> UpdateManifest {
        let target = target();
        UpdateManifest {
            schema_version: 1,
            channel: "stable".to_owned(),
            release_version: "0.2.0".to_owned(),
            target,
            minimum_app_version: "0.1.0".to_owned(),
            created_at: "2026-07-30T12:00:00Z".to_owned(),
            archive: UpdateArchive {
                url:
                    "https://github.com/youssefsz/yt-media/releases/download/tools-v0.2.0/tools.zip"
                        .to_owned(),
                filename: "tools.zip".to_owned(),
                size: archive_size,
                sha256: archive_digest,
            },
            tools: Tool::ALL
                .into_iter()
                .map(|tool| {
                    let bytes = tool_bytes(tool);
                    UpdateTool {
                        tool,
                        version: match tool {
                            Tool::YtDlp => "2026.07.01",
                            Tool::Ffmpeg | Tool::Ffprobe => "8.1",
                            Tool::Deno => "2.9.0",
                        }
                        .to_owned(),
                        archive_path: tool.executable_name(target),
                        size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                        sha256: digest(&bytes),
                    }
                })
                .collect(),
        }
    }

    fn signing_material(
        manifest: &UpdateManifest,
    ) -> Result<(Vec<u8>, String, String), Box<dyn std::error::Error>> {
        let signing = SigningKey::from_bytes(&[7_u8; 32]);
        let bytes = manifest.canonical_bytes()?;
        let signature = signing.sign(&bytes);
        Ok((
            bytes,
            hex::encode(signature.to_bytes()),
            hex::encode(signing.verifying_key().to_bytes()),
        ))
    }

    fn write_archive(path: &Path, extra: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let file = File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default().unix_permissions(0o755);
        for tool in Tool::ALL {
            zip.start_file(tool.executable_name(target()), options)?;
            zip.write_all(&tool_bytes(tool))?;
        }
        if let Some(extra) = extra {
            zip.start_file(extra, options)?;
            zip.write_all(b"unexpected")?;
        }
        zip.finish()?;
        Ok(())
    }

    #[test]
    fn rejects_unsigned_wrong_target_and_oversized_manifests()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut update = manifest(1, "0".repeat(64));
        let (bytes, _signature, public_key) = signing_material(&update)?;
        assert!(matches!(
            VerifiedUpdateManifest::verify(
                &bytes,
                &"00".repeat(64),
                &public_key,
                target(),
                &Version::parse("0.1.0")?
            ),
            Err(UpdateError::InvalidSignature(_))
        ));
        update.target = SupportedTarget::ALL
            .into_iter()
            .find(|candidate| *candidate != target())
            .ok_or("alternate target missing")?;
        let (bytes, signature, public_key) = signing_material(&update)?;
        assert!(matches!(
            VerifiedUpdateManifest::verify(
                &bytes,
                &signature,
                &public_key,
                target(),
                &Version::parse("0.1.0")?
            ),
            Err(UpdateError::WrongTarget { .. })
        ));
        update.target = target();
        update.archive.size = MAX_UPDATE_ARCHIVE_BYTES + 1;
        let (bytes, signature, public_key) = signing_material(&update)?;
        assert!(matches!(
            VerifiedUpdateManifest::verify(
                &bytes,
                &signature,
                &public_key,
                target(),
                &Version::parse("0.1.0")?
            ),
            Err(UpdateError::InvalidArchiveSize { .. })
        ));
        update.archive.size = 1;
        update.created_at = "2026-99-30T12:00:00Z".to_owned();
        let (bytes, signature, public_key) = signing_material(&update)?;
        assert!(matches!(
            VerifiedUpdateManifest::verify(
                &bytes,
                &signature,
                &public_key,
                target(),
                &Version::parse("0.1.0")?
            ),
            Err(UpdateError::InvalidCreationTime)
        ));
        Ok(())
    }

    #[test]
    fn rejects_noncanonical_manifest_and_path_traversal() -> Result<(), Box<dyn std::error::Error>>
    {
        let update = manifest(1, "0".repeat(64));
        let (_bytes, signature, public_key) = signing_material(&update)?;
        let pretty = serde_json::to_vec_pretty(&update)?;
        assert!(matches!(
            VerifiedUpdateManifest::verify(
                &pretty,
                &signature,
                &public_key,
                target(),
                &Version::parse("0.1.0")?
            ),
            Err(UpdateError::NonCanonicalManifest)
        ));
        let mut unsafe_update = update;
        let first = unsafe_update.tools.first_mut().ok_or("tool missing")?;
        first.archive_path = "../escape".to_owned();
        let (bytes, signature, public_key) = signing_material(&unsafe_update)?;
        assert!(matches!(
            VerifiedUpdateManifest::verify(
                &bytes,
                &signature,
                &public_key,
                target(),
                &Version::parse("0.1.0")?
            ),
            Err(UpdateError::UnsafeRelativePath { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn installs_healthy_set_and_rejects_corruption_without_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let archive = directory.path().join("tools.zip");
        write_archive(&archive, None)?;
        let archive_bytes = fs::read(&archive)?;
        let update = manifest(u64::try_from(archive_bytes.len())?, digest(&archive_bytes));
        let (manifest_bytes, signature, public_key) = signing_material(&update)?;
        let runner = Arc::new(HealthRunner::default());
        let manager = UpdateManager::new(
            directory.path().join("managed"),
            target(),
            Version::parse("0.1.0")?,
            public_key,
            runner,
        );
        let installed = manager
            .install(
                &manifest_bytes,
                &signature,
                &archive,
                CancellationToken::new(),
            )
            .await?;
        assert!(Tool::ALL.iter().all(|tool| installed.path(*tool).is_some()));
        fs::write(&archive, b"corrupt")?;
        assert!(matches!(
            manager
                .install(
                    &manifest_bytes,
                    &signature,
                    &archive,
                    CancellationToken::new()
                )
                .await,
            Err(UpdateError::ArchiveSizeMismatch { .. } | UpdateError::ArchiveDigestMismatch { .. })
        ));
        assert!(manager.load_active(CancellationToken::new()).await.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn unhealthy_and_unexpected_archives_never_activate()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let archive = directory.path().join("tools.zip");
        write_archive(&archive, Some("../escape"))?;
        let archive_bytes = fs::read(&archive)?;
        let update = manifest(u64::try_from(archive_bytes.len())?, digest(&archive_bytes));
        let (manifest_bytes, signature, public_key) = signing_material(&update)?;
        let manager = UpdateManager::new(
            directory.path().join("managed"),
            target(),
            Version::parse("0.1.0")?,
            public_key,
            Arc::new(HealthRunner::default()),
        );
        assert!(
            manager
                .install(
                    &manifest_bytes,
                    &signature,
                    &archive,
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
        assert!(
            !directory
                .path()
                .join("managed")
                .join("active")
                .join(target().triple())
                .exists()
        );

        write_archive(&archive, None)?;
        let archive_bytes = fs::read(&archive)?;
        let update = manifest(u64::try_from(archive_bytes.len())?, digest(&archive_bytes));
        let (manifest_bytes, signature, public_key) = signing_material(&update)?;
        let manager = UpdateManager::new(
            directory.path().join("managed"),
            target(),
            Version::parse("0.1.0")?,
            public_key,
            Arc::new(HealthRunner {
                unhealthy: true,
                calls: Mutex::default(),
            }),
        );
        assert!(matches!(
            manager
                .install(
                    &manifest_bytes,
                    &signature,
                    &archive,
                    CancellationToken::new()
                )
                .await,
            Err(UpdateError::MissingCapability { .. })
        ));
        assert!(
            !directory
                .path()
                .join("managed")
                .join("active")
                .join(target().triple())
                .exists()
        );
        Ok(())
    }

    #[tokio::test]
    async fn repeated_failures_roll_back_and_reset_preserves_external_baseline()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let managed = directory.path().join("managed");
        let active = managed.join("active").join(target().triple());
        let last_good = managed.join("last-good").join(target().triple());
        fs::create_dir_all(&active)?;
        fs::create_dir_all(&last_good)?;
        fs::write(active.join("marker"), b"bad")?;
        fs::write(last_good.join("marker"), b"good")?;
        let baseline = directory.path().join("bundle");
        fs::create_dir_all(&baseline)?;
        fs::write(baseline.join("immutable"), b"baseline")?;
        let update_manager = UpdateManager::new(
            &managed,
            target(),
            Version::parse("0.1.0")?,
            "00".repeat(32),
            Arc::new(HealthRunner::default()),
        );
        assert!(!update_manager.record_startup_failure().await?);
        assert!(update_manager.record_startup_failure().await?);
        assert_eq!(fs::read(active.join("marker"))?, b"good");
        update_manager.reset_to_bundled().await?;
        assert!(!active.exists());
        assert_eq!(fs::read(baseline.join("immutable"))?, b"baseline");
        Ok(())
    }

    #[tokio::test]
    async fn background_check_is_durably_limited_and_manual_check_is_never_suppressed()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let update_manager = UpdateManager::new(
            directory.path(),
            target(),
            Version::parse("0.1.0")?,
            "00".repeat(32),
            Arc::new(HealthRunner::default()),
        );
        assert!(
            update_manager
                .claim_check(UpdateCheckMode::Background, 1_000)
                .await?
        );
        assert!(
            !update_manager
                .claim_check(UpdateCheckMode::Background, 1_001)
                .await?
        );
        assert!(
            update_manager
                .claim_check(UpdateCheckMode::Manual, 1_002)
                .await?
        );
        assert!(
            update_manager
                .claim_check(
                    UpdateCheckMode::Background,
                    1_002 + BACKGROUND_UPDATE_INTERVAL_SECS
                )
                .await?
        );
        Ok(())
    }
}
