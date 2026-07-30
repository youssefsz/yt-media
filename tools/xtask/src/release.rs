//! Deterministic release assembly, signed tool manifests, inspection, and evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fmt::Write as FmtWrite,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use clap::{Args, Subcommand};
use ed25519_dalek::{Signer, SigningKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use thiserror::Error;
use xz2::write::XzEncoder;
use yt_media_engine::{
    cancellation::CancellationToken,
    process::TokioProcessRunner,
    resolver::VerifiedToolSet,
    target::SupportedTarget,
    tool::Tool,
    update::{UpdateArchive, UpdateManifest, UpdateTool, VerifiedUpdateManifest},
};
use zip::write::SimpleFileOptions;

use crate::archive::extract_archive;

const SIGNING_KEY_ENV: &str = "YT_MEDIA_TOOL_MANIFEST_SIGNING_KEY_HEX";
const PUBLIC_KEY_ENV: &str = "YT_MEDIA_UPDATE_PUBLIC_KEY_HEX";
const MAX_ARTIFACT_BYTES: u64 = 1_073_741_824;
const MAX_INSTALLED_BYTES: u64 = 1_073_741_824;
const MAX_COLD_START_MS: u64 = 15_000;
const MAX_IDLE_MEMORY_BYTES: u64 = 1_073_741_824;
const MAX_ACTIVE_DOWNLOAD_MEMORY_BYTES: u64 = 2_147_483_648;
const MAX_FIXTURE_ANALYSIS_MS: u64 = 30_000;

/// Release automation arguments.
#[derive(Debug, Args)]
pub struct ReleaseArgs {
    #[command(subcommand)]
    command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    /// Build one immutable target tool archive and canonical unsigned manifest.
    Tools(ToolsArguments),
    /// Sign one canonical manifest using a protected environment secret.
    Sign(SignArguments),
    /// Verify a manifest signature and its immutable archive identity.
    Verify(VerifyArguments),
    /// Assemble one out-of-box CLI archive.
    Cli(CliArguments),
    /// Inspect a CLI archive without executing untrusted archive paths.
    InspectCli(InspectCliArguments),
    /// Generate checksums, SBOM, inventory, provenance, notes, and size evidence.
    Metadata(MetadataArguments),
}

#[derive(Debug, Args)]
struct ToolsArguments {
    #[arg(long)]
    target: SupportedTarget,
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    release_version: String,
    #[arg(long)]
    minimum_app_version: String,
    #[arg(long)]
    created_at: String,
    #[arg(long)]
    asset_base_url: String,
}

#[derive(Debug, Args)]
struct SignArguments {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct VerifyArguments {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    signature: PathBuf,
    #[arg(long)]
    archive: PathBuf,
    #[arg(long)]
    target: SupportedTarget,
    #[arg(long)]
    app_version: String,
}

#[derive(Debug, Args)]
struct CliArguments {
    #[arg(long)]
    target: SupportedTarget,
    #[arg(long)]
    binary: PathBuf,
    #[arg(long)]
    sidecars: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    version: String,
    #[arg(long)]
    notices: PathBuf,
    #[arg(long)]
    baseline_manifest: PathBuf,
}

#[derive(Debug, Args)]
struct InspectCliArguments {
    #[arg(long)]
    target: SupportedTarget,
    #[arg(long)]
    archive: PathBuf,
    #[arg(long)]
    version: String,
}

#[derive(Debug, Args)]
struct MetadataArguments {
    #[arg(long)]
    target: SupportedTarget,
    #[arg(long)]
    artifacts: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    source_sha: String,
    #[arg(long)]
    version: String,
}

/// Runs one release operation.
///
/// # Errors
///
/// Returns a typed release assembly, signing, inspection, or evidence failure.
pub fn run(arguments: ReleaseArgs) -> Result<(), ReleaseError> {
    match arguments.command {
        ReleaseCommand::Tools(arguments) => build_tools(arguments),
        ReleaseCommand::Sign(arguments) => sign_manifest(&arguments),
        ReleaseCommand::Verify(arguments) => verify_manifest(arguments),
        ReleaseCommand::Cli(arguments) => build_cli(arguments),
        ReleaseCommand::InspectCli(arguments) => inspect_cli(&arguments),
        ReleaseCommand::Metadata(arguments) => generate_metadata(&arguments),
    }
}

fn build_tools(arguments: ToolsArguments) -> Result<(), ReleaseError> {
    require_native(arguments.target)?;
    verify_staged(arguments.target, &arguments.input)?;
    Version::parse(&arguments.release_version).map_err(ReleaseError::Version)?;
    Version::parse(&arguments.minimum_app_version).map_err(ReleaseError::Version)?;
    fs::create_dir_all(&arguments.output)
        .map_err(|source| io_error("create release output", &arguments.output, source))?;
    let archive_name = format!(
        "yt-media-tools-{}-{}.zip",
        arguments.release_version,
        arguments.target.triple()
    );
    let archive_path = arguments.output.join(&archive_name);
    create_tool_zip(arguments.target, &arguments.input, &archive_path)?;
    let archive_size = fs::metadata(&archive_path)
        .map_err(|source| io_error("inspect tool archive", &archive_path, source))?
        .len();
    let archive_digest = sha256_file(&archive_path)?;
    let checksums = read_sidecar_checksums(&arguments.input.join("SHA256SUMS"))?;
    let tools = Tool::ALL
        .into_iter()
        .map(|tool| {
            let name = tool.staged_name(arguments.target);
            let path = arguments.input.join(&name);
            let size = fs::metadata(&path)
                .map_err(|source| io_error("inspect staged sidecar", &path, source))?
                .len();
            let sha256 = checksums
                .get(&name)
                .cloned()
                .ok_or(ReleaseError::MissingSidecar { tool })?;
            Ok(UpdateTool {
                tool,
                version: tool.baseline_version().to_owned(),
                archive_path: name,
                size,
                sha256,
            })
        })
        .collect::<Result<Vec<_>, ReleaseError>>()?;
    let manifest = UpdateManifest {
        schema_version: 1,
        channel: "stable".to_owned(),
        release_version: arguments.release_version,
        target: arguments.target,
        minimum_app_version: arguments.minimum_app_version,
        created_at: arguments.created_at,
        archive: UpdateArchive {
            url: format!(
                "{}/{}",
                arguments.asset_base_url.trim_end_matches('/'),
                archive_name
            ),
            filename: archive_name,
            size: archive_size,
            sha256: archive_digest,
        },
        tools,
    };
    let canonical = manifest.canonical_bytes().map_err(ReleaseError::Update)?;
    let manifest_path = arguments.output.join(format!(
        "tool-update-{}.manifest.json",
        arguments.target.triple()
    ));
    write_atomic(&manifest_path, &canonical)?;
    Ok(())
}

fn sign_manifest(arguments: &SignArguments) -> Result<(), ReleaseError> {
    let bytes = fs::read(&arguments.manifest).map_err(|source| {
        io_error(
            "read canonical update manifest",
            &arguments.manifest,
            source,
        )
    })?;
    let value: UpdateManifest = serde_json::from_slice(&bytes).map_err(ReleaseError::Json)?;
    if value.canonical_bytes().map_err(ReleaseError::Update)? != bytes {
        return Err(ReleaseError::NonCanonicalManifest);
    }
    let secret_hex = env::var(SIGNING_KEY_ENV).map_err(|_| ReleaseError::SigningKeyUnavailable)?;
    let secret = decode_fixed::<32>(&secret_hex, "signing key")?;
    let signing_key = SigningKey::from_bytes(&secret);
    if let Ok(expected_public) = env::var(PUBLIC_KEY_ENV) {
        let expected = decode_fixed::<32>(&expected_public, "public key")?;
        if signing_key.verifying_key().to_bytes() != expected {
            return Err(ReleaseError::SigningKeyMismatch);
        }
    }
    let signature = signing_key.sign(&bytes);
    let mut encoded = hex::encode(signature.to_bytes());
    encoded.push('\n');
    write_atomic(&arguments.output, encoded.as_bytes())
}

fn verify_manifest(arguments: VerifyArguments) -> Result<(), ReleaseError> {
    let manifest_bytes = fs::read(&arguments.manifest)
        .map_err(|source| io_error("read update manifest", &arguments.manifest, source))?;
    let signature = fs::read_to_string(&arguments.signature)
        .map_err(|source| io_error("read update signature", &arguments.signature, source))?;
    let public_key = env::var(PUBLIC_KEY_ENV).map_err(|_| ReleaseError::PublicKeyUnavailable)?;
    let application_version =
        Version::parse(&arguments.app_version).map_err(ReleaseError::Version)?;
    let verified = VerifiedUpdateManifest::verify(
        &manifest_bytes,
        &signature,
        &public_key,
        arguments.target,
        &application_version,
    )
    .map_err(ReleaseError::Update)?;
    let metadata = fs::metadata(&arguments.archive)
        .map_err(|source| io_error("inspect tool archive", &arguments.archive, source))?;
    if metadata.len() != verified.manifest().archive.size {
        return Err(ReleaseError::SizeMismatch {
            path: arguments.archive,
            expected: verified.manifest().archive.size,
            found: metadata.len(),
        });
    }
    let found = sha256_file(&arguments.archive)?;
    if found != verified.manifest().archive.sha256 {
        return Err(ReleaseError::DigestMismatch {
            path: arguments.archive,
        });
    }
    inspect_tool_zip(&arguments.archive, verified.manifest())
}

fn build_cli(arguments: CliArguments) -> Result<(), ReleaseError> {
    require_native(arguments.target)?;
    verify_staged(arguments.target, &arguments.sidecars)?;
    Version::parse(&arguments.version).map_err(ReleaseError::Version)?;
    let binary_name = if arguments.target.is_windows() {
        "yt-media.exe"
    } else {
        "yt-media"
    };
    if !arguments.binary.is_file() {
        return Err(ReleaseError::MissingFile {
            path: arguments.binary,
        });
    }
    for path in [&arguments.notices, &arguments.baseline_manifest] {
        if !path.is_file() {
            return Err(ReleaseError::MissingFile {
                path: (*path).clone(),
            });
        }
    }
    fs::create_dir_all(&arguments.output)
        .map_err(|source| io_error("create CLI release output", &arguments.output, source))?;
    let root_name = format!(
        "yt-media-{}-{}",
        arguments.version,
        arguments.target.triple()
    );
    let extension = if arguments.target.is_windows() {
        "zip"
    } else {
        "tar.xz"
    };
    let archive = arguments.output.join(format!("{root_name}.{extension}"));
    let temporary = tempdir().map_err(ReleaseError::Temporary)?;
    let root = temporary.path().join(&root_name);
    let sidecars = root.join("sidecars");
    fs::create_dir_all(&sidecars)
        .map_err(|source| io_error("create CLI staging", &sidecars, source))?;
    copy_file(&arguments.binary, &root.join(binary_name))?;
    copy_file(&arguments.notices, &root.join("THIRD_PARTY_NOTICES.md"))?;
    copy_file(
        &arguments.baseline_manifest,
        &root.join("sidecar-manifest.v1.json"),
    )?;
    write_atomic(
        &root.join("VERSION"),
        format!("{}\n", arguments.version).as_bytes(),
    )?;
    for tool in Tool::ALL {
        let name = tool.staged_name(arguments.target);
        copy_file(&arguments.sidecars.join(&name), &sidecars.join(name))?;
    }
    copy_file(
        &arguments.sidecars.join("SHA256SUMS"),
        &sidecars.join("SHA256SUMS"),
    )?;
    let checksums = payload_checksums(&root)?;
    write_atomic(&root.join("SHA256SUMS"), checksums.as_bytes())?;
    if arguments.target.is_windows() {
        create_directory_zip(temporary.path(), &root_name, &archive)?;
    } else {
        create_directory_tar_xz(temporary.path(), &root_name, &archive)?;
    }
    inspect_cli(&InspectCliArguments {
        target: arguments.target,
        archive,
        version: arguments.version,
    })
}

fn inspect_cli(arguments: &InspectCliArguments) -> Result<(), ReleaseError> {
    let extension = if arguments.target.is_windows() {
        yt_media_engine::manifest::ArchiveFormat::Zip
    } else {
        yt_media_engine::manifest::ArchiveFormat::TarXz
    };
    let temporary = tempdir().map_err(ReleaseError::Temporary)?;
    extract_archive(&arguments.archive, extension, temporary.path(), None)
        .map_err(ReleaseError::Archive)?;
    let root = temporary.path().join(format!(
        "yt-media-{}-{}",
        arguments.version,
        arguments.target.triple()
    ));
    let binary = root.join(if arguments.target.is_windows() {
        "yt-media.exe"
    } else {
        "yt-media"
    });
    for required in [
        binary,
        root.join("VERSION"),
        root.join("SHA256SUMS"),
        root.join("THIRD_PARTY_NOTICES.md"),
        root.join("sidecar-manifest.v1.json"),
        root.join("sidecars").join("SHA256SUMS"),
    ] {
        if !required.is_file() {
            return Err(ReleaseError::MissingFile { path: required });
        }
    }
    for tool in Tool::ALL {
        let path = root
            .join("sidecars")
            .join(tool.staged_name(arguments.target));
        if !path.is_file() {
            return Err(ReleaseError::MissingSidecar { tool });
        }
    }
    reject_cache_entries(&root)?;
    verify_payload_checksums(&root)
}

fn generate_metadata(arguments: &MetadataArguments) -> Result<(), ReleaseError> {
    Version::parse(&arguments.version).map_err(ReleaseError::Version)?;
    if arguments.source_sha.len() != 40
        || !arguments
            .source_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ReleaseError::InvalidSourceSha);
    }
    fs::create_dir_all(&arguments.output)
        .map_err(|source| io_error("create release metadata output", &arguments.output, source))?;
    let artifacts = artifact_records(&arguments.artifacts, &arguments.output)?;
    let checksum_text = artifacts.iter().fold(String::new(), |mut text, artifact| {
        let _ = writeln!(text, "{}  {}", artifact.sha256, artifact.path);
        text
    });
    write_atomic(
        &arguments.output.join("SHA256SUMS"),
        checksum_text.as_bytes(),
    )?;
    write_json(
        &arguments.output.join("artifact-inventory.json"),
        &json!({
            "schema_version": 1,
            "target": arguments.target,
            "source_sha": arguments.source_sha,
            "artifacts": artifacts,
        }),
    )?;
    write_sbom(arguments)?;
    write_provenance(arguments)?;
    write_performance(arguments)?;
    let notes = format!(
        "# YT Media {}\n\nTarget: `{}`\n\nSource: `{}`\n\nThis is validated draft output. It is not a published application release.\n",
        arguments.version,
        arguments.target.triple(),
        arguments.source_sha
    );
    write_atomic(&arguments.output.join("release-notes.md"), notes.as_bytes())
}

fn write_sbom(arguments: &MetadataArguments) -> Result<(), ReleaseError> {
    let packages = cargo_packages()?;
    let spdx_packages = packages
        .iter()
        .enumerate()
        .map(|(index, package)| {
            json!({
                "SPDXID": format!("SPDXRef-Package-{index}"),
                "name": package.name,
                "versionInfo": package.version,
                "downloadLocation": package.source.as_deref().unwrap_or("NOASSERTION"),
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": package.license.as_deref().unwrap_or("NOASSERTION"),
            })
        })
        .collect::<Vec<_>>();
    write_json(
        &arguments.output.join("sbom.spdx.json"),
        &json!({
            "spdxVersion": "SPDX-2.3",
            "dataLicense": "CC0-1.0",
            "SPDXID": "SPDXRef-DOCUMENT",
            "name": format!("yt-media-{}-{}", arguments.version, arguments.target.triple()),
            "documentNamespace": format!(
                "https://github.com/youssefsz/yt-media/sbom/{}/{}",
                arguments.source_sha,
                arguments.target.triple()
            ),
            "creationInfo": {
                "created": env::var("YT_MEDIA_BUILD_CREATED_AT")
                    .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned()),
                "creators": ["Tool: yt-media-xtask"]
            },
            "packages": spdx_packages,
        }),
    )
}

fn write_provenance(arguments: &MetadataArguments) -> Result<(), ReleaseError> {
    let subjects = artifact_records(&arguments.artifacts, &arguments.output)?
        .into_iter()
        .map(|artifact| {
            json!({
                "name": artifact.path,
                "digest": {"sha256": artifact.sha256}
            })
        })
        .collect::<Vec<_>>();
    write_json(
        &arguments.output.join("provenance.intoto.json"),
        &json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": subjects,
            "predicateType": "https://slsa.dev/provenance/v1",
            "predicate": {
                "buildDefinition": {
                    "buildType": "https://github.com/youssefsz/yt-media/.github/workflows/release-prepare.yml@v1",
                    "externalParameters": {
                        "target": arguments.target,
                        "source_sha": arguments.source_sha,
                    },
                    "resolvedDependencies": [{
                        "uri": "git+https://github.com/youssefsz/yt-media",
                        "digest": {"gitCommit": arguments.source_sha}
                    }]
                },
                "runDetails": {
                    "builder": {"id": "https://github.com/actions/runner"},
                    "metadata": {
                        "invocationId": env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "local".to_owned())
                    }
                }
            }
        }),
    )
}

fn write_performance(arguments: &MetadataArguments) -> Result<(), ReleaseError> {
    let total_size = artifact_records(&arguments.artifacts, &arguments.output)?
        .iter()
        .try_fold(0_u64, |total, artifact| total.checked_add(artifact.size))
        .ok_or(ReleaseError::SizeOverflow)?;
    let installed_bytes = required_metric("YT_MEDIA_INSTALLED_BYTES")?;
    let cold_start_ms = required_metric("YT_MEDIA_COLD_START_MS")?;
    let idle_memory_bytes = required_metric("YT_MEDIA_IDLE_MEMORY_BYTES")?;
    let active_download_memory_bytes = required_metric("YT_MEDIA_ACTIVE_MEMORY_BYTES")?;
    let fixture_analysis_ms = required_metric("YT_MEDIA_ANALYSIS_MS")?;
    for (metric, observed, maximum) in [
        ("artifact_bytes", total_size, MAX_ARTIFACT_BYTES),
        ("installed_bytes", installed_bytes, MAX_INSTALLED_BYTES),
        ("cold_start_ms", cold_start_ms, MAX_COLD_START_MS),
        (
            "idle_memory_bytes",
            idle_memory_bytes,
            MAX_IDLE_MEMORY_BYTES,
        ),
        (
            "active_download_memory_bytes",
            active_download_memory_bytes,
            MAX_ACTIVE_DOWNLOAD_MEMORY_BYTES,
        ),
        (
            "fixture_analysis_ms",
            fixture_analysis_ms,
            MAX_FIXTURE_ANALYSIS_MS,
        ),
    ] {
        require_performance_threshold(metric, observed, maximum)?;
    }
    write_json(
        &arguments.output.join("performance.json"),
        &json!({
            "schema_version": 1,
            "target": arguments.target,
            "artifact_bytes": total_size,
            "installed_bytes": installed_bytes,
            "cold_start_ms": cold_start_ms,
            "idle_memory_bytes": idle_memory_bytes,
            "active_download_memory_bytes": active_download_memory_bytes,
            "fixture_analysis_ms": fixture_analysis_ms,
            "thresholds": {
                "artifact_bytes": MAX_ARTIFACT_BYTES,
                "installed_bytes": MAX_INSTALLED_BYTES,
                "cold_start_ms": MAX_COLD_START_MS,
                "idle_memory_bytes": MAX_IDLE_MEMORY_BYTES,
                "active_download_memory_bytes": MAX_ACTIVE_DOWNLOAD_MEMORY_BYTES,
                "fixture_analysis_ms": MAX_FIXTURE_ANALYSIS_MS
            }
        }),
    )
}

fn required_metric(variable: &'static str) -> Result<u64, ReleaseError> {
    let value =
        env::var(variable).map_err(|_| ReleaseError::PerformanceMetricMissing { variable })?;
    let parsed = value
        .parse::<u64>()
        .map_err(|source| ReleaseError::PerformanceMetricInvalid { variable, source })?;
    if parsed == 0 {
        return Err(ReleaseError::PerformanceMetricZero { variable });
    }
    Ok(parsed)
}

fn require_performance_threshold(
    metric: &'static str,
    observed: u64,
    maximum: u64,
) -> Result<(), ReleaseError> {
    if observed > maximum {
        return Err(ReleaseError::PerformanceThresholdExceeded {
            metric,
            observed,
            maximum,
        });
    }
    Ok(())
}

fn verify_staged(target: SupportedTarget, root: &Path) -> Result<(), ReleaseError> {
    let runtime = tokio::runtime::Runtime::new().map_err(ReleaseError::Runtime)?;
    runtime
        .block_on(VerifiedToolSet::verify_staged(
            target,
            root,
            Arc::new(TokioProcessRunner),
            CancellationToken::new(),
        ))
        .map(|_verified| ())
        .map_err(ReleaseError::ToolSet)
}

fn require_native(target: SupportedTarget) -> Result<(), ReleaseError> {
    let current = SupportedTarget::current().map_err(ReleaseError::Target)?;
    if current == target {
        Ok(())
    } else {
        Err(ReleaseError::NonNativeTarget {
            requested: target,
            current,
        })
    }
}

fn create_tool_zip(
    target: SupportedTarget,
    input: &Path,
    output: &Path,
) -> Result<(), ReleaseError> {
    let file =
        File::create(output).map_err(|source| io_error("create tool archive", output, source))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);
    for tool in Tool::ALL {
        let name = tool.staged_name(target);
        writer
            .start_file(&name, options)
            .map_err(ReleaseError::Zip)?;
        let mut source = File::open(input.join(&name))
            .map_err(|error| io_error("open staged sidecar", &input.join(&name), error))?;
        io::copy(&mut source, &mut writer)
            .map_err(|error| io_error("write tool archive", output, error))?;
    }
    writer.finish().map_err(ReleaseError::Zip)?;
    Ok(())
}

fn inspect_tool_zip(path: &Path, manifest: &UpdateManifest) -> Result<(), ReleaseError> {
    let file = File::open(path).map_err(|source| io_error("open tool archive", path, source))?;
    let mut archive = zip::ZipArchive::new(file).map_err(ReleaseError::Zip)?;
    if archive.len() != Tool::ALL.len() {
        return Err(ReleaseError::UnexpectedArchiveLayout);
    }
    let expected = manifest
        .tools
        .iter()
        .map(|tool| (tool.archive_path.as_str(), tool))
        .collect::<BTreeMap<_, _>>();
    let mut names = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(ReleaseError::Zip)?;
        let Some(tool) = expected.get(entry.name()) else {
            return Err(ReleaseError::UnexpectedArchiveLayout);
        };
        if !entry.is_file() || !names.insert(entry.name().to_owned()) || entry.size() != tool.size {
            return Err(ReleaseError::UnexpectedArchiveLayout);
        }
        let mut digest = Sha256::new();
        io::copy(&mut entry, &mut DigestWriter(&mut digest))
            .map_err(|source| io_error("hash tool archive entry", path, source))?;
        if format!("{:x}", digest.finalize()) != tool.sha256 {
            return Err(ReleaseError::DigestMismatch {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn create_directory_zip(parent: &Path, root_name: &str, output: &Path) -> Result<(), ReleaseError> {
    let file = File::create(output).map_err(|source| io_error("create CLI ZIP", output, source))?;
    let mut writer = zip::ZipWriter::new(file);
    for path in sorted_files(&parent.join(root_name))? {
        let relative = path
            .strip_prefix(parent)
            .map_err(|_| ReleaseError::UnexpectedArchiveLayout)?;
        let name = relative.to_string_lossy().replace('\\', "/");
        let executable = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
            });
        let mode = if executable { 0o755 } else { 0o644 };
        writer
            .start_file(
                name,
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(mode),
            )
            .map_err(ReleaseError::Zip)?;
        let mut file =
            File::open(&path).map_err(|source| io_error("open CLI payload", &path, source))?;
        io::copy(&mut file, &mut writer)
            .map_err(|source| io_error("write CLI ZIP", output, source))?;
    }
    writer.finish().map_err(ReleaseError::Zip)?;
    Ok(())
}

fn create_directory_tar_xz(
    parent: &Path,
    root_name: &str,
    output: &Path,
) -> Result<(), ReleaseError> {
    let file =
        File::create(output).map_err(|source| io_error("create CLI tar.xz", output, source))?;
    let encoder = XzEncoder::new(file, 9);
    let mut builder = tar::Builder::new(encoder);
    builder.mode(tar::HeaderMode::Deterministic);
    for path in sorted_files(&parent.join(root_name))? {
        let relative = path
            .strip_prefix(parent)
            .map_err(|_| ReleaseError::UnexpectedArchiveLayout)?;
        let bytes =
            fs::read(&path).map_err(|source| io_error("read CLI payload", &path, source))?;
        let executable = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                name == "yt-media" || Tool::ALL.iter().any(|tool| tool.name() == name)
            });
        let mut header = tar::Header::new_gnu();
        header.set_size(u64::try_from(bytes.len()).map_err(|_| ReleaseError::SizeOverflow)?);
        header.set_mode(if executable { 0o755 } else { 0o644 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, relative, bytes.as_slice())
            .map_err(|source| io_error("write CLI tar.xz", output, source))?;
    }
    let encoder = builder
        .into_inner()
        .map_err(|source| io_error("finish CLI tar", output, source))?;
    encoder
        .finish()
        .map_err(|source| io_error("finish CLI xz", output, source))?;
    Ok(())
}

fn sorted_files(root: &Path) -> Result<Vec<PathBuf>, ReleaseError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|source| io_error("inspect release staging", &directory, source))?;
        for entry in entries {
            let entry =
                entry.map_err(|source| io_error("inspect release staging", &directory, source))?;
            let kind = entry.file_type().map_err(|source| {
                io_error("inspect release staging entry", &entry.path(), source)
            })?;
            if kind.is_symlink() {
                return Err(ReleaseError::Symlink { path: entry.path() });
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            } else {
                return Err(ReleaseError::SpecialFile { path: entry.path() });
            }
        }
    }
    files.sort_by(|left, right| {
        left.to_string_lossy()
            .replace('\\', "/")
            .cmp(&right.to_string_lossy().replace('\\', "/"))
    });
    Ok(files)
}

fn payload_checksums(root: &Path) -> Result<String, ReleaseError> {
    let mut lines = String::new();
    for path in sorted_files(root)? {
        if path.file_name() == Some(OsStr::new("SHA256SUMS")) && path.parent() == Some(root) {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ReleaseError::UnexpectedArchiveLayout)?
            .to_string_lossy()
            .replace('\\', "/");
        writeln!(lines, "{}  {relative}", sha256_file(&path)?)
            .map_err(|_| ReleaseError::SizeOverflow)?;
    }
    Ok(lines)
}

fn verify_payload_checksums(root: &Path) -> Result<(), ReleaseError> {
    let text = fs::read_to_string(root.join("SHA256SUMS"))
        .map_err(|source| io_error("read CLI checksums", &root.join("SHA256SUMS"), source))?;
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let Some((digest, name)) = line.split_once("  ") else {
            return Err(ReleaseError::InvalidChecksums);
        };
        if digest.len() != 64
            || name.is_empty()
            || name.contains('\\')
            || name.starts_with('/')
            || name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !names.insert(name.to_owned())
        {
            return Err(ReleaseError::InvalidChecksums);
        }
        let path = root.join(name);
        if sha256_file(&path)? != digest {
            return Err(ReleaseError::DigestMismatch { path });
        }
    }
    let expected = sorted_files(root)?
        .into_iter()
        .filter(|path| {
            !(path.file_name() == Some(OsStr::new("SHA256SUMS")) && path.parent() == Some(root))
        })
        .count();
    if names.len() != expected {
        return Err(ReleaseError::InvalidChecksums);
    }
    Ok(())
}

fn reject_cache_entries(root: &Path) -> Result<(), ReleaseError> {
    for path in sorted_files(root)? {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ReleaseError::UnexpectedArchiveLayout)?;
        let forbidden = relative.components().any(|component| {
            let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
            let temporary_extension =
                Path::new(name.as_str())
                    .extension()
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("part")
                            || extension.eq_ignore_ascii_case("tmp")
                    });
            matches!(name.as_str(), ".cache" | "cache" | "update" | "staging")
                || temporary_extension
        });
        if forbidden {
            return Err(ReleaseError::CacheArtifact { path });
        }
    }
    Ok(())
}

fn read_sidecar_checksums(path: &Path) -> Result<BTreeMap<String, String>, ReleaseError> {
    let text = fs::read_to_string(path)
        .map_err(|source| io_error("read staged checksums", path, source))?;
    let mut checksums = BTreeMap::new();
    for line in text.lines() {
        let Some((digest, name)) = line.split_once("  ") else {
            return Err(ReleaseError::InvalidChecksums);
        };
        if digest.len() != 64
            || name.contains(['/', '\\'])
            || checksums
                .insert(name.to_owned(), digest.to_owned())
                .is_some()
        {
            return Err(ReleaseError::InvalidChecksums);
        }
    }
    Ok(checksums)
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), ReleaseError> {
    let parent = destination.parent().ok_or_else(|| ReleaseError::NoParent {
        path: destination.to_path_buf(),
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error("create release payload parent", parent, error))?;
    fs::copy(source, destination)
        .map_err(|error| io_error("copy release payload", source, error))?;
    Ok(())
}

fn artifact_records(root: &Path, excluded: &Path) -> Result<Vec<ArtifactRecord>, ReleaseError> {
    let mut records = sorted_files(root)?
        .into_iter()
        .filter(|path| !path.starts_with(excluded))
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| ReleaseError::UnexpectedArchiveLayout)?
                .to_string_lossy()
                .replace('\\', "/");
            let size = fs::metadata(&path)
                .map_err(|source| io_error("inspect release artifact", &path, source))?
                .len();
            Ok(ArtifactRecord {
                path: relative,
                size,
                sha256: sha256_file(&path)?,
            })
        })
        .collect::<Result<Vec<_>, ReleaseError>>()?;
    records.sort_by(|left, right| left.path.cmp(&right.path));
    if records.is_empty() {
        return Err(ReleaseError::NoArtifacts);
    }
    Ok(records)
}

fn cargo_packages() -> Result<Vec<CargoPackage>, ReleaseError> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .map_err(ReleaseError::CargoMetadataIo)?;
    if !output.status.success() {
        return Err(ReleaseError::CargoMetadataFailed);
    }
    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).map_err(ReleaseError::Json)?;
    Ok(metadata.packages)
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), ReleaseError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(ReleaseError::Json)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ReleaseError> {
    let parent = path.parent().ok_or_else(|| ReleaseError::NoParent {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| io_error("create release file parent", parent, source))?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)
        .map_err(|source| io_error("write temporary release file", &temporary, source))?;
    if path.exists() {
        fs::remove_file(path).map_err(|source| io_error("replace release file", path, source))?;
    }
    fs::rename(&temporary, path).map_err(|source| io_error("activate release file", path, source))
}

fn sha256_file(path: &Path) -> Result<String, ReleaseError> {
    let mut file =
        File::open(path).map_err(|source| io_error("open file for hashing", path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|source| io_error("hash file", path, source))?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn decode_fixed<const N: usize>(value: &str, field: &'static str) -> Result<[u8; N], ReleaseError> {
    let bytes = hex::decode(value).map_err(ReleaseError::Hex)?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| ReleaseError::KeyLength {
            field,
            expected: N,
            found: bytes.len(),
        })
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> ReleaseError {
    ReleaseError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Serialize)]
struct ArtifactRecord {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    license: Option<String>,
    source: Option<String>,
}

/// Release automation failure.
#[derive(Debug, Error)]
pub enum ReleaseError {
    /// Current runner target differs from the requested native artifact.
    #[error("release target {requested} must build natively; current target is {current}")]
    NonNativeTarget {
        /// Requested target.
        requested: SupportedTarget,
        /// Current target.
        current: SupportedTarget,
    },
    /// Current target identification failed.
    #[error(transparent)]
    Target(#[from] yt_media_engine::target::TargetError),
    /// Semantic version was invalid.
    #[error("release version is invalid")]
    Version(#[source] semver::Error),
    /// Runtime creation failed.
    #[error("could not create release verification runtime")]
    Runtime(#[source] io::Error),
    /// Sidecar verification failed.
    #[error("release sidecars failed verification")]
    ToolSet(#[source] yt_media_engine::resolver::ToolSetVerificationError),
    /// Signed update contract failed.
    #[error("update manifest validation failed")]
    Update(#[source] yt_media_engine::update::UpdateError),
    /// JSON operation failed.
    #[error("release JSON operation failed")]
    Json(#[source] serde_json::Error),
    /// ZIP operation failed.
    #[error("release ZIP operation failed")]
    Zip(#[source] zip::result::ZipError),
    /// Secure extraction failed.
    #[error("release archive inspection failed")]
    Archive(#[source] crate::archive::ArchiveError),
    /// Temporary directory creation failed.
    #[error("could not create temporary release directory")]
    Temporary(#[source] io::Error),
    /// Secret signing seed was unavailable.
    #[error("{SIGNING_KEY_ENV} is required only inside the protected signing environment")]
    SigningKeyUnavailable,
    /// Public verification key was unavailable.
    #[error("{PUBLIC_KEY_ENV} is required for release verification")]
    PublicKeyUnavailable,
    /// Signing key did not correspond to the configured embedded public key.
    #[error("tool manifest signing key does not match the configured application public key")]
    SigningKeyMismatch,
    /// Hex key material was malformed.
    #[error("release key material is not valid hexadecimal")]
    Hex(#[source] hex::FromHexError),
    /// Key material had the wrong byte length.
    #[error("{field} is {found} bytes; expected {expected}")]
    KeyLength {
        /// Affected key.
        field: &'static str,
        /// Required bytes.
        expected: usize,
        /// Observed bytes.
        found: usize,
    },
    /// Sign input was not the canonical form it claimed to be.
    #[error("refusing to sign a non-canonical update manifest")]
    NonCanonicalManifest,
    /// Required payload file was absent.
    #[error("required release file `{}` is missing", path.display())]
    MissingFile {
        /// Missing path.
        path: PathBuf,
    },
    /// Required sidecar was absent.
    #[error("required release sidecar `{tool}` is missing")]
    MissingSidecar {
        /// Missing tool.
        tool: Tool,
    },
    /// A path unexpectedly had no parent.
    #[error("release path `{}` has no parent", path.display())]
    NoParent {
        /// Affected path.
        path: PathBuf,
    },
    /// Artifact size differed from the signed value.
    #[error("release artifact `{}` is {found} bytes; expected {expected}", path.display())]
    SizeMismatch {
        /// Affected path.
        path: PathBuf,
        /// Signed size.
        expected: u64,
        /// Observed size.
        found: u64,
    },
    /// Artifact digest differed from its signed or checksummed value.
    #[error("release artifact `{}` has an unexpected digest", path.display())]
    DigestMismatch {
        /// Affected path.
        path: PathBuf,
    },
    /// Archive did not contain exactly its declared payload.
    #[error("release archive layout is unexpected")]
    UnexpectedArchiveLayout,
    /// Checksum inventory was malformed or incomplete.
    #[error("release checksum inventory is malformed or incomplete")]
    InvalidChecksums,
    /// Cache, update, partial, or staging content leaked into an archive.
    #[error("release archive contains forbidden cache artifact `{}`", path.display())]
    CacheArtifact {
        /// Rejected path.
        path: PathBuf,
    },
    /// Release staging contained a symlink.
    #[error("release staging contains symlink `{}`", path.display())]
    Symlink {
        /// Rejected path.
        path: PathBuf,
    },
    /// Release staging contained a special file.
    #[error("release staging contains special file `{}`", path.display())]
    SpecialFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// Metadata input had no artifacts.
    #[error("release metadata input contains no artifacts")]
    NoArtifacts,
    /// Source commit was not a full hexadecimal Git commit ID.
    #[error("release source SHA must be a full 40-character commit ID")]
    InvalidSourceSha,
    /// A size calculation overflowed.
    #[error("release size calculation overflowed")]
    SizeOverflow,
    /// A required performance sample was not provided by the platform smoke test.
    #[error("required release performance metric {variable} is missing")]
    PerformanceMetricMissing {
        /// Required environment variable.
        variable: &'static str,
    },
    /// A performance sample was not an unsigned integer.
    #[error("release performance metric {variable} is invalid")]
    PerformanceMetricInvalid {
        /// Required environment variable.
        variable: &'static str,
        /// Integer parsing failure.
        #[source]
        source: std::num::ParseIntError,
    },
    /// A performance sample did not observe a usable non-zero value.
    #[error("release performance metric {variable} did not observe a non-zero value")]
    PerformanceMetricZero {
        /// Required environment variable.
        variable: &'static str,
    },
    /// A reviewed release performance limit was exceeded.
    #[error("release performance metric {metric} is {observed}; reviewed maximum is {maximum}")]
    PerformanceThresholdExceeded {
        /// Stable metric name.
        metric: &'static str,
        /// Observed value.
        observed: u64,
        /// Inclusive reviewed maximum.
        maximum: u64,
    },
    /// Cargo metadata could not start.
    #[error("could not run cargo metadata")]
    CargoMetadataIo(#[source] io::Error),
    /// Cargo metadata exited unsuccessfully.
    #[error("cargo metadata failed")]
    CargoMetadataFailed,
    /// A filesystem operation failed.
    #[error("{action} failed for `{}`", path.display())]
    Io {
        /// Stable action.
        action: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Filesystem source.
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::{ReleaseError, reject_cache_entries, require_performance_threshold};
    use std::{error::Error, fs};

    #[test]
    fn cache_scan_ignores_temporary_components_above_archive_root() -> Result<(), Box<dyn Error>> {
        let parent = tempfile::Builder::new()
            .prefix(".tmp-release-parent")
            .tempdir()?;
        let root = parent.path().join("yt-media-0.1.0-test-target");
        fs::create_dir_all(root.join("sidecars"))?;
        fs::write(root.join("SHA256SUMS"), b"digest  file\n")?;
        fs::write(root.join("sidecars").join("yt-dlp"), b"fixture")?;

        reject_cache_entries(&root)?;
        Ok(())
    }

    #[test]
    fn cache_scan_rejects_only_forbidden_archive_relative_entries() -> Result<(), Box<dyn Error>> {
        for relative in [
            "cache/download",
            ".cache/download",
            "staging/tool",
            "update/tool",
            "sidecars/tool.part",
            "sidecars/tool.tmp",
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join(relative);
            fs::create_dir_all(path.parent().ok_or("fixture has no parent")?)?;
            fs::write(&path, b"fixture")?;

            assert!(matches!(
                reject_cache_entries(directory.path()),
                Err(ReleaseError::CacheArtifact { .. })
            ));
        }
        Ok(())
    }

    #[test]
    fn performance_threshold_is_inclusive_and_rejects_an_overrun() {
        assert!(require_performance_threshold("fixture", 10, 10).is_ok());
        assert!(matches!(
            require_performance_threshold("fixture", 11, 10),
            Err(ReleaseError::PerformanceThresholdExceeded {
                metric: "fixture",
                observed: 11,
                maximum: 10,
            })
        ));
    }
}
