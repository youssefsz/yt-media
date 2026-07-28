//! Versioned sidecar inventory shared by runtime verification and `xtask`.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{target::SupportedTarget, tool::Tool};

/// The only manifest schema understood by this engine version.
pub const SIDECAR_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Complete baseline inventory for every release target.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarManifest {
    /// Manifest contract version.
    pub schema_version: u32,
    /// Target-specific tool inventories.
    pub targets: Vec<TargetManifest>,
}

impl SidecarManifest {
    /// Parses and validates a JSON manifest.
    ///
    /// # Errors
    ///
    /// Returns a typed schema, JSON, completeness, or invariant failure.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ManifestError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(ManifestError::InvalidJson)?;
        let schema_version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or(ManifestError::MissingSchemaVersion)?;
        if schema_version != u64::from(SIDECAR_MANIFEST_SCHEMA_VERSION) {
            return Err(ManifestError::UnsupportedSchemaVersion {
                found: schema_version,
                supported: SIDECAR_MANIFEST_SCHEMA_VERSION,
            });
        }
        let manifest: Self = serde_json::from_value(value).map_err(ManifestError::InvalidJson)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validates completeness and security-sensitive invariants.
    ///
    /// # Errors
    ///
    /// Returns the first structural invariant violation.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != SIDECAR_MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchemaVersion {
                found: u64::from(self.schema_version),
                supported: SIDECAR_MANIFEST_SCHEMA_VERSION,
            });
        }

        let mut targets = BTreeSet::new();
        for target_manifest in &self.targets {
            if !targets.insert(target_manifest.target) {
                return Err(ManifestError::DuplicateTarget {
                    target: target_manifest.target,
                });
            }
            target_manifest.validate()?;
        }
        for target in SupportedTarget::ALL {
            if !targets.contains(&target) {
                return Err(ManifestError::MissingTarget { target });
            }
        }
        Ok(())
    }

    /// Returns the inventory for one target.
    #[must_use]
    pub fn target(&self, target: SupportedTarget) -> Option<&TargetManifest> {
        self.targets
            .iter()
            .find(|manifest| manifest.target == target)
    }
}

/// Complete tool inventory for one release target.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetManifest {
    /// Rust target triple.
    pub target: SupportedTarget,
    /// The four required tool records.
    pub tools: Vec<ToolManifest>,
}

impl TargetManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        let mut tools = BTreeSet::new();
        let mut executable_paths = BTreeSet::new();
        for tool_manifest in &self.tools {
            if !tools.insert(tool_manifest.tool) {
                return Err(ManifestError::DuplicateTool {
                    target: self.target,
                    tool: tool_manifest.tool,
                });
            }
            tool_manifest.validate(self.target)?;
            for executable in &tool_manifest.executables {
                let path = executable
                    .archive_path
                    .replace('\\', "/")
                    .to_ascii_lowercase();
                if !executable_paths.insert(path) {
                    return Err(ManifestError::DuplicateTargetExecutablePath {
                        target: self.target,
                        path: executable.archive_path.clone(),
                    });
                }
            }
        }
        for tool in Tool::ALL {
            if !tools.contains(&tool) {
                return Err(ManifestError::MissingTool {
                    target: self.target,
                    tool,
                });
            }
        }
        Ok(())
    }

    /// Returns one tool record.
    #[must_use]
    pub fn tool(&self, tool: Tool) -> Option<&ToolManifest> {
        self.tools.iter().find(|manifest| manifest.tool == tool)
    }
}

/// Inventory record for one external tool.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolManifest {
    /// Stable tool identity.
    pub tool: Tool,
    /// Expected version probe value.
    pub version: String,
    /// Verified upstream source artifact.
    pub source: SourceArtifact,
    /// Executables consumed by the product.
    pub executables: Vec<ExecutableArtifact>,
    /// Whether executable digests originate upstream or from the native build workflow.
    pub distribution: Distribution,
    /// Auditable origin and license metadata.
    pub provenance: Provenance,
}

impl ToolManifest {
    fn validate(&self, target: SupportedTarget) -> Result<(), ManifestError> {
        if self.version != self.tool.baseline_version() {
            return Err(ManifestError::UnexpectedVersion {
                target,
                tool: self.tool,
                expected: self.tool.baseline_version(),
                found: self.version.clone(),
            });
        }
        self.source.validate(self.tool)?;
        if self.executables.is_empty() {
            return Err(ManifestError::MissingExecutable {
                target,
                tool: self.tool,
            });
        }

        let mut destinations = BTreeSet::new();
        for executable in &self.executables {
            validate_relative_path(&executable.archive_path).map_err(|reason| {
                ManifestError::UnsafeExecutablePath {
                    target,
                    tool: self.tool,
                    path: executable.archive_path.clone(),
                    reason,
                }
            })?;
            let destination_key = executable
                .archive_path
                .replace('\\', "/")
                .to_ascii_lowercase();
            if !destinations.insert(destination_key) {
                return Err(ManifestError::DuplicateExecutablePath {
                    target,
                    tool: self.tool,
                    path: executable.archive_path.clone(),
                });
            }
            match self.distribution {
                Distribution::UpstreamRelease => {
                    let digest = executable.sha256.as_deref().ok_or(
                        ManifestError::MissingExecutableDigest {
                            target,
                            tool: self.tool,
                        },
                    )?;
                    validate_sha256(digest).map_err(|reason| ManifestError::MalformedHash {
                        tool: self.tool,
                        field: "executables.sha256",
                        reason,
                    })?;
                    if executable.size == Some(0) || executable.size.is_none() {
                        return Err(ManifestError::InvalidSize {
                            tool: self.tool,
                            field: "executables.size",
                        });
                    }
                }
                Distribution::NativeBuild { .. } => {
                    if executable.sha256.is_some() || executable.size.is_some() {
                        return Err(ManifestError::BuildDigestMustComeFromReceipt {
                            target,
                            tool: self.tool,
                        });
                    }
                }
            }
        }
        if let Distribution::NativeBuild { ref receipt_file } = self.distribution {
            validate_relative_path(receipt_file).map_err(|reason| {
                ManifestError::UnsafeReceiptPath {
                    target,
                    tool: self.tool,
                    path: receipt_file.clone(),
                    reason,
                }
            })?;
        }
        self.provenance.validate(self.tool)
    }
}

/// A pinned upstream archive or raw file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceArtifact {
    /// Immutable HTTPS download URL.
    pub url: String,
    /// Tag, commit, or release identifier represented by the URL.
    pub source_ref: String,
    /// Downloaded filename.
    pub filename: String,
    /// Container encoding.
    pub archive_format: ArchiveFormat,
    /// SHA-256 digest of the downloaded bytes.
    pub sha256: String,
    /// Exact byte size of the downloaded bytes.
    pub size: u64,
}

impl SourceArtifact {
    fn validate(&self, tool: Tool) -> Result<(), ManifestError> {
        if !self.url.starts_with("https://") {
            return Err(ManifestError::InsecureSourceUrl {
                tool,
                url: self.url.clone(),
            });
        }
        if self.source_ref.trim().is_empty() {
            return Err(ManifestError::MissingSourceRef { tool });
        }
        validate_relative_path(&self.filename).map_err(|reason| {
            ManifestError::UnsafeSourcePath {
                tool,
                path: self.filename.clone(),
                reason,
            }
        })?;
        validate_sha256(&self.sha256).map_err(|reason| ManifestError::MalformedHash {
            tool,
            field: "source.sha256",
            reason,
        })?;
        if self.size == 0 {
            return Err(ManifestError::InvalidSize {
                tool,
                field: "source.size",
            });
        }
        Ok(())
    }
}

/// Supported source artifact encodings.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    /// One executable with no container.
    Raw,
    /// ZIP archive.
    Zip,
    /// XZ-compressed tar archive.
    TarXz,
    /// Gzip-compressed tar archive.
    TarGz,
}

/// One executable expected after fetch or native build.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableArtifact {
    /// Relative path inside the extracted cache or native build output.
    pub archive_path: String,
    /// Upstream executable digest; native builds use a build receipt instead.
    pub sha256: Option<String>,
    /// Upstream executable size; native builds use a build receipt instead.
    pub size: Option<u64>,
}

/// Origin of executable digests.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Distribution {
    /// Executable digest and size are pinned directly in the baseline manifest.
    UpstreamRelease,
    /// A native workflow records executable digests and sizes after a reproducible source build.
    NativeBuild {
        /// Relative build receipt path expected in the verified cache.
        receipt_file: String,
    },
}

/// Auditable source and build metadata.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Canonical upstream repository.
    pub repository: String,
    /// Upstream license identifier or explicit review note.
    pub license: String,
    /// Repository-owned build definition for native builds, when applicable.
    pub build_definition: Option<String>,
    /// Additional pinned sources used by a native build.
    #[serde(default)]
    pub build_inputs: Vec<BuildInput>,
    /// Additional deterministic build metadata.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl Provenance {
    fn validate(&self, tool: Tool) -> Result<(), ManifestError> {
        if !self.repository.starts_with("https://") {
            return Err(ManifestError::InvalidProvenance {
                tool,
                field: "repository",
            });
        }
        if self.license.trim().is_empty() {
            return Err(ManifestError::InvalidProvenance {
                tool,
                field: "license",
            });
        }
        if let Some(path) = &self.build_definition {
            validate_relative_path(path).map_err(|reason| ManifestError::UnsafeBuildPath {
                tool,
                path: path.clone(),
                reason,
            })?;
        }
        for input in &self.build_inputs {
            input.source.validate(tool)?;
            if input.name.trim().is_empty() {
                return Err(ManifestError::InvalidProvenance {
                    tool,
                    field: "build_inputs.name",
                });
            }
        }
        Ok(())
    }
}

/// An additional source archive required by a native build.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildInput {
    /// Stable dependency name.
    pub name: String,
    /// Pinned source artifact.
    pub source: SourceArtifact,
}

/// Versioned digest receipt emitted by one native `FFmpeg` build.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReceipt {
    /// Receipt contract version.
    pub schema_version: u32,
    /// Native runner target.
    pub target: SupportedTarget,
    /// Exact `FFmpeg` source ref compiled by the workflow.
    pub source_ref: String,
    /// Ordered configure arguments and relevant compiler settings.
    pub build_configuration: Vec<String>,
    /// Digests of the produced `FFmpeg` and `FFprobe` executables.
    pub executables: Vec<ReceiptExecutable>,
}

impl BuildReceipt {
    /// Parses and validates one receipt for its expected target and source ref.
    ///
    /// # Errors
    ///
    /// Returns a typed JSON or receipt invariant failure.
    pub fn from_json(
        bytes: &[u8],
        expected_target: SupportedTarget,
        expected_source_ref: &str,
    ) -> Result<Self, ManifestError> {
        let receipt: Self =
            serde_json::from_slice(bytes).map_err(ManifestError::InvalidBuildReceiptJson)?;
        receipt.validate(expected_target, expected_source_ref)?;
        Ok(receipt)
    }

    /// Validates one native build receipt.
    ///
    /// # Errors
    ///
    /// Returns a typed error for schema, target, source, executable, path, hash, or size failures.
    pub fn validate(
        &self,
        expected_target: SupportedTarget,
        expected_source_ref: &str,
    ) -> Result<(), ManifestError> {
        if self.schema_version != SIDECAR_MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedBuildReceiptSchema {
                found: self.schema_version,
            });
        }
        if self.target != expected_target {
            return Err(ManifestError::BuildReceiptTargetMismatch {
                expected: expected_target,
                found: self.target,
            });
        }
        if self.source_ref != expected_source_ref {
            return Err(ManifestError::BuildReceiptSourceMismatch {
                expected: expected_source_ref.to_owned(),
                found: self.source_ref.clone(),
            });
        }
        if self.build_configuration.is_empty()
            || self
                .build_configuration
                .iter()
                .any(|argument| argument.trim().is_empty())
        {
            return Err(ManifestError::InvalidBuildConfiguration);
        }
        let mut tools = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for executable in &self.executables {
            if !matches!(executable.tool, Tool::Ffmpeg | Tool::Ffprobe) {
                return Err(ManifestError::UnexpectedBuildReceiptTool {
                    tool: executable.tool,
                });
            }
            if !tools.insert(executable.tool) {
                return Err(ManifestError::DuplicateBuildReceiptTool {
                    tool: executable.tool,
                });
            }
            validate_relative_path(&executable.path).map_err(|reason| {
                ManifestError::UnsafeBuildReceiptExecutablePath {
                    path: executable.path.clone(),
                    reason,
                }
            })?;
            if !paths.insert(executable.path.replace('\\', "/").to_ascii_lowercase()) {
                return Err(ManifestError::DuplicateBuildReceiptPath {
                    path: executable.path.clone(),
                });
            }
            validate_sha256(&executable.sha256).map_err(|reason| {
                ManifestError::MalformedBuildReceiptHash {
                    tool: executable.tool,
                    reason,
                }
            })?;
            if executable.size == 0 {
                return Err(ManifestError::InvalidBuildReceiptSize {
                    tool: executable.tool,
                });
            }
        }
        for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
            if !tools.contains(&tool) {
                return Err(ManifestError::MissingBuildReceiptTool { tool });
            }
        }
        Ok(())
    }

    /// Returns the digest record for one tool.
    #[must_use]
    pub fn executable(&self, tool: Tool) -> Option<&ReceiptExecutable> {
        self.executables
            .iter()
            .find(|executable| executable.tool == tool)
    }
}

/// One executable digest recorded by a native build.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptExecutable {
    /// `FFmpeg` or `FFprobe` identity.
    pub tool: Tool,
    /// Relative build output path.
    pub path: String,
    /// SHA-256 digest.
    pub sha256: String,
    /// Exact byte size.
    pub size: u64,
}

/// Validates one lower-case hexadecimal SHA-256 value.
///
/// # Errors
///
/// Returns a static reason when the value is not exactly 64 lower-case hexadecimal characters.
pub fn validate_sha256(value: &str) -> Result<(), &'static str> {
    if value.len() != 64 {
        return Err("digest must contain exactly 64 hexadecimal characters");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("digest must use lower-case hexadecimal characters");
    }
    Ok(())
}

/// Validates an archive-relative path without filesystem access.
///
/// # Errors
///
/// Returns a static reason for empty, absolute, parent, root, or prefix components.
pub fn validate_relative_path(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("path must not be empty");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err("absolute paths are forbidden");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => return Err("current-directory components are forbidden"),
            Component::ParentDir => return Err("parent-directory components are forbidden"),
            Component::RootDir => return Err("root components are forbidden"),
            Component::Prefix(_) => return Err("platform path prefixes are forbidden"),
        }
    }
    Ok(())
}

/// Invalid sidecar manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// JSON parsing or type conversion failed.
    #[error("invalid sidecar manifest JSON")]
    InvalidJson(#[source] serde_json::Error),
    /// Native build receipt JSON parsing failed.
    #[error("invalid native build receipt JSON")]
    InvalidBuildReceiptJson(#[source] serde_json::Error),
    /// The schema field was absent or not an unsigned integer.
    #[error("sidecar manifest is missing an integer schema_version")]
    MissingSchemaVersion,
    /// The manifest uses an unsupported schema.
    #[error("unsupported sidecar manifest schema {found}; supported schema is {supported}")]
    UnsupportedSchemaVersion {
        /// Version found in the document.
        found: u64,
        /// Version understood by this engine.
        supported: u32,
    },
    /// A target was repeated.
    #[error("target `{target}` appears more than once")]
    DuplicateTarget {
        /// Repeated target.
        target: SupportedTarget,
    },
    /// A required target was absent.
    #[error("required target `{target}` is missing")]
    MissingTarget {
        /// Missing target.
        target: SupportedTarget,
    },
    /// A tool was repeated within one target.
    #[error("tool `{tool}` appears more than once for `{target}`")]
    DuplicateTool {
        /// Affected target.
        target: SupportedTarget,
        /// Repeated tool.
        tool: Tool,
    },
    /// A required tool was absent.
    #[error("tool `{tool}` is missing for `{target}`")]
    MissingTool {
        /// Affected target.
        target: SupportedTarget,
        /// Missing tool.
        tool: Tool,
    },
    /// A baseline version differs from the locked Plan 01 version.
    #[error("tool `{tool}` for `{target}` uses {found}; expected {expected}")]
    UnexpectedVersion {
        /// Affected target.
        target: SupportedTarget,
        /// Affected tool.
        tool: Tool,
        /// Locked version.
        expected: &'static str,
        /// Manifest value.
        found: String,
    },
    /// No executable was declared.
    #[error("tool `{tool}` for `{target}` declares no executable")]
    MissingExecutable {
        /// Affected target.
        target: SupportedTarget,
        /// Affected tool.
        tool: Tool,
    },
    /// A release executable omitted its digest.
    #[error("release tool `{tool}` for `{target}` omits its executable digest")]
    MissingExecutableDigest {
        /// Affected target.
        target: SupportedTarget,
        /// Affected tool.
        tool: Tool,
    },
    /// Native build digests must be supplied only by the build receipt.
    #[error(
        "native build tool `{tool}` for `{target}` must obtain executable digests from its receipt"
    )]
    BuildDigestMustComeFromReceipt {
        /// Affected target.
        target: SupportedTarget,
        /// Affected tool.
        tool: Tool,
    },
    /// A digest was malformed.
    #[error("tool `{tool}` has malformed {field}: {reason}")]
    MalformedHash {
        /// Affected tool.
        tool: Tool,
        /// Manifest field.
        field: &'static str,
        /// Validation reason.
        reason: &'static str,
    },
    /// A declared byte size was absent or zero.
    #[error("tool `{tool}` has invalid {field}")]
    InvalidSize {
        /// Affected tool.
        tool: Tool,
        /// Manifest field.
        field: &'static str,
    },
    /// The source URL was not HTTPS.
    #[error("tool `{tool}` uses insecure source URL `{url}`")]
    InsecureSourceUrl {
        /// Affected tool.
        tool: Tool,
        /// Rejected URL.
        url: String,
    },
    /// The source ref was blank.
    #[error("tool `{tool}` has no source ref")]
    MissingSourceRef {
        /// Affected tool.
        tool: Tool,
    },
    /// A source filename was unsafe.
    #[error("tool `{tool}` source path `{path}` is unsafe: {reason}")]
    UnsafeSourcePath {
        /// Affected tool.
        tool: Tool,
        /// Rejected path.
        path: String,
        /// Validation reason.
        reason: &'static str,
    },
    /// An executable path was unsafe.
    #[error("tool `{tool}` for `{target}` executable path `{path}` is unsafe: {reason}")]
    UnsafeExecutablePath {
        /// Affected target.
        target: SupportedTarget,
        /// Affected tool.
        tool: Tool,
        /// Rejected path.
        path: String,
        /// Validation reason.
        reason: &'static str,
    },
    /// Two executable paths collide.
    #[error("tool `{tool}` for `{target}` repeats destination `{path}`")]
    DuplicateExecutablePath {
        /// Affected target.
        target: SupportedTarget,
        /// Affected tool.
        tool: Tool,
        /// Repeated path.
        path: String,
    },
    /// Two tool records collide in the same target cache.
    #[error("target `{target}` repeats executable destination `{path}` across tools")]
    DuplicateTargetExecutablePath {
        /// Affected target.
        target: SupportedTarget,
        /// Repeated destination.
        path: String,
    },
    /// A build receipt path was unsafe.
    #[error("tool `{tool}` for `{target}` receipt path `{path}` is unsafe: {reason}")]
    UnsafeReceiptPath {
        /// Affected target.
        target: SupportedTarget,
        /// Affected tool.
        tool: Tool,
        /// Rejected path.
        path: String,
        /// Validation reason.
        reason: &'static str,
    },
    /// A build definition path was unsafe.
    #[error("tool `{tool}` build path `{path}` is unsafe: {reason}")]
    UnsafeBuildPath {
        /// Affected tool.
        tool: Tool,
        /// Rejected path.
        path: String,
        /// Validation reason.
        reason: &'static str,
    },
    /// Provenance metadata was absent or malformed.
    #[error("tool `{tool}` has invalid provenance field {field}")]
    InvalidProvenance {
        /// Affected tool.
        tool: Tool,
        /// Invalid field.
        field: &'static str,
    },
    /// Native build receipt schema is unsupported.
    #[error("unsupported native build receipt schema {found}")]
    UnsupportedBuildReceiptSchema {
        /// Receipt schema.
        found: u32,
    },
    /// Receipt target differs from the requested target.
    #[error("native build receipt target is `{found}`; expected `{expected}`")]
    BuildReceiptTargetMismatch {
        /// Requested target.
        expected: SupportedTarget,
        /// Receipt target.
        found: SupportedTarget,
    },
    /// Receipt source differs from the pinned source.
    #[error("native build receipt source is `{found}`; expected `{expected}`")]
    BuildReceiptSourceMismatch {
        /// Pinned source ref.
        expected: String,
        /// Receipt source ref.
        found: String,
    },
    /// Receipt did not preserve a non-empty build configuration.
    #[error("native build receipt has no valid build configuration")]
    InvalidBuildConfiguration,
    /// Receipt contains a tool other than `FFmpeg` or `FFprobe`.
    #[error("native build receipt unexpectedly contains `{tool}`")]
    UnexpectedBuildReceiptTool {
        /// Unexpected tool.
        tool: Tool,
    },
    /// Receipt repeats a tool.
    #[error("native build receipt repeats `{tool}`")]
    DuplicateBuildReceiptTool {
        /// Repeated tool.
        tool: Tool,
    },
    /// Receipt omits `FFmpeg` or `FFprobe`.
    #[error("native build receipt is missing `{tool}`")]
    MissingBuildReceiptTool {
        /// Missing tool.
        tool: Tool,
    },
    /// Receipt path was unsafe.
    #[error("native build receipt executable path `{path}` is unsafe: {reason}")]
    UnsafeBuildReceiptExecutablePath {
        /// Rejected path.
        path: String,
        /// Validation reason.
        reason: &'static str,
    },
    /// Receipt repeats a destination path.
    #[error("native build receipt repeats executable path `{path}`")]
    DuplicateBuildReceiptPath {
        /// Repeated path.
        path: String,
    },
    /// Receipt digest was malformed.
    #[error("native build receipt has malformed `{tool}` digest: {reason}")]
    MalformedBuildReceiptHash {
        /// Affected tool.
        tool: Tool,
        /// Validation reason.
        reason: &'static str,
    },
    /// Receipt size was zero.
    #[error("native build receipt has invalid `{tool}` size")]
    InvalidBuildReceiptSize {
        /// Affected tool.
        tool: Tool,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        ArchiveFormat, BuildReceipt, Distribution, ExecutableArtifact, ManifestError, Provenance,
        ReceiptExecutable, SIDECAR_MANIFEST_SCHEMA_VERSION, SidecarManifest, SourceArtifact,
        TargetManifest, ToolManifest, validate_sha256,
    };
    use crate::{target::SupportedTarget, tool::Tool};

    fn tool_manifest(tool: Tool) -> ToolManifest {
        ToolManifest {
            tool,
            version: tool.baseline_version().to_owned(),
            source: SourceArtifact {
                url: format!("https://example.invalid/{}", tool.name()),
                source_ref: tool.baseline_version().to_owned(),
                filename: tool.name().to_owned(),
                archive_format: ArchiveFormat::Raw,
                sha256: "a".repeat(64),
                size: 1,
            },
            executables: vec![ExecutableArtifact {
                archive_path: tool.name().to_owned(),
                sha256: Some("b".repeat(64)),
                size: Some(1),
            }],
            distribution: Distribution::UpstreamRelease,
            provenance: Provenance {
                repository: "https://example.invalid/repository".to_owned(),
                license: "test-only".to_owned(),
                build_definition: None,
                build_inputs: Vec::new(),
                metadata: BTreeMap::new(),
            },
        }
    }

    fn complete_manifest() -> SidecarManifest {
        SidecarManifest {
            schema_version: SIDECAR_MANIFEST_SCHEMA_VERSION,
            targets: SupportedTarget::ALL
                .into_iter()
                .map(|target| TargetManifest {
                    target,
                    tools: Tool::ALL.into_iter().map(tool_manifest).collect(),
                })
                .collect(),
        }
    }

    fn complete_build_receipt() -> BuildReceipt {
        BuildReceipt {
            schema_version: SIDECAR_MANIFEST_SCHEMA_VERSION,
            target: SupportedTarget::WindowsX64,
            source_ref: "n8.0.1@commit".to_owned(),
            build_configuration: vec!["--disable-shared".to_owned()],
            executables: [Tool::Ffmpeg, Tool::Ffprobe]
                .into_iter()
                .map(|tool| ReceiptExecutable {
                    tool,
                    path: format!("bin/{}.exe", tool.name()),
                    sha256: "c".repeat(64),
                    size: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn rejects_an_unknown_schema_before_typed_deserialization() {
        let result = SidecarManifest::from_json(br#"{"schema_version":99,"targets":[]}"#);
        assert!(matches!(
            result,
            Err(ManifestError::UnsupportedSchemaVersion { found: 99, .. })
        ));
    }

    #[test]
    fn rejects_duplicate_targets() {
        let mut manifest = complete_manifest();
        let duplicate = manifest.targets.first().cloned();
        assert!(duplicate.is_some());
        if let Some(duplicate) = duplicate {
            manifest.targets.push(duplicate);
        }
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::DuplicateTarget { .. })
        ));
    }

    #[test]
    fn rejects_missing_tools() {
        let mut manifest = complete_manifest();
        let target = manifest.targets.first_mut();
        assert!(target.is_some());
        if let Some(target) = target {
            target.tools.retain(|tool| tool.tool != Tool::Deno);
        }
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::MissingTool {
                tool: Tool::Deno,
                ..
            })
        ));
    }

    #[test]
    fn rejects_malformed_hashes() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256("abcd").is_err());
    }

    #[test]
    fn rejects_unsupported_triples_in_json() {
        let json = br#"{
          "schema_version": 1,
          "targets": [{"target": "i686-pc-windows-msvc", "tools": []}]
        }"#;
        assert!(matches!(
            SidecarManifest::from_json(json),
            Err(ManifestError::InvalidJson(_))
        ));
    }

    #[test]
    fn validates_a_complete_native_build_receipt() {
        let receipt = complete_build_receipt();
        assert!(
            receipt
                .validate(SupportedTarget::WindowsX64, "n8.0.1@commit")
                .is_ok()
        );
    }

    #[test]
    fn rejects_native_receipt_target_and_source_mismatches() {
        let receipt = complete_build_receipt();
        assert!(matches!(
            receipt.validate(SupportedTarget::WindowsArm64, "n8.0.1@commit"),
            Err(ManifestError::BuildReceiptTargetMismatch { .. })
        ));
        assert!(matches!(
            receipt.validate(SupportedTarget::WindowsX64, "n8.0.1@another-commit"),
            Err(ManifestError::BuildReceiptSourceMismatch { .. })
        ));
    }

    #[test]
    fn rejects_incomplete_or_unverifiable_native_receipts() {
        let mut missing_tool = complete_build_receipt();
        missing_tool
            .executables
            .retain(|executable| executable.tool != Tool::Ffprobe);
        assert!(matches!(
            missing_tool.validate(SupportedTarget::WindowsX64, "n8.0.1@commit"),
            Err(ManifestError::MissingBuildReceiptTool {
                tool: Tool::Ffprobe
            })
        ));

        let mut malformed_hash = complete_build_receipt();
        if let Some(executable) = malformed_hash.executables.first_mut() {
            executable.sha256 = "not-a-digest".to_owned();
        }
        assert!(matches!(
            malformed_hash.validate(SupportedTarget::WindowsX64, "n8.0.1@commit"),
            Err(ManifestError::MalformedBuildReceiptHash { .. })
        ));
    }
}
