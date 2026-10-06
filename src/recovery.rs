use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

const MAGIC: &[u8] = b"MELLOW_RECOVERY_V1\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRecord {
    pub original_path: Option<PathBuf>,
    pub content: String,
    /// The journal this record came from when it belongs to an earlier
    /// session's unnamed draft; it is removed once the draft is restored or
    /// discarded.
    pub orphan_journal: Option<PathBuf>,
}

/// What a journal protects: a file on disk, or one unnamed draft. Every
/// unnamed buffer has its own draft id, so drafts never share a journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryKey {
    File(PathBuf),
    Draft(u64),
}

pub fn load(key: &RecoveryKey) -> Result<Option<RecoveryRecord>> {
    let journal = journal_path(key)?;
    if !journal.exists() {
        return Ok(None);
    }
    read_journal(&journal).map(Some)
}

fn read_journal(journal: &Path) -> Result<RecoveryRecord> {
    let mut bytes = Vec::new();
    fs::File::open(journal)
        .with_context(|| format!("failed to open recovery journal {}", journal.display()))?
        .read_to_end(&mut bytes)?;

    if !bytes.starts_with(MAGIC) {
        bail!("unsupported recovery journal format");
    }

    let payload = &bytes[MAGIC.len()..];
    let Some(separator) = payload.iter().position(|byte| *byte == 0) else {
        bail!("invalid recovery journal");
    };
    let path_bytes = &payload[..separator];
    let content_bytes = &payload[separator + 1..];

    let original_path = if path_bytes.is_empty() {
        None
    } else {
        Some(PathBuf::from(
            String::from_utf8(path_bytes.to_vec()).context("recovery path is not UTF-8")?,
        ))
    };
    let content =
        String::from_utf8(content_bytes.to_vec()).context("recovery content is not UTF-8")?;

    Ok(RecoveryRecord {
        original_path,
        content,
        orphan_journal: None,
    })
}

pub fn write(key: &RecoveryKey, content: &str) -> Result<PathBuf> {
    let dir = recovery_dir()?;
    ensure_private_dir(&dir)?;

    let journal = journal_path(key)?;
    let path = match key {
        RecoveryKey::File(path) => Some(path.as_path()),
        RecoveryKey::Draft(_) => None,
    };
    write_journal(&dir, &journal, path, content)?;
    Ok(journal)
}

fn write_journal(dir: &Path, journal: &Path, path: Option<&Path>, content: &str) -> Result<()> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = dir.join(format!(".journal-{}-{nonce}.tmp", std::process::id()));

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temp)
            .with_context(|| format!("failed to create recovery journal {}", temp.display()))?;
        file.write_all(MAGIC)?;
        if let Some(path) = path {
            file.write_all(path.to_string_lossy().as_bytes())?;
        }
        file.write_all(&[0])?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temp, journal)?;
        if let Ok(directory) = fs::File::open(dir) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn clear(key: &RecoveryKey) -> Result<()> {
    remove_journal(&journal_path(key)?)
}

pub fn remove_journal(journal: &Path) -> Result<()> {
    match fs::remove_file(journal) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove recovery journal {}", journal.display())),
    }
}

/// Unnamed drafts left by earlier sessions in this working directory whose
/// owning process is gone, oldest first. Drafts of a Mellow still running in
/// the same project are left alone.
pub fn orphan_drafts() -> Vec<RecoveryRecord> {
    let Ok(dir) = recovery_dir() else {
        return Vec::new();
    };
    orphan_drafts_in(
        &dir,
        &workspace_tag(),
        &legacy_draft_name(),
        std::process::id(),
        process_alive,
    )
}

fn orphan_drafts_in(
    dir: &Path,
    workspace: &str,
    legacy: &str,
    own_pid: u32,
    alive: impl Fn(u32) -> bool,
) -> Vec<RecoveryRecord> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let prefix = format!("untitled-{workspace}-");
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let orphaned = if name == legacy {
            true
        } else if let Some(rest) = name.strip_prefix(&prefix) {
            match rest
                .split('-')
                .next()
                .and_then(|pid| pid.parse::<u32>().ok())
            {
                Some(pid) => pid != own_pid && !alive(pid),
                None => false,
            }
        } else {
            false
        };
        if !orphaned {
            continue;
        }
        let journal = entry.path();
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        if let Ok(mut record) = read_journal(&journal) {
            record.orphan_journal = Some(journal);
            found.push((modified, record));
        }
    }
    found.sort_by_key(|left| left.0);
    found.into_iter().map(|(_, record)| record).collect()
}

fn journal_path(key: &RecoveryKey) -> Result<PathBuf> {
    let dir = recovery_dir()?;
    Ok(dir.join(journal_name(key, &workspace_tag(), std::process::id())))
}

fn journal_name(key: &RecoveryKey, workspace: &str, pid: u32) -> String {
    match key {
        RecoveryKey::File(path) => {
            format!("{:016x}.journal", fnv1a(path.to_string_lossy().as_bytes()))
        }
        RecoveryKey::Draft(id) => format!("untitled-{workspace}-{pid}-{id:016x}.journal"),
    }
}

/// Journal name used for every unnamed buffer before drafts had their own
/// ids; still recovered so an upgrade does not lose a pending draft.
fn legacy_draft_name() -> String {
    let identity = format!(
        "{}::untitled",
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .to_string_lossy()
    );
    format!("{:016x}.journal", fnv1a(identity.as_bytes()))
}

fn workspace_tag() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    format!("{:016x}", fnv1a(cwd.to_string_lossy().as_bytes()))
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // Signal 0 checks existence; EPERM still means the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// A fresh id for an unnamed buffer, unique within and across processes.
pub fn new_draft_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    fnv1a(&[nanos.to_le_bytes(), count.to_le_bytes()].concat())
}

fn recovery_dir() -> Result<PathBuf> {
    Ok(crate::settings::state_root().join("recovery"))
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create recovery directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn recovery_round_trip_and_clear() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        let key = RecoveryKey::File(path.clone());
        write(&key, "unsaved\ntext").unwrap();

        let record = load(&key).unwrap().unwrap();
        assert_eq!(record.original_path.as_deref(), Some(path.as_path()));
        assert_eq!(record.content, "unsaved\ntext");

        clear(&key).unwrap();
        assert!(load(&key).unwrap().is_none());
    }

    #[test]
    fn each_draft_has_its_own_journal() {
        let first = RecoveryKey::Draft(new_draft_id());
        let second = RecoveryKey::Draft(new_draft_id());
        assert_ne!(first, second);

        write(&first, "FIRST_UNSAVED_DRAFT").unwrap();
        write(&second, "SECOND_UNSAVED_DRAFT").unwrap();
        clear(&second).unwrap();

        assert_eq!(
            load(&first).unwrap().unwrap().content,
            "FIRST_UNSAVED_DRAFT"
        );
        assert!(load(&second).unwrap().is_none());
        clear(&first).unwrap();
    }

    #[test]
    fn orphan_drafts_are_this_workspace_and_dead_processes_only() {
        let dir = tempdir().unwrap();
        let put = |name: &str, content: &str| {
            write_journal(dir.path(), &dir.path().join(name), None, content).unwrap();
        };
        let draft = |pid: u32, id: u64| journal_name(&RecoveryKey::Draft(id), "ws", pid);
        put(&draft(111, 1), "crashed one");
        put(&draft(111, 2), "crashed two");
        put(&draft(222, 3), "still running elsewhere");
        put(&draft(333, 4), "this process");
        put(
            &journal_name(&RecoveryKey::Draft(5), "other", 111),
            "other project",
        );
        put("legacy.journal", "pre-upgrade draft");
        put(
            &journal_name(&RecoveryKey::File(PathBuf::from("/x")), "ws", 111),
            "named file",
        );

        let found = orphan_drafts_in(dir.path(), "ws", "legacy.journal", 333, |pid| pid == 222);
        let mut contents: Vec<&str> = found.iter().map(|record| record.content.as_str()).collect();
        contents.sort_unstable();
        assert_eq!(
            contents,
            ["crashed one", "crashed two", "pre-upgrade draft"]
        );
        assert!(found.iter().all(|record| record.orphan_journal.is_some()));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_journal_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("private.txt");
        let key = RecoveryKey::File(path);
        let journal = write(&key, "private").unwrap();
        let mode = fs::metadata(journal).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        clear(&key).unwrap();
    }
}
