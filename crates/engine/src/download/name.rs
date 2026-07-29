//! Portable output-name sanitization and exclusive collision reservation.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use super::DownloadError;

const MAX_STEM_CHARS: usize = 160;
const MAX_STEM_BYTES: usize = 200;
const MAX_COLLISIONS: u32 = 10_000;

/// Sanitizes one untrusted output stem for Windows, macOS, and Linux.
///
/// Control characters and portable reserved punctuation become spaces, whitespace is collapsed,
/// trailing dots and spaces are removed, Windows device names are prefixed, and the readable stem
/// is bounded by both character and UTF-8 byte counts.
#[must_use]
pub fn sanitize_output_stem(input: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in input.chars() {
        let invalid = character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            );
        if invalid || character.is_whitespace() {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        if normalized.chars().count() >= MAX_STEM_CHARS
            || normalized.len().saturating_add(character.len_utf8()) > MAX_STEM_BYTES
        {
            break;
        }
        normalized.push(character);
    }

    while normalized.ends_with([' ', '.']) {
        normalized.pop();
    }
    if normalized.is_empty() || normalized == "." || normalized == ".." {
        "media".clone_into(&mut normalized);
    }
    if is_windows_device_name(&normalized) {
        normalized.insert(0, '_');
    }
    normalized
}

fn is_windows_device_name(value: &str) -> bool {
    let device = value
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device
            .strip_prefix("COM")
            .is_some_and(is_reserved_device_number)
        || device
            .strip_prefix("LPT")
            .is_some_and(is_reserved_device_number)
}

fn is_reserved_device_number(value: &str) -> bool {
    matches!(value, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
}

pub(crate) struct OutputReservation {
    final_path: PathBuf,
    lock_path: PathBuf,
    _lock: File,
}

impl OutputReservation {
    pub(crate) fn path(&self) -> &Path {
        &self.final_path
    }

    pub(crate) fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub(crate) fn publish(self, temporary: &Path) -> Result<PathBuf, DownloadError> {
        fs::hard_link(temporary, &self.final_path).map_err(|source| DownloadError::Filesystem {
            operation: "publish-no-clobber",
            path: bounded_path(&self.final_path),
            source,
        })?;
        if let Err(source) = fs::remove_file(temporary) {
            let _ignored = fs::remove_file(&self.final_path);
            return Err(DownloadError::Filesystem {
                operation: "remove-published-temporary",
                path: bounded_path(temporary),
                source,
            });
        }
        let final_path = self.final_path.clone();
        drop(self);
        Ok(final_path)
    }
}

impl Drop for OutputReservation {
    fn drop(&mut self) {
        let _ignored = fs::remove_file(&self.lock_path);
    }
}

pub(crate) fn reserve_output(
    directory: &Path,
    stem: &str,
    extension: &str,
) -> Result<OutputReservation, DownloadError> {
    for collision in 0..MAX_COLLISIONS {
        let suffix = if collision == 0 {
            String::new()
        } else {
            format!(" ({collision})")
        };
        let filename = format!("{stem}{suffix}.{extension}");
        let final_path = directory.join(&filename);
        let lock_path = directory.join(format!(".{filename}.yt-media-reserve"));
        let lock = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(lock) => lock,
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(DownloadError::Filesystem {
                    operation: "reserve-output-name",
                    path: bounded_path(&lock_path),
                    source,
                });
            }
        };
        if final_path.exists() {
            drop(lock);
            let _ignored = fs::remove_file(&lock_path);
            continue;
        }
        return Ok(OutputReservation {
            final_path,
            lock_path,
            _lock: lock,
        });
    }
    Err(DownloadError::CollisionLimit)
}

pub(crate) fn bounded_path(path: &Path) -> String {
    path.to_string_lossy().chars().take(1_024).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Barrier},
        thread,
    };

    use tempfile::tempdir;

    use super::{reserve_output, sanitize_output_stem};

    #[test]
    fn sanitizes_portable_reserved_characters_and_trailing_punctuation() {
        assert_eq!(sanitize_output_stem("  a<bad>: / name?.  "), "a bad name");
    }

    #[test]
    fn prevents_windows_device_names_with_extensions() {
        assert_eq!(sanitize_output_stem("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_output_stem("lpt9"), "_lpt9");
        assert_eq!(sanitize_output_stem("com10"), "com10");
    }

    #[test]
    fn bounds_unicode_names_without_splitting_characters() {
        let sanitized = sanitize_output_stem(&"é".repeat(500));
        assert!(sanitized.chars().count() <= 160);
        assert!(sanitized.len() <= 200);
        assert!(std::str::from_utf8(sanitized.as_bytes()).is_ok());
    }

    #[test]
    fn collision_reservations_are_exclusive_and_deterministic() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        let barrier = Arc::new(Barrier::new(3));
        let reserved = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let path = directory.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            let reserved = Arc::clone(&reserved);
            threads.push(thread::spawn(move || {
                barrier.wait();
                let reservation = reserve_output(&path, "name", "mp3");
                reserved.wait();
                reservation.map(|reservation| reservation.path().to_path_buf())
            }));
        }
        barrier.wait();
        reserved.wait();
        let mut paths = threads
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths.len(), 2);
        assert_ne!(paths[0], paths[1]);
    }

    #[test]
    fn existing_user_files_are_never_selected() {
        let directory = tempdir();
        assert!(directory.is_ok());
        let Some(directory) = directory.ok() else {
            return;
        };
        assert!(fs::write(directory.path().join("name.mp4"), b"user").is_ok());
        let reservation = reserve_output(directory.path(), "name", "mp4");
        assert!(reservation.is_ok());
        if let Ok(reservation) = reservation {
            assert_eq!(
                reservation
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str()),
                Some("name (1).mp4")
            );
        }
    }
}
