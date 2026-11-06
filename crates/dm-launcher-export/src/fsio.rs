//! Backup-then-atomic-replace file writes shared by every launcher format.
use crate::ExportError;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileAction {
    Created,
    Updated,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub action: FileAction,
    pub backup: Option<String>,
}

pub(crate) fn io_error(path: &Path, source: std::io::Error) -> ExportError {
    ExportError::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub(crate) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ExportError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path, error)),
    }
}

/// A validated replacement for one file, prepared before anything is written.
#[derive(Debug)]
pub(crate) struct PendingWrite {
    pub path: PathBuf,
    pub original: Option<Vec<u8>>,
    pub contents: Vec<u8>,
}

/// Apply writes in order. If one fails, the earlier ones are restored so a
/// launcher never keeps half of a multi-file export.
pub(crate) fn commit_all(writes: Vec<PendingWrite>) -> Result<Vec<FileChange>, ExportError> {
    let mut changes: Vec<FileChange> = Vec::with_capacity(writes.len());
    for write in writes {
        match commit(&write) {
            Ok(change) => changes.push(change),
            Err(error) => {
                for change in changes.iter().rev() {
                    let _ = rollback(change);
                }
                return Err(error);
            }
        }
    }
    Ok(changes)
}

fn commit(write: &PendingWrite) -> Result<FileChange, ExportError> {
    let display = write.path.to_string_lossy().into_owned();
    if write.original.as_deref() == Some(write.contents.as_slice()) {
        return Ok(FileChange {
            path: display,
            action: FileAction::Unchanged,
            backup: None,
        });
    }
    if read_optional(&write.path)? != write.original {
        return Err(ExportError::Refused(format!(
            "{display} changed while exporting; nothing was written to it"
        )));
    }
    let backup = match write.original {
        Some(_) => Some(create_backup(&write.path)?),
        None => None,
    };
    write_atomic(&write.path, &write.contents)?;
    Ok(FileChange {
        path: display,
        action: if backup.is_some() {
            FileAction::Updated
        } else {
            FileAction::Created
        },
        backup: backup.map(|path| path.to_string_lossy().into_owned()),
    })
}

fn rollback(change: &FileChange) -> Result<(), ExportError> {
    let path = Path::new(&change.path);
    match (&change.action, &change.backup) {
        (FileAction::Updated, Some(backup)) => {
            let bytes = fs::read(backup).map_err(|error| io_error(Path::new(backup), error))?;
            write_atomic(path, &bytes)
        }
        (FileAction::Created, _) => fs::remove_file(path).map_err(|error| io_error(path, error)),
        _ => Ok(()),
    }
}

/// Copy `path` next to itself under a timestamped name that never overwrites.
pub(crate) fn create_backup(path: &Path) -> Result<PathBuf, ExportError> {
    let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    for attempt in 0u32..1000 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let candidate = path.with_file_name(format!("{name}.dlsite-manager-{stamp}{suffix}.bak"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(&bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|error| io_error(&candidate, error))?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(&candidate, error)),
        }
    }
    Err(ExportError::Refused(format!(
        "could not choose a backup name for {}",
        path.display()
    )))
}

/// Write through a same-directory temporary file and rename it into place.
/// A symlinked target keeps its link: the file it points at is replaced.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), ExportError> {
    let target = match path.canonicalize() {
        Ok(target) => target,
        Err(error) if error.kind() == ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(io_error(path, error)),
    };
    let parent = target
        .parent()
        .ok_or_else(|| ExportError::Refused(format!("{} has no parent", target.display())))?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let temp = parent.join(format!(
        ".dlsite-manager-{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = write_temp(&temp, &target, contents)
        .and_then(|()| fs::rename(&temp, &target).map_err(|error| io_error(&target, error)));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return result;
    }
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn write_temp(temp: &Path, target: &Path, contents: &[u8]) -> Result<(), ExportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|error| io_error(temp, error))?;
    file.write_all(contents)
        .map_err(|error| io_error(temp, error))?;
    if let Ok(metadata) = fs::metadata(target) {
        fs::set_permissions(temp, metadata.permissions()).map_err(|error| io_error(temp, error))?;
    }
    file.sync_all().map_err(|error| io_error(temp, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(path: &Path, original: Option<&[u8]>, contents: &[u8]) -> PendingWrite {
        PendingWrite {
            path: path.to_path_buf(),
            original: original.map(<[u8]>::to_vec),
            contents: contents.to_vec(),
        }
    }

    #[test]
    fn unchanged_contents_are_not_rewritten_or_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        fs::write(&path, b"same").unwrap();
        let changes = commit_all(vec![pending(&path, Some(b"same"), b"same")]).unwrap();
        assert_eq!(changes[0].action, FileAction::Unchanged);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn update_keeps_a_backup_with_the_old_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        fs::write(&path, b"old").unwrap();
        let changes = commit_all(vec![pending(&path, Some(b"old"), b"new")]).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let backup = changes[0].backup.as_deref().unwrap();
        assert!(backup.contains(".dlsite-manager-"));
        assert_eq!(fs::read(backup).unwrap(), b"old");
    }

    #[test]
    fn repeated_backups_in_one_second_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        fs::write(&path, b"x").unwrap();
        let first = create_backup(&path).unwrap();
        let second = create_backup(&path).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_file_changed_after_reading_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        fs::write(&path, b"launcher wrote this").unwrap();
        let error = commit_all(vec![pending(&path, Some(b"old"), b"new")]).unwrap_err();
        assert!(error.to_string().contains("changed while exporting"));
        assert_eq!(fs::read(&path).unwrap(), b"launcher wrote this");
    }

    #[test]
    fn a_failed_later_write_restores_earlier_files() {
        let dir = tempfile::tempdir().unwrap();
        let updated = dir.path().join("updated.json");
        let created = dir.path().join("created.json");
        let conflicted = dir.path().join("conflicted.json");
        fs::write(&updated, b"old").unwrap();
        fs::write(&conflicted, b"other").unwrap();
        let error = commit_all(vec![
            pending(&updated, Some(b"old"), b"new"),
            pending(&created, None, b"new"),
            pending(&conflicted, Some(b"expected"), b"new"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("changed while exporting"));
        assert_eq!(fs::read(&updated).unwrap(), b"old");
        assert!(!created.exists());
        assert_eq!(fs::read(&conflicted).unwrap(), b"other");
    }

    #[test]
    fn atomic_write_leaves_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/a.json");
        write_atomic(&path, b"data").unwrap();
        write_atomic(&path, b"data2").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"data2");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_symlinks_and_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("link.json");
        fs::write(&real, b"old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&real, &link).unwrap();
        write_atomic(&link, b"new").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"new");
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }
}
