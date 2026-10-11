//! Private, bounded local checkpoints. History is separate from crash journals:
//! it survives a successful save and is restored through an ordinary undoable edit.
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: String,
    pub event: String,
    pub created_ms: u64,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct HistoryStore {
    root: PathBuf,
}
const MAX_CONTENT: usize = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 20;
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_STORE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;

impl HistoryStore {
    pub fn default_location() -> Result<Self> {
        let root = crate::settings::state_root().join("history");
        Self::open(root)
    }
    pub fn open(root: PathBuf) -> Result<Self> {
        // macOS uses /var as a system alias of /private/var, including its
        // default temporary directory. Honor that OS alias in production too;
        // do not canonicalize user-controlled storage-path symlinks.
        #[cfg(target_os = "macos")]
        let root = if let Ok(relative) = root.strip_prefix("/var") {
            if fs::canonicalize("/var")? != Path::new("/private/var") {
                bail!("unexpected macOS /var alias");
            }
            Path::new("/private/var").join(relative)
        } else {
            root
        };
        private_directory(&root)?;
        Ok(Self { root })
    }

    /// Returns false for excluded secrets, oversized text, or a duplicate.
    /// Callers may explain this skip without disclosing the content.
    pub fn record(&self, path: &Path, content: &str, event: &str) -> Result<bool> {
        if !matches!(
            event,
            "save" | "before-save" | "ai-accept" | "before-restore" | "file-operation"
        ) {
            bail!("unsupported history event");
        }
        if content.len() > MAX_CONTENT || excluded(path, content) {
            return Ok(false);
        }
        let path = absolute_path(path)?;
        if excluded(&path, content) {
            return Ok(false);
        }
        let directory = self.directory(&path)?;
        private_directory(&directory)?;
        let entries = self.versions(&path)?;
        if entries.last().is_some_and(|entry| entry.content == content) {
            return Ok(false);
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let id = format!("{now:032x}-{:08x}", std::process::id());
        let record = serde_json::json!({"version":1,"path":path.to_str().context("history requires a UTF-8 path")?,"event":event,"created_ms":(now / 1_000_000) as u64,"content":content});
        let bytes = serde_json::to_vec(&record)?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let record_path = directory.join(format!("{id}.json"));
        let mut file = options.open(&record_path)?;
        if let Err(error) = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(())
        })() {
            let _ = fs::remove_file(&record_path);
            return Err(error);
        }
        if let Ok(directory) = fs::File::open(&directory) {
            let _ = directory.sync_all();
        }
        self.prune()?;
        Ok(true)
    }

    pub fn versions(&self, path: &Path) -> Result<Vec<HistoryEntry>> {
        let path = absolute_path(path)?;
        let directory = self.directory(&path)?;
        if !directory.exists() {
            return Ok(Vec::new());
        }
        check_private(&directory, true)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(directory)? {
            let record = entry?.path();
            if record.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            check_private(&record, false)?;
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let file = options.open(&record)?;
            if file.metadata()?.len() > (MAX_CONTENT * 6 + 4096) as u64 {
                bail!("history record exceeds size limit");
            }
            let value: serde_json::Value =
                serde_json::from_reader(file.take((MAX_CONTENT * 6 + 4097) as u64))
                    .context("invalid history checkpoint")?;
            if value["version"] != 1 || value["path"].as_str() != path.to_str() {
                bail!("history checkpoint identity mismatch");
            }
            let content = value["content"]
                .as_str()
                .context("history content missing")?
                .to_owned();
            if content.len() > MAX_CONTENT || excluded(&path, &content) {
                continue;
            }
            entries.push(HistoryEntry {
                id: record
                    .file_stem()
                    .context("invalid checkpoint name")?
                    .to_string_lossy()
                    .into_owned(),
                event: value["event"]
                    .as_str()
                    .context("history event missing")?
                    .to_owned(),
                created_ms: value["created_ms"]
                    .as_u64()
                    .context("history timestamp missing")?,
                content,
            });
        }
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    /// Remove this file's checkpoints after validating the store and every
    /// checkpoint identity. Other files' history is left untouched. Unexpected
    /// files or unsafe permissions stop the operation before any removal.
    pub fn forget(&self, path: &Path) -> Result<usize> {
        let path = absolute_path(path)?;
        let directory = self.directory(&path)?;
        if !directory.exists() {
            return Ok(0);
        }
        check_private(&directory, true)?;
        // Validate all JSON records (including excluded historical content)
        // before making this destructive operation concrete.
        self.versions(&path)?;
        let mut records = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let record = entry?.path();
            check_private(&record, false)?;
            if record.extension().and_then(|extension| extension.to_str()) != Some("json") {
                bail!("unexpected file in local history; nothing forgotten");
            }
            records.push(record);
        }
        for record in &records {
            fs::remove_file(record)?;
        }
        fs::remove_dir(directory)?;
        Ok(records.len())
    }

    fn directory(&self, path: &Path) -> Result<PathBuf> {
        check_private(&self.root, true)?;
        let path = absolute_path(path)?;
        let text = path.to_str().context("history requires a UTF-8 path")?;
        let hash = text.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        Ok(self.root.join(format!("{hash:016x}")))
    }

    fn prune(&self) -> Result<()> {
        check_private(&self.root, true)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let mut global = Vec::new();
        for directory in fs::read_dir(&self.root)? {
            let directory = directory?.path();
            check_private(&directory, true)?;
            let mut records = Vec::new();
            for entry in fs::read_dir(&directory)? {
                let path = entry?.path();
                check_private(&path, false)?;
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let meta = fs::metadata(&path)?;
                let created = meta.modified()?.duration_since(UNIX_EPOCH)?.as_millis() as u64;
                if now.saturating_sub(created) > MAX_AGE_MS {
                    fs::remove_file(path)?;
                    continue;
                }
                records.push((path, meta.len()));
            }
            records.sort_by(|a, b| a.0.cmp(&b.0));
            let mut bytes: u64 = records.iter().map(|record| record.1).sum();
            while records.len() > MAX_RECORDS || bytes > MAX_FILE_BYTES {
                let (path, size) = records.remove(0);
                fs::remove_file(path)?;
                bytes -= size;
            }
            global.extend(records);
        }
        global.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
        let mut bytes: u64 = global.iter().map(|record| record.1).sum();
        for (path, size) in global {
            if bytes <= MAX_STORE_BYTES {
                break;
            }
            fs::remove_file(path)?;
            bytes -= size;
        }
        Ok(())
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        fs::canonicalize(path).context("resolve history file")
    } else {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        if let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name())
            && parent.exists()
        {
            return Ok(fs::canonicalize(parent)?.join(name));
        }
        Ok(absolute)
    }
}

/// Conservative opt-out: never retain credential files or recognizable secret
/// assignments. History is local, but private permissions are not encryption.
pub fn excluded(path: &Path, content: &str) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || matches!(
            name.as_str(),
            "credentials"
                | "id_rsa"
                | "id_ed25519"
                | "secrets.json"
                | "secrets.yaml"
                | "secrets.yml"
        )
    {
        return true;
    }
    if path.components().any(|component| {
        matches!(
            component
                .as_os_str()
                .to_string_lossy()
                .to_ascii_lowercase()
                .as_str(),
            ".aws" | ".ssh" | "secrets"
        )
    }) {
        return true;
    }
    let lower = content.to_ascii_lowercase();
    // Generic token/secret fields are common in JSON/YAML and camelCase
    // credentials. Restrict these broad words to recognizable assignments so
    // ordinary prose about tokens does not lose history unnecessarily.
    static ASSIGNMENTS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let assignments = ASSIGNMENTS.get_or_init(|| {
        regex::Regex::new(r#"(?i)[\"']?([a-z][a-z0-9_-]*)[\"']?\s*[:=]"#)
            .expect("fixed history secret assignment regex")
    });
    if assignments.captures_iter(content).any(|capture| {
        let key: String = capture[1]
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_lowercase())
            .collect();
        matches!(
            key.as_str(),
            "token"
                | "secret"
                | "password"
                | "apikey"
                | "accesstoken"
                | "authtoken"
                | "clientsecret"
                | "secretkey"
                | "privatekey"
                | "credential"
                | "credentials"
        )
    }) {
        return true;
    }
    [
        "private key-----",
        "password",
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
        "client_secret",
        "secret_key",
        "bearer ",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn private_directory(path: &Path) -> Result<()> {
    // Reject symlinks in existing ancestors before creating any directories.
    for ancestor in path.ancestors() {
        if let Ok(meta) = fs::symlink_metadata(ancestor)
            && meta.file_type().is_symlink()
        {
            bail!("history path contains a symlink: {}", ancestor.display());
        }
    }
    if path.exists() {
        return check_private(path, true);
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    check_private(path, true)
}

fn check_private(path: &Path, directory: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink()
        || (directory && !meta.is_dir())
        || (!directory && !meta.is_file())
    {
        bail!("unsafe local history path: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments or memory preconditions.
        if meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || (!directory && meta.nlink() != 1)
        {
            bail!(
                "history path must be privately owned with no group/other access: {}",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_temp_alias_works_but_user_aliases_are_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let physical = fs::canonicalize(dir.path()).unwrap();
        let relative = physical.strip_prefix("/private/var").unwrap();
        let root = Path::new("/var").join(relative).join("history");
        let store = HistoryStore::open(root).unwrap();
        let path = dir.path().join("notes.txt");
        assert!(
            store
                .record(&path, "survives production temp path", "save")
                .unwrap()
        );
        assert_eq!(store.versions(&path).unwrap().len(), 1);
        let alias = physical.join("user-alias");
        symlink(physical.join("history"), &alias).unwrap();
        assert!(HistoryStore::open(alias).is_err());
    }
    #[test]
    fn checkpoints_survive_restart_and_keep_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("history");
        let path = dir.path().join("notes.txt");
        let store = HistoryStore::open(root.clone()).unwrap();
        assert!(store.record(&path, "first", "before-save").unwrap());
        assert!(store.record(&path, "second 🦀", "save").unwrap());
        assert!(!store.record(&path, "second 🦀", "save").unwrap());
        let entries = HistoryStore::open(root).unwrap().versions(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "first");
        assert_eq!(entries[1].content, "second 🦀");
        assert_eq!(entries[1].event, "save");
        assert!(entries[1].created_ms > 0);
    }
    #[test]
    fn secret_paths_content_and_oversized_files_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(fs::canonicalize(dir.path()).unwrap().join("history")).unwrap();
        for (name, text) in [
            (".env", "innocent"),
            ("settings.json", "{\"api_key\":\"dummy\"}"),
            ("id_rsa", "key"),
            ("config.json", "{\"token\":\"synthetic\"}"),
            ("config.yaml", "secret: synthetic\n"),
            ("camel.json", "{\"clientSecret\":\"synthetic\"}"),
        ] {
            assert!(!store.record(&dir.path().join(name), text, "save").unwrap());
        }
        assert!(
            !store
                .record(
                    &dir.path().join("big.txt"),
                    &"x".repeat(MAX_CONTENT + 1),
                    "save"
                )
                .unwrap()
        );
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 0);
    }
    #[cfg(unix)]
    #[test]
    fn aliases_to_excluded_credentials_are_not_recorded() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(fs::canonicalize(dir.path()).unwrap().join("history")).unwrap();
        let secret = dir.path().join(".env");
        fs::write(&secret, "plain-but-private").unwrap();
        let alias = dir.path().join("notes.txt");
        symlink(&secret, &alias).unwrap();
        assert!(!store.record(&alias, "plain-but-private", "save").unwrap());
        assert!(store.versions(&alias).unwrap().is_empty());
    }

    #[test]
    fn retention_limits_versions() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(fs::canonicalize(dir.path()).unwrap().join("history")).unwrap();
        let path = dir.path().join("notes.txt");
        for index in 0..25 {
            store
                .record(&path, &format!("version {index}"), "save")
                .unwrap();
        }
        let entries = store.versions(&path).unwrap();
        assert_eq!(entries.len(), MAX_RECORDS);
        assert_eq!(entries[0].content, "version 5");
        assert_eq!(entries.last().unwrap().content, "version 24");
    }
    #[test]
    fn forgetting_one_file_preserves_unrelated_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(fs::canonicalize(dir.path()).unwrap().join("history")).unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        store.record(&first, "first original", "save").unwrap();
        store.record(&first, "first revision", "save").unwrap();
        store.record(&second, "retain this", "save").unwrap();
        assert_eq!(store.forget(&first).unwrap(), 2);
        assert!(store.versions(&first).unwrap().is_empty());
        assert_eq!(store.forget(&first).unwrap(), 0);
        assert_eq!(store.versions(&second).unwrap()[0].content, "retain this");
    }
    #[cfg(unix)]
    #[test]
    fn forgetting_refuses_unsafe_or_mismatched_records_without_partial_removal() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(fs::canonicalize(dir.path()).unwrap().join("history")).unwrap();
        let path = dir.path().join("notes.txt");
        store.record(&path, "retain me", "save").unwrap();
        let directory = store.directory(&path).unwrap();
        let record = fs::read_dir(&directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.forget(&path).is_err());
        assert!(record.exists());
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = directory.join("unsafe.json");
        symlink(&record, &alias).unwrap();
        assert!(store.forget(&path).is_err());
        assert!(record.exists());
        fs::remove_file(alias).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        value["path"] = serde_json::json!("/some/unrelated/file");
        fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(store.forget(&path).is_err());
        assert!(record.exists());
    }
    #[cfg(unix)]
    #[test]
    fn private_modes_and_symlink_rejection() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("history");
        let store = HistoryStore::open(root.clone()).unwrap();
        let path = dir.path().join("notes.txt");
        store.record(&path, "private text", "save").unwrap();
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        let record = fs::read_dir(store.directory(&path).unwrap())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::metadata(&record).unwrap().mode() & 0o777, 0o600);
        fs::remove_file(&record).unwrap();
        symlink(&path, &record).unwrap();
        assert!(store.versions(&path).is_err());
        let alias = fs::canonicalize(dir.path()).unwrap().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(HistoryStore::open(alias).is_err());
    }
}
