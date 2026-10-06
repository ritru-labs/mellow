use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitLineChange {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHunkStage {
    Unstaged,
    Staged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBranch {
    pub name: String,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    pub oid: String,
    pub short_oid: String,
    pub date: String,
    pub author: String,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBlameLine {
    pub row: usize,
    pub short_oid: String,
    pub author: String,
    pub date: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileChange {
    pub path: PathBuf,
    pub index_status: char,
    pub worktree_status: char,
    pub untracked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHunk {
    pub path: PathBuf,
    pub stage: GitHunkStage,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub header: String,
    pub preview: String,
    pub patch: String,
    pub synthetic_file: bool,
    pub untracked: bool,
}

impl GitHunk {
    pub fn target_row(&self) -> usize {
        if self.synthetic_file || self.patch.is_empty() {
            return self.new_start.saturating_sub(1);
        }

        first_changed_row(&self.patch, self.new_start)
            .unwrap_or_else(|| self.new_start.saturating_sub(1))
    }

    pub fn action_label(&self) -> &'static str {
        match self.stage {
            GitHunkStage::Unstaged => "unstaged",
            GitHunkStage::Staged => "staged",
        }
    }
}

/// One file's Git state, in the words the status bar uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileState {
    New,
    Modified,
    Staged,
    PartlyStaged,
    Conflict,
}

impl GitFileChange {
    pub fn state(&self) -> GitFileState {
        let (index, worktree) = (self.index_status, self.worktree_status);
        if self.untracked {
            GitFileState::New
        } else if index == 'U'
            || worktree == 'U'
            || (index == 'A' && worktree == 'A')
            || (index == 'D' && worktree == 'D')
        {
            GitFileState::Conflict
        } else if index != ' ' && worktree != ' ' {
            GitFileState::PartlyStaged
        } else if index != ' ' {
            GitFileState::Staged
        } else {
            GitFileState::Modified
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GitSnapshot {
    pub branch: String,
    /// Commits on this branch not yet pushed / not yet pulled. Zero when the
    /// branch has no upstream.
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<GitFileChange>,
    pub hunks: Vec<GitHunk>,
    line_changes: HashMap<PathBuf, HashMap<usize, GitLineChange>>,
}

impl GitSnapshot {
    pub fn line_change(&self, path: &Path, row: usize) -> Option<GitLineChange> {
        self.line_changes
            .get(path)
            .and_then(|changes| changes.get(&row))
            .copied()
    }

    pub fn file(&self, relative: &Path) -> Option<&GitFileChange> {
        self.files.iter().find(|file| file.path == relative)
    }

    pub fn staged_count(&self) -> usize {
        self.hunks
            .iter()
            .filter(|hunk| hunk.stage == GitHunkStage::Staged)
            .count()
    }

    pub fn unstaged_count(&self) -> usize {
        self.hunks
            .iter()
            .filter(|hunk| hunk.stage == GitHunkStage::Unstaged)
            .count()
    }
}

#[derive(Debug, Clone)]
pub struct GitRepository {
    root: PathBuf,
    /// Per-file results of the last refresh, shared by clones so the
    /// background refresh and forced refreshes reuse each other's work.
    cache: Arc<Mutex<RefreshCache>>,
}

/// File metadata that changes whenever the file's bytes do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatKey {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl StatKey {
    fn of(path: &Path) -> Option<Self> {
        let metadata = fs::symlink_metadata(path).ok()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        };
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity,
        })
    }

    /// Changed long enough ago that a further write would show up as a new
    /// timestamp even on filesystems with coarse (1–2 s) clocks.
    fn settled(&self) -> bool {
        #[cfg(unix)]
        let changed = SystemTime::UNIX_EPOCH
            .checked_add(Duration::new(
                u64::try_from(self.identity.2).unwrap_or(0),
                u32::try_from(self.identity.3).unwrap_or(0),
            ))
            .into_iter()
            .chain(self.modified)
            .max();
        #[cfg(not(unix))]
        let changed = self.modified;
        changed.is_some_and(|changed| {
            SystemTime::now()
                .duration_since(changed)
                .is_ok_and(|age| age >= REFRESH_SETTLE_TIME)
        })
    }
}

/// How old a file change must be before its diff is cached.
const REFRESH_SETTLE_TIME: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(2)
};

/// Everything a changed file's hunks and gutter marks depend on, besides
/// HEAD and the index (see `RefreshCache::repository`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    index_status: char,
    worktree_status: char,
    worktree: Option<StatKey>,
}

#[derive(Debug, Clone)]
struct CachedFile {
    fingerprint: FileFingerprint,
    hunks: Vec<GitHunk>,
    line_changes: HashMap<usize, GitLineChange>,
}

/// Lets a refresh skip the three `git diff` runs per changed file when the
/// file, the index and HEAD are all as they were last time. Without it an
/// idle editor with 50 changed files ran ~140 git processes a second.
#[derive(Debug, Default)]
struct RefreshCache {
    repository: Option<(Option<String>, Option<StatKey>)>,
    files: HashMap<PathBuf, CachedFile>,
    #[cfg(test)]
    computed: usize,
}

impl GitRepository {
    pub fn discover(start: &Path) -> Result<Option<Self>> {
        let output = run_git(start, &["rev-parse", "--show-toplevel"])?;
        if !output.status.success() {
            return Ok(None);
        }

        let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if root.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            root: PathBuf::from(root),
            cache: Arc::default(),
        }))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn refresh(&self) -> Result<GitSnapshot> {
        let branch = self.branch_name();
        let repository = self.head_and_index();
        let files = self.status_files()?;
        let mut hunks = Vec::new();
        let mut line_changes = HashMap::new();

        // Work on a private copy so the lock is never held while git runs.
        let mut previous = {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if cache.repository.as_ref() != Some(&repository) {
                cache.files.clear();
            }
            std::mem::take(&mut cache.files)
        };
        let cacheable = repository.1.is_none_or(|index| index.settled());
        let mut next = HashMap::new();
        #[cfg(test)]
        let mut computed = 0;

        for file in &files {
            let worktree = StatKey::of(&self.root.join(&file.path));
            let fingerprint = FileFingerprint {
                index_status: file.index_status,
                worktree_status: file.worktree_status,
                worktree,
            };
            let entry = match previous.remove(&file.path) {
                Some(cached) if cached.fingerprint == fingerprint => cached,
                _ => {
                    #[cfg(test)]
                    {
                        computed += 1;
                    }
                    let (file_hunks, file_lines) = self.file_changes(file)?;
                    CachedFile {
                        fingerprint,
                        hunks: file_hunks,
                        line_changes: file_lines,
                    }
                }
            };
            hunks.extend(entry.hunks.iter().cloned());
            line_changes.insert(file.path.clone(), entry.line_changes.clone());
            if cacheable && worktree.is_none_or(|stat| stat.settled()) {
                next.insert(file.path.clone(), entry);
            }
        }

        {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            cache.repository = Some(repository);
            cache.files = next;
            #[cfg(test)]
            {
                cache.computed += computed;
            }
        }

        hunks.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| stage_rank(left.stage).cmp(&stage_rank(right.stage)))
                .then_with(|| left.new_start.cmp(&right.new_start))
        });

        let (ahead, behind) = self.ahead_behind();
        Ok(GitSnapshot {
            branch,
            ahead,
            behind,
            files,
            hunks,
            line_changes,
        })
    }

    /// HEAD's commit (None before the first commit) and the index file's
    /// metadata: when either changes, every cached diff is stale.
    fn head_and_index(&self) -> (Option<String>, Option<StatKey>) {
        let Ok(output) = run_git(&self.root, &["rev-parse", "--git-path", "index", "HEAD"]) else {
            return (None, None);
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let index = lines
            .next()
            .map(|path| self.root.join(path))
            .and_then(|path| StatKey::of(&path));
        let head = output
            .status
            .success()
            .then(|| lines.next().map(str::to_owned))
            .flatten();
        (head, index)
    }

    #[cfg(test)]
    fn files_computed(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .computed
    }

    /// Hunks (unstaged, then staged) and gutter marks for one changed file.
    #[allow(clippy::type_complexity)]
    fn file_changes(
        &self,
        file: &GitFileChange,
    ) -> Result<(Vec<GitHunk>, HashMap<usize, GitLineChange>)> {
        let relative = &file.path;
        let mut hunks: Vec<GitHunk> = if file.untracked {
            synthetic_untracked_hunk(&self.root, relative)?
                .into_iter()
                .collect()
        } else {
            self.diff_hunks(relative, GitHunkStage::Unstaged)?
        };
        hunks.extend(self.diff_hunks(relative, GitHunkStage::Staged)?);

        if hunks.is_empty() {
            let stage = if file.index_status != ' ' && file.index_status != '?' {
                GitHunkStage::Staged
            } else {
                GitHunkStage::Unstaged
            };
            hunks.push(GitHunk {
                path: relative.clone(),
                stage,
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 0,
                header: "file-level change".to_owned(),
                preview: status_preview(file),
                patch: String::new(),
                synthetic_file: true,
                untracked: file.untracked,
            });
        }

        let line_changes = if file.untracked {
            untracked_line_changes(&self.root.join(relative))?
        } else {
            self.line_changes_for_path(relative)?
        };
        Ok((hunks, line_changes))
    }

    /// `git add` for one whole file.
    pub fn stage_file(&self, path: &Path) -> Result<()> {
        let relative = self.repository_relative_path(path)?;
        self.run_checked(&["add", "--", &path_arg(&relative)])
    }

    /// Take one whole file out of the index, keeping its working-tree edits.
    pub fn unstage_file(&self, path: &Path) -> Result<()> {
        let relative = self.repository_relative_path(path)?;
        let path = path_arg(&relative);
        if self
            .run_checked(&["restore", "--staged", "--", &path])
            .is_ok()
        {
            return Ok(());
        }
        // No HEAD yet (first commit): drop the file from the index instead.
        self.run_checked(&["rm", "--cached", "-q", "--", &path])
    }

    fn ahead_behind(&self) -> (usize, usize) {
        let Ok(output) = run_git(
            &self.root,
            &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
        ) else {
            return (0, 0);
        };
        if !output.status.success() {
            return (0, 0);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut counts = text.split_whitespace().map(|n| n.parse().unwrap_or(0));
        (counts.next().unwrap_or(0), counts.next().unwrap_or(0))
    }

    pub fn stage_hunk(&self, hunk: &GitHunk) -> Result<()> {
        if hunk.stage != GitHunkStage::Unstaged {
            bail!("selected hunk is already staged");
        }

        if hunk.synthetic_file || hunk.untracked {
            return self.run_checked(&["add", "--", &path_arg(&hunk.path)]);
        }

        self.apply_patch(&hunk.patch, true, false)
    }

    pub fn unstage_hunk(&self, hunk: &GitHunk) -> Result<()> {
        if hunk.stage != GitHunkStage::Staged {
            bail!("selected hunk is not staged");
        }

        if hunk.synthetic_file {
            let path = path_arg(&hunk.path);
            let restore = self.run_checked(&["restore", "--staged", "--", &path]);
            if restore.is_ok() {
                return Ok(());
            }
            return self.run_checked(&["reset", "-q", "HEAD", "--", &path]);
        }

        self.apply_patch(&hunk.patch, true, true)
    }

    pub fn revert_hunk(&self, hunk: &GitHunk) -> Result<()> {
        if hunk.stage != GitHunkStage::Unstaged {
            bail!("only unstaged hunks can be reverted");
        }
        if hunk.untracked {
            bail!("untracked files are never deleted by hunk revert");
        }

        if hunk.synthetic_file {
            return self.run_checked(&["restore", "--worktree", "--", &path_arg(&hunk.path)]);
        }

        self.apply_patch(&hunk.patch, false, true)
    }

    pub fn commit_staged(&self, message: &str) -> Result<String> {
        let message = message.trim();
        if message.is_empty() {
            bail!("commit message cannot be empty");
        }
        let staged = run_git(&self.root, &["diff", "--cached", "--quiet"])?;
        match staged.status.code() {
            Some(0) => bail!("there are no staged changes to commit"),
            Some(1) => {}
            _ => bail!("failed to inspect staged changes: {}", stderr_text(&staged)),
        }

        let output = run_git(&self.root, &["commit", "-m", message])?;
        if !output.status.success() {
            bail!("git commit failed: {}", stderr_text(&output));
        }
        let head = run_git(&self.root, &["rev-parse", "--short=12", "HEAD"])?;
        if !head.status.success() {
            bail!(
                "commit created but HEAD lookup failed: {}",
                stderr_text(&head)
            );
        }
        Ok(String::from_utf8_lossy(&head.stdout).trim().to_owned())
    }

    pub fn branches(&self) -> Result<Vec<GitBranch>> {
        let output = run_git(
            &self.root,
            &[
                "for-each-ref",
                "--format=%(HEAD)%09%(refname:short)",
                "refs/heads",
            ],
        )?;
        if !output.status.success() {
            bail!("git branch list failed: {}", stderr_text(&output));
        }
        let mut branches = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.splitn(2, '\t');
            let marker = fields.next().unwrap_or("");
            let name = fields.next().unwrap_or("").trim();
            if !name.is_empty() {
                branches.push(GitBranch {
                    name: name.to_owned(),
                    current: marker.trim() == "*",
                });
            }
        }
        branches.sort_by(|left, right| {
            right
                .current
                .cmp(&left.current)
                .then(left.name.cmp(&right.name))
        });
        Ok(branches)
    }

    pub fn create_branch(&self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("branch name cannot be empty");
        }
        let check = run_git(&self.root, &["check-ref-format", "--branch", name])?;
        if !check.status.success() {
            bail!("invalid branch name: {}", stderr_text(&check));
        }
        self.run_checked(&["branch", name])
    }

    pub fn rename_branch(&self, old_name: &str, new_name: &str) -> Result<()> {
        let old_name = old_name.trim();
        let new_name = new_name.trim();
        if old_name.is_empty() || new_name.is_empty() {
            bail!("branch names cannot be empty");
        }
        let check = run_git(&self.root, &["check-ref-format", "--branch", new_name])?;
        if !check.status.success() {
            bail!("invalid branch name: {}", stderr_text(&check));
        }
        self.run_checked(&["branch", "-m", old_name, new_name])
    }

    pub fn delete_branch(&self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("branch name cannot be empty");
        }
        if self.branch_name() == name {
            bail!("cannot delete the current branch");
        }
        self.run_checked(&["branch", "-d", name])
    }

    pub fn switch_branch(&self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("branch name cannot be empty");
        }
        self.run_checked(&["switch", name])
    }

    fn repository_relative_path(&self, path: &Path) -> Result<PathBuf> {
        if !path.is_absolute() {
            return Ok(path.to_path_buf());
        }

        if let Ok(relative) = path.strip_prefix(&self.root) {
            return Ok(relative.to_path_buf());
        }

        // On macOS, temporary paths can be presented as /var/... while Git
        // canonicalizes the same repository as /private/var/.... Canonicalize
        // only the parent directory so a symlink file remains addressed by its
        // repository path rather than by an external target.
        let canonical_root = fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        let normalized = match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => fs::canonicalize(parent)
                .map(|parent| parent.join(name))
                .unwrap_or_else(|_| path.to_path_buf()),
            _ => path.to_path_buf(),
        };

        normalized
            .strip_prefix(&canonical_root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                anyhow::anyhow!(
                    "{} is outside Git repository {}",
                    path.display(),
                    self.root.display()
                )
            })
    }

    pub fn branch_contains_path(&self, branch: &str, path: &Path) -> Result<bool> {
        let relative = self.repository_relative_path(path)?;
        let spec = format!("{branch}:{}", path_arg(&relative));
        let output = run_git(&self.root, &["cat-file", "-e", &spec])?;
        Ok(output.status.success())
    }

    pub fn history(&self, limit: usize) -> Result<Vec<GitCommit>> {
        let limit = limit.clamp(1, 200).to_string();
        let output = run_git(
            &self.root,
            &[
                "log",
                "--date=short",
                &format!("--max-count={limit}"),
                "--pretty=format:%H%x09%h%x09%ad%x09%an%x09%s",
            ],
        )?;
        if !output.status.success() {
            bail!("git log failed: {}", stderr_text(&output));
        }
        let mut commits = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.splitn(5, '\t');
            let (Some(oid), Some(short_oid), Some(date), Some(author), Some(subject)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            commits.push(GitCommit {
                oid: oid.to_owned(),
                short_oid: short_oid.to_owned(),
                date: date.to_owned(),
                author: author.to_owned(),
                subject: subject.to_owned(),
            });
        }
        Ok(commits)
    }

    pub fn conflict_paths(&self) -> Result<Vec<PathBuf>> {
        let output = run_git(&self.root, &["ls-files", "-u", "-z"])?;
        if !output.status.success() {
            bail!("git conflict scan failed: {}", stderr_text(&output));
        }

        // Unmerged index records are NUL-delimited and shaped as:
        // "<mode> <object> <stage>\t<path>". The same path appears once per
        // conflict stage, so deduplicate after parsing the tab-delimited path.
        let mut paths = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .filter_map(|field| {
                let tab = field.iter().position(|byte| *byte == b'\t')?;
                let path = &field[tab + 1..];
                (!path.is_empty())
                    .then(|| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    pub fn mark_resolved(&self, path: &Path) -> Result<()> {
        let relative = self.repository_relative_path(path)?;
        if !self
            .conflict_paths()?
            .iter()
            .any(|candidate| candidate == &relative)
        {
            bail!("{} is not an unresolved Git conflict", relative.display());
        }
        self.run_checked(&["add", "--", &path_arg(&relative)])
    }

    pub fn fetch(&self) -> Result<()> {
        self.run_checked(&["fetch", "--prune"])
    }

    pub fn upstream_ref(&self) -> Result<String> {
        let output = run_git(
            &self.root,
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )?;
        if !output.status.success() {
            bail!(
                "current branch has no configured upstream: {}",
                stderr_text(&output)
            );
        }
        let upstream = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if upstream.is_empty() {
            bail!("current branch has no configured upstream");
        }
        Ok(upstream)
    }

    pub fn fast_forward_upstream(&self) -> Result<()> {
        self.run_checked(&["merge", "--ff-only", "@{u}"])
    }

    pub fn push(&self) -> Result<()> {
        self.run_checked(&["push"])
    }

    pub fn blame(&self, path: &Path) -> Result<Vec<GitBlameLine>> {
        let relative = self.repository_relative_path(path)?;
        let output = run_git(
            &self.root,
            &["blame", "--line-porcelain", "--", &path_arg(&relative)],
        )?;
        if !output.status.success() {
            bail!("git blame failed: {}", stderr_text(&output));
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let mut rows = Vec::new();
        let mut current_oid = String::new();
        let mut author = String::new();
        let mut date = String::new();
        let mut row = 0usize;

        for line in text.lines() {
            if line.starts_with('\t') {
                rows.push(GitBlameLine {
                    row,
                    short_oid: current_oid.chars().take(10).collect(),
                    author: author.clone(),
                    date: date.clone(),
                    text: line.trim_start_matches('\t').to_owned(),
                });
                row = row.saturating_add(1);
                continue;
            }

            let mut fields = line.split_whitespace();
            if let (Some(oid), Some(_orig), Some(_final_line)) =
                (fields.next(), fields.next(), fields.next())
                && oid.len() >= 8
                && oid.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '^')
            {
                current_oid = oid.trim_start_matches('^').to_owned();
                author.clear();
                date.clear();
                continue;
            }
            if let Some(value) = line.strip_prefix("author ") {
                author = value.to_owned();
            } else if let Some(timestamp) = line.strip_prefix("author-time ") {
                date = timestamp.to_owned();
            }
        }
        Ok(rows)
    }

    fn branch_name(&self) -> String {
        run_git(&self.root, &["branch", "--show-current"])
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "detached".to_owned())
    }

    fn status_files(&self) -> Result<Vec<GitFileChange>> {
        let output = run_git(
            &self.root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        if !output.status.success() {
            bail!("git status failed: {}", stderr_text(&output));
        }

        let mut files = Vec::new();
        let fields: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
        let mut index = 0usize;

        while index < fields.len() {
            let entry = fields[index];
            if entry.is_empty() {
                index += 1;
                continue;
            }
            if entry.len() < 4 {
                index += 1;
                continue;
            }

            let index_status = char::from(entry[0]);
            let worktree_status = char::from(entry[1]);
            let path_bytes = &entry[3..];
            let mut path = PathBuf::from(String::from_utf8_lossy(path_bytes).into_owned());

            if matches!(index_status, 'R' | 'C') && index + 1 < fields.len() {
                index += 1;
                let destination = fields[index];
                if !destination.is_empty() {
                    path = PathBuf::from(String::from_utf8_lossy(destination).into_owned());
                }
            }

            files.push(GitFileChange {
                path,
                index_status,
                worktree_status,
                untracked: index_status == '?' && worktree_status == '?',
            });
            index += 1;
        }

        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }

    fn diff_hunks(&self, path: &Path, stage: GitHunkStage) -> Result<Vec<GitHunk>> {
        let path_string = path_arg(path);
        let args = match stage {
            GitHunkStage::Unstaged => vec![
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--binary",
                "--unified=3",
                "--",
                path_string.as_str(),
            ],
            GitHunkStage::Staged => vec![
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-color",
                "--binary",
                "--unified=3",
                "--",
                path_string.as_str(),
            ],
        };
        let output = run_git(&self.root, &args)?;
        if !output.status.success() {
            bail!(
                "git diff failed for {}: {}",
                path.display(),
                stderr_text(&output)
            );
        }

        let patch = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(parse_patch_hunks(path, stage, &patch))
    }

    fn line_changes_for_path(&self, path: &Path) -> Result<HashMap<usize, GitLineChange>> {
        let path_string = path_arg(path);
        let output = run_git(
            &self.root,
            &[
                "diff",
                "HEAD",
                "--no-ext-diff",
                "--no-color",
                "--unified=0",
                "--",
                path_string.as_str(),
            ],
        )?;
        if !output.status.success() {
            return Ok(HashMap::new());
        }

        let patch = String::from_utf8_lossy(&output.stdout);
        let hunks = parse_patch_hunks(path, GitHunkStage::Unstaged, &patch);
        let mut changes = HashMap::new();
        for hunk in hunks {
            let change = if hunk.old_count == 0 && hunk.new_count > 0 {
                GitLineChange::Added
            } else if hunk.new_count == 0 {
                GitLineChange::Deleted
            } else {
                GitLineChange::Modified
            };

            if hunk.new_count == 0 {
                changes.insert(hunk.target_row(), change);
            } else {
                for row in hunk.new_start.saturating_sub(1)
                    ..hunk.new_start.saturating_sub(1) + hunk.new_count
                {
                    changes.insert(row, change);
                }
            }
        }
        Ok(changes)
    }

    fn apply_patch(&self, patch: &str, cached: bool, reverse: bool) -> Result<()> {
        if patch.trim().is_empty() {
            bail!("selected change has no text patch");
        }

        let mut command = git_command(&self.root);
        command.arg("apply");
        if cached {
            command.arg("--cached");
        }
        if reverse {
            command.arg("-R");
        }
        command
            .arg("--whitespace=nowarn")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().context("failed to start git apply")?;
        child
            .stdin
            .as_mut()
            .context("git apply stdin unavailable")?
            .write_all(patch.as_bytes())
            .context("failed to write selected patch to git apply")?;
        let output = child
            .wait_with_output()
            .context("failed to wait for git apply")?;
        if !output.status.success() {
            bail!("git apply failed: {}", stderr_text(&output));
        }
        Ok(())
    }

    fn run_checked(&self, args: &[&str]) -> Result<()> {
        let output = run_git(&self.root, args)?;
        if !output.status.success() {
            bail!("git command failed: {}", stderr_text(&output));
        }
        Ok(())
    }
}

fn parse_patch_hunks(path: &Path, stage: GitHunkStage, patch: &str) -> Vec<GitHunk> {
    if patch.trim().is_empty() {
        return Vec::new();
    }

    let lines: Vec<&str> = patch.split_inclusive('\n').collect();
    let Some(first_hunk) = lines.iter().position(|line| line.starts_with("@@ ")) else {
        return Vec::new();
    };
    let prefix: String = lines[..first_hunk].concat();
    let mut hunks = Vec::new();
    let mut index = first_hunk;

    while index < lines.len() {
        if !lines[index].starts_with("@@ ") {
            index += 1;
            continue;
        }

        let hunk_start = index;
        index += 1;
        while index < lines.len() && !lines[index].starts_with("@@ ") {
            index += 1;
        }

        let hunk_lines = &lines[hunk_start..index];
        let header = hunk_lines[0].trim_end().to_owned();
        let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(&header) else {
            continue;
        };
        let preview = hunk_lines
            .iter()
            .skip(1)
            .find_map(|line| {
                let trimmed = line.trim_end_matches('\n');
                if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
                    Some(trimmed.trim_start_matches('+').trim().to_owned())
                } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
                    Some(trimmed.trim_start_matches('-').trim().to_owned())
                } else {
                    None
                }
            })
            .filter(|preview| !preview.is_empty())
            .unwrap_or_else(|| header.clone());

        hunks.push(GitHunk {
            path: path.to_path_buf(),
            stage,
            old_start,
            old_count,
            new_start,
            new_count,
            header,
            preview,
            patch: format!("{prefix}{}", hunk_lines.concat()),
            synthetic_file: false,
            untracked: false,
        });
    }

    hunks
}

fn first_changed_row(patch: &str, new_start: usize) -> Option<usize> {
    let mut in_hunk = false;
    let mut new_line = new_start;

    for line in patch.lines() {
        if line.starts_with("@@ ") {
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }

        if line.starts_with('+') && !line.starts_with("+++") {
            return Some(new_line.saturating_sub(1));
        }
        if line.starts_with('-') && !line.starts_with("---") {
            return Some(new_line.saturating_sub(1));
        }
        if line.starts_with(' ') {
            new_line = new_line.saturating_add(1);
        }
    }

    None
}

fn parse_hunk_header(header: &str) -> Option<(usize, usize, usize, usize)> {
    let body = header.strip_prefix("@@ ")?;
    let end = body.find(" @@")?;
    let mut ranges = body[..end].split_whitespace();
    let old = ranges.next()?.strip_prefix('-')?;
    let new = ranges.next()?.strip_prefix('+')?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some((old_start, old_count, new_start, new_count))
}

fn parse_range(range: &str) -> Option<(usize, usize)> {
    let mut parts = range.split(',');
    let start = parts.next()?.parse().ok()?;
    let count = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    Some((start, count))
}

fn synthetic_untracked_hunk(root: &Path, path: &Path) -> Result<Option<GitHunk>> {
    let absolute = root.join(path);
    if !absolute.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&absolute).unwrap_or_default();
    let line_count = text.lines().count().max(1);
    let preview = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("untracked file")
        .chars()
        .take(120)
        .collect();

    Ok(Some(GitHunk {
        path: path.to_path_buf(),
        stage: GitHunkStage::Unstaged,
        old_start: 0,
        old_count: 0,
        new_start: 1,
        new_count: line_count,
        header: "untracked file".to_owned(),
        preview,
        patch: String::new(),
        synthetic_file: true,
        untracked: true,
    }))
}

fn untracked_line_changes(path: &Path) -> Result<HashMap<usize, GitLineChange>> {
    let mut changes = HashMap::new();
    if !path.is_file() {
        return Ok(changes);
    }
    let text = fs::read_to_string(path).unwrap_or_default();
    for row in 0..text.lines().count().max(1) {
        changes.insert(row, GitLineChange::Added);
    }
    Ok(changes)
}

fn status_preview(file: &GitFileChange) -> String {
    if file.untracked {
        "untracked file".to_owned()
    } else {
        format!(
            "index {} · worktree {}",
            display_status(file.index_status),
            display_status(file.worktree_status)
        )
    }
}

fn display_status(status: char) -> &'static str {
    match status {
        'M' => "modified",
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'U' => "conflict",
        '?' => "untracked",
        _ => "clean",
    }
}

fn stage_rank(stage: GitHunkStage) -> u8 {
    match stage {
        GitHunkStage::Unstaged => 0,
        GitHunkStage::Staged => 1,
    }
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A hint for a network command that failed because Git wanted to ask
/// for a password, passphrase or host key: Mellow never lets it prompt.
pub fn credential_hint(error: &str) -> &'static str {
    const SIGNS: [&str; 5] = [
        "terminal prompts disabled",
        "could not read Username",
        "could not read Password",
        "Permission denied (publickey",
        "Host key verification failed",
    ];
    if SIGNS.iter().any(|sign| error.contains(sign)) {
        " · Git needs a password, key or host confirmation: run it once in the terminal (Ctrl+T) or set up a credential helper / ssh-agent"
    } else {
        ""
    }
}

/// A `git -C cwd` command that is safe to run behind the editor:
///
/// - no optional locks, so the background refresh never takes
///   `.git/index.lock` and fails the user's own `git add` or `git commit`;
/// - no terminal prompts, and no controlling terminal at all, so a
///   credential or SSH passphrase prompt fails with a message instead of
///   drawing over the editor and waiting for input it can never get.
pub fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; a failure leaves the child
        // in the editor's session, which is the old behaviour.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    command
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
    git_command(cwd)
        .args(args)
        .output()
        .context("failed to execute git")
}

fn stderr_text(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("exit status {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(
            dir.path(),
            &["config", "user.email", "mellow@example.invalid"],
        );
        git(dir.path(), &["config", "user.name", "Mellow Tests"]);
        fs::write(
            dir.path().join("demo.txt"),
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
        )
        .unwrap();
        git(dir.path(), &["add", "demo.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);
        dir
    }

    fn settle() {
        std::thread::sleep(REFRESH_SETTLE_TIME * 3);
    }

    /// Audit: with N changed files every refresh ran 3 + 3N git processes,
    /// twice a second (~1 CPU core with 50 files). Unchanged files must be
    /// served from the cache, and any edit, stage or commit must not be.
    #[test]
    fn refresh_reuses_unchanged_files_and_notices_every_change() {
        let dir = repo();
        let repository = GitRepository::discover(dir.path()).unwrap().unwrap();
        let demo = dir.path().join("demo.txt");
        fs::write(
            &demo,
            "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
        )
        .unwrap();
        fs::write(dir.path().join("fresh.txt"), "new\n").unwrap();
        settle();

        let first = repository.refresh().unwrap();
        assert_eq!(repository.files_computed(), 2);
        let again = repository.refresh().unwrap();
        assert_eq!(
            repository.files_computed(),
            2,
            "nothing changed: no diffs rerun"
        );
        assert_eq!(again.hunks, first.hunks);
        assert_eq!(again.line_changes, first.line_changes);

        // Same length, different bytes: still noticed.
        fs::write(
            &demo,
            "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
        )
        .unwrap();
        let edited = repository.refresh().unwrap();
        assert_eq!(
            repository.files_computed(),
            3,
            "only the edited file reruns"
        );
        assert_eq!(
            edited.line_change(Path::new("demo.txt"), 1),
            Some(GitLineChange::Modified)
        );
        assert_eq!(edited.line_change(Path::new("demo.txt"), 0), None);

        // Staging rewrites the index, so every cached diff is stale.
        settle();
        repository.refresh().unwrap();
        let before_stage = repository.files_computed();
        repository.stage_file(&demo).unwrap();
        let staged = repository.refresh().unwrap();
        assert_eq!(repository.files_computed(), before_stage + 2);
        assert!(
            staged.hunks.iter().any(
                |hunk| hunk.path == Path::new("demo.txt") && hunk.stage == GitHunkStage::Staged
            )
        );

        // A commit moves HEAD: the committed file disappears from the list.
        git(dir.path(), &["commit", "-q", "-m", "edit"]);
        let committed = repository.refresh().unwrap();
        assert!(committed.file(Path::new("demo.txt")).is_none());
        assert!(committed.file(Path::new("fresh.txt")).is_some());
    }

    /// Audit: the background `git status` could take `.git/index.lock`
    /// to refresh stat data, making the user's own git commands fail.
    #[test]
    fn refresh_never_rewrites_the_index() {
        let dir = repo();
        let repository = GitRepository::discover(dir.path()).unwrap().unwrap();
        let index = dir.path().join(".git/index");
        let demo = dir.path().join("demo.txt");
        // Same content, new timestamp: plain `git status` would rewrite the
        // index to record the new stat data.
        let content = fs::read(&demo).unwrap();
        settle();
        fs::write(&demo, content).unwrap();
        let before = StatKey::of(&index);
        repository.refresh().unwrap();
        assert_eq!(StatKey::of(&index), before);
    }

    /// Git (and ssh) must never prompt on the editor's terminal.
    #[test]
    fn git_runs_outside_the_editors_terminal_session() {
        let dir = repo();
        let output = git_command(dir.path())
            .args(["-c", "alias.session=!ps -o sid= -p $$", "session"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr_text(&output));
        let child_session: i32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap();
        let own_session = unsafe { libc::getsid(0) };
        assert_ne!(child_session, own_session);

        let command = git_command(dir.path());
        let envs: Vec<_> = command.get_envs().collect();
        for (name, value) in [("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")] {
            assert!(
                envs.contains(&(
                    std::ffi::OsStr::new(name),
                    Some(std::ffi::OsStr::new(value))
                )),
                "{name}"
            );
        }
    }

    #[test]
    fn network_failures_that_wanted_a_prompt_say_how_to_proceed() {
        assert!(
            credential_hint(
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
            )
            .contains("Ctrl+T")
        );
        assert!(
            credential_hint("git@github.com: Permission denied (publickey).").contains("ssh-agent")
        );
        assert_eq!(
            credential_hint("fatal: 'origin' does not appear to be a git repository"),
            ""
        );
    }

    #[test]
    fn file_states_follow_stage_and_unstage_of_whole_files() {
        let dir = repo();
        let repository = GitRepository::discover(dir.path()).unwrap().unwrap();
        let demo = Path::new("demo.txt");
        let fresh = Path::new("fresh.txt");
        fs::write(dir.path().join(demo), "changed\n").unwrap();
        fs::write(dir.path().join(fresh), "new\n").unwrap();

        let state = |name: &Path| {
            let snapshot = repository.refresh().unwrap();
            snapshot.file(name).map(GitFileChange::state)
        };
        assert_eq!(state(demo), Some(GitFileState::Modified));
        assert_eq!(state(fresh), Some(GitFileState::New));

        repository.stage_file(&dir.path().join(demo)).unwrap();
        repository.stage_file(fresh).unwrap();
        assert_eq!(state(demo), Some(GitFileState::Staged));
        assert_eq!(state(fresh), Some(GitFileState::Staged));

        fs::write(dir.path().join(demo), "changed again\n").unwrap();
        assert_eq!(state(demo), Some(GitFileState::PartlyStaged));

        repository.unstage_file(demo).unwrap();
        repository.unstage_file(fresh).unwrap();
        assert_eq!(state(demo), Some(GitFileState::Modified));
        assert_eq!(state(fresh), Some(GitFileState::New));
        assert_eq!(
            fs::read_to_string(dir.path().join(demo)).unwrap(),
            "changed again\n",
            "unstaging must keep working-tree edits"
        );
    }

    #[test]
    fn ahead_behind_counts_commits_against_upstream() {
        let remote = repo();
        let clone_parent = tempdir().unwrap();
        let local = clone_parent.path().join("local");
        let output = Command::new("git")
            .args(["clone", "-q"])
            .arg(remote.path())
            .arg(&local)
            .output()
            .unwrap();
        assert!(output.status.success());
        git(&local, &["config", "user.email", "mellow@example.invalid"]);
        git(&local, &["config", "user.name", "Mellow Tests"]);

        git(&local, &["commit", "-q", "--allow-empty", "-m", "local"]);
        git(remote.path(), &["commit", "-q", "--allow-empty", "-m", "a"]);
        git(remote.path(), &["commit", "-q", "--allow-empty", "-m", "b"]);
        git(&local, &["fetch", "-q"]);

        let snapshot = GitRepository::discover(&local)
            .unwrap()
            .unwrap()
            .refresh()
            .unwrap();
        assert_eq!((snapshot.ahead, snapshot.behind), (1, 2));

        let no_upstream = GitRepository::discover(remote.path())
            .unwrap()
            .unwrap()
            .refresh()
            .unwrap();
        assert_eq!((no_upstream.ahead, no_upstream.behind), (0, 0));
    }

    #[test]
    fn conflict_scan_and_mark_resolved_follow_real_unmerged_index_state() {
        let dir = repo();
        let current = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["branch", "--show-current"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();

        git(dir.path(), &["switch", "-q", "-c", "side"]);
        fs::write(dir.path().join("demo.txt"), "side\n").unwrap();
        git(dir.path(), &["add", "demo.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "side"]);
        git(dir.path(), &["switch", "-q", &current]);
        fs::write(dir.path().join("demo.txt"), "main\n").unwrap();
        git(dir.path(), &["add", "demo.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "main"]);

        let merge = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["merge", "side"])
            .output()
            .unwrap();
        assert!(!merge.status.success());

        let repository = GitRepository::discover(dir.path()).unwrap().unwrap();
        assert_eq!(
            repository.conflict_paths().unwrap(),
            vec![PathBuf::from("demo.txt")]
        );

        fs::write(dir.path().join("demo.txt"), "resolved\n").unwrap();
        repository
            .mark_resolved(&dir.path().join("demo.txt"))
            .unwrap();
        assert!(repository.conflict_paths().unwrap().is_empty());
    }

    #[test]
    fn goal17_branch_rename_delete_and_blame_are_real_git_operations() {
        let dir = repo();
        let repository = GitRepository::discover(dir.path()).unwrap().unwrap();

        repository.create_branch("feature/old").unwrap();
        repository
            .rename_branch("feature/old", "feature/new")
            .unwrap();
        let branches = repository.branches().unwrap();
        assert!(branches.iter().any(|branch| branch.name == "feature/new"));
        assert!(!branches.iter().any(|branch| branch.name == "feature/old"));

        repository.delete_branch("feature/new").unwrap();
        assert!(
            !repository
                .branches()
                .unwrap()
                .iter()
                .any(|branch| branch.name == "feature/new")
        );

        let blame = repository.blame(&dir.path().join("demo.txt")).unwrap();
        assert_eq!(blame.len(), 12);
        assert_eq!(blame[0].row, 0);
        assert!(!blame[0].short_oid.is_empty());
        assert_eq!(blame[0].text, "one");
    }

    #[test]
    fn goal17_fetch_fast_forward_and_push_work_against_configured_remote() {
        let remote_dir = tempdir().unwrap();
        git(remote_dir.path(), &["init", "--bare", "-q"]);

        let local = repo();
        let current = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(local.path())
                .args(["branch", "--show-current"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        git(
            local.path(),
            &[
                "remote",
                "add",
                "origin",
                remote_dir.path().to_string_lossy().as_ref(),
            ],
        );
        git(local.path(), &["push", "-q", "-u", "origin", &current]);

        let peer_dir = tempdir().unwrap();
        let output = Command::new("git")
            .args([
                "clone",
                "-q",
                remote_dir.path().to_string_lossy().as_ref(),
                peer_dir.path().to_string_lossy().as_ref(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        git(
            peer_dir.path(),
            &["config", "user.email", "peer@example.invalid"],
        );
        git(peer_dir.path(), &["config", "user.name", "Peer"]);
        fs::write(peer_dir.path().join("demo.txt"), "remote update\n").unwrap();
        git(peer_dir.path(), &["add", "demo.txt"]);
        git(peer_dir.path(), &["commit", "-q", "-m", "remote update"]);
        git(peer_dir.path(), &["push", "-q"]);

        let repository = GitRepository::discover(local.path()).unwrap().unwrap();
        repository.fetch().unwrap();
        assert_eq!(
            repository.upstream_ref().unwrap(),
            format!("origin/{current}")
        );
        repository.fast_forward_upstream().unwrap();
        assert_eq!(
            fs::read_to_string(local.path().join("demo.txt")).unwrap(),
            "remote update\n"
        );

        fs::write(local.path().join("demo.txt"), "local push\n").unwrap();
        git(local.path(), &["add", "demo.txt"]);
        git(local.path(), &["commit", "-q", "-m", "local push"]);
        repository.push().unwrap();
    }

    #[test]
    fn goal17_commit_branch_and_history_workflows_use_system_git_safely() {
        let dir = repo();
        let repository = GitRepository::discover(dir.path()).unwrap().unwrap();

        fs::write(dir.path().join("demo.txt"), "changed\n").unwrap();
        git(dir.path(), &["add", "demo.txt"]);
        let oid = repository.commit_staged("update demo").unwrap();
        assert!(!oid.is_empty());

        repository.create_branch("feature/demo").unwrap();
        let branches = repository.branches().unwrap();
        assert!(branches.iter().any(|branch| branch.name == "feature/demo"));
        repository.switch_branch("feature/demo").unwrap();
        let branches = repository.branches().unwrap();
        assert!(
            branches
                .iter()
                .any(|branch| branch.name == "feature/demo" && branch.current)
        );

        let history = repository.history(10).unwrap();
        assert!(history.iter().any(|commit| commit.subject == "update demo"));
        assert!(repository.commit_staged("nothing staged").is_err());
    }

    #[test]
    fn goal17_rejects_invalid_branch_names_and_empty_commit_messages() {
        let dir = repo();
        let repository = GitRepository::discover(dir.path()).unwrap().unwrap();
        assert!(repository.create_branch("../escape").is_err());
        assert!(repository.create_branch("").is_err());
        assert!(repository.commit_staged("   ").is_err());
    }

    #[test]
    fn parses_multiple_zero_context_hunks_and_exact_target_rows() {
        let patch = "diff --git a/demo.txt b/demo.txt\n--- a/demo.txt\n+++ b/demo.txt\n@@ -1 +1 @@\n-one\n+ONE\n@@ -3,0 +4,2 @@\n+four\n+five\n";
        let hunks = parse_patch_hunks(Path::new("demo.txt"), GitHunkStage::Unstaged, patch);
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 1));
        assert_eq!((hunks[1].new_start, hunks[1].new_count), (4, 2));
        assert_eq!(hunks[0].target_row(), 0);
        assert_eq!(hunks[1].target_row(), 3);
        assert!(hunks[0].patch.contains("@@ -1 +1 @@"));
        assert!(!hunks[0].patch.contains("@@ -3,0 +4,2 @@"));
    }

    #[test]
    fn hunk_target_row_skips_context_to_first_real_change() {
        let patch = "diff --git a/demo.txt b/demo.txt\n--- a/demo.txt\n+++ b/demo.txt\n@@ -9,4 +9,4 @@\n nine\n ten\n eleven\n-twelve\n+TWELVE\n";
        let hunk = parse_patch_hunks(Path::new("demo.txt"), GitHunkStage::Unstaged, patch)
            .pop()
            .unwrap();

        assert_eq!(hunk.new_start, 9);
        assert_eq!(hunk.target_row(), 11);
    }

    #[test]
    fn refresh_tracks_status_hunks_and_gutter_lines() {
        let dir = repo();
        fs::write(
            dir.path().join("demo.txt"),
            "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\n",
        )
        .unwrap();

        let repo = GitRepository::discover(dir.path()).unwrap().unwrap();
        let snapshot = repo.refresh().unwrap();

        assert_eq!(snapshot.files.len(), 1);
        assert!(!snapshot.hunks.is_empty());
        assert_eq!(
            snapshot.line_change(Path::new("demo.txt"), 0),
            Some(GitLineChange::Modified)
        );
        assert_eq!(
            snapshot.line_change(Path::new("demo.txt"), 12),
            Some(GitLineChange::Added)
        );
    }

    #[test]
    fn stage_and_unstage_selected_hunk_leave_other_hunk_unstaged() {
        let dir = repo();
        fs::write(
            dir.path().join("demo.txt"),
            "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nTWELVE\n",
        )
        .unwrap();

        let repo = GitRepository::discover(dir.path()).unwrap().unwrap();
        let snapshot = repo.refresh().unwrap();
        let first = snapshot
            .hunks
            .iter()
            .find(|hunk| hunk.stage == GitHunkStage::Unstaged)
            .unwrap()
            .clone();

        repo.stage_hunk(&first).unwrap();
        let staged = repo.refresh().unwrap();
        assert_eq!(staged.staged_count(), 1);
        assert_eq!(staged.unstaged_count(), 1);

        let staged_hunk = staged
            .hunks
            .iter()
            .find(|hunk| hunk.stage == GitHunkStage::Staged)
            .unwrap()
            .clone();
        repo.unstage_hunk(&staged_hunk).unwrap();

        let final_snapshot = repo.refresh().unwrap();
        assert_eq!(final_snapshot.staged_count(), 0);
        assert_eq!(final_snapshot.unstaged_count(), 2);
    }

    #[test]
    fn revert_selected_hunk_is_bounded_and_does_not_touch_other_change() {
        let dir = repo();
        fs::write(
            dir.path().join("demo.txt"),
            "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nTWELVE\n",
        )
        .unwrap();

        let repo = GitRepository::discover(dir.path()).unwrap().unwrap();
        let snapshot = repo.refresh().unwrap();
        let first = snapshot
            .hunks
            .iter()
            .find(|hunk| hunk.stage == GitHunkStage::Unstaged)
            .unwrap()
            .clone();

        repo.revert_hunk(&first).unwrap();
        let text = fs::read_to_string(dir.path().join("demo.txt")).unwrap();
        assert!(text.starts_with("one\n"));
        assert!(text.contains("TWELVE"));
    }

    #[test]
    fn untracked_file_can_be_staged_but_is_never_deleted_by_revert() {
        let dir = repo();
        fs::write(dir.path().join("new.txt"), "new file\n").unwrap();

        let repo = GitRepository::discover(dir.path()).unwrap().unwrap();
        let snapshot = repo.refresh().unwrap();
        let untracked = snapshot
            .hunks
            .iter()
            .find(|hunk| hunk.path == Path::new("new.txt"))
            .unwrap()
            .clone();

        assert!(repo.revert_hunk(&untracked).is_err());
        assert!(dir.path().join("new.txt").exists());

        repo.stage_hunk(&untracked).unwrap();
        let staged = repo.refresh().unwrap();
        assert!(staged.hunks.iter().any(|hunk| {
            hunk.path == Path::new("new.txt") && hunk.stage == GitHunkStage::Staged
        }));
    }
}
