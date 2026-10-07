/*
 * flattencom - Capture Output Protection
 *
 * Protects active capture identities and publishes complete exports atomically.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Process-wide capture identities and atomic output publication.
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::FlattenError;

// Current writers pin their identity with an open handle. Completed readable
// segments retain process-local path/alias protection without retaining a file
// descriptor for every segment in a keep-all capture.
static ACTIVE: OnceLock<Mutex<HashMap<PathBuf, CaptureIdentity>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<PathBuf, CaptureIdentity>> {
    ACTIVE.get_or_init(Mutex::default)
}

#[derive(Debug)]
enum CaptureIdentity {
    Writing(same_file::Handle),
    Completed(SegmentSnapshot),
}

/// Metadata captured from the flushed, still-open writer before closing it.
/// Identity numbers can be recycled after external deletion; retention therefore
/// also requires unchanged size and timestamps. External filesystem mutations
/// are not synchronized by this process-local registry.
#[derive(Debug, PartialEq, Eq)]
struct SegmentSnapshot {
    identity: (u64, u64),
    len: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
}

fn file_identity(handle: &same_file::Handle) -> std::io::Result<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = handle.as_file().metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        let information = winapi_util::file::information(handle.as_file())?;
        Ok((information.volume_serial_number(), information.file_index()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = handle;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "File identity snapshots are unsupported on this platform",
        ))
    }
}

impl SegmentSnapshot {
    fn from_handle(handle: &same_file::Handle) -> std::io::Result<Self> {
        let metadata = handle.as_file().metadata()?;
        Ok(Self {
            identity: file_identity(handle)?,
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        })
    }
}

impl CaptureIdentity {
    fn protects(&self, candidate: &same_file::Handle) -> std::io::Result<bool> {
        match self {
            Self::Writing(handle) => Ok(handle == candidate),
            Self::Completed(snapshot) => {
                if snapshot.identity != file_identity(candidate)? {
                    return Ok(false);
                }
                // A deleted segment's inode number can be recycled by an unrelated
                // new file, so identity alone would report a false active capture.
                // Creation time is immutable for a given file; compare it whenever
                // the platform reports it and stay conservative otherwise.
                let current = SegmentSnapshot::from_handle(candidate)?;
                Ok(match (snapshot.created, current.created) {
                    (Some(recorded), Some(observed)) => recorded == observed,
                    _ => true,
                })
            }
        }
    }
}

fn key(path: &Path) -> Result<PathBuf, FlattenError> {
    if path.exists() {
        return path
            .canonicalize()
            .map_err(|e| FlattenError::io(e.to_string()));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(parent
        .canonicalize()
        .map_err(|e| FlattenError::io(e.to_string()))?
        .join(
            path.file_name()
                .ok_or_else(|| FlattenError::io("Output requires a filename"))?,
        ))
}

fn active_capture_error() -> FlattenError {
    FlattenError::io("Output is an active capture; choose a different file")
}

fn check(path: &Path, active: &HashMap<PathBuf, CaptureIdentity>) -> Result<PathBuf, FlattenError> {
    let path = key(path)?;
    if active.contains_key(&path) {
        return Err(active_capture_error());
    }
    match same_file::Handle::from_path(&path) {
        Ok(candidate) => {
            for identity in active.values() {
                if identity
                    .protects(&candidate)
                    .map_err(|e| FlattenError::io(e.to_string()))?
                {
                    return Err(active_capture_error());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        // An inaccessible existing file cannot safely be declared unrelated.
        Err(error) => return Err(FlattenError::io(error.to_string())),
    }
    Ok(path)
}

/// Keeps current and completed capture segments protected, including aliases,
/// until retention expires the segment or its recorder is dropped.
#[derive(Debug)]
pub(crate) struct CaptureLease(PathBuf);

impl CaptureLease {
    pub(crate) fn open(
        path: &Path,
        create_new: bool,
    ) -> Result<(std::fs::File, Self), FlattenError> {
        let mut active = registry().lock().expect("capture registry");
        let normalized = check(path, &active)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(!create_new)
            .create_new(create_new)
            .open(&normalized)
            .map_err(|e| FlattenError::io(e.to_string()))?;
        let identity = file
            .try_clone()
            .and_then(same_file::Handle::from_file)
            .map_err(|e| FlattenError::io(e.to_string()))?;
        // Validate the opened object as well as the pre-open pathname.
        for other in active.values() {
            if other
                .protects(&identity)
                .map_err(|e| FlattenError::io(e.to_string()))?
            {
                return Err(active_capture_error());
            }
        }
        active.insert(normalized.clone(), CaptureIdentity::Writing(identity));
        Ok((file, Self(normalized)))
    }

    /// Release the completed segment's identity handle, preserving path/alias
    /// protection and a conservative snapshot for later retention checks.
    pub(crate) fn complete(&self) -> Result<(), FlattenError> {
        let mut active = registry().lock().expect("capture registry");
        let identity = active.get_mut(&self.0).expect("active capture lease");
        if let CaptureIdentity::Writing(handle) = identity {
            let snapshot = SegmentSnapshot::from_handle(handle)
                .map_err(|e| FlattenError::io(e.to_string()))?;
            *identity = CaptureIdentity::Completed(snapshot);
        }
        Ok(())
    }

    /// Compare a current path to the leased file, including renamed aliases.
    /// A missing path is not a match; other identity-query failures are reported.
    pub(crate) fn matches(&self, path: &Path) -> Result<bool, FlattenError> {
        let candidate = match same_file::Handle::from_path(path) {
            Ok(candidate) => candidate,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(FlattenError::io(error.to_string())),
        };
        let active = registry().lock().expect("capture registry");
        match active.get(&self.0).expect("active capture lease") {
            CaptureIdentity::Writing(handle) => Ok(*handle == candidate),
            CaptureIdentity::Completed(snapshot) => SegmentSnapshot::from_handle(&candidate)
                .map(|current| *snapshot == current)
                .map_err(|e| FlattenError::io(e.to_string())),
        }
    }
}

impl Drop for CaptureLease {
    fn drop(&mut self) {
        registry().lock().expect("capture registry").remove(&self.0);
    }
}

/// Publish a complete output atomically, refusing any active capture identity.
/// The callback writes only to a temporary file in the destination directory.
pub fn atomic_output<T>(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<T, FlattenError>,
) -> Result<T, FlattenError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| FlattenError::io(e.to_string()))?;
    let active = registry().lock().expect("capture registry");
    let normalized = check(path, &active)?;
    drop(active);
    let mut file = tempfile::NamedTempFile::new_in(normalized.parent().expect("absolute output"))
        .map_err(|e| FlattenError::io(e.to_string()))?;
    let result = write(file.as_file_mut())?;
    file.flush()
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| FlattenError::io(e.to_string()))?;
    let active = registry().lock().expect("capture registry");
    let current = check(path, &active)?;
    let destination = check(&normalized, &active)?;
    if current != normalized || destination != normalized {
        return Err(FlattenError::io("Output destination changed during export"));
    }
    file.persist(&normalized)
        .map_err(|e| FlattenError::io(e.to_string()))?;
    drop(active);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_segments_protect_aliases_without_pinning_handles() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("segment.log");
        let alias = directory.path().join("alias.log");
        let moved = directory.path().join("moved.log");
        let (mut writer, lease) = CaptureLease::open(&path, true).unwrap();
        writer.write_all(b"complete segment").unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        lease.complete().unwrap();
        lease.complete().unwrap();
        drop(writer);
        assert!(matches!(
            registry().lock().unwrap().get(&lease.0),
            Some(CaptureIdentity::Completed(_))
        ));
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(!lease.matches(&path).unwrap());
        for protected in [&moved, &alias] {
            assert!(lease.matches(protected).unwrap());
            assert!(atomic_output(protected, |_| Ok(())).is_err());
            assert!(CaptureLease::open(protected, false).is_err());
        }
        assert!(atomic_output(&path, |_| Ok(())).is_err());
        // Conservatively preserve an externally modified segment on expiry.
        std::fs::write(&moved, b"externally modified segment").unwrap();
        assert!(!lease.matches(&moved).unwrap());
        assert!(atomic_output(&alias, |_| Ok(())).is_err());
        drop(lease);
        assert!(atomic_output(&alias, |_| Ok(())).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    }

    #[test]
    fn renamed_capture_and_aliases_remain_protected_until_lease_drop() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("live.bin");
        let renamed = dir.path().join("renamed.bin");
        let alias = dir.path().join("alias.bin");
        let (mut writer, lease) = CaptureLease::open(&original, true).unwrap();
        writer.write_all(b"HEAD").unwrap();
        std::fs::rename(&original, &renamed).unwrap();
        std::fs::hard_link(&renamed, &alias).unwrap();
        assert!(lease.matches(&renamed).unwrap());
        assert!(lease.matches(&alias).unwrap());
        assert!(!lease.matches(&original).unwrap());
        // The old pathname remains reserved even if a different file appears there.
        std::fs::write(&original, b"unrelated").unwrap();
        assert!(!lease.matches(&original).unwrap());
        for output in [&original, &renamed, &alias] {
            assert!(
                atomic_output(output, |file| {
                    file.write_all(b"EXPORT")
                        .map_err(|e| FlattenError::io(e.to_string()))
                })
                .is_err()
            );
            assert!(CaptureLease::open(output, false).is_err());
        }
        writer.write_all(b"TAIL").unwrap();
        drop(writer);
        // Closing the writer alone must not release the identity or path reservation.
        assert!(atomic_output(&renamed, |_| Ok(())).is_err());
        assert_eq!(std::fs::read(&renamed).unwrap(), b"HEADTAIL");
        assert_eq!(std::fs::read(&alias).unwrap(), b"HEADTAIL");
        assert_eq!(std::fs::read(&original).unwrap(), b"unrelated");
        drop(lease);
        atomic_output(&renamed, |file| {
            file.write_all(b"EXPORT")
                .map_err(|e| FlattenError::io(e.to_string()))
        })
        .unwrap();
        assert_eq!(std::fs::read(&renamed).unwrap(), b"EXPORT");
        assert_eq!(std::fs::read(&alias).unwrap(), b"HEADTAIL");
    }

    #[test]
    fn capture_moved_to_export_destination_during_write_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.bin");
        let destination = dir.path().join("export.bin");
        let (mut writer, lease) = CaptureLease::open(&live, true).unwrap();
        writer.write_all(b"HEAD").unwrap();
        let result = atomic_output(&destination, |file| {
            file.write_all(b"EXPORT").unwrap();
            std::fs::rename(&live, &destination).unwrap();
            Ok(())
        });
        assert!(result.is_err());
        writer.write_all(b"TAIL").unwrap();
        drop(writer);
        drop(lease);
        assert_eq!(std::fs::read(destination).unwrap(), b"HEADTAIL");
    }

    #[test]
    fn opened_capture_can_be_read_after_path_replacement() {
        use std::io::{Read, Seek};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.bin");
        let moved = dir.path().join("moved.bin");
        let (mut writer, lease) = CaptureLease::open(&path, true).unwrap();
        writer.write_all(b"HEAD").unwrap();
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"unrelated").unwrap();
        let mut snapshot = writer.try_clone().unwrap();
        snapshot.rewind().unwrap();
        let mut bytes = Vec::new();
        snapshot.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"HEAD");
        drop(snapshot);
        writer.write_all(b"TAIL").unwrap();
        drop(writer);
        drop(lease);
        assert_eq!(std::fs::read(moved).unwrap(), b"HEADTAIL");
        assert_eq!(std::fs::read(path).unwrap(), b"unrelated");
    }

    #[cfg(unix)]
    #[test]
    fn retargeted_export_preserves_active_capture_and_tail() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let other = dir.path().join("other.txt");
        let alias = dir.path().join("alias.txt");
        std::fs::write(&original, b"original").unwrap();
        std::fs::write(&other, b"other").unwrap();
        symlink(&original, &alias).unwrap();
        let mut capture = None;
        let result = atomic_output(&alias, |output| {
            output.write_all(b"export").unwrap();
            capture = Some(CaptureLease::open(&original, false).unwrap());
            std::fs::remove_file(&alias).unwrap();
            symlink(&other, &alias).unwrap();
            Ok(())
        });
        assert!(result.is_err());
        let (mut file, lease) = capture.unwrap();
        file.write_all(b" tail").unwrap();
        drop(file);
        drop(lease);
        assert_eq!(std::fs::read(original).unwrap(), b"original tail");
        assert_eq!(std::fs::read(other).unwrap(), b"other");
    }

    #[cfg(unix)]
    #[test]
    fn retargeted_alias_is_rejected_even_without_active_capture() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let other = dir.path().join("other.txt");
        let alias = dir.path().join("alias.txt");
        std::fs::write(&original, b"original").unwrap();
        std::fs::write(&other, b"other").unwrap();
        symlink(&original, &alias).unwrap();
        assert!(
            atomic_output(&alias, |output| {
                output.write_all(b"export").unwrap();
                std::fs::remove_file(&alias).unwrap();
                symlink(&other, &alias).unwrap();
                Ok(())
            })
            .is_err()
        );
        assert_eq!(std::fs::read(original).unwrap(), b"original");
        assert_eq!(std::fs::read(other).unwrap(), b"other");
    }

    #[test]
    fn aliases_and_failed_publication_preserve_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.txt");
        let (mut file, lease) = CaptureLease::open(&path, true).unwrap();
        file.write_all(b"original").unwrap();
        let alias = dir.path().join("alias.txt");
        std::fs::hard_link(&path, &alias).unwrap();
        for output in [&path, &alias] {
            assert!(atomic_output(output, |_| Ok(())).is_err());
        }
        drop(file);
        drop(lease);
        let failed: Result<(), _> = atomic_output(&path, |f| {
            f.write_all(b"partial").unwrap();
            Err(FlattenError::io("injected failure"))
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        atomic_output(&path, |f| {
            f.write_all(b"complete")
                .map_err(|e| FlattenError::io(e.to_string()))
        })
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"complete");
    }
}
