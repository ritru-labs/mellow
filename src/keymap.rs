use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::command::{COMMAND_SPECS, Command, KeyBinding};

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct KeymapConfig {
    overrides: HashMap<String, Vec<KeyBinding>>,
    source: Option<PathBuf>,
}

impl KeymapConfig {
    pub fn load() -> Result<Self, String> {
        let path = keymap_path();
        Self::load_from_path(&path)
    }

    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self {
                overrides: HashMap::new(),
                source: Some(path.to_path_buf()),
            });
        }
        let text = fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let mut overrides = HashMap::new();

        let mut preset: Option<(usize, String)> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((id, value)) = line.split_once('=') else {
                return Err(format!(
                    "{}:{} expected 'command.id = binding'",
                    path.display(),
                    index + 1
                ));
            };
            let id = id.trim();
            if id == "preset" {
                preset = Some((index, value.trim().to_owned()));
                continue;
            }
            if !COMMAND_SPECS.iter().any(|spec| spec.id == id) {
                return Err(format!(
                    "{}:{} unknown command id '{id}'",
                    path.display(),
                    index + 1
                ));
            }
            if overrides.contains_key(id) {
                return Err(format!(
                    "{}:{} duplicate override for '{id}'",
                    path.display(),
                    index + 1
                ));
            }
            let value = value.trim();
            let bindings = if value.eq_ignore_ascii_case("none") || value.is_empty() {
                Vec::new()
            } else {
                value
                    .split(',')
                    .map(|binding| {
                        parse_binding(binding.trim()).ok_or_else(|| {
                            format!(
                                "{}:{} unsupported binding '{}'",
                                path.display(),
                                index + 1,
                                binding.trim()
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            overrides.insert(id.to_owned(), bindings);
        }

        let mut merged = match preset {
            None => HashMap::new(),
            Some((line, name)) => preset_bindings(&name)
                .map_err(|error| format!("{}:{} {error}", path.display(), line + 1))?,
        };
        // The user's own lines win over the preset.
        merged.extend(overrides);
        validate_conflicts(&merged)?;
        Ok(Self {
            overrides: merged,
            source: Some(path.to_path_buf()),
        })
    }

    pub fn resolve(&self, key: KeyEvent) -> Option<Command> {
        resolve_impl(key, Some(&self.overrides))
    }

    pub fn bindings_for(&self, spec: &crate::command::CommandSpec) -> &[KeyBinding] {
        self.overrides
            .get(spec.id)
            .map(Vec::as_slice)
            .unwrap_or(spec.bindings)
    }

    /// The shortcut to advertise for `command`: the first binding this
    /// terminal can actually deliver, or `None` when the command is only
    /// reachable from the command palette.
    pub fn shortcut_label(&self, command: Command, enhanced_keyboard: bool) -> Option<String> {
        let spec = COMMAND_SPECS.iter().find(|spec| spec.command == command)?;
        self.bindings_for(spec)
            .iter()
            .find(|binding| enhanced_keyboard || !binding.needs_enhanced_keyboard())
            .map(|binding| binding.label())
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    #[cfg(test)]
    fn overrides(&self) -> &HashMap<String, Vec<KeyBinding>> {
        &self.overrides
    }
}

fn keymap_path() -> PathBuf {
    if let Some(path) = crate::brand::env_var_os("KEYMAP") {
        return PathBuf::from(path);
    }
    crate::settings::config_root().join("keybindings.conf")
}

/// Default bindings for a named preset. `default` keeps Mellow's own keys.
fn preset_bindings(name: &str) -> Result<HashMap<String, Vec<KeyBinding>>, String> {
    let entries: &[(&str, &str)] = match name.to_ascii_lowercase().as_str() {
        "default" => &[],
        // VS Code: Ctrl+P opens files, Ctrl+Shift+P opens commands.
        "vscode" => &[
            ("file.open", "ctrl+p"),
            ("workbench.commands", "ctrl+shift+p"),
        ],
        other => {
            return Err(format!(
                "unknown keymap preset '{other}' (choose default or vscode)"
            ));
        }
    };
    entries
        .iter()
        .map(|(id, binding)| {
            parse_binding(binding)
                .map(|parsed| ((*id).to_owned(), vec![parsed]))
                .ok_or_else(|| format!("preset binding '{binding}' is not supported"))
        })
        .collect()
}

fn parse_binding(input: &str) -> Option<KeyBinding> {
    let normalized = input.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "ctrl+tab" => return Some(KeyBinding::CtrlTab),
        "ctrl+shift+tab" => return Some(KeyBinding::CtrlShiftTab),
        "ctrl+space" => return Some(KeyBinding::CtrlSpace),
        "ctrl+pagedown" | "ctrl+pgdn" => return Some(KeyBinding::CtrlPageDown),
        "ctrl+pageup" | "ctrl+pgup" => return Some(KeyBinding::CtrlPageUp),
        "ctrl+\\" => return Some(KeyBinding::CtrlBackslash),
        "alt+up" => return Some(KeyBinding::AltUp),
        "alt+down" => return Some(KeyBinding::AltDown),
        "alt+shift+down" => return Some(KeyBinding::AltShiftDown),
        "ctrl+alt+up" => return Some(KeyBinding::CtrlAltUp),
        "ctrl+alt+down" => return Some(KeyBinding::CtrlAltDown),
        _ => {}
    }
    if let Some(number) = normalized.strip_prefix("shift+f")
        && let Ok(number) = number.parse::<u8>()
        && (1..=24).contains(&number)
    {
        return Some(KeyBinding::ShiftFunction(number));
    }
    if let Some(number) = normalized.strip_prefix('f')
        && let Ok(number) = number.parse::<u8>()
        && (1..=24).contains(&number)
    {
        return Some(KeyBinding::Function(number));
    }
    for (prefix, make) in [("ctrl+shift+", 0u8), ("ctrl+", 1u8), ("alt+", 2u8)] {
        if let Some(value) = normalized.strip_prefix(prefix) {
            let mut chars = value.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            return Some(match make {
                0 => KeyBinding::CtrlShiftChar(ch),
                1 => KeyBinding::CtrlChar(ch),
                _ => KeyBinding::AltChar(ch),
            });
        }
    }
    None
}

fn validate_conflicts(overrides: &HashMap<String, Vec<KeyBinding>>) -> Result<(), String> {
    let mut seen: HashMap<KeyBinding, &str> = HashMap::new();
    for spec in COMMAND_SPECS {
        let bindings = overrides
            .get(spec.id)
            .map(Vec::as_slice)
            .unwrap_or(spec.bindings);
        for binding in bindings {
            if let Some(previous) = seen.insert(*binding, spec.id)
                && previous != spec.id
            {
                return Err(format!(
                    "keybinding conflict: {previous} and {} use {:?}",
                    spec.id, binding
                ));
            }
        }
    }
    Ok(())
}

fn binding_matches(binding: KeyBinding, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt_or_super = key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::SUPER);

    match binding {
        KeyBinding::CtrlChar(expected) => {
            ctrl && !shift
                && !alt_or_super
                && matches!(key.code, KeyCode::Char(actual)
                    if actual.eq_ignore_ascii_case(&expected)
                        || legacy_control_alias(expected) == Some(actual))
        }
        KeyBinding::CtrlShiftChar(expected) => {
            ctrl && shift
                && !alt_or_super
                && matches!(key.code, KeyCode::Char(actual) if actual.eq_ignore_ascii_case(&expected))
        }
        KeyBinding::Function(expected) => {
            !ctrl && !shift && !alt_or_super && key.code == KeyCode::F(expected)
        }
        KeyBinding::ShiftFunction(expected) => {
            !ctrl && shift && !alt_or_super && key.code == KeyCode::F(expected)
        }
        KeyBinding::CtrlTab => ctrl && !shift && !alt_or_super && key.code == KeyCode::Tab,
        KeyBinding::CtrlShiftTab => {
            ctrl && shift && !alt_or_super && matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
        }
        KeyBinding::AltUp => {
            !ctrl && !shift && key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Up
        }
        KeyBinding::AltDown => {
            !ctrl
                && !shift
                && key.modifiers.contains(KeyModifiers::ALT)
                && key.code == KeyCode::Down
        }
        KeyBinding::AltShiftDown => {
            !ctrl && shift && key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Down
        }
        KeyBinding::CtrlAltUp => {
            ctrl && !shift && key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Up
        }
        KeyBinding::CtrlAltDown => {
            ctrl && !shift && key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Down
        }
        KeyBinding::AltChar(expected) => {
            !ctrl
                && !shift
                && key.modifiers.contains(KeyModifiers::ALT)
                && !key.modifiers.contains(KeyModifiers::SUPER)
                && matches!(key.code, KeyCode::Char(actual) if actual.eq_ignore_ascii_case(&expected))
        }
        KeyBinding::CtrlBackslash => {
            ctrl && !shift
                && !alt_or_super
                && matches!(key.code, KeyCode::Char('\\') | KeyCode::Char('4'))
        }
        KeyBinding::CtrlSpace => {
            ctrl && !shift
                && !alt_or_super
                && matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('\0'))
        }
        KeyBinding::CtrlPageDown => {
            ctrl && !shift && !alt_or_super && key.code == KeyCode::PageDown
        }
        KeyBinding::CtrlPageUp => ctrl && !shift && !alt_or_super && key.code == KeyCode::PageUp,
    }
}

/// Without enhanced keyboard reporting, terminals send C0 control bytes for
/// some punctuation chords and crossterm decodes 0x1C..=0x1F as Ctrl+4..=7.
/// Accept those spellings so the advertised chord still works.
fn legacy_control_alias(expected: char) -> Option<char> {
    match expected {
        '/' | '_' => Some('7'),
        ']' => Some('5'),
        '^' => Some('6'),
        _ => None,
    }
}

fn resolve_registered(
    key: KeyEvent,
    overrides: Option<&HashMap<String, Vec<KeyBinding>>>,
) -> Option<Command> {
    COMMAND_SPECS
        .iter()
        .find(|spec| {
            let bindings = overrides
                .and_then(|overrides| overrides.get(spec.id))
                .map(Vec::as_slice)
                .unwrap_or(spec.bindings);
            bindings
                .iter()
                .copied()
                .any(|binding| binding_matches(binding, key))
        })
        .map(|spec| spec.command)
}

/// Fixed editing keys: Ctrl (or Alt, as macOS terminals send Option) with
/// arrows/Backspace/Delete moves or deletes by word; Ctrl+Home/End jumps to
/// the document edges. Shift extends the selection.
fn word_and_document_motion(key: KeyEvent, shift: bool) -> Option<Command> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if !(ctrl || alt) || (ctrl && alt) {
        return None;
    }
    match (key.code, shift) {
        (KeyCode::Left, false) => Some(Command::MoveWordLeft),
        (KeyCode::Right, false) => Some(Command::MoveWordRight),
        (KeyCode::Left, true) => Some(Command::SelectWordLeft),
        (KeyCode::Right, true) => Some(Command::SelectWordRight),
        (KeyCode::Backspace, false) => Some(Command::DeleteWordLeft),
        (KeyCode::Delete, false) => Some(Command::DeleteWordRight),
        (KeyCode::Home, false) if ctrl => Some(Command::DocumentStart),
        (KeyCode::End, false) if ctrl => Some(Command::DocumentEnd),
        (KeyCode::Home, true) if ctrl => Some(Command::SelectDocumentStart),
        (KeyCode::End, true) if ctrl => Some(Command::SelectDocumentEnd),
        _ => None,
    }
}

fn resolve_impl(
    key: KeyEvent,
    overrides: Option<&HashMap<String, Vec<KeyBinding>>>,
) -> Option<Command> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }

    if let Some(command) = resolve_registered(key, overrides) {
        return Some(command);
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if let Some(command) = word_and_document_motion(key, shift) {
        return Some(command);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }

    if key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }

    if shift {
        return match key.code {
            KeyCode::F(3) => Some(Command::FindPrevious),
            KeyCode::Left => Some(Command::SelectLeft),
            KeyCode::Right => Some(Command::SelectRight),
            KeyCode::Up => Some(Command::SelectUp),
            KeyCode::Down => Some(Command::SelectDown),
            KeyCode::Home => Some(Command::SelectHome),
            KeyCode::End => Some(Command::SelectEnd),
            KeyCode::PageUp => Some(Command::SelectPageUp),
            KeyCode::PageDown => Some(Command::SelectPageDown),
            KeyCode::BackTab => Some(Command::Outdent),
            KeyCode::Tab => Some(Command::Outdent),
            KeyCode::Char(ch) => Some(Command::Insert(ch)),
            _ => None,
        };
    }

    match key.code {
        KeyCode::Esc => Some(Command::CancelSelection),
        KeyCode::F(1) => Some(Command::ShowHelp),
        KeyCode::F(3) => Some(Command::FindNext),
        KeyCode::Left => Some(Command::MoveLeft),
        KeyCode::Right => Some(Command::MoveRight),
        KeyCode::Up => Some(Command::MoveUp),
        KeyCode::Down => Some(Command::MoveDown),
        KeyCode::Home => Some(Command::Home),
        KeyCode::End => Some(Command::End),
        KeyCode::PageUp => Some(Command::PageUp),
        KeyCode::PageDown => Some(Command::PageDown),
        KeyCode::Enter => Some(Command::Newline),
        KeyCode::Backspace => Some(Command::Backspace),
        KeyCode::Delete => Some(Command::Delete),
        KeyCode::Tab => Some(Command::Tab),
        KeyCode::Char(ch) => Some(Command::Insert(ch)),
        _ => None,
    }
}

#[cfg(test)]
pub fn resolve(key: KeyEvent) -> Option<Command> {
    resolve_impl(key, None)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn vscode_preset_moves_file_open_and_commands() {
        let dir = std::env::temp_dir().join(format!("mellow-keymap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys");
        std::fs::write(&path, "preset = vscode\n").unwrap();
        let config = KeymapConfig::load_from_path(&path).unwrap();
        let ctrl = |ch: char| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        let ctrl_shift = |ch: char| {
            KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )
        };
        assert_eq!(config.resolve(ctrl('p')), Some(Command::OpenFile));
        assert_eq!(config.resolve(ctrl_shift('p')), Some(Command::ShowPalette));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn user_lines_override_a_preset_and_unknown_presets_are_rejected() {
        let dir = std::env::temp_dir().join(format!("mellow-keymap-b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys");
        std::fs::write(&path, "preset = vscode\nfile.open = ctrl+o\n").unwrap();
        let config = KeymapConfig::load_from_path(&path).unwrap();
        let ctrl = |ch: char| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        assert_eq!(config.resolve(ctrl('o')), Some(Command::OpenFile));
        assert_eq!(config.resolve(ctrl('p')), None);

        std::fs::write(&path, "preset = emacs\n").unwrap();
        let error = KeymapConfig::load_from_path(&path).unwrap_err();
        assert!(error.contains("unknown keymap preset 'emacs'"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn punctuation_chords_survive_legacy_terminal_encoding() {
        // xterm sends 0x1F for Ctrl+/ and 0x1C for Ctrl+\; crossterm decodes
        // those bytes as Ctrl+7 and Ctrl+4.
        let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        assert_eq!(resolve(ctrl('7')), Some(Command::ToggleComment));
        assert_eq!(resolve(ctrl('/')), Some(Command::ToggleComment));
        assert_eq!(resolve(ctrl('4')), Some(Command::SplitEditor));
        assert_eq!(resolve(ctrl('t')), Some(Command::ToggleTerminal));
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL)),
            Some(Command::NextTab)
        );
    }

    #[test]
    fn advertised_shortcuts_match_terminal_capabilities() {
        let config = KeymapConfig::default();
        assert_eq!(
            config
                .shortcut_label(Command::ToggleTerminal, false)
                .as_deref(),
            Some("Ctrl+T")
        );
        assert_eq!(
            config
                .shortcut_label(Command::ToggleTerminal, true)
                .as_deref(),
            Some("Ctrl+`")
        );
        assert_eq!(
            config.shortcut_label(Command::NextTab, false).as_deref(),
            Some("Ctrl+PgDn")
        );
        assert_eq!(config.shortcut_label(Command::SaveAs, false), None);
        assert_eq!(
            config.shortcut_label(Command::SaveAs, true).as_deref(),
            Some("Ctrl+Shift+S")
        );
    }

    #[test]
    fn goal15_user_keymap_overrides_defaults_and_can_unbind_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.conf");
        std::fs::write(
            &path,
            "ai.intent = none\nfile.save = Ctrl+K\neditor.add_cursor_below = Ctrl+Alt+Down\n",
        )
        .unwrap();
        let config = KeymapConfig::load_from_path(&path).unwrap();
        assert_eq!(
            config.resolve(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(Command::Save)
        );
        assert_eq!(
            config.resolve(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            None
        );
        assert!(config.overrides().contains_key("file.save"));
    }

    #[test]
    fn goal15_user_keymap_rejects_conflicts_and_unknown_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.conf");
        std::fs::write(&path, "file.save = Ctrl+K\n").unwrap();
        assert!(
            KeymapConfig::load_from_path(&path)
                .unwrap_err()
                .contains("conflict")
        );

        std::fs::write(&path, "missing.command = Ctrl+J\n").unwrap();
        assert!(
            KeymapConfig::load_from_path(&path)
                .unwrap_err()
                .contains("unknown command")
        );
    }

    #[test]
    fn familiar_shortcuts_resolve_to_commands() {
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(Command::Save)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Some(Command::Quit)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(Command::ShowPalette)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(Command::Undo)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Some(Command::SelectAll)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Command::Copy)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Command::Cut)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            Some(Command::Paste)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(Command::SelectLeft)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Command::OpenFile)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL)),
            Some(Command::NextTab)
        );
        assert_eq!(
            resolve(KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::PreviousTab)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT)),
            Some(Command::ToggleWordWrap)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            Some(Command::ToggleExplorer)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)),
            Some(Command::SplitEditor)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
            Some(Command::FocusNextPane)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            Some(Command::TriggerCompletion)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE)),
            Some(Command::GoToDefinition)
        );
        assert_eq!(
            resolve(KeyEvent::new(
                KeyCode::Char('m'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::ShowProblems)
        );
        assert_eq!(
            resolve(KeyEvent::new(
                KeyCode::Char('g'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::ShowChanges)
        );
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Some(Command::Replace)
        );
        assert_eq!(
            resolve(KeyEvent::new(
                KeyCode::Char('f'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::ProjectSearch)
        );
    }

    #[test]
    fn f1_opens_help() {
        assert_eq!(
            resolve(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
            Some(Command::ShowHelp)
        );
    }
}
