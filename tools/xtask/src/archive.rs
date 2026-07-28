//! Secure extraction for pinned sidecar and source archives.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use tar::EntryType;
use thiserror::Error;
use xz2::read::XzDecoder;
use yt_media_engine::manifest::ArchiveFormat;

const MAX_EXTRACTED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Extracts one verified archive into an existing empty directory.
///
/// `raw_filename` is required only for [`ArchiveFormat::Raw`]. Every archive path is normalized
/// with portable separator rules before filesystem access. Links, special files, duplicate
/// destinations, parent traversal, absolute paths, and platform prefixes are rejected.
///
/// # Errors
///
/// Returns a typed I/O, container, path, link, duplicate, or size failure.
pub fn extract_archive(
    archive_path: &Path,
    archive_format: ArchiveFormat,
    destination: &Path,
    raw_filename: Option<&str>,
) -> Result<(), ArchiveError> {
    ensure_empty_directory(destination)?;
    match archive_format {
        ArchiveFormat::Raw => {
            let filename = raw_filename.ok_or(ArchiveError::MissingRawFilename)?;
            let relative = normalize_archive_path(filename)?;
            let output = destination.join(relative);
            create_parent(&output)?;
            fs::copy(archive_path, output).map_err(ArchiveError::Io)?;
            Ok(())
        }
        ArchiveFormat::Zip => extract_zip(archive_path, destination),
        ArchiveFormat::TarXz => {
            let file = File::open(archive_path).map_err(ArchiveError::Io)?;
            extract_tar(XzDecoder::new(file), destination)
        }
        ArchiveFormat::TarGz => {
            let file = File::open(archive_path).map_err(ArchiveError::Io)?;
            extract_tar(GzDecoder::new(file), destination)
        }
    }
}

fn ensure_empty_directory(destination: &Path) -> Result<(), ArchiveError> {
    fs::create_dir_all(destination).map_err(ArchiveError::Io)?;
    let mut entries = fs::read_dir(destination).map_err(ArchiveError::Io)?;
    if entries
        .next()
        .transpose()
        .map_err(ArchiveError::Io)?
        .is_some()
    {
        return Err(ArchiveError::DestinationNotEmpty {
            path: destination.to_path_buf(),
        });
    }
    Ok(())
}

fn extract_zip(archive_path: &Path, destination: &Path) -> Result<(), ArchiveError> {
    let file = File::open(archive_path).map_err(ArchiveError::Io)?;
    let mut archive = zip::ZipArchive::new(file).map_err(ArchiveError::Zip)?;
    let mut destinations = DestinationSet::default();
    let mut extracted_bytes = 0_u64;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(ArchiveError::Zip)?;
        let relative = normalize_archive_path(entry.name())?;
        let kind = if entry.is_dir() {
            DestinationKind::Directory
        } else {
            reject_zip_link(&entry)?;
            DestinationKind::File
        };
        destinations.insert(&relative, kind)?;
        if kind == DestinationKind::File {
            extracted_bytes = extracted_bytes
                .checked_add(entry.size())
                .ok_or(ArchiveError::ExpandedSizeLimit)?;
            if extracted_bytes > MAX_EXTRACTED_BYTES {
                return Err(ArchiveError::ExpandedSizeLimit);
            }
        }
        let output = destination.join(&relative);
        match kind {
            DestinationKind::Directory => {
                fs::create_dir_all(output).map_err(ArchiveError::Io)?;
            }
            DestinationKind::File => {
                create_parent(&output)?;
                let mut file = File::create(output).map_err(ArchiveError::Io)?;
                io::copy(&mut entry, &mut file).map_err(ArchiveError::Io)?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, archive_mode: u32) -> Result<(), ArchiveError> {
    use std::os::unix::fs::PermissionsExt;

    // Preserve only ordinary permission bits. Verified source archives never gain set-id or
    // sticky semantics merely because their metadata requested them.
    let permissions = fs::Permissions::from_mode(archive_mode & 0o777);
    fs::set_permissions(path, permissions).map_err(ArchiveError::Io)
}

fn reject_zip_link(entry: &zip::read::ZipFile<'_, File>) -> Result<(), ArchiveError> {
    if let Some(mode) = entry.unix_mode() {
        let file_type = mode & 0o170_000;
        if file_type == 0o120_000 {
            return Err(ArchiveError::Link {
                path: entry.name().to_owned(),
            });
        }
        if file_type != 0 && file_type != 0o100_000 && file_type != 0o040_000 {
            return Err(ArchiveError::SpecialFile {
                path: entry.name().to_owned(),
            });
        }
    }
    Ok(())
}

fn extract_tar<R: Read>(reader: R, destination: &Path) -> Result<(), ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(ArchiveError::Io)?;
    let mut destinations = DestinationSet::default();
    let mut extracted_bytes = 0_u64;

    for entry_result in entries {
        let mut entry = entry_result.map_err(ArchiveError::Io)?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_global_extensions()
            || entry_type.is_pax_local_extensions()
            || entry_type.is_gnu_longname()
            || entry_type.is_gnu_longlink()
        {
            continue;
        }
        let entry_path = entry.path().map_err(ArchiveError::Io)?;
        let path_text = entry_path.to_str().ok_or(ArchiveError::NonUnicodePath)?;
        let relative = normalize_archive_path(path_text)?;
        let kind = tar_entry_kind(entry_type, path_text)?;
        destinations.insert(&relative, kind)?;
        if kind == DestinationKind::File {
            let size = entry.header().size().map_err(ArchiveError::Io)?;
            extracted_bytes = extracted_bytes
                .checked_add(size)
                .ok_or(ArchiveError::ExpandedSizeLimit)?;
            if extracted_bytes > MAX_EXTRACTED_BYTES {
                return Err(ArchiveError::ExpandedSizeLimit);
            }
        }
        let output = destination.join(&relative);
        match kind {
            DestinationKind::Directory => {
                fs::create_dir_all(output).map_err(ArchiveError::Io)?;
            }
            DestinationKind::File => {
                #[cfg(unix)]
                let mode = entry.header().mode().map_err(ArchiveError::Io)?;
                create_parent(&output)?;
                let mut file = File::create(&output).map_err(ArchiveError::Io)?;
                io::copy(&mut entry, &mut file).map_err(ArchiveError::Io)?;
                drop(file);
                #[cfg(unix)]
                set_unix_mode(&output, mode)?;
            }
        }
    }
    Ok(())
}

fn tar_entry_kind(entry_type: EntryType, path: &str) -> Result<DestinationKind, ArchiveError> {
    if entry_type.is_dir() {
        Ok(DestinationKind::Directory)
    } else if entry_type.is_file() {
        Ok(DestinationKind::File)
    } else if entry_type.is_symlink() || entry_type.is_hard_link() {
        Err(ArchiveError::Link {
            path: path.to_owned(),
        })
    } else {
        Err(ArchiveError::SpecialFile {
            path: path.to_owned(),
        })
    }
}

fn create_parent(path: &Path) -> Result<(), ArchiveError> {
    let parent = path.parent().ok_or_else(|| ArchiveError::NoParent {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent).map_err(ArchiveError::Io)
}

fn normalize_archive_path(value: &str) -> Result<PathBuf, ArchiveError> {
    if value.is_empty() {
        return Err(ArchiveError::UnsafePath {
            path: value.to_owned(),
            reason: "path is empty",
        });
    }
    if value.contains('\0') {
        return Err(ArchiveError::UnsafePath {
            path: value.to_owned(),
            reason: "path contains a NUL byte",
        });
    }
    let portable = value.replace('\\', "/");
    if portable.starts_with('/') || portable.starts_with("//") {
        return Err(ArchiveError::UnsafePath {
            path: value.to_owned(),
            reason: "absolute paths are forbidden",
        });
    }
    let without_trailing_separator = portable.trim_end_matches('/');
    if without_trailing_separator.is_empty() {
        return Err(ArchiveError::UnsafePath {
            path: value.to_owned(),
            reason: "root paths are forbidden",
        });
    }

    let mut normalized = PathBuf::new();
    for (index, component) in without_trailing_separator.split('/').enumerate() {
        if component.is_empty() || component == "." || component == ".." {
            return Err(ArchiveError::UnsafePath {
                path: value.to_owned(),
                reason: "empty, current, and parent components are forbidden",
            });
        }
        if index == 0 && component.contains(':') {
            return Err(ArchiveError::UnsafePath {
                path: value.to_owned(),
                reason: "platform path prefixes are forbidden",
            });
        }
        normalized.push(component);
    }
    Ok(normalized)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DestinationKind {
    File,
    Directory,
}

#[derive(Default)]
struct DestinationSet {
    entries: BTreeMap<String, DestinationKind>,
}

impl DestinationSet {
    fn insert(&mut self, path: &Path, kind: DestinationKind) -> Result<(), ArchiveError> {
        let key = portable_key(path)?;
        if self.entries.contains_key(&key) {
            return Err(ArchiveError::DuplicateDestination {
                path: path.to_path_buf(),
            });
        }
        let components = key.split('/').collect::<Vec<_>>();
        for index in 1..components.len() {
            let ancestor = components[..index].join("/");
            if self.entries.get(&ancestor) == Some(&DestinationKind::File) {
                return Err(ArchiveError::DestinationConflict {
                    path: path.to_path_buf(),
                });
            }
        }
        if kind == DestinationKind::File {
            let prefix = format!("{key}/");
            if self
                .entries
                .keys()
                .any(|existing| existing.starts_with(&prefix))
            {
                return Err(ArchiveError::DestinationConflict {
                    path: path.to_path_buf(),
                });
            }
        }
        self.entries.insert(key, kind);
        Ok(())
    }
}

fn portable_key(path: &Path) -> Result<String, ArchiveError> {
    let text = path.to_str().ok_or(ArchiveError::NonUnicodePath)?;
    Ok(text.replace('\\', "/").to_ascii_lowercase())
}

/// Secure extraction failure.
#[derive(Debug, Error)]
pub enum ArchiveError {
    /// Filesystem or decompression I/O failed.
    #[error("archive I/O failed")]
    Io(#[source] io::Error),
    /// ZIP container parsing failed.
    #[error("invalid ZIP archive")]
    Zip(#[source] zip::result::ZipError),
    /// Raw artifacts require a destination filename.
    #[error("raw artifact extraction requires a filename")]
    MissingRawFilename,
    /// Destination must be empty to avoid ambiguous replacement behavior.
    #[error("archive destination `{}` is not empty", path.display())]
    DestinationNotEmpty {
        /// Rejected destination.
        path: PathBuf,
    },
    /// A path was unsafe.
    #[error("archive path `{path}` is unsafe: {reason}")]
    UnsafePath {
        /// Rejected archive path.
        path: String,
        /// Security reason.
        reason: &'static str,
    },
    /// Archive path was not Unicode and cannot be normalized portably.
    #[error("archive contains a non-Unicode path")]
    NonUnicodePath,
    /// Link entries are forbidden.
    #[error("archive link `{path}` is forbidden")]
    Link {
        /// Rejected path.
        path: String,
    },
    /// Device nodes and other special entries are forbidden.
    #[error("archive special file `{path}` is forbidden")]
    SpecialFile {
        /// Rejected path.
        path: String,
    },
    /// Two entries map to the same portable destination.
    #[error("archive repeats destination `{}`", path.display())]
    DuplicateDestination {
        /// Repeated destination.
        path: PathBuf,
    },
    /// File/directory entry ordering conflicts.
    #[error("archive destination `{}` conflicts with another entry", path.display())]
    DestinationConflict {
        /// Conflicting destination.
        path: PathBuf,
    },
    /// Expanded archive content exceeded the hard safety limit.
    #[error("archive expands beyond the {MAX_EXTRACTED_BYTES}-byte safety limit")]
    ExpandedSizeLimit,
    /// A destination unexpectedly had no parent.
    #[error("archive destination `{}` has no parent", path.display())]
    NoParent {
        /// Rejected path.
        path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Write,
    };

    use tempfile::tempdir;
    use yt_media_engine::manifest::ArchiveFormat;
    use zip::write::SimpleFileOptions;

    use super::{ArchiveError, extract_archive};

    #[test]
    fn rejects_zip_parent_traversal() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let archive = directory.path().join("traversal.zip");
        let result = write_zip(&archive, &[("../escape", b"bad", None)]);
        assert!(result.is_ok());
        let destination = directory.path().join("output");
        let result = extract_archive(&archive, ArchiveFormat::Zip, &destination, None);
        assert!(matches!(result, Err(ArchiveError::UnsafePath { .. })));
        assert!(!directory.path().join("escape").exists());
    }

    #[test]
    fn rejects_zip_absolute_and_windows_prefix_paths() {
        for path in ["/absolute", r"C:\absolute"] {
            let directory = tempdir();
            assert!(directory.is_ok());
            let Some(directory) = directory.ok() else {
                continue;
            };
            let archive = directory.path().join("absolute.zip");
            assert!(write_zip(&archive, &[(path, b"bad", None)]).is_ok());
            let result = extract_archive(
                &archive,
                ArchiveFormat::Zip,
                &directory.path().join("output"),
                None,
            );
            assert!(matches!(result, Err(ArchiveError::UnsafePath { .. })));
        }
    }

    #[test]
    fn rejects_case_insensitive_duplicate_destinations() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let archive = directory.path().join("duplicate.zip");
        assert!(
            write_zip(
                &archive,
                &[("bin/tool", b"one", None), ("BIN/TOOL", b"two", None)]
            )
            .is_ok()
        );
        let result = extract_archive(
            &archive,
            ArchiveFormat::Zip,
            &directory.path().join("output"),
            None,
        );
        assert!(matches!(
            result,
            Err(ArchiveError::DuplicateDestination { .. })
        ));
    }

    #[test]
    fn rejects_zip_symlinks() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let archive = directory.path().join("symlink.zip");
        let file = File::create(&archive);
        assert!(file.is_ok());
        let Some(file) = file.ok() else {
            return;
        };
        let mut writer = zip::ZipWriter::new(file);
        assert!(
            writer
                .add_symlink("link", "target", SimpleFileOptions::default())
                .is_ok()
        );
        assert!(writer.finish().is_ok());
        let result = extract_archive(
            &archive,
            ArchiveFormat::Zip,
            &directory.path().join("output"),
            None,
        );
        assert!(matches!(result, Err(ArchiveError::Link { .. })));
    }

    #[test]
    fn rejects_tar_symlinks() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let archive = directory.path().join("symlink.tar.gz");
        let file = File::create(&archive);
        assert!(file.is_ok());
        let Some(file) = file.ok() else {
            return;
        };
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        assert!(builder.append_link(&mut header, "link", "target").is_ok());
        assert!(
            builder
                .into_inner()
                .and_then(flate2::write::GzEncoder::finish)
                .is_ok()
        );
        let result = extract_archive(
            &archive,
            ArchiveFormat::TarGz,
            &directory.path().join("output"),
            None,
        );
        assert!(matches!(result, Err(ArchiveError::Link { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_safe_tar_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let archive = directory.path().join("executable.tar.gz");
        let file = File::create(&archive);
        assert!(file.is_ok());
        let Some(file) = file.ok() else {
            return;
        };
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let bytes = b"#!/bin/sh\n";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        header.set_mode(0o4755);
        header.set_cksum();
        assert!(
            builder
                .append_data(&mut header, "configure", &bytes[..])
                .is_ok()
        );
        assert!(
            builder
                .into_inner()
                .and_then(flate2::write::GzEncoder::finish)
                .is_ok()
        );
        let destination = directory.path().join("output");
        assert!(extract_archive(&archive, ArchiveFormat::TarGz, &destination, None).is_ok());
        let metadata = fs::metadata(destination.join("configure"));
        assert!(metadata.is_ok());
        if let Ok(metadata) = metadata {
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o755);
        }
    }

    fn write_zip(
        path: &std::path::Path,
        entries: &[(&str, &[u8], Option<u32>)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = File::create(path)?;
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes, mode) in entries {
            let options = mode.map_or_else(SimpleFileOptions::default, |mode| {
                SimpleFileOptions::default().unix_permissions(mode)
            });
            writer.start_file(*name, options)?;
            writer.write_all(bytes)?;
        }
        writer.finish()?;
        assert!(fs::metadata(path)?.len() > 0);
        Ok(())
    }
}
