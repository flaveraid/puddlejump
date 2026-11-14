//! Observing the filesystem: what exists at a path, without changing anything.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rustix::fs::{accessat, Access, AtFlags, CWD};
use serde::Serialize;
use thiserror::Error;

/// What exists at a path. A final symlink is never followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PathState {
    /// Nothing exists at the path.
    Missing,
    /// A regular file (or anything that is not one of the other variants).
    File,
    /// A real directory.
    Directory,
    /// A symlink to something that exists.
    Symlink {
        /// The destination exactly as stored in the link.
        destination: PathBuf,
    },
    /// A symlink whose destination does not exist.
    BrokenSymlink {
        /// The destination exactly as stored in the link.
        destination: PathBuf,
    },
}

/// A path could not be inspected.
#[derive(Debug, Error)]
#[error("cannot inspect {path}: {source}")]
pub struct FileSystemStateError {
    /// The path that could not be inspected.
    pub path: PathBuf,
    /// The underlying I/O error.
    pub source: io::Error,
}

/// A read-only view of the filesystem, so planning can be tested without one.
pub trait FileSystemState {
    /// What exists at `path`, without following a final symlink.
    ///
    /// Errors if the path cannot be inspected for a reason other than not
    /// existing.
    fn state(&self, path: &Path) -> Result<PathState, FileSystemStateError>;

    /// Whether the current user can create files in `path`, or, if it does not
    /// exist yet, in its nearest existing ancestor.
    fn is_writable(&self, path: &Path) -> bool;
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFileSystem;

impl FileSystemState for RealFileSystem {
    fn state(&self, path: &Path) -> Result<PathState, FileSystemStateError> {
        let error = |source| FileSystemStateError {
            path: path.to_path_buf(),
            source,
        };
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(PathState::Missing)
            }
            Err(source) => return Err(error(source)),
        };
        if metadata.file_type().is_symlink() {
            let destination = fs::read_link(path).map_err(error)?;
            return Ok(if path.exists() {
                PathState::Symlink { destination }
            } else {
                PathState::BrokenSymlink { destination }
            });
        }
        Ok(if metadata.is_dir() {
            PathState::Directory
        } else {
            PathState::File
        })
    }

    /// Asks the kernel with `faccessat(AT_EACCESS)`, so ownership, groups,
    /// ACLs, and read-only mounts are all accounted for, using the effective
    /// user the process would actually write as.
    fn is_writable(&self, path: &Path) -> bool {
        let Some(existing) = path.ancestors().find(|ancestor| ancestor.is_dir()) else {
            return false;
        };
        accessat(CWD, existing, Access::WRITE_OK, AtFlags::EACCESS).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    fn state_of(path: &Path) -> PathState {
        RealFileSystem.state(path).unwrap()
    }

    #[test]
    fn a_missing_path_is_missing() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(state_of(&root.path().join("nothing")), PathState::Missing);
    }

    #[test]
    fn a_regular_file_is_a_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        fs::write(&path, "").unwrap();
        assert_eq!(state_of(&path), PathState::File);
    }

    #[test]
    fn a_directory_is_a_directory() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(state_of(root.path()), PathState::Directory);
    }

    #[test]
    fn a_symlink_to_a_file_reports_its_stored_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("file");
        fs::write(&destination, "").unwrap();
        let link = root.path().join("link");
        symlink(&destination, &link).unwrap();

        assert_eq!(state_of(&link), PathState::Symlink { destination });
    }

    #[test]
    fn a_symlink_to_a_directory_is_a_symlink_not_a_directory() {
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join("link");
        symlink(root.path(), &link).unwrap();

        assert_eq!(
            state_of(&link),
            PathState::Symlink {
                destination: root.path().to_path_buf()
            }
        );
    }

    #[test]
    fn a_relative_symlink_destination_is_reported_as_stored() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "").unwrap();
        let link = root.path().join("link");
        symlink("file", &link).unwrap();

        assert_eq!(
            state_of(&link),
            PathState::Symlink {
                destination: PathBuf::from("file")
            }
        );
    }

    #[test]
    fn a_symlink_to_nothing_is_broken() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("missing");
        let link = root.path().join("link");
        symlink(&destination, &link).unwrap();

        assert_eq!(state_of(&link), PathState::BrokenSymlink { destination });
    }

    #[test]
    fn a_path_below_a_file_cannot_be_inspected() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        fs::write(&file, "").unwrap();

        let error = RealFileSystem.state(&file.join("child")).unwrap_err();
        assert_eq!(error.path, file.join("child"));
    }

    #[test]
    fn a_writable_directory_is_writable() {
        let root = tempfile::tempdir().unwrap();
        assert!(RealFileSystem.is_writable(root.path()));
    }

    #[test]
    fn a_missing_path_is_writable_if_its_nearest_existing_ancestor_is() {
        let root = tempfile::tempdir().unwrap();
        assert!(RealFileSystem.is_writable(&root.path().join("a/b/c")));
    }

    #[test]
    fn a_read_only_directory_is_not_writable() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("locked");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();

        let writable = RealFileSystem.is_writable(&directory.join("child"));

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!writable);
    }

    /// `/etc` is normally mode 755 and owned by root, so the owner write bit
    /// is set even though an ordinary user cannot write there. A mode-bit
    /// check would say writable; only a real access check says it is not.
    /// Skipped for root, who can write anywhere.
    #[test]
    fn a_directory_owned_by_another_user_with_the_owner_write_bit_is_not_writable() {
        let etc = Path::new("/etc");
        let mode = fs::metadata(etc).unwrap().mode();
        let writable_by_mode_bits = mode & 0o222 != 0;
        let running_as_root = fs::metadata("/proc/self").unwrap().uid() == 0;
        if !writable_by_mode_bits || running_as_root {
            return;
        }

        assert!(!RealFileSystem.is_writable(etc));
    }
}
