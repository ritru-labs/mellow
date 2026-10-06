use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;

const DEFAULT_LIMIT: usize = 5_000;
const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
}

pub fn discover_tree(root: &Path, expanded: &HashSet<PathBuf>) -> Result<Vec<ExplorerEntry>> {
    let mut entries = Vec::new();
    visit_tree(root, root, 0, DEFAULT_LIMIT, expanded, &mut entries)?;
    Ok(entries)
}

fn visit_tree(
    root: &Path,
    dir: &Path,
    depth: usize,
    limit: usize,
    expanded: &HashSet<PathBuf>,
    output: &mut Vec<ExplorerEntry>,
) -> Result<()> {
    if output.len() >= limit || depth > MAX_DEPTH {
        return Ok(());
    }

    let Some(entries) = read_dir_entries(dir, depth)? else {
        return Ok(());
    };
    let mut entries = entries;
    entries.sort_by_key(|entry| {
        (
            !entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false),
            entry.file_name(),
        )
    });

    for entry in entries {
        if output.len() >= limit {
            break;
        }

        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if should_skip_name(&name) {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };

        if file_type.is_dir() {
            let relative = relative.to_path_buf();
            output.push(ExplorerEntry {
                path: relative.clone(),
                depth,
                is_dir: true,
            });
            if expanded.contains(&relative) {
                visit_tree(root, &path, depth + 1, limit, expanded, output)?;
            }
        } else if file_type.is_file() {
            output.push(ExplorerEntry {
                path: relative.to_path_buf(),
                depth,
                is_dir: false,
            });
        }
    }

    Ok(())
}

pub fn discover_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(discover_file_list(root)?.files)
}

/// Project files for search and Quick Open, and whether the list is cut
/// short.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileList {
    pub files: Vec<PathBuf>,
    pub truncated: bool,
}

/// Lists project files. Inside a Git repository this honours .gitignore
/// (tracked plus untracked, non-ignored files); elsewhere it walks the tree
/// skipping heavy internal folders.
pub fn discover_file_list(root: &Path) -> Result<FileList> {
    if let Some(list) = git_file_list(root, DEFAULT_LIMIT) {
        return Ok(list);
    }
    let mut files = discover_files_with_limit(root, DEFAULT_LIMIT + 1)?;
    let truncated = files.len() > DEFAULT_LIMIT;
    files.truncate(DEFAULT_LIMIT);
    Ok(FileList { files, truncated })
}

fn git_file_list(root: &Path, limit: usize) -> Option<FileList> {
    let output = crate::git::git_command(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut files: Vec<PathBuf> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).into_owned()))
        // Deleted-but-tracked files are still listed by --cached.
        .filter(|path| root.join(path).is_file())
        .collect();
    files.sort();
    files.dedup();
    let truncated = files.len() > limit;
    files.truncate(limit);
    Some(FileList { files, truncated })
}

pub fn discover_files_with_limit(root: &Path, limit: usize) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    visit(root, root, 0, limit, &mut files)?;
    files.sort();
    Ok(files)
}

fn visit(
    root: &Path,
    dir: &Path,
    depth: usize,
    limit: usize,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    if files.len() >= limit || depth > MAX_DEPTH {
        return Ok(());
    }

    let Some(mut entries) = read_dir_entries(dir, depth)? else {
        return Ok(());
    };
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if files.len() >= limit {
            break;
        }

        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if should_skip_name(&name) {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            visit(root, &path, depth + 1, limit, files)?;
        } else if file_type.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            files.push(relative.to_path_buf());
        }
    }

    Ok(())
}

/// A directory's entries. Below the project root an unreadable folder
/// (`/etc/ssl/private`, another user's home) is skipped rather than failing
/// the whole walk, which used to make Quick Open and project search report
/// "Permission denied" for the entire project.
fn read_dir_entries(dir: &Path, depth: usize) -> Result<Option<Vec<fs::DirEntry>>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(Some(entries.filter_map(Result::ok).collect())),
        Err(_) if depth > 0 => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn should_skip_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".hg" | ".svn" | "target" | "node_modules" | ".venv" | "venv" | "__pycache__"
    )
}

pub fn path_completions(root: &Path, input: &str, limit: usize) -> Result<Vec<String>> {
    path_completions_with_home(
        root,
        input,
        limit,
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

fn path_completions_with_home(
    root: &Path,
    input: &str,
    limit: usize,
    home: Option<&Path>,
) -> Result<Vec<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    if trimmed == "~" {
        return Ok(if home.is_some() {
            vec!["~/".to_owned()]
        } else {
            Vec::new()
        });
    }
    // Preserve the user's path spelling, including ./, ../, absolute and ~/.
    // This is deliberate file navigation, independent of bounded project scans.
    let (parent_text, prefix) = match trimmed.rfind(std::path::MAIN_SEPARATOR) {
        Some(index) => (&trimmed[..=index], &trimmed[index + 1..]),
        None => ("", trimmed),
    };
    let parent = Path::new(parent_text);
    if parent.components().any(|component| {
        matches!(
            component,
            std::path::Component::Normal(name)
                if should_skip_name(&name.to_string_lossy())
        )
    }) {
        return Ok(Vec::new());
    }

    let directory = if let Some(rest) = parent_text.strip_prefix("~/") {
        let Some(home) = home else {
            return Ok(Vec::new());
        };
        home.join(rest)
    } else {
        root.join(parent)
    };
    if !directory.is_dir() {
        return Ok(Vec::new());
    }

    let prefix_lower = prefix.to_ascii_lowercase();
    let mut entries: Vec<_> = fs::read_dir(&directory)?
        .take(DEFAULT_LIMIT)
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    let mut output = Vec::new();
    for entry in entries {
        if output.len() >= limit {
            break;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if should_skip_name(&name) || !name.to_ascii_lowercase().starts_with(&prefix_lower) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() && !file_type.is_file() {
            continue;
        }

        let mut display = format!("{parent_text}{name}");
        if file_type.is_dir() {
            display.push(std::path::MAIN_SEPARATOR);
        }
        output.push(display);
    }

    Ok(output)
}

/// Fuzzy Quick Open score: every query word must appear in order in the
/// path (`mnapp` finds `src/main/app.rs`). Higher is better: consecutive
/// letters, word starts and file-name matches score more; long paths a
/// little less. `None` when it does not match.
pub fn fuzzy_score(path: &Path, query: &str) -> Option<i64> {
    let text = path.to_string_lossy().to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let name_start = text
        .rfind('/')
        .map_or(0, |index| text[..=index].chars().count());
    let mut total = 0i64;
    for word in query.to_lowercase().split_whitespace() {
        let mut position = 0usize;
        let mut previous: Option<usize> = None;
        let mut first: Option<usize> = None;
        for needle in word.chars() {
            let found = (position..chars.len()).find(|index| chars[*index] == needle)?;
            first.get_or_insert(found);
            total += 1;
            if previous.is_some_and(|previous| previous + 1 == found) {
                total += 5;
            }
            if found == 0 || matches!(chars[found - 1], '/' | '_' | '-' | '.' | ' ') {
                total += 8;
            }
            if found >= name_start {
                total += 2;
            }
            previous = Some(found);
            position = found + 1;
        }
        if first.is_some_and(|first| first >= name_start) {
            total += 10;
        }
    }
    Some(total * 100 - chars.len() as i64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    /// An unreadable folder inside the project must not hide every other
    /// file. (Root ignores permissions, so there it only checks the walk.)
    #[cfg(unix)]
    #[test]
    fn unreadable_subdirectory_is_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("visible.txt"), "x").unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret.txt"), "x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let files = discover_files_with_limit(dir.path(), 100);
        let mut expanded = HashSet::new();
        expanded.insert(PathBuf::from("locked"));
        let tree = discover_tree(dir.path(), &expanded);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        let files = files.unwrap();
        assert!(files.contains(&PathBuf::from("visible.txt")), "{files:?}");
        let tree = tree.unwrap();
        assert!(
            tree.iter()
                .any(|entry| entry.path == Path::new("visible.txt"))
        );
    }

    #[test]
    fn discovers_project_files_but_skips_heavy_internal_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("README.md"), "# hello").unwrap();
        fs::write(dir.path().join(".git/config"), "hidden").unwrap();
        fs::write(dir.path().join("target/debug/build"), "hidden").unwrap();

        let files = discover_files(dir.path()).unwrap();
        assert_eq!(
            files,
            vec![PathBuf::from("README.md"), PathBuf::from("src/main.rs")]
        );
    }

    #[test]
    fn explorer_tree_only_descends_into_expanded_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn demo() {}").unwrap();
        fs::write(dir.path().join("src/nested/mod.rs"), "").unwrap();

        let collapsed = discover_tree(dir.path(), &HashSet::new()).unwrap();
        assert!(
            collapsed
                .iter()
                .any(|entry| entry.path == Path::new("src") && entry.is_dir)
        );
        assert!(
            !collapsed
                .iter()
                .any(|entry| entry.path == Path::new("src/lib.rs"))
        );

        let mut expanded = HashSet::new();
        expanded.insert(PathBuf::from("src"));
        let first_level = discover_tree(dir.path(), &expanded).unwrap();
        assert!(
            first_level
                .iter()
                .any(|entry| entry.path == Path::new("src/lib.rs"))
        );
        assert!(
            first_level
                .iter()
                .any(|entry| entry.path == Path::new("src/nested") && entry.depth == 1)
        );
        assert!(
            !first_level
                .iter()
                .any(|entry| entry.path == Path::new("src/nested/mod.rs"))
        );

        expanded.insert(PathBuf::from("src/nested"));
        let fully_expanded = discover_tree(dir.path(), &expanded).unwrap();
        assert!(
            fully_expanded
                .iter()
                .any(|entry| entry.path == Path::new("src/nested/mod.rs") && entry.depth == 2)
        );
    }

    #[test]
    fn release_readiness_path_completion_is_bounded_and_skips_internal_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/components")).unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        fs::write(dir.path().join("src/components/mod.rs"), "").unwrap();
        fs::write(dir.path().join("target/debug/build"), "").unwrap();

        let top = path_completions(dir.path(), "s", 20).unwrap();
        assert_eq!(top, vec!["src/"]);

        let src = path_completions(dir.path(), "src/", 20).unwrap();
        assert!(src.contains(&"src/components/".to_owned()));
        assert!(src.contains(&"src/lib.rs".to_owned()));
        assert!(src.contains(&"src/main.rs".to_owned()));

        let prefix = path_completions(dir.path(), "src/m", 20).unwrap();
        assert_eq!(prefix, vec!["src/main.rs"]);

        assert_eq!(path_completions(dir.path(), "src/", 1).unwrap().len(), 1);
        assert!(path_completions(dir.path(), "src/", 0).unwrap().is_empty());
        assert!(
            path_completions(dir.path(), "target/", 20)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn intentional_path_completion_supports_parent_absolute_and_home_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("project/nested");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(dir.path().join("home/docs")).unwrap();
        fs::write(root.join("local.txt"), "").unwrap();
        fs::write(dir.path().join("project/parent.txt"), "").unwrap();
        fs::write(dir.path().join("outer.txt"), "").unwrap();
        fs::write(dir.path().join("home/docs/note.txt"), "").unwrap();
        assert_eq!(
            path_completions(&root, "./lo", 10).unwrap(),
            vec!["./local.txt"]
        );
        assert_eq!(
            path_completions(&root, "../pa", 10).unwrap(),
            vec!["../parent.txt"]
        );
        assert_eq!(
            path_completions(&root, "../../ou", 10).unwrap(),
            vec!["../../outer.txt"]
        );
        let absolute = format!("{}/ou", dir.path().display());
        assert_eq!(
            path_completions(&root, &absolute, 10).unwrap(),
            vec![format!("{}/outer.txt", dir.path().display())]
        );
        let home = dir.path().join("home");
        assert_eq!(
            path_completions_with_home(&root, "~/do", 10, Some(&home)).unwrap(),
            vec!["~/docs/"]
        );
        assert_eq!(
            path_completions_with_home(&root, "~/docs/no", 10, Some(&home)).unwrap(),
            vec!["~/docs/note.txt"]
        );
        assert!(
            path_completions_with_home(&root, "~/do", 10, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn quick_open_query_matches_all_tokens_case_insensitively() {
        let path = Path::new("src/components/EditorState.rs");
        assert!(fuzzy_score(path, "editor state").is_some());
        assert!(fuzzy_score(path, "SRC editor").is_some());
        assert!(fuzzy_score(path, "editor missing").is_none());
    }

    /// Audit: Quick Open matched substrings without ranking.
    #[test]
    fn fuzzy_quick_open_ranks_file_name_and_word_start_matches_first() {
        let score = |path: &str, query: &str| fuzzy_score(Path::new(path), query);
        assert!(
            score("src/main/app.rs", "mnapp").is_some(),
            "letters in order"
        );
        assert!(score("src/main/app.rs", "ppam").is_none(), "order matters");

        let mut paths = vec![
            "docs/application-notes.md",
            "src/legacy/apply_patch.rs",
            "src/app.rs",
            "tests/snapshots/app_layout.snap",
        ];
        paths.sort_by_key(|path| std::cmp::Reverse(score(path, "app")));
        assert_eq!(paths[0], "src/app.rs", "{paths:?}");

        assert!(
            score("src/ui/render.rs", "rend") > score("src/frontend/ui.rs", "rend"),
            "file-name match beats a match in a folder name"
        );
    }

    #[test]
    fn git_file_list_honours_gitignore_and_skips_deleted_files() {
        let dir = tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        fs::write(dir.path().join(".gitignore"), "build/\n*.log\n").unwrap();
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/out.bin"), "x").unwrap();
        fs::write(dir.path().join("debug.log"), "x").unwrap();
        fs::write(dir.path().join("kept.rs"), "x").unwrap();
        fs::write(dir.path().join("gone.rs"), "x").unwrap();
        git(&["add", "kept.rs", "gone.rs"]);
        fs::remove_file(dir.path().join("gone.rs")).unwrap();
        fs::write(dir.path().join("new.rs"), "x").unwrap();

        let list = discover_file_list(dir.path()).unwrap();
        assert_eq!(
            list.files,
            [".gitignore", "kept.rs", "new.rs"].map(PathBuf::from)
        );
        assert!(!list.truncated);
    }
}
