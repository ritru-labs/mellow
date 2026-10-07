#[cfg(test)]
use std::sync::Arc;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
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

/// Writes journals on a background thread, so the disk syncs behind crash
/// safety never delay typing. Queued jobs for one journal are merged: only
/// the newest write or clear for it reaches the disk.
pub struct JournalWriter {
    /// `None` if the thread could not start; jobs then run inline.
    jobs: Option<mpsc::Sender<Job>>,
    failures: mpsc::Receiver<String>,
    report: mpsc::Sender<String>,
    #[cfg(test)]
    test: Arc<WriterTestHooks>,
}

enum Job {
    Write(RecoveryKey, String),
    Clear(RecoveryKey),
    Idle(mpsc::Sender<()>),
}

#[cfg(test)]
#[derive(Default)]
struct WriterTestHooks {
    paused: std::sync::atomic::AtomicBool,
    writes: std::sync::atomic::AtomicUsize,
}

/// Holds the journal thread before its next batch, like a stalled disk.
#[cfg(test)]
pub struct PausedJournal(Arc<WriterTestHooks>);

#[cfg(test)]
impl Drop for PausedJournal {
    fn drop(&mut self) {
        self.0
            .paused
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Longest wait for queued journal work at a sync point (quit, save, close).
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

impl JournalWriter {
    pub fn start() -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (report, failures) = mpsc::channel();
        #[cfg(test)]
        let test = Arc::new(WriterTestHooks::default());
        let worker_report = report.clone();
        #[cfg(test)]
        let worker_test = Arc::clone(&test);
        let started = thread::Builder::new()
            .name("mellow-journal".to_owned())
            .spawn(move || {
                while let Ok(first) = inbox.recv() {
                    #[cfg(test)]
                    while worker_test.paused.load(std::sync::atomic::Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(1));
                    }
                    let mut latest: Vec<(RecoveryKey, Option<String>)> = Vec::new();
                    let mut idle_waiters = Vec::new();
                    for job in std::iter::once(first).chain(inbox.try_iter()) {
                        let (key, content) = match job {
                            Job::Write(key, content) => (key, Some(content)),
                            Job::Clear(key) => (key, None),
                            Job::Idle(waiter) => {
                                idle_waiters.push(waiter);
                                continue;
                            }
                        };
                        match latest.iter_mut().find(|(queued, _)| *queued == key) {
                            Some(entry) => entry.1 = content,
                            None => latest.push((key, content)),
                        }
                    }
                    for (key, content) in latest {
                        #[cfg(test)]
                        if content.is_some() {
                            worker_test
                                .writes
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        let result = match content {
                            Some(content) => write(&key, &content).map(drop),
                            None => clear(&key),
                        };
                        if let Err(error) = result {
                            let _ = worker_report.send(error.to_string());
                        }
                    }
                    for waiter in idle_waiters {
                        let _ = waiter.send(());
                    }
                }
            });
        Self {
            jobs: started.ok().map(|_| jobs),
            failures,
            report,
            #[cfg(test)]
            test,
        }
    }

    /// Queues the newest content of a journal.
    pub fn write(&self, key: RecoveryKey, content: String) {
        self.queue(Job::Write(key, content));
    }

    /// Queues removal of a journal.
    pub fn clear(&self, key: RecoveryKey) {
        self.queue(Job::Clear(key));
    }

    fn queue(&self, job: Job) {
        let job = match &self.jobs {
            Some(jobs) => match jobs.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let result = match job {
            Job::Write(key, content) => write(&key, &content).map(drop),
            Job::Clear(key) => clear(&key),
            Job::Idle(_) => Ok(()),
        };
        if let Err(error) = result {
            let _ = self.report.send(error.to_string());
        }
    }

    /// Waits until everything queued so far is on disk.
    pub fn wait_idle(&self) {
        let Some(jobs) = &self.jobs else {
            return;
        };
        let (done, finished) = mpsc::channel();
        if jobs.send(Job::Idle(done)).is_ok() {
            let _ = finished.recv_timeout(IDLE_TIMEOUT);
        }
    }

    /// Writes now, after anything queued, so an older queued write can never
    /// land on top of it.
    pub fn write_now(&self, key: &RecoveryKey, content: &str) -> Result<PathBuf> {
        self.wait_idle();
        write(key, content)
    }

    /// Clears now, after anything queued, so a queued write can never bring
    /// the journal back.
    pub fn clear_now(&self, key: &RecoveryKey) -> Result<()> {
        self.wait_idle();
        clear(key)
    }

    /// Errors from background writes since the last call.
    pub fn take_failures(&self) -> Vec<String> {
        self.failures.try_iter().collect()
    }

    /// Holds the worker before its next batch until the guard is dropped.
    #[cfg(test)]
    pub fn pause(&self) -> PausedJournal {
        self.test
            .paused
            .store(true, std::sync::atomic::Ordering::SeqCst);
        PausedJournal(Arc::clone(&self.test))
    }

    #[cfg(test)]
    pub fn writes_performed(&self) -> usize {
        self.test.writes.load(std::sync::atomic::Ordering::Relaxed)
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
    fn queued_journal_work_keeps_only_the_newest_job_per_journal() {
        let dir = tempdir().unwrap();
        let key = |name: &str| RecoveryKey::File(dir.path().join(name));
        let writer = JournalWriter::start();

        let paused = writer.pause();
        writer.write(key("a"), "old".to_owned());
        writer.write(key("a"), "new".to_owned());
        writer.write(key("gone"), "saved meanwhile".to_owned());
        writer.clear(key("gone"));
        writer.write(key("b"), "kept".to_owned());
        drop(paused);
        writer.wait_idle();

        assert_eq!(load(&key("a")).unwrap().unwrap().content, "new");
        assert!(load(&key("gone")).unwrap().is_none());
        assert_eq!(load(&key("b")).unwrap().unwrap().content, "kept");
        assert_eq!(writer.writes_performed(), 2, "a once, b once");
        assert!(writer.take_failures().is_empty());
        clear(&key("a")).unwrap();
        clear(&key("b")).unwrap();
    }

    /// Saving clears the journal: a write still queued from typing must not
    /// land afterwards and offer stale text as "unsaved" on the next start.
    #[test]
    fn clear_now_is_never_overtaken_by_a_queued_write() {
        let dir = tempdir().unwrap();
        let key = RecoveryKey::File(dir.path().join("saved.txt"));
        let writer = JournalWriter::start();
        for round in 0..20 {
            writer.write(key.clone(), format!("typed before save {round}"));
            writer.clear_now(&key).unwrap();
            writer.wait_idle();
            assert!(load(&key).unwrap().is_none(), "round {round}");
        }
    }

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
