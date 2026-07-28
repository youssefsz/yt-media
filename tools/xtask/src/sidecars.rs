//! Verified sidecar fetch, build, probe, and stage commands.

use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use clap::{Args, Parser, Subcommand};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::{Builder as TempBuilder, NamedTempFile};
use thiserror::Error;
use yt_media_engine::{
    cancellation::CancellationToken,
    manifest::{
        BuildReceipt, Distribution, ReceiptExecutable, SidecarManifest, SourceArtifact,
        TargetManifest, ToolManifest,
    },
    process::{OutputLimit, ProcessError, ProcessRunner, ProcessSpec, TokioProcessRunner},
    resolver::{ToolSetVerificationError, VerifiedToolSet},
    target::{SupportedTarget, TargetError},
    tool::Tool,
};

use crate::archive::{ArchiveError, extract_archive};

const DEFAULT_MANIFEST: &str = "sidecars/manifest.v1.json";
const DEFAULT_CACHE: &str = ".sidecar-cache";
const DEFAULT_STAGE_ROOT: &str = "target/sidecars";
const SOURCE_MARKER: &str = ".verified-source";
const BUILD_RECEIPT: &str = "ffmpeg-build-receipt.v1.json";
const BUILD_OUTPUT_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const BUILD_OUTPUT_LIMIT_LINES: usize = 200_000;
const MAX_DOWNLOAD_ATTEMPTS: u8 = 3;

/// Runs the `xtask` command-line interface.
///
/// # Errors
///
/// Returns a typed sidecar operation failure.
pub fn run_cli() -> Result<(), SidecarError> {
    run(XtaskCli::parse())
}

fn run(cli: XtaskCli) -> Result<(), SidecarError> {
    match cli.command {
        RootCommand::Sidecars(sidecars) => {
            let repository = repository_root()?;
            let manifest_path = sidecars
                .manifest
                .unwrap_or_else(|| repository.join(DEFAULT_MANIFEST));
            let cache = sidecars
                .cache
                .unwrap_or_else(|| repository.join(DEFAULT_CACHE));
            let context = SidecarContext::load(repository, manifest_path, cache)?;
            match sidecars.command {
                SidecarCommand::Fetch(arguments) => context.fetch(arguments.target),
                SidecarCommand::Build(arguments) => context.build(arguments.target),
                SidecarCommand::RecordBuild(arguments) => {
                    context.record_build(arguments.target, &arguments.input)
                }
                SidecarCommand::Verify(arguments) => {
                    context.verify(arguments.target).map(|_verified| ())
                }
                SidecarCommand::Probe(arguments) => {
                    context.probe(arguments.target, arguments.ejs_url.as_deref())
                }
                SidecarCommand::Stage(arguments) => {
                    let stage_root = arguments
                        .output
                        .unwrap_or_else(|| context.repository.join(DEFAULT_STAGE_ROOT));
                    context.stage(arguments.target, &stage_root)
                }
            }
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Repository maintenance automation")]
struct XtaskCli {
    #[command(subcommand)]
    command: RootCommand,
}

#[derive(Debug, Subcommand)]
enum RootCommand {
    /// Fetch, build, verify, probe, or stage sidecars.
    Sidecars(SidecarsArgs),
}

#[derive(Debug, Args)]
struct SidecarsArgs {
    /// Versioned sidecar manifest path.
    #[arg(long, global = true)]
    manifest: Option<PathBuf>,
    /// Ignored sidecar cache root.
    #[arg(long, global = true)]
    cache: Option<PathBuf>,
    #[command(subcommand)]
    command: SidecarCommand,
}

#[derive(Debug, Subcommand)]
enum SidecarCommand {
    /// Download and securely extract pinned upstream inputs.
    Fetch(TargetArguments),
    /// Build `FFmpeg` and `FFprobe` from pinned native sources.
    Build(TargetArguments),
    /// Record already-built `FFmpeg` and `FFprobe` files into the verified cache.
    RecordBuild(RecordBuildArguments),
    /// Rehash cached executables and run exact version probes.
    Verify(TargetArguments),
    /// Run version, encoder, muxer, and yt-dlp/Deno pairing probes.
    Probe(ProbeArguments),
    /// Copy only verified executables using Tauri target-triple names.
    Stage(StageArguments),
}

#[derive(Clone, Copy, Debug, Args)]
struct TargetArguments {
    /// Supported Rust target triple.
    #[arg(long)]
    target: SupportedTarget,
}

#[derive(Debug, Args)]
struct RecordBuildArguments {
    /// Supported Rust target triple.
    #[arg(long)]
    target: SupportedTarget,
    /// Directory containing native `ffmpeg` and `ffprobe` build outputs.
    #[arg(long)]
    input: PathBuf,
}

#[derive(Debug, Args)]
struct ProbeArguments {
    /// Supported Rust target triple.
    #[arg(long)]
    target: SupportedTarget,
    /// Optional public `YouTube` URL for an explicit network-dependent EJS smoke test.
    #[arg(long)]
    ejs_url: Option<String>,
}

#[derive(Debug, Args)]
struct StageArguments {
    /// Supported Rust target triple.
    #[arg(long)]
    target: SupportedTarget,
    /// Staging root; the target triple is appended.
    #[arg(long)]
    output: Option<PathBuf>,
}

struct SidecarContext {
    repository: PathBuf,
    cache: PathBuf,
    manifest: SidecarManifest,
    client: Client,
}

impl SidecarContext {
    fn load(
        repository: PathBuf,
        manifest_path: PathBuf,
        cache: PathBuf,
    ) -> Result<Self, SidecarError> {
        let bytes = fs::read(&manifest_path).map_err(|source| SidecarError::Io {
            action: "read sidecar manifest",
            path: manifest_path,
            source,
        })?;
        let manifest = SidecarManifest::from_json(&bytes)?;
        let client = Client::builder()
            .user_agent("yt-media-xtask/0.1")
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_mins(30))
            .build()?;
        Ok(Self {
            repository,
            cache,
            manifest,
            client,
        })
    }

    fn target_manifest(&self, target: SupportedTarget) -> Result<&TargetManifest, SidecarError> {
        self.manifest
            .target(target)
            .ok_or(SidecarError::TargetMissing { target })
    }

    fn fetch(&self, target: SupportedTarget) -> Result<(), SidecarError> {
        let manifest = self.target_manifest(target)?;
        fs::create_dir_all(self.download_root()).map_err(|source| SidecarError::Io {
            action: "create download cache",
            path: self.download_root(),
            source,
        })?;
        fs::create_dir_all(self.target_root(target)).map_err(|source| SidecarError::Io {
            action: "create target cache",
            path: self.target_root(target),
            source,
        })?;
        fs::create_dir_all(self.source_root()).map_err(|source| SidecarError::Io {
            action: "create source cache",
            path: self.source_root(),
            source,
        })?;

        let mut fetched_sources = BTreeSet::new();
        for tool_manifest in &manifest.tools {
            let archive = self.ensure_download(&tool_manifest.source)?;
            if matches!(tool_manifest.distribution, Distribution::NativeBuild { .. })
                && fetched_sources.insert(tool_manifest.source.sha256.clone())
            {
                self.ensure_source_tree(&tool_manifest.source, &archive)?;
            }
            for build_input in &tool_manifest.provenance.build_inputs {
                let archive = self.ensure_download(&build_input.source)?;
                if fetched_sources.insert(build_input.source.sha256.clone()) {
                    self.ensure_source_tree(&build_input.source, &archive)?;
                }
            }
            if matches!(tool_manifest.distribution, Distribution::UpstreamRelease) {
                self.install_release_tool(target, tool_manifest, &archive)?;
            }
        }
        Ok(())
    }

    fn ensure_download(&self, source: &SourceArtifact) -> Result<PathBuf, SidecarError> {
        let destination = self
            .download_root()
            .join(format!("{}-{}", source.sha256, source.filename));
        if destination.is_file() && verify_file(&destination, source.size, &source.sha256).is_ok() {
            return Ok(destination);
        }

        let response = self.download(source)?;
        if response
            .content_length()
            .is_some_and(|length| length != source.size)
        {
            return Err(SidecarError::DownloadSize {
                url: source.url.clone(),
                expected: source.size,
                found: response.content_length().unwrap_or_default(),
            });
        }
        let mut temporary =
            NamedTempFile::new_in(self.download_root()).map_err(|source| SidecarError::Io {
                action: "create temporary download",
                path: self.download_root(),
                source,
            })?;
        let copied = io::copy(
            &mut response.take(source.size.saturating_add(1)),
            temporary.as_file_mut(),
        )
        .map_err(|source| SidecarError::Io {
            action: "download source artifact",
            path: temporary.path().to_path_buf(),
            source,
        })?;
        temporary
            .as_file_mut()
            .sync_all()
            .map_err(|source| SidecarError::Io {
                action: "flush source artifact",
                path: temporary.path().to_path_buf(),
                source,
            })?;
        if copied != source.size {
            return Err(SidecarError::DownloadSize {
                url: source.url.clone(),
                expected: source.size,
                found: copied,
            });
        }
        verify_file(temporary.path(), source.size, &source.sha256)?;
        replace_named_temp(temporary, &destination)?;
        Ok(destination)
    }

    fn download(&self, source: &SourceArtifact) -> Result<Response, SidecarError> {
        for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
            let result = self
                .client
                .get(&source.url)
                .send()
                .and_then(Response::error_for_status);
            match result {
                Ok(response) => return Ok(response),
                Err(error)
                    if attempt < MAX_DOWNLOAD_ATTEMPTS && is_retryable_download_error(&error) =>
                {
                    thread::sleep(Duration::from_secs(u64::from(attempt)));
                }
                Err(source_error) => {
                    return Err(SidecarError::DownloadNetwork {
                        url: source.url.clone(),
                        attempts: attempt,
                        source: source_error,
                    });
                }
            }
        }
        Err(SidecarError::DownloadAttemptsExhausted {
            url: source.url.clone(),
        })
    }

    fn ensure_source_tree(
        &self,
        source: &SourceArtifact,
        archive: &Path,
    ) -> Result<PathBuf, SidecarError> {
        let destination = self.source_directory(source);
        let marker = destination.join(SOURCE_MARKER);
        if fs::read_to_string(&marker).is_ok_and(|value| value.trim() == source.sha256) {
            return Ok(destination);
        }

        let temporary = TempBuilder::new()
            .prefix("source-")
            .tempdir_in(self.source_root())
            .map_err(|source| SidecarError::Io {
                action: "create source extraction directory",
                path: self.source_root(),
                source,
            })?;
        extract_archive(
            archive,
            source.archive_format,
            temporary.path(),
            Some(&source.filename),
        )?;
        fs::write(temporary.path().join(SOURCE_MARKER), &source.sha256).map_err(|source| {
            SidecarError::Io {
                action: "write source verification marker",
                path: temporary.path().join(SOURCE_MARKER),
                source,
            }
        })?;
        if destination.exists() {
            fs::remove_dir_all(&destination).map_err(|source| SidecarError::Io {
                action: "remove stale source cache",
                path: destination.clone(),
                source,
            })?;
        }
        let temporary_path = temporary.keep();
        fs::rename(&temporary_path, &destination).map_err(|source| SidecarError::Io {
            action: "activate source cache",
            path: destination.clone(),
            source,
        })?;
        Ok(destination)
    }

    fn install_release_tool(
        &self,
        target: SupportedTarget,
        tool_manifest: &ToolManifest,
        archive: &Path,
    ) -> Result<(), SidecarError> {
        for executable in &tool_manifest.executables {
            let expected_hash =
                executable
                    .sha256
                    .as_deref()
                    .ok_or(SidecarError::MissingReleaseDigest {
                        tool: tool_manifest.tool,
                    })?;
            let expected_size = executable.size.ok_or(SidecarError::MissingReleaseDigest {
                tool: tool_manifest.tool,
            })?;
            let destination = self.target_root(target).join(&executable.archive_path);
            if destination.is_file()
                && verify_file(&destination, expected_size, expected_hash).is_ok()
            {
                continue;
            }

            let temporary = TempBuilder::new()
                .prefix("release-")
                .tempdir_in(&self.cache)
                .map_err(|source| SidecarError::Io {
                    action: "create release extraction directory",
                    path: self.cache.clone(),
                    source,
                })?;
            extract_archive(
                archive,
                tool_manifest.source.archive_format,
                temporary.path(),
                Some(&tool_manifest.source.filename),
            )?;
            let extracted = temporary.path().join(&executable.archive_path);
            verify_file(&extracted, expected_size, expected_hash)?;
            copy_verified_file(&extracted, &destination)?;
            verify_file(&destination, expected_size, expected_hash)?;
        }
        Ok(())
    }

    fn verify(&self, target: SupportedTarget) -> Result<VerifiedToolSet, SidecarError> {
        let manifest = self.target_manifest(target)?;
        let runtime = tokio::runtime::Runtime::new().map_err(SidecarError::Runtime)?;
        runtime
            .block_on(VerifiedToolSet::verify(
                manifest,
                self.target_root(target),
                Arc::new(TokioProcessRunner),
                CancellationToken::new(),
            ))
            .map_err(|error| SidecarError::Verification(Box::new(error)))
    }

    fn probe(&self, target: SupportedTarget, ejs_url: Option<&str>) -> Result<(), SidecarError> {
        let verified = self.verify(target)?;
        let runtime = tokio::runtime::Runtime::new().map_err(SidecarError::Runtime)?;
        runtime.block_on(probe_verified_capabilities(&verified, ejs_url))
    }

    fn stage(&self, target: SupportedTarget, stage_root: &Path) -> Result<(), SidecarError> {
        let verified = self.verify(target)?;
        fs::create_dir_all(stage_root).map_err(|source| SidecarError::Io {
            action: "create sidecar staging root",
            path: stage_root.to_path_buf(),
            source,
        })?;
        let destination = stage_root.join(target.triple());
        let temporary = TempBuilder::new()
            .prefix("stage-")
            .tempdir_in(stage_root)
            .map_err(|source| SidecarError::Io {
                action: "create temporary sidecar staging directory",
                path: stage_root.to_path_buf(),
                source,
            })?;
        let mut checksums = Vec::new();
        for tool in Tool::ALL {
            let source = verified
                .path(tool)
                .ok_or(SidecarError::VerifiedToolMissing { tool })?;
            let staged_name = tool.staged_name(target);
            let staged_path = temporary.path().join(&staged_name);
            copy_verified_file(source.as_path(), &staged_path)?;
            let (_size, sha256) = file_identity(&staged_path)?;
            checksums.push(format!("{sha256}  {staged_name}"));
        }
        checksums.sort();
        let mut checksum_document = checksums.join("\n").into_bytes();
        checksum_document.push(b'\n');
        write_bytes_atomic(&temporary.path().join("SHA256SUMS"), &checksum_document)?;
        if destination.exists() {
            fs::remove_dir_all(&destination).map_err(|source| SidecarError::Io {
                action: "remove previous sidecar staging directory",
                path: destination.clone(),
                source,
            })?;
        }
        let temporary_path = temporary.keep();
        fs::rename(&temporary_path, &destination).map_err(|source| SidecarError::Io {
            action: "activate sidecar staging directory",
            path: destination,
            source,
        })
    }

    fn build(&self, target: SupportedTarget) -> Result<(), SidecarError> {
        require_current_target(target)?;
        self.fetch(target)?;
        let manifest = self.target_manifest(target)?;
        let ffmpeg_manifest =
            manifest
                .tool(Tool::Ffmpeg)
                .ok_or(SidecarError::ManifestToolMissing {
                    target,
                    tool: Tool::Ffmpeg,
                })?;
        let x264 = ffmpeg_manifest
            .provenance
            .build_inputs
            .iter()
            .find(|input| input.name == "x264")
            .ok_or(SidecarError::BuildInputMissing { name: "x264" })?;
        let lame = ffmpeg_manifest
            .provenance
            .build_inputs
            .iter()
            .find(|input| input.name == "lame")
            .ok_or(SidecarError::BuildInputMissing { name: "lame" })?;

        let build_root = self.cache.join("build").join(target.triple());
        if build_root.exists() {
            fs::remove_dir_all(&build_root).map_err(|source| SidecarError::Io {
                action: "clean native build directory",
                path: build_root.clone(),
                source,
            })?;
        }
        fs::create_dir_all(&build_root).map_err(|source| SidecarError::Io {
            action: "create native build directory",
            path: build_root.clone(),
            source,
        })?;
        let prefix = build_root.join("prefix");
        fs::create_dir_all(&prefix).map_err(|source| SidecarError::Io {
            action: "create native dependency prefix",
            path: prefix.clone(),
            source,
        })?;

        let x264_source =
            self.prepare_build_source(&x264.source, &build_root.join("x264-source"))?;
        let lame_source =
            self.prepare_build_source(&lame.source, &build_root.join("lame-source"))?;
        let ffmpeg_source =
            self.prepare_build_source(&ffmpeg_manifest.source, &build_root.join("ffmpeg-source"))?;
        let jobs = std::thread::available_parallelism()
            .map_or(2, std::num::NonZeroUsize::get)
            .to_string();
        let reproducible_path = build_tool_path(&build_root)?;
        let reproducible_cflags = format!(
            "-O2 -ffile-prefix-map={reproducible_path}=. \
             -fdebug-prefix-map={reproducible_path}=."
        );
        let source_date_epoch = ffmpeg_manifest
            .provenance
            .metadata
            .get("source_date_epoch")
            .ok_or(SidecarError::BuildMetadataMissing {
                field: "source_date_epoch",
            })?;
        let install = build_root.join("ffmpeg-install");
        build_codec_dependencies(
            target,
            &x264_source,
            &lame_source,
            &prefix,
            &jobs,
            &reproducible_cflags,
            source_date_epoch,
        )?;
        let configuration = build_ffmpeg(
            &ffmpeg_source,
            &prefix,
            &install,
            &jobs,
            &reproducible_cflags,
            source_date_epoch,
        )?;
        self.record_build_with_configuration(target, &install.join("bin"), configuration)
    }

    fn prepare_build_source(
        &self,
        source: &SourceArtifact,
        destination: &Path,
    ) -> Result<PathBuf, SidecarError> {
        if destination.exists() {
            fs::remove_dir_all(destination).map_err(|error| SidecarError::Io {
                action: "clean build source directory",
                path: destination.to_path_buf(),
                source: error,
            })?;
        }
        fs::create_dir_all(destination).map_err(|error| SidecarError::Io {
            action: "create build source directory",
            path: destination.to_path_buf(),
            source: error,
        })?;
        let archive = self.ensure_download(source)?;
        extract_archive(
            &archive,
            source.archive_format,
            destination,
            Some(&source.filename),
        )?;
        single_child_directory(destination)
    }

    fn record_build(&self, target: SupportedTarget, input: &Path) -> Result<(), SidecarError> {
        let prefix = PathBuf::from("<external-native-build>");
        let install = PathBuf::from("<artifact>");
        self.record_build_with_configuration(
            target,
            input,
            ffmpeg_build_configuration(&prefix, &install)?,
        )
    }

    fn record_build_with_configuration(
        &self,
        target: SupportedTarget,
        input: &Path,
        build_configuration: Vec<String>,
    ) -> Result<(), SidecarError> {
        require_current_target(target)?;
        let manifest = self.target_manifest(target)?;
        let ffmpeg_manifest =
            manifest
                .tool(Tool::Ffmpeg)
                .ok_or(SidecarError::ManifestToolMissing {
                    target,
                    tool: Tool::Ffmpeg,
                })?;
        let target_root = self.target_root(target);
        fs::create_dir_all(target_root.join("bin")).map_err(|source| SidecarError::Io {
            action: "create native target cache",
            path: target_root.join("bin"),
            source,
        })?;
        let mut receipt_executables = Vec::new();
        for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
            let filename = tool.executable_name(target);
            let source = find_build_executable(input, &filename)?;
            let relative = format!("bin/{filename}");
            let destination = target_root.join(&relative);
            copy_verified_file(&source, &destination)?;
            let (size, sha256) = file_identity(&destination)?;
            receipt_executables.push(ReceiptExecutable {
                tool,
                path: relative,
                sha256,
                size,
            });
        }
        let receipt = BuildReceipt {
            schema_version: 1,
            target,
            source_ref: ffmpeg_manifest.source.source_ref.clone(),
            build_configuration,
            executables: receipt_executables,
        };
        receipt.validate(target, &ffmpeg_manifest.source.source_ref)?;
        write_json_atomic(&target_root.join(BUILD_RECEIPT), &receipt)
    }

    fn download_root(&self) -> PathBuf {
        self.cache.join("downloads")
    }

    fn source_root(&self) -> PathBuf {
        self.cache.join("sources")
    }

    fn source_directory(&self, source: &SourceArtifact) -> PathBuf {
        self.source_root().join(&source.sha256)
    }

    fn target_root(&self, target: SupportedTarget) -> PathBuf {
        self.cache.join("targets").join(target.triple())
    }
}

fn repository_root() -> Result<PathBuf, SidecarError> {
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_directory
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| SidecarError::RepositoryRoot {
            path: manifest_directory.to_path_buf(),
        })
}

fn require_current_target(target: SupportedTarget) -> Result<(), SidecarError> {
    let current = SupportedTarget::current()?;
    if current == target {
        Ok(())
    } else {
        Err(SidecarError::TargetNotNative {
            requested: target,
            current,
        })
    }
}

fn verify_file(path: &Path, expected_size: u64, expected_hash: &str) -> Result<(), SidecarError> {
    let (size, hash) = file_identity(path)?;
    if size != expected_size {
        return Err(SidecarError::FileSize {
            path: path.to_path_buf(),
            expected: expected_size,
            found: size,
        });
    }
    if hash != expected_hash {
        return Err(SidecarError::Checksum {
            path: path.to_path_buf(),
            expected: expected_hash.to_owned(),
            found: hash,
        });
    }
    Ok(())
}

fn file_identity(path: &Path) -> Result<(u64, String), SidecarError> {
    let mut file = File::open(path).map_err(|source| SidecarError::Io {
        action: "open file for hashing",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| SidecarError::Io {
        action: "inspect file for hashing",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(SidecarError::NotFile {
            path: path.to_path_buf(),
        });
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer).map_err(|source| SidecarError::Io {
            action: "hash file",
            path: path.to_path_buf(),
            source,
        })?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok((metadata.len(), format!("{:x}", digest.finalize())))
}

fn copy_verified_file(source: &Path, destination: &Path) -> Result<(), SidecarError> {
    let parent = destination.parent().ok_or_else(|| SidecarError::NoParent {
        path: destination.to_path_buf(),
    })?;
    fs::create_dir_all(parent).map_err(|source| SidecarError::Io {
        action: "create executable destination",
        path: parent.to_path_buf(),
        source,
    })?;
    let mut source_file = File::open(source).map_err(|error| SidecarError::Io {
        action: "open verified source",
        path: source.to_path_buf(),
        source: error,
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| SidecarError::Io {
        action: "create temporary executable",
        path: parent.to_path_buf(),
        source: error,
    })?;
    io::copy(&mut source_file, temporary.as_file_mut()).map_err(|error| SidecarError::Io {
        action: "copy verified executable",
        path: destination.to_path_buf(),
        source: error,
    })?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| SidecarError::Io {
            action: "flush verified executable",
            path: temporary.path().to_path_buf(),
            source: error,
        })?;
    #[cfg(unix)]
    set_executable(temporary.path())?;
    replace_named_temp(temporary, destination)
}

fn replace_named_temp(temporary: NamedTempFile, destination: &Path) -> Result<(), SidecarError> {
    if destination.exists() {
        fs::remove_file(destination).map_err(|source| SidecarError::Io {
            action: "remove stale cached file",
            path: destination.to_path_buf(),
            source,
        })?;
    }
    temporary
        .persist(destination)
        .map_err(|error| SidecarError::Io {
            action: "activate verified file",
            path: destination.to_path_buf(),
            source: error.error,
        })?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), SidecarError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(|source| SidecarError::Io {
        action: "inspect executable permissions",
        path: path.to_path_buf(),
        source,
    })?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|source| SidecarError::Io {
        action: "set executable permissions",
        path: path.to_path_buf(),
        source,
    })
}

async fn run_checked(
    runner: &dyn ProcessRunner,
    spec: ProcessSpec,
) -> Result<yt_media_engine::process::ProcessOutput, SidecarError> {
    let output_limit =
        OutputLimit::new(4 * 1024 * 1024, 100_000).map_err(SidecarError::ProcessSpec)?;
    let output = runner
        .run(spec.output_limit(output_limit), CancellationToken::new())
        .await?;
    if output.status.success {
        Ok(output)
    } else {
        Err(SidecarError::ProbeNonZero {
            code: output.status.code,
            stdout: output.capture.stdout.bytes,
            stderr: output.capture.stderr.bytes,
        })
    }
}

async fn probe_verified_capabilities(
    verified: &VerifiedToolSet,
    ejs_url: Option<&str>,
) -> Result<(), SidecarError> {
    let ffmpeg = verified
        .path(Tool::Ffmpeg)
        .ok_or(SidecarError::VerifiedToolMissing { tool: Tool::Ffmpeg })?;
    let deno = verified
        .path(Tool::Deno)
        .ok_or(SidecarError::VerifiedToolMissing { tool: Tool::Deno })?;
    let ytdlp = verified
        .path(Tool::YtDlp)
        .ok_or(SidecarError::VerifiedToolMissing { tool: Tool::YtDlp })?;
    let runner = TokioProcessRunner;

    probe_ffmpeg_capabilities(&runner, ffmpeg.as_path()).await?;
    probe_ejs_pairing(
        &runner,
        ffmpeg.as_path(),
        deno.as_path(),
        ytdlp.as_path(),
        ejs_url,
    )
    .await
}

async fn probe_ffmpeg_capabilities(
    runner: &TokioProcessRunner,
    ffmpeg: &Path,
) -> Result<(), SidecarError> {
    let encoders = run_checked(
        runner,
        ProcessSpec::new(ffmpeg)
            .arguments(["-hide_banner", "-encoders"])
            .timeout(Duration::from_secs(30)),
    )
    .await?;
    let encoder_text = combined_text(&encoders)?;
    for required in ["libx264", " aac ", "libmp3lame"] {
        if !encoder_text.contains(required) {
            return Err(SidecarError::MissingCapability {
                tool: Tool::Ffmpeg,
                capability: required.to_owned(),
            });
        }
    }

    let muxers = run_checked(
        runner,
        ProcessSpec::new(ffmpeg)
            .arguments(["-hide_banner", "-muxers"])
            .timeout(Duration::from_secs(30)),
    )
    .await?;
    let muxer_text = combined_text(&muxers)?;
    if !muxer_text
        .lines()
        .any(|line| line.contains(" mp4 ") || line.trim_end().ends_with(" mp4"))
    {
        return Err(SidecarError::MissingCapability {
            tool: Tool::Ffmpeg,
            capability: "MP4 muxer".to_owned(),
        });
    }
    Ok(())
}

async fn probe_ejs_pairing(
    runner: &TokioProcessRunner,
    ffmpeg: &Path,
    deno: &Path,
    ytdlp: &Path,
    ejs_url: Option<&str>,
) -> Result<(), SidecarError> {
    let runtime_argument = format!("deno:{}", deno.display());
    let deno_execution = run_checked(
        runner,
        ProcessSpec::new(deno)
            .arguments([
                OsString::from("eval"),
                OsString::from("--no-config"),
                OsString::from("--no-remote"),
                OsString::from("console.log(6*7)"),
            ])
            .timeout(Duration::from_secs(30)),
    )
    .await?;
    if combined_text(&deno_execution)?.trim() != "42" {
        return Err(SidecarError::MissingCapability {
            tool: Tool::Deno,
            capability: "restricted JavaScript execution".to_owned(),
        });
    }

    let _pairing = run_checked(
        runner,
        ProcessSpec::new(ytdlp)
            .arguments([
                OsString::from("--ignore-config"),
                OsString::from("--no-js-runtimes"),
                OsString::from("--js-runtimes"),
                OsString::from(&runtime_argument),
                OsString::from("--list-extractors"),
            ])
            .timeout(Duration::from_mins(1)),
    )
    .await?;

    if let Some(url) = ejs_url {
        let ffmpeg_directory = ffmpeg.parent().ok_or_else(|| SidecarError::NoParent {
            path: ffmpeg.to_path_buf(),
        })?;
        let ejs_smoke = run_checked(
            runner,
            ProcessSpec::new(ytdlp)
                .arguments([
                    OsString::from("--ignore-config"),
                    OsString::from("--verbose"),
                    OsString::from("--simulate"),
                    OsString::from("--no-playlist"),
                    OsString::from("--js-runtimes"),
                    OsString::from(runtime_argument),
                    OsString::from("--ffmpeg-location"),
                    ffmpeg_directory.as_os_str().to_owned(),
                    OsString::from("--dump-single-json"),
                    OsString::from(url),
                ])
                .timeout(Duration::from_mins(2)),
        )
        .await?;
        reject_ejs_warnings(&combined_text(&ejs_smoke)?)?;
    }
    Ok(())
}

fn combined_text(output: &yt_media_engine::process::ProcessOutput) -> Result<String, SidecarError> {
    let mut bytes = output.capture.stdout.bytes.clone();
    bytes.extend_from_slice(&output.capture.stderr.bytes);
    String::from_utf8(bytes).map_err(SidecarError::ProbeUtf8)
}

fn reject_ejs_warnings(output: &str) -> Result<(), SidecarError> {
    const FAILURE_MARKERS: [&str; 3] = [
        "no supported javascript runtime could be found",
        "signature solving failed",
        "n challenge solving failed",
    ];
    let lower = output.to_ascii_lowercase();
    if let Some(marker) = FAILURE_MARKERS
        .into_iter()
        .find(|marker| lower.contains(marker))
    {
        return Err(SidecarError::MissingCapability {
            tool: Tool::YtDlp,
            capability: format!("EJS smoke completed without `{marker}`"),
        });
    }
    Ok(())
}

fn is_retryable_download_error(error: &reqwest::Error) -> bool {
    error.is_connect()
        || error.is_timeout()
        || error.status().is_some_and(is_retryable_download_status)
}

fn is_retryable_download_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn build_codec_dependencies(
    target: SupportedTarget,
    x264_source: &Path,
    lame_source: &Path,
    prefix: &Path,
    jobs: &str,
    cflags: &str,
    source_date_epoch: &str,
) -> Result<(), SidecarError> {
    let prefix = build_tool_path(prefix)?;
    let x264_configuration = x264_build_configuration(target, &prefix);
    let x264_cflags = x264_compiler_flags(target, cflags);
    run_native_command(
        x264_source,
        "bash",
        x264_configuration,
        &[
            ("CFLAGS", &x264_cflags),
            ("SOURCE_DATE_EPOCH", source_date_epoch),
        ],
    )?;
    run_native_command(
        x264_source,
        "make",
        [OsString::from(format!("-j{jobs}"))],
        &[("SOURCE_DATE_EPOCH", source_date_epoch)],
    )?;
    run_native_command(
        x264_source,
        "make",
        [OsString::from("install")],
        &[("SOURCE_DATE_EPOCH", source_date_epoch)],
    )?;
    run_native_command(
        lame_source,
        "bash",
        [
            OsString::from("./configure"),
            OsString::from(format!("--prefix={prefix}")),
            OsString::from("--enable-static"),
            OsString::from("--disable-shared"),
            OsString::from("--disable-frontend"),
            OsString::from("--with-pic"),
        ],
        &[("CFLAGS", cflags), ("SOURCE_DATE_EPOCH", source_date_epoch)],
    )?;
    run_native_command(
        lame_source,
        "make",
        [OsString::from(format!("-j{jobs}"))],
        &[("SOURCE_DATE_EPOCH", source_date_epoch)],
    )?;
    run_native_command(
        lame_source,
        "make",
        [OsString::from("install")],
        &[("SOURCE_DATE_EPOCH", source_date_epoch)],
    )
}

fn x264_build_configuration(target: SupportedTarget, prefix: &str) -> Vec<OsString> {
    let mut configuration = vec![
        OsString::from("./configure"),
        OsString::from(format!("--prefix={prefix}")),
        OsString::from("--enable-static"),
        OsString::from("--disable-cli"),
        OsString::from("--disable-opencl"),
    ];
    if target == SupportedTarget::WindowsArm64 {
        // x264's GNU-style AArch64 assembly probe does not support CLANGARM64's COFF assembler.
        // The portable C implementation still provides the required H.264 encoder.
        configuration.push(OsString::from("--disable-asm"));
    }
    configuration
}

fn x264_compiler_flags(target: SupportedTarget, common_flags: &str) -> String {
    if target == SupportedTarget::WindowsArm64 {
        // CLANGARM64 exposes __SSE__ for compatibility even though x264's x86-only v4si type is
        // unavailable on AArch64. Keep the workaround isolated to x264's portable ARM64 build.
        format!("{common_flags} -U__SSE__")
    } else {
        common_flags.to_owned()
    }
}

fn build_ffmpeg(
    source: &Path,
    prefix: &Path,
    install: &Path,
    jobs: &str,
    cflags: &str,
    source_date_epoch: &str,
) -> Result<Vec<String>, SidecarError> {
    let configuration = ffmpeg_build_configuration(prefix, install)?;
    let mut configure_arguments = vec![OsString::from("./configure")];
    configure_arguments.extend(configuration.iter().cloned().map(OsString::from));
    let pkg_config_path = build_tool_path(&prefix.join("lib").join("pkgconfig"))?;
    run_native_command(
        source,
        "bash",
        configure_arguments,
        &[
            ("CFLAGS", cflags),
            ("PKG_CONFIG_PATH", &pkg_config_path),
            ("SOURCE_DATE_EPOCH", source_date_epoch),
            ("ZERO_AR_DATE", "1"),
        ],
    )?;
    run_native_command(
        source,
        "make",
        [OsString::from(format!("-j{jobs}"))],
        &[
            ("SOURCE_DATE_EPOCH", source_date_epoch),
            ("ZERO_AR_DATE", "1"),
        ],
    )?;
    run_native_command(
        source,
        "make",
        [OsString::from("install")],
        &[
            ("SOURCE_DATE_EPOCH", source_date_epoch),
            ("ZERO_AR_DATE", "1"),
        ],
    )?;
    Ok(configuration)
}

fn ffmpeg_build_configuration(prefix: &Path, install: &Path) -> Result<Vec<String>, SidecarError> {
    let prefix = build_tool_path(prefix)?;
    let install = build_tool_path(install)?;
    Ok(vec![
        format!("--prefix={install}"),
        "--pkg-config-flags=--static".to_owned(),
        format!("--extra-cflags=-I{prefix}/include"),
        format!("--extra-ldflags=-L{prefix}/lib"),
        "--enable-gpl".to_owned(),
        "--enable-libx264".to_owned(),
        "--enable-libmp3lame".to_owned(),
        "--enable-static".to_owned(),
        "--disable-shared".to_owned(),
        "--disable-avdevice".to_owned(),
        "--disable-network".to_owned(),
        "--disable-doc".to_owned(),
        "--disable-debug".to_owned(),
        "--disable-ffplay".to_owned(),
        "--disable-programs".to_owned(),
        "--enable-ffmpeg".to_owned(),
        "--enable-ffprobe".to_owned(),
        "--disable-autodetect".to_owned(),
    ])
}

fn build_tool_path(path: &Path) -> Result<String, SidecarError> {
    let text = path
        .to_str()
        .ok_or_else(|| SidecarError::BuildPathEncoding {
            path: path.to_path_buf(),
        })?;
    if cfg!(windows) {
        msys_path(text).ok_or_else(|| SidecarError::BuildPathEncoding {
            path: path.to_path_buf(),
        })
    } else {
        Ok(text.to_owned())
    }
}

fn msys_path(value: &str) -> Option<String> {
    let portable = value.replace('\\', "/");
    if portable.starts_with("//") {
        return None;
    }
    if portable.starts_with('/') || !portable.contains(':') {
        return Some(portable);
    }
    let bytes = portable.as_bytes();
    if bytes.len() < 3
        || bytes.get(1) != Some(&b':')
        || !bytes.first().is_some_and(u8::is_ascii_alphabetic)
    {
        return None;
    }
    let drive = char::from(bytes[0]).to_ascii_lowercase();
    Some(format!(
        "/{drive}/{}",
        portable[2..].trim_start_matches('/')
    ))
}

fn run_native_command<I>(
    directory: &Path,
    program: &str,
    arguments: I,
    environment: &[(&str, &str)],
) -> Result<(), SidecarError>
where
    I: IntoIterator<Item = OsString>,
{
    let executable = find_program(program)?;
    let mut spec = ProcessSpec::new(executable)
        .arguments(arguments)
        .current_directory(directory)
        .timeout(Duration::from_hours(1));
    for (name, value) in environment {
        spec = spec.environment(*name, *value);
    }
    let limit = OutputLimit::new(BUILD_OUTPUT_LIMIT_BYTES, BUILD_OUTPUT_LIMIT_LINES)
        .map_err(SidecarError::ProcessSpec)?;
    let runtime = tokio::runtime::Runtime::new().map_err(SidecarError::Runtime)?;
    let output = runtime
        .block_on(TokioProcessRunner.run(spec.output_limit(limit), CancellationToken::new()))?;
    io::stdout()
        .write_all(&output.capture.stdout.bytes)
        .map_err(|source| SidecarError::Io {
            action: "write build stdout",
            path: directory.to_path_buf(),
            source,
        })?;
    io::stderr()
        .write_all(&output.capture.stderr.bytes)
        .map_err(|source| SidecarError::Io {
            action: "write build stderr",
            path: directory.to_path_buf(),
            source,
        })?;
    if output.status.success {
        Ok(())
    } else {
        Err(SidecarError::BuildNonZero {
            program: program.to_owned(),
            code: output.status.code,
        })
    }
}

fn find_program(name: &str) -> Result<PathBuf, SidecarError> {
    let path = env::var_os("PATH").ok_or_else(|| SidecarError::BuildProgramMissing {
        program: name.to_owned(),
    })?;
    let candidates = if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            name.to_owned(),
        ]
    } else {
        vec![name.to_owned()]
    };
    for directory in env::split_paths(&path) {
        for candidate in &candidates {
            let path = directory.join(candidate);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(SidecarError::BuildProgramMissing {
        program: name.to_owned(),
    })
}

fn single_child_directory(path: &Path) -> Result<PathBuf, SidecarError> {
    let directories = fs::read_dir(path)
        .map_err(|source| SidecarError::Io {
            action: "inspect extracted source",
            path: path.to_path_buf(),
            source,
        })?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(std::fs::FileType::is_dir)
                .map(|_| entry.path())
        })
        .collect::<Vec<_>>();
    if directories.len() == 1 {
        directories
            .into_iter()
            .next()
            .ok_or_else(|| SidecarError::SourceLayout {
                path: path.to_path_buf(),
            })
    } else {
        Err(SidecarError::SourceLayout {
            path: path.to_path_buf(),
        })
    }
}

fn find_build_executable(input: &Path, filename: &str) -> Result<PathBuf, SidecarError> {
    for candidate in [input.join(filename), input.join("bin").join(filename)] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(SidecarError::BuildExecutableMissing {
        path: input.to_path_buf(),
        filename: filename.to_owned(),
    })
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), SidecarError> {
    let parent = path.parent().ok_or_else(|| SidecarError::NoParent {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent).map_err(|source| SidecarError::Io {
        action: "create JSON destination",
        path: parent.to_path_buf(),
        source,
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| SidecarError::Io {
        action: "create temporary JSON",
        path: parent.to_path_buf(),
        source,
    })?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    temporary
        .as_file_mut()
        .write_all(b"\n")
        .map_err(|source| SidecarError::Io {
            action: "finish JSON",
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| SidecarError::Io {
            action: "flush JSON",
            path: temporary.path().to_path_buf(),
            source,
        })?;
    replace_named_temp(temporary, path)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), SidecarError> {
    let parent = path.parent().ok_or_else(|| SidecarError::NoParent {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent).map_err(|source| SidecarError::Io {
        action: "create file destination",
        path: parent.to_path_buf(),
        source,
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| SidecarError::Io {
        action: "create temporary file",
        path: parent.to_path_buf(),
        source,
    })?;
    temporary
        .as_file_mut()
        .write_all(bytes)
        .map_err(|source| SidecarError::Io {
            action: "write temporary file",
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| SidecarError::Io {
            action: "flush temporary file",
            path: temporary.path().to_path_buf(),
            source,
        })?;
    replace_named_temp(temporary, path)
}

/// Sidecar automation failure.
#[derive(Debug, Error)]
pub enum SidecarError {
    /// Repository layout was not recognizable.
    #[error("could not determine repository root from `{}`", path.display())]
    RepositoryRoot {
        /// xtask manifest directory.
        path: PathBuf,
    },
    /// Sidecar manifest was invalid.
    #[error(transparent)]
    Manifest(#[from] yt_media_engine::manifest::ManifestError),
    /// Requested target was absent.
    #[error("sidecar manifest has no `{target}` target")]
    TargetMissing {
        /// Missing target.
        target: SupportedTarget,
    },
    /// Manifest target omitted one tool.
    #[error("sidecar manifest target `{target}` has no `{tool}` record")]
    ManifestToolMissing {
        /// Target.
        target: SupportedTarget,
        /// Missing tool.
        tool: Tool,
    },
    /// A build input was absent.
    #[error("FFmpeg build input `{name}` is missing")]
    BuildInputMissing {
        /// Missing input name.
        name: &'static str,
    },
    /// Required reproducibility metadata was absent.
    #[error("FFmpeg build metadata `{field}` is missing")]
    BuildMetadataMissing {
        /// Missing metadata field.
        field: &'static str,
    },
    /// Network operation failed.
    #[error("sidecar network operation failed")]
    Network(#[from] reqwest::Error),
    /// An artifact download failed after bounded retry handling.
    #[error("download `{url}` failed after {attempts} attempt(s)")]
    DownloadNetwork {
        /// Artifact URL.
        url: String,
        /// Number of attempts performed.
        attempts: u8,
        /// Final HTTP failure.
        #[source]
        source: reqwest::Error,
    },
    /// The bounded download loop ended without a response or final error.
    #[error("download `{url}` exhausted its retry budget")]
    DownloadAttemptsExhausted {
        /// Artifact URL.
        url: String,
    },
    /// Downloaded size differed from the manifest.
    #[error("download `{url}` produced {found} bytes; expected {expected}")]
    DownloadSize {
        /// Source URL.
        url: String,
        /// Expected bytes.
        expected: u64,
        /// Observed bytes.
        found: u64,
    },
    /// Filesystem operation failed.
    #[error("could not {action} at `{}`", path.display())]
    Io {
        /// Operation description.
        action: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Expected regular file was not a file.
    #[error("`{}` is not a regular file", path.display())]
    NotFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// File size mismatch.
    #[error("`{}` has {found} bytes; expected {expected}", path.display())]
    FileSize {
        /// Affected path.
        path: PathBuf,
        /// Expected bytes.
        expected: u64,
        /// Observed bytes.
        found: u64,
    },
    /// File checksum mismatch.
    #[error("checksum mismatch for `{}`: found {found}, expected {expected}", path.display())]
    Checksum {
        /// Affected path.
        path: PathBuf,
        /// Expected SHA-256.
        expected: String,
        /// Observed SHA-256.
        found: String,
    },
    /// Secure extraction failed.
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// Release record omitted its digest or size.
    #[error("upstream release `{tool}` omits its digest or size")]
    MissingReleaseDigest {
        /// Affected tool.
        tool: Tool,
    },
    /// Tokio runtime construction failed.
    #[error("could not create Tokio runtime")]
    Runtime(#[source] io::Error),
    /// Complete target verification failed.
    #[error(transparent)]
    Verification(Box<ToolSetVerificationError>),
    /// Process specification failed.
    #[error("invalid process specification")]
    ProcessSpec(#[source] yt_media_engine::process::ProcessSpecError),
    /// Process execution failed.
    #[error(transparent)]
    Process(Box<ProcessError>),
    /// A probe returned a non-zero status.
    #[error("sidecar probe returned non-zero status {code:?}")]
    ProbeNonZero {
        /// Exit code.
        code: Option<i32>,
        /// Bounded stdout.
        stdout: Vec<u8>,
        /// Bounded stderr.
        stderr: Vec<u8>,
    },
    /// Probe output was not UTF-8.
    #[error("sidecar capability probe output was not valid UTF-8")]
    ProbeUtf8(#[source] std::string::FromUtf8Error),
    /// A required smoke-test capability was absent.
    #[error("`{tool}` is missing required capability `{capability}`")]
    MissingCapability {
        /// Affected tool.
        tool: Tool,
        /// Missing capability.
        capability: String,
    },
    /// Verified set unexpectedly omitted a tool.
    #[error("verified tool set omitted `{tool}`")]
    VerifiedToolMissing {
        /// Missing tool.
        tool: Tool,
    },
    /// A path unexpectedly had no parent.
    #[error("path `{}` has no parent", path.display())]
    NoParent {
        /// Affected path.
        path: PathBuf,
    },
    /// Current target detection failed.
    #[error(transparent)]
    Target(#[from] TargetError),
    /// Native build was requested for another target.
    #[error("cannot build `{requested}` sidecars on `{current}`")]
    TargetNotNative {
        /// Requested target.
        requested: SupportedTarget,
        /// Current target.
        current: SupportedTarget,
    },
    /// A native build tool was absent from PATH.
    #[error("native build program `{program}` is not available on PATH")]
    BuildProgramMissing {
        /// Missing program.
        program: String,
    },
    /// Native build command failed.
    #[error("native build program `{program}` returned status {code:?}")]
    BuildNonZero {
        /// Failed program.
        program: String,
        /// Exit code.
        code: Option<i32>,
    },
    /// A native path could not be represented for POSIX build tools.
    #[error("native build path `{}` cannot be represented for the build toolchain", path.display())]
    BuildPathEncoding {
        /// Unrepresentable path.
        path: PathBuf,
    },
    /// Extracted source did not contain exactly one root directory.
    #[error("source archive at `{}` did not contain exactly one root directory", path.display())]
    SourceLayout {
        /// Extracted source path.
        path: PathBuf,
    },
    /// Native build output omitted an executable.
    #[error("native build directory `{}` has no `{filename}`", path.display())]
    BuildExecutableMissing {
        /// Build output path.
        path: PathBuf,
        /// Expected filename.
        filename: String,
    },
    /// JSON serialization failed.
    #[error("could not serialize sidecar metadata")]
    Json(#[from] serde_json::Error),
}

impl From<ToolSetVerificationError> for SidecarError {
    fn from(error: ToolSetVerificationError) -> Self {
        Self::Verification(Box::new(error))
    }
}

impl From<ProcessError> for SidecarError {
    fn from(error: ProcessError) -> Self {
        Self::Process(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use reqwest::StatusCode;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;
    use yt_media_engine::target::SupportedTarget;

    use super::{
        SidecarError, is_retryable_download_status, msys_path, reject_ejs_warnings, verify_file,
        x264_build_configuration, x264_compiler_flags,
    };

    #[test]
    fn rejects_checksum_mismatch() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let path = directory.path().join("artifact");
        let mut file = fs::File::create(&path);
        assert!(file.is_ok());
        if let Ok(file) = file.as_mut() {
            assert!(file.write_all(b"actual").is_ok());
        }
        let expected = format!("{:x}", Sha256::digest(b"expected"));
        let result = verify_file(&path, 6, &expected);
        assert!(matches!(result, Err(SidecarError::Checksum { .. })));
    }

    #[test]
    fn accepts_exact_size_and_checksum() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let path = directory.path().join("artifact");
        assert!(fs::write(&path, b"actual").is_ok());
        let expected = format!("{:x}", Sha256::digest(b"actual"));
        assert!(verify_file(&path, 6, &expected).is_ok());
    }

    #[test]
    fn accepts_a_clean_ejs_smoke_log() {
        assert!(reject_ejs_warnings("[debug] deno runtime initialized").is_ok());
    }

    #[test]
    fn rejects_standard_ejs_failure_warnings() {
        for warning in [
            "No supported JavaScript runtime could be found",
            "Signature solving failed: formats may be missing",
            "n challenge solving failed: formats may be missing",
        ] {
            assert!(matches!(
                reject_ejs_warnings(warning),
                Err(SidecarError::MissingCapability { .. })
            ));
        }
    }

    #[test]
    fn converts_windows_paths_for_msys_build_tools() {
        assert_eq!(
            msys_path(r"D:\work tree\sidecars"),
            Some("/d/work tree/sidecars".to_owned())
        );
        assert_eq!(
            msys_path("<external-native-build>"),
            Some("<external-native-build>".to_owned())
        );
        assert_eq!(msys_path(r"\\server\share"), None);
    }

    #[test]
    fn retries_only_transient_http_statuses() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
        ] {
            assert!(is_retryable_download_status(status));
        }
        for status in [StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND] {
            assert!(!is_retryable_download_status(status));
        }
    }

    #[test]
    fn disables_x264_assembly_only_for_windows_arm64() {
        let windows_arm = x264_build_configuration(SupportedTarget::WindowsArm64, "/prefix");
        let windows_x64 = x264_build_configuration(SupportedTarget::WindowsX64, "/prefix");
        assert!(
            windows_arm
                .iter()
                .any(|argument| argument == "--disable-asm")
        );
        assert!(
            !windows_x64
                .iter()
                .any(|argument| argument == "--disable-asm")
        );
    }

    #[test]
    fn undefines_x86_sse_macro_only_for_windows_arm64_x264() {
        let windows_arm = x264_compiler_flags(SupportedTarget::WindowsArm64, "-O2");
        let windows_x64 = x264_compiler_flags(SupportedTarget::WindowsX64, "-O2");
        let linux_arm = x264_compiler_flags(SupportedTarget::LinuxArm64, "-O2");

        assert_eq!(windows_arm, "-O2 -U__SSE__");
        assert_eq!(windows_x64, "-O2");
        assert_eq!(linux_arm, "-O2");
    }
}
