//! Generic hardlink-based directory snapshot primitive — task 3.1's
//! filestore half. Odoo-agnostic on purpose, the same posture as
//! `pg_admin.rs`: this module doesn't know or care that the directory it's
//! snapshotting might be an Odoo filestore, only that hardlinking is a
//! fast, space-efficient way to freeze a directory tree's current
//! contents.
//!
//! **Why hardlinks are the right primitive for Odoo's actual filestore**:
//! Odoo's filestore is content-hash-addressed and effectively
//! immutable-in-place — a file's content never changes while its name
//! (its hash) stays the same; attachments are added or removed, never
//! edited in place. That's exactly the case hardlinking is safe for: the
//! same mechanism as `cp -al`, an instant, metadata-only operation that
//! shares the same underlying data blocks between the original and the
//! snapshot, which stays correct precisely because the data those blocks
//! hold never mutates.
//!
//! **This primitive is generic, not Odoo-specific** — it makes no attempt
//! to verify that a given source directory actually has that
//! immutable-in-place property; the caller is responsible for that. A
//! directory of files that *are* edited in place would see the same edit
//! reflected in every snapshot ever taken of it, silently — hardlinked
//! files share one inode, so writing through one path writes through all
//! of them. The tests below deliberately prove both halves of this: that
//! the real Odoo pattern (delete-and-recreate under a new name) is
//! snapshot-safe, *and* that an in-place edit is NOT — rather than only
//! proving the happy path and leaving the limitation implicit.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum FilestoreError {
    #[error("source directory {0:?} does not exist or is not a directory")]
    SourceNotFound(PathBuf),
    #[error("destination {0:?} already exists")]
    DestinationExists(PathBuf),
    #[error("io error at {path:?}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotStats {
    pub files_linked: u64,
    pub dirs_created: u64,
    pub symlinks_recreated: u64,
}

/// Recursively hardlink-clone `source` into `dest`, which must not already
/// exist. Regular files are hardlinked (`fs::hard_link` — instant,
/// metadata-only, no bytes copied, requires `dest` be on the same
/// filesystem as `source`); directories are recreated; symlinks are
/// recreated as symlinks rather than followed, so a symlink that happens to
/// point outside the source tree (or at itself) can't turn this into an
/// infinite loop or an accidental full copy of something unrelated.
pub fn snapshot_directory(source: &Path, dest: &Path) -> Result<SnapshotStats, FilestoreError> {
    if !source.is_dir() {
        return Err(FilestoreError::SourceNotFound(source.to_path_buf()));
    }
    if dest.exists() {
        return Err(FilestoreError::DestinationExists(dest.to_path_buf()));
    }
    let mut stats = SnapshotStats::default();
    clone_dir_recursive(source, dest, &mut stats)?;
    Ok(stats)
}

/// How a regular file is reproduced in the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileMode {
    /// Share the same inode — instant, no bytes copied, and correct only
    /// because Odoo's filestore is content-addressed and never edited in
    /// place (see this module's doc comment).
    Hardlink,
    /// Copy the bytes. Slower and bigger, and the right choice for a
    /// **backup**: an archive is meant to be an independent artifact you
    /// can move to another disk or keep after deleting the original, and
    /// a hardlink is none of those things — it can't cross a filesystem
    /// boundary, and it makes "how big is this backup" a lie, since the
    /// space is shared with the live data it was taken from.
    Copy,
}

fn clone_dir_recursive(source: &Path, dest: &Path, stats: &mut SnapshotStats) -> Result<(), FilestoreError> {
    clone_dir_recursive_with(source, dest, stats, FileMode::Hardlink)
}

fn clone_dir_recursive_with(
    source: &Path,
    dest: &Path,
    stats: &mut SnapshotStats,
    mode: FileMode,
) -> Result<(), FilestoreError> {
    std::fs::create_dir_all(dest).map_err(|e| FilestoreError::Io { path: dest.to_path_buf(), source: e })?;
    stats.dirs_created += 1;

    let entries = std::fs::read_dir(source).map_err(|e| FilestoreError::Io { path: source.to_path_buf(), source: e })?;
    for entry in entries {
        let entry = entry.map_err(|e| FilestoreError::Io { path: source.to_path_buf(), source: e })?;
        let file_type = entry.file_type().map_err(|e| FilestoreError::Io { path: entry.path(), source: e })?;
        let dest_path = dest.join(entry.file_name());

        if file_type.is_dir() {
            clone_dir_recursive_with(&entry.path(), &dest_path, stats, mode)?;
        } else if file_type.is_symlink() {
            recreate_symlink(&entry.path(), &dest_path)?;
            stats.symlinks_recreated += 1;
        } else {
            match mode {
                FileMode::Hardlink => {
                    std::fs::hard_link(entry.path(), &dest_path)
                        .map_err(|e| FilestoreError::Io { path: dest_path.clone(), source: e })?;
                }
                FileMode::Copy => {
                    std::fs::copy(entry.path(), &dest_path)
                        .map_err(|e| FilestoreError::Io { path: dest_path.clone(), source: e })?;
                }
            }
            stats.files_linked += 1;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn recreate_symlink(source: &Path, dest: &Path) -> Result<(), FilestoreError> {
    let target = std::fs::read_link(source).map_err(|e| FilestoreError::Io { path: source.to_path_buf(), source: e })?;
    std::os::unix::fs::symlink(&target, dest).map_err(|e| FilestoreError::Io { path: dest.to_path_buf(), source: e })
}

#[cfg(not(unix))]
fn recreate_symlink(source: &Path, dest: &Path) -> Result<(), FilestoreError> {
    // Non-Unix symlink recreation needs a different std API (and, on
    // Windows, a privilege the process may not have) — left unimplemented
    // rather than silently skipping the entry or faking success.
    Err(FilestoreError::Io {
        path: dest.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::Unsupported, format!("symlink recreation not implemented on this platform (source: {source:?})")),
    })
}

/// Removes a snapshot directory tree entirely. Note this only ever removes
/// the *snapshot's* copy — the hardlinked files it shared with the source
/// only actually free their disk space once every link to them (source and
/// every other snapshot included) is gone, which is the normal, correct
/// behavior of a hardlink and not something this function needs to reason
/// about.
/// As `snapshot_directory`, but copying the bytes instead of hardlinking
/// them — for a **backup**, which has to stand on its own.
///
/// A snapshot lives beside the data it was taken from and is meant to be
/// instant, so sharing inodes is exactly right. A backup is meant to be
/// archived, moved to another disk, or kept long after the original is
/// gone, and for that the bytes have to actually be there.
pub fn copy_directory(source: &Path, dest: &Path) -> Result<SnapshotStats, FilestoreError> {
    if !source.is_dir() {
        return Err(FilestoreError::SourceNotFound(source.to_path_buf()));
    }
    if dest.exists() {
        return Err(FilestoreError::DestinationExists(dest.to_path_buf()));
    }
    let mut stats = SnapshotStats::default();
    clone_dir_recursive_with(source, dest, &mut stats, FileMode::Copy)?;
    Ok(stats)
}

pub fn remove_snapshot_directory(dest: &Path) -> Result<(), FilestoreError> {
    std::fs::remove_dir_all(dest).map_err(|e| FilestoreError::Io { path: dest.to_path_buf(), source: e })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    #[test]
    fn hardlinks_every_regular_file_and_recreates_directory_structure() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(source.join("checksum-ab")).unwrap();
        fs::write(source.join("checksum-ab").join("abc123"), b"attachment bytes").unwrap();
        fs::create_dir_all(source.join("checksum-cd")).unwrap();
        fs::write(source.join("checksum-cd").join("cde456"), b"more bytes").unwrap();

        let dest = root.path().join("snapshot-1");
        let stats = snapshot_directory(&source, &dest).unwrap();

        assert_eq!(stats.files_linked, 2);
        assert_eq!(stats.dirs_created, 3, "source root + two subdirectories");
        assert_eq!(fs::read(dest.join("checksum-ab").join("abc123")).unwrap(), b"attachment bytes");
        assert_eq!(fs::read(dest.join("checksum-cd").join("cde456")).unwrap(), b"more bytes");
    }

    #[test]
    fn hardlinked_files_genuinely_share_the_same_inode() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("abc123"), b"content").unwrap();

        let dest = root.path().join("snapshot-1");
        snapshot_directory(&source, &dest).unwrap();

        let source_ino = fs::metadata(source.join("abc123")).unwrap().ino();
        let dest_ino = fs::metadata(dest.join("abc123")).unwrap().ino();
        assert_eq!(source_ino, dest_ino, "a hardlink must share the source's inode — this is what makes it instant and zero-copy");
    }

    #[test]
    fn delete_and_recreate_in_source_does_not_affect_an_existing_snapshot() {
        // This is the real Odoo filestore access pattern: a file is never
        // edited, only unlinked and replaced (usually under a different
        // content-hash name). Proves hardlink snapshots are safe for it.
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("abc123"), b"original content").unwrap();

        let dest = root.path().join("snapshot-1");
        snapshot_directory(&source, &dest).unwrap();

        fs::remove_file(source.join("abc123")).unwrap();
        fs::write(source.join("def456"), b"a newer, differently-named attachment").unwrap();

        assert_eq!(fs::read(dest.join("abc123")).unwrap(), b"original content", "the snapshot must still have the file that was deleted from source afterward");
        assert!(!dest.join("def456").exists(), "a file added to source after the snapshot must not appear in it");
    }

    #[test]
    fn editing_a_file_in_place_after_snapshotting_leaks_through_the_shared_inode() {
        // The honest limitation this module's doc comment warns about:
        // hardlinking is only safe for content that's never mutated in
        // place. This test proves that limitation is real, not theoretical
        // — a caller snapshotting a directory that DOES get edited in place
        // would silently get a broken snapshot, exactly like this.
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("mutable.txt"), b"version 1").unwrap();

        let dest = root.path().join("snapshot-1");
        snapshot_directory(&source, &dest).unwrap();

        // An in-place edit (open-write-truncate, not unlink+recreate).
        let overwritten = "version 2 - overwritten in place";
        fs::write(source.join("mutable.txt"), overwritten).unwrap();

        assert_eq!(
            fs::read_to_string(dest.join("mutable.txt")).unwrap(),
            overwritten,
            "an in-place edit to the source is visible through the snapshot too, since they share the same inode — this is the real, documented limitation"
        );
    }

    #[test]
    fn symlinks_are_recreated_as_symlinks_not_followed() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("real_file"), b"real content").unwrap();
        std::os::unix::fs::symlink("real_file", source.join("link_to_real_file")).unwrap();

        let dest = root.path().join("snapshot-1");
        let stats = snapshot_directory(&source, &dest).unwrap();

        assert_eq!(stats.symlinks_recreated, 1);
        assert_eq!(stats.files_linked, 1);
        let dest_link = dest.join("link_to_real_file");
        assert!(fs::symlink_metadata(&dest_link).unwrap().file_type().is_symlink(), "must recreate an actual symlink, not follow and copy its target");
        assert_eq!(fs::read_link(&dest_link).unwrap(), std::path::PathBuf::from("real_file"));
    }

    #[test]
    fn empty_directory_snapshots_cleanly() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();

        let dest = root.path().join("snapshot-1");
        let stats = snapshot_directory(&source, &dest).unwrap();

        assert_eq!(stats.files_linked, 0);
        assert_eq!(stats.dirs_created, 1);
        assert!(dest.is_dir());
    }

    #[test]
    fn rejects_a_missing_source_directory() {
        let root = TempDir::new().unwrap();
        let err = snapshot_directory(&root.path().join("does-not-exist"), &root.path().join("dest")).unwrap_err();
        assert!(matches!(err, FilestoreError::SourceNotFound(_)));
    }

    #[test]
    fn rejects_an_already_existing_destination() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let dest = root.path().join("dest");
        fs::create_dir_all(&dest).unwrap();

        let err = snapshot_directory(&source, &dest).unwrap_err();
        assert!(matches!(err, FilestoreError::DestinationExists(_)));
    }

    #[test]
    fn remove_snapshot_directory_deletes_the_snapshots_own_copy_only() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("abc123"), b"content").unwrap();

        let dest = root.path().join("snapshot-1");
        snapshot_directory(&source, &dest).unwrap();

        remove_snapshot_directory(&dest).unwrap();

        assert!(!dest.exists(), "the snapshot directory itself must be gone");
        assert!(source.join("abc123").exists(), "removing a snapshot must never touch the source");
        assert_eq!(fs::read(source.join("abc123")).unwrap(), b"content", "the source file's data must survive, proven not just by existing but by still holding its bytes");
    }
}
