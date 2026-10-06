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
        "theme = {}\nword_wrap = {}\nshow_whitespace = {}\nshow_indent_guides = {}\nexplorer_visible = {}\nauto_completion = {}\ncopy_on_select = {}\n",
        settings.theme_preset.config_name(),
        settings.word_wrap,
        settings.show_whitespace,
        settings.show_indent_guides,
        settings.explorer_visible,
        settings.auto_completion,
        settings.copy_on_select,
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

fn config_base() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn state_base() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Settings, keybindings and the AI setup (`~/.config/mellow`).
pub fn config_root() -> PathBuf {
    config_base().join(crate::brand::DIR_NAME)
}

/// Sessions, recovery journals and first-run state (`~/.local/state/mellow`).
pub fn state_root() -> PathBuf {
    state_base().join(crate::brand::DIR_NAME)
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
    use tempfile::tempdir;

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
