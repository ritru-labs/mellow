//! Recoverable file removal. No copy/delete fallback: a cross-device move
//! fails with the original intact. Restoring never replaces an existing path.
use anyhow::{Context, Result, bail, ensure};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt},
};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct TrashEntry {
    pub original: PathBuf,
    pub stored: PathBuf,
    item_identity: (u64, u64),
    parent_identity: Option<(u64, u64)>,
}

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn read_manifest(directory: &Path) -> Result<(PathBuf, Option<(u64, u64)>)> {
    use std::os::unix::ffi::OsStringExt;
    private_directory(directory)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("original.json"))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "trash manifest must be private, regular and owned by this user"
    );
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "trash manifest exceeds limit");
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    // Read older manifests without silently upgrading their evidence.
    let (path, parent) = if value.is_array() {
        (serde_json::from_value::<Vec<u8>>(value)?, None)
    } else {
        let path = serde_json::from_value::<Vec<u8>>(
            value.get("original").context("missing trash path")?.clone(),
        )?;
        let dev = value
            .get("parent_device")
            .and_then(serde_json::Value::as_u64)
            .context("missing parent device")?;
        let ino = value
            .get("parent_inode")
            .and_then(serde_json::Value::as_u64)
            .context("missing parent inode")?;
        (path, Some((dev, ino)))
    };
    let original = PathBuf::from(std::ffi::OsString::from_vec(path));
    ensure!(
        original.is_absolute()
            && original.file_name().is_some()
            && !original.components().any(|component| matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )),
        "invalid trash destination"
    );
    Ok((original, parent))
}

fn private_root(root: &Path) -> Result<()> {
    if matches!(fs::symlink_metadata(root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        fs::create_dir_all(root.parent().context("trash needs a parent")?)?;
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(root)?;
    }
    private_directory(root)
}

fn private_directory(path: &Path) -> Result<()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "trash directory must be private and owned by this user"
    );
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        // Darwin ACLs can grant access despite mode 0700. Conservatively
        // refuse every explicit ACL rather than treating mode bits as proof.
        unsafe extern "C" {
            fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
            fn acl_get_entry(
                acl: *mut libc::c_void,
                entry_id: libc::c_int,
                entry: *mut *mut libc::c_void,
            ) -> libc::c_int;
            fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
        }
        // SAFETY: file's descriptor is live. acl_get_fd allocates an ACL that
        // is released exactly once after querying its first entry.
        let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(error).context("checking trash ACL privacy");
            }
        } else {
            let mut entry = std::ptr::null_mut();
            let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
            let error = std::io::Error::last_os_error();
            unsafe {
                acl_free(acl);
            }
            // Darwin returns -1/EINVAL when an ACL has no entries.
            ensure!(
                result == -1 && error.raw_os_error() == Some(libc::EINVAL),
                "trash directory has an explicit ACL; use a private directory without ACL grants"
            );
        }
    }
    Ok(())
}

pub fn default_root() -> PathBuf {
    crate::settings::state_root().join("trash")
}

pub fn move_to_trash(root: &Path, source: &Path) -> Result<()> {
    private_root(root)?;
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()?.join(source)
    };
    // Canonicalize the parent only: the final symlink is the item to move.
    let parent = fs::canonicalize(source.parent().context("source needs a parent")?)?;
    let original = parent.join(
        source
            .file_name()
            .context("cannot trash a filesystem root")?,
    );
    let metadata = fs::symlink_metadata(&original)?;
    ensure!(
        metadata.is_file() || metadata.is_dir() || metadata.file_type().is_symlink(),
        "special files cannot be trashed"
    );
    let id = format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let entry = root.join(id);
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(&entry)?;
    let manifest = entry.join("original.json");
    // Preserve non-UTF8 names without lossy path conversion.
    let parent_metadata = fs::metadata(&parent)?;
    let bytes = serde_json::to_vec(
        &serde_json::json!({"original": original.as_os_str().as_bytes(), "parent_device": parent_metadata.dev(), "parent_inode": parent_metadata.ino()}),
    )?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&manifest)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::File::open(&entry)?.sync_all()?;
    rename_exclusive(&original, &entry.join("item")).context(
        "recoverable removal unavailable (possibly another filesystem); original retained",
    )?;
    fs::File::open(&entry)?.sync_all()?;
    Ok(())
}

pub fn entries(root: &Path) -> Result<Vec<TrashEntry>> {
    if matches!(fs::symlink_metadata(root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Vec::new());
    }
    private_directory(root)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let stored = entry.path().join("item");
        if fs::symlink_metadata(&stored).is_err() {
            continue;
        }
        let (original, parent_identity) = read_manifest(&entry.path())?;
        let item_identity = identity(&fs::symlink_metadata(&stored)?);
        entries.push(TrashEntry {
            original,
            stored,
            item_identity,
            parent_identity,
        });
    }
    entries.sort_by(|a, b| b.stored.cmp(&a.stored));
    Ok(entries)
}

pub fn restore(entry: &TrashEntry) -> Result<()> {
    let directory = entry
        .stored
        .parent()
        .context("trash item needs a directory")?;
    ensure!(
        entry.stored.file_name().is_some_and(|name| name == "item"),
        "invalid trash item path"
    );
    private_directory(directory.parent().context("trash entry needs a root")?)?;
    let (original, parent_identity) = read_manifest(directory)?;
    ensure!(
        original == entry.original && parent_identity == entry.parent_identity,
        "trash manifest changed; review the entry again"
    );
    ensure!(
        identity(&fs::symlink_metadata(&entry.stored)?) == entry.item_identity,
        "trash item changed; review the entry again"
    );
    let parent = original
        .parent()
        .context("restore destination needs a parent")?;
    ensure!(
        fs::canonicalize(parent)? == parent,
        "restore parent became a symlink; trash retained"
    );
    if let Some(expected) = parent_identity {
        ensure!(
            identity(&fs::metadata(parent)?) == expected,
            "restore parent was replaced; trash retained"
        );
    }
    rename_exclusive(&entry.stored, &entry.original)
        .context("restore refused; existing files and trash retained")
}

pub fn rename_exclusive(source: &Path, destination: &Path) -> Result<()> {
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    // Some supported static musl toolchains do not export the renameat2 libc
    // wrapper. Invoke the same kernel operation directly; unsupported kernels
    // still fail closed rather than fall back to an overwriting rename.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("exclusive rename is unsupported on this platform");
    if result != 0 {
        bail!(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn restore_refuses_changed_manifest_or_item_after_review() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trash");
        let source = dir.path().join("source");
        fs::write(&source, "recover me").unwrap();
        move_to_trash(&root, &source).unwrap();
        let entry = entries(&root).unwrap().remove(0);
        let manifest = entry.stored.parent().unwrap().join("original.json");
        let original = fs::read(&manifest).unwrap();
        fs::write(
            &manifest,
            serde_json::to_vec(dir.path().join("different").as_os_str().as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(restore(&entry).is_err());
        assert!(entry.stored.exists());
        assert!(!source.exists());
        fs::write(&manifest, original).unwrap();
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, "replacement work").unwrap();
        fs::rename(replacement, &entry.stored).unwrap();
        assert!(restore(&entry).is_err());
        assert_eq!(
            fs::read_to_string(&entry.stored).unwrap(),
            "replacement work"
        );
    }
    #[test]
    fn restore_refuses_a_replaced_original_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trash");
        let parent = dir.path().join("project");
        fs::create_dir(&parent).unwrap();
        let source = parent.join("note");
        fs::write(&source, "old work").unwrap();
        move_to_trash(&root, &source).unwrap();
        let entry = entries(&root).unwrap().remove(0);
        fs::rename(&parent, dir.path().join("previous-project")).unwrap();
        fs::create_dir(&parent).unwrap();
        assert!(restore(&entry).is_err());
        assert!(!source.exists());
        assert!(entry.stored.exists());
    }
    #[test]
    fn trash_manifest_never_follows_symlinks_or_reads_unbounded_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trash");
        let source = dir.path().join("source");
        fs::write(&source, "keep").unwrap();
        move_to_trash(&root, &source).unwrap();
        let entry = entries(&root).unwrap().remove(0);
        let manifest = entry.stored.parent().unwrap().join("original.json");
        fs::remove_file(&manifest).unwrap();
        let target = dir.path().join("private-data");
        fs::write(&target, "must not read").unwrap();
        std::os::unix::fs::symlink(&target, &manifest).unwrap();
        assert!(entries(&root).is_err());
        assert!(restore(&entry).is_err());
        fs::remove_file(&manifest).unwrap();
        fs::write(&manifest, vec![b' '; 65537]).unwrap();
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            entries(&root)
                .unwrap_err()
                .to_string()
                .contains("exceeds limit")
        );
        assert!(entry.stored.exists());
    }
    #[test]
    fn trash_refuses_symlink_root_and_nonprivate_entries() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        assert!(entries(&linked).is_err());
        let source = dir.path().join("source");
        fs::write(&source, "keep").unwrap();
        move_to_trash(&real, &source).unwrap();
        let entry = entries(&real).unwrap().remove(0);
        fs::set_permissions(
            entry.stored.parent().unwrap(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(entries(&real).is_err());
        assert!(restore(&entry).is_err());
        assert!(entry.stored.exists());
    }
    #[test]
    fn exclusive_rename_never_replaces_a_directory_or_dangling_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, "keep").unwrap();
        let destination = dir.path().join("destination");
        std::os::unix::fs::symlink(dir.path().join("missing"), &destination).unwrap();
        assert!(rename_exclusive(&source, &destination).is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "keep");
        assert!(
            fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&destination).unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(rename_exclusive(&source, &destination).is_err());
        assert!(destination.is_dir());
    }
    #[test]
    fn exclusive_rename_preserves_existing_regular_file_and_moves_to_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::write(&source, "preserve source").unwrap();
        fs::write(&destination, "newer destination").unwrap();
        let source_identity = identity(&fs::metadata(&source).unwrap());
        assert!(rename_exclusive(&source, &destination).is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "preserve source");
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "newer destination"
        );
        fs::remove_file(&destination).unwrap();
        rename_exclusive(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "preserve source");
        assert_eq!(
            identity(&fs::metadata(&destination).unwrap()),
            source_identity
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn trash_rejects_acl_access_even_when_mode_bits_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trash");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let output = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read,write,search"])
            .arg(&root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o077, 0);
        let source = dir.path().join("source");
        fs::write(&source, "keep").unwrap();
        let error = move_to_trash(&root, &source).unwrap_err().to_string();
        assert!(error.contains("ACL"), "{error}");
        assert_eq!(fs::read_to_string(source).unwrap(), "keep");
    }
    #[test]
    fn trash_survives_restart_and_restore_never_overwrites_new_work() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trash");
        let file = dir.path().join("notes.txt");
        fs::write(&file, "before").unwrap();
        move_to_trash(&root, &file).unwrap();
        assert!(!file.exists());
        let found = entries(&root).unwrap();
        assert_eq!(found.len(), 1);
        fs::write(&file, "newer").unwrap();
        assert!(restore(&found[0]).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "newer");
        assert!(found[0].stored.exists());
        fs::remove_file(&file).unwrap();
        restore(&found[0]).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "before");
    }
    #[test]
    fn trash_moves_symlink_without_touching_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let root = dir.path().join("trash");
        move_to_trash(&root, &link).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        restore(&entries(&root).unwrap()[0]).unwrap();
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    }
    #[test]
    fn trash_refuses_nonprivate_or_symlink_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trash");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "keep").unwrap();
        assert!(move_to_trash(&root, &file).is_err());
        assert!(file.exists());
    }
}
