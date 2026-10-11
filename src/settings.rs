use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::theme::ThemePreset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorSettings {
    pub theme_preset: ThemePreset,
    pub word_wrap: bool,
    pub show_whitespace: bool,
    pub show_indent_guides: bool,
    pub explorer_visible: bool,
    pub auto_completion: bool,
    pub copy_on_select: bool,
    /// Run the language's formatter (rustfmt, ruff, shfmt...) when saving.
    pub format_on_save: bool,
    pub history_enabled: bool,
    pub indent_width: usize,
    pub ai_max_requests: u32,
    pub ai_max_output_tokens: u32,
    pub ai_session_token_budget: u64,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            theme_preset: ThemePreset::Dark,
            word_wrap: false,
            show_whitespace: false,
            show_indent_guides: true,
            explorer_visible: false,
            auto_completion: true,
            // Dragging should never silently replace the system clipboard.
            copy_on_select: false,
            // Never reformat someone's files unless they ask for it.
            format_on_save: false,
            history_enabled: true,
            indent_width: 4,
            ai_max_requests: 20,
            ai_max_output_tokens: 4096,
            ai_session_token_budget: 100_000,
        }
    }
}

pub fn settings_path() -> PathBuf {
    if let Some(path) = crate::brand::env_var_os("SETTINGS") {
        return PathBuf::from(path);
    }
    config_root().join("settings.conf")
}

pub fn load() -> Result<EditorSettings> {
    load_from(&settings_path())
}

pub fn load_from(path: &Path) -> Result<EditorSettings> {
    if !path.exists() {
        return Ok(EditorSettings::default());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read settings {}", path.display()))?;
    let mut settings = EditorSettings::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            anyhow::bail!("{}:{} expected key = value", path.display(), index + 1);
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "theme" => {
                let Some(preset) = ThemePreset::from_config_name(value) else {
                    let known: Vec<&str> = ThemePreset::ALL
                        .iter()
                        .map(|preset| preset.config_name())
                        .collect();
                    anyhow::bail!(
                        "{}:{} unknown theme '{}' (choose {})",
                        path.display(),
                        index + 1,
                        value,
                        known.join(", ")
                    );
                };
                settings.theme_preset = preset;
            }
            "word_wrap" => settings.word_wrap = parse_bool(path, index, value)?,
            "show_whitespace" => settings.show_whitespace = parse_bool(path, index, value)?,
            "show_indent_guides" => settings.show_indent_guides = parse_bool(path, index, value)?,
            "explorer_visible" => settings.explorer_visible = parse_bool(path, index, value)?,
            "auto_completion" => settings.auto_completion = parse_bool(path, index, value)?,
            "copy_on_select" => settings.copy_on_select = parse_bool(path, index, value)?,
            "history_enabled" => settings.history_enabled = parse_bool(path, index, value)?,
            "ai_max_requests" => {
                settings.ai_max_requests = value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=1000).contains(n))
                    .context("ai_max_requests must be 1 through 1000")?
            }
            "ai_max_output_tokens" => {
                settings.ai_max_output_tokens = value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=16000).contains(n))
                    .context("ai_max_output_tokens must be 1 through 16000")?
            }
            "ai_session_token_budget" => {
                settings.ai_session_token_budget = value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=10_000_000).contains(n))
                    .context("ai_session_token_budget must be 1 through 10000000")?
            }
            "indent_width" => {
                settings.indent_width = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=8).contains(n))
                    .context("indent_width must be 1 through 8")?;
            }
            "format_on_save" => settings.format_on_save = parse_bool(path, index, value)?,
            _ => anyhow::bail!("{}:{} unknown setting '{}'", path.display(), index + 1, key),
        }
    }
    Ok(settings)
}

pub fn write(settings: EditorSettings) -> Result<PathBuf> {
    let path = settings_path();
    let parent = path.parent().context("settings path has no parent")?;
    fs::create_dir_all(parent)?;
    let text = format!(
        "theme = {}\nword_wrap = {}\nshow_whitespace = {}\nshow_indent_guides = {}\nexplorer_visible = {}\nauto_completion = {}\ncopy_on_select = {}\nformat_on_save = {}\nhistory_enabled = {}\nindent_width = {}\nai_max_requests = {}\nai_max_output_tokens = {}\nai_session_token_budget = {}\n",
        settings.theme_preset.config_name(),
        settings.word_wrap,
        settings.show_whitespace,
        settings.show_indent_guides,
        settings.explorer_visible,
        settings.auto_completion,
        settings.copy_on_select,
        settings.format_on_save,
        settings.history_enabled,
        settings.indent_width,
        settings.ai_max_requests,
        settings.ai_max_output_tokens,
        settings.ai_session_token_budget,
    );
    let temp = parent.join(format!(".settings-{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(path)
}

pub fn onboarding_seen_path() -> PathBuf {
    state_root().join("onboarding-v1.seen")
}

pub fn onboarding_seen() -> bool {
    onboarding_seen_path().is_file()
}

pub fn mark_onboarding_seen() -> Result<()> {
    let path = onboarding_seen_path();
    let parent = path.parent().context("onboarding path has no parent")?;
    fs::create_dir_all(parent)?;
    fs::write(path, b"seen\n")?;
    Ok(())
}

#[cfg(not(test))]
fn config_base() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(not(test))]
fn state_base() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Unit tests never touch the developer's real `~/.config` or
/// `~/.local/state`: each test process gets its own folder under the
/// system temp directory.
#[cfg(test)]
fn test_base() -> PathBuf {
    std::env::temp_dir().join(format!("mellow-unit-tests-{}", std::process::id()))
}

#[cfg(test)]
fn config_base() -> PathBuf {
    test_base().join("config")
}

#[cfg(test)]
fn state_base() -> PathBuf {
    test_base().join("state")
}

/// Settings, keybindings and the AI setup (`~/.config/mellow`).
pub fn config_root() -> PathBuf {
    config_base().join(crate::brand::DIR_NAME)
}

/// Sessions, recovery journals and first-run state (`~/.local/state/mellow`).
pub fn state_root() -> PathBuf {
    state_base().join(crate::brand::DIR_NAME)
}

/// Resolve indentation from outer to inner EditorConfig files. Unknown keys
/// are left alone; malformed or unsupported glob expressions never match.
pub fn project_indentation(path: &Path) -> (Option<usize>, Option<bool>) {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut configs = Vec::new();
    let mut parent = absolute.parent();
    while let Some(dir) = parent {
        if let Ok(text) = fs::read_to_string(dir.join(".editorconfig")) {
            let root = text
                .lines()
                .take_while(|l| !l.trim().starts_with('['))
                .any(|l| {
                    l.split_once('=')
                        .is_some_and(|(k, v)| k.trim() == "root" && v.trim() == "true")
                });
            configs.push((dir.to_path_buf(), text));
            if root {
                break;
            }
        }
        parent = dir.parent();
    }
    let mut width = None;
    let mut tabs = None;
    for (dir, text) in configs.into_iter().rev() {
        let relative = absolute
            .strip_prefix(&dir)
            .unwrap_or(&absolute)
            .to_string_lossy();
        let mut applies = false;
        for raw in text.lines() {
            let line = raw.trim();
            if line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(pattern) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                applies = editorconfig_matches(pattern, &relative);
                continue;
            }
            if !applies {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                ("indent_style", "tab") => tabs = Some(true),
                ("indent_style", "space") => tabs = Some(false),
                ("indent_style", "unset") => tabs = None,
                ("indent_size" | "tab_width", "unset") => width = None,
                ("indent_size" | "tab_width", value) => {
                    if let Ok(n) = value.parse::<usize>()
                        && (1..=8).contains(&n)
                    {
                        width = Some(n);
                    }
                }
                _ => {}
            }
        }
    }
    (width, tabs)
}

fn editorconfig_matches(pattern: &str, relative: &str) -> bool {
    let target = if pattern.contains('/') {
        relative
    } else {
        relative.rsplit('/').next().unwrap_or(relative)
    };
    let pattern = pattern.trim_start_matches('/');
    let mut re = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                re.push_str(".*");
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => re.push_str("(?:"),
            '}' => re.push(')'),
            ',' => re.push('|'),
            _ => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).is_ok_and(|r| r.is_match(target))
}

fn parse_bool(path: &Path, index: usize, value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => anyhow::bail!("{}:{} expected boolean", path.display(), index + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_tests_keep_state_and_config_out_of_the_real_home_folder() {
        let temp = std::env::temp_dir();
        assert!(
            state_root().starts_with(&temp),
            "{}",
            state_root().display()
        );
        assert!(
            config_root().starts_with(&temp),
            "{}",
            config_root().display()
        );
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            assert!(!state_root().starts_with(home.join(".local")));
            assert!(!config_root().starts_with(home.join(".config")));
        }
    }
    use tempfile::tempdir;

    #[test]
    fn format_on_save_is_off_by_default_and_read_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.conf");
        assert!(!EditorSettings::default().format_on_save);
        fs::write(&path, "format_on_save = true\n").unwrap();
        assert!(load_from(&path).unwrap().format_on_save);
        fs::write(&path, "format_on_save = off\n").unwrap();
        assert!(!load_from(&path).unwrap().format_on_save);
    }

    #[test]
    fn design_lock_copy_on_select_setting_is_optional_and_explicit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.conf");
        fs::write(&path, "copy_on_select = true\n").unwrap();
        assert!(load_from(&path).unwrap().copy_on_select);
        // Off unless the user opts in: a drag must not replace the clipboard.
        fs::write(&path, "auto_completion = false\n").unwrap();
        assert!(!load_from(&path).unwrap().copy_on_select);
    }

    #[test]
    fn settings_round_trip_parser_supports_documented_values() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.conf");
        fs::write(&path, "theme = high-contrast\nword_wrap = true\nshow_whitespace = yes\nshow_indent_guides = false\nexplorer_visible = on\nauto_completion = false\n").unwrap();
        let settings = load_from(&path).unwrap();
        assert_eq!(settings.theme_preset, ThemePreset::HighContrast);
        assert!(settings.word_wrap && settings.show_whitespace && settings.explorer_visible);
        assert!(!settings.show_indent_guides);
        assert!(!settings.auto_completion);
    }

    #[test]
    fn every_theme_is_accepted_by_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.conf");
        for preset in ThemePreset::ALL {
            fs::write(&path, format!("theme = {}\n", preset.config_name())).unwrap();
            assert_eq!(load_from(&path).unwrap().theme_preset, preset);
        }
        fs::write(&path, "theme = Tokyo_Night\n").unwrap();
        assert_eq!(
            load_from(&path).unwrap().theme_preset,
            ThemePreset::TokyoNight
        );
        fs::write(&path, "theme = solarized\n").unwrap();
        let error = load_from(&path).unwrap_err().to_string();
        assert!(error.contains("unknown theme 'solarized'"), "{error}");
        assert!(error.contains("gruvbox-dark"), "{error}");
    }

    #[test]
    fn settings_reject_unknown_keys_and_invalid_values() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.conf");
        fs::write(&path, "mystery = true\n").unwrap();
        assert!(load_from(&path).is_err());
        fs::write(&path, "word_wrap = maybe\n").unwrap();
        assert!(load_from(&path).is_err());
    }
}
