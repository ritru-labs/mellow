#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Save,
    SaveAs,
    NewFile,
    OpenFile,
    CloseTab,
    NextTab,
    PreviousTab,
    ToggleTerminal,
    HideTerminal,
    NewTerminal,
    NextTerminal,
    CloseTerminal,
    CycleTerminalSize,
    CopyTerminalScreen,
    Quit,
    Undo,
    Redo,
    ShowPalette,
    ShowHelp,
    ShowSettings,
    AiIntent,
    AiSetup,
    SelectAll,
    Copy,
    Cut,
    Paste,
    ToggleComment,
    MoveLinesUp,
    MoveLinesDown,
    DuplicateLines,
    CycleTheme,
    ToggleWhitespace,
    ToggleIndentGuides,
    ToggleWordWrap,
    AddCursorAbove,
    AddCursorBelow,
    ClearSecondaryCursors,
    ReloadKeybindings,
    ToggleExplorer,
    SplitEditor,
    FocusNextPane,
    ToggleSplitOrientation,
    CloseSplit,
    TriggerCompletion,
    GoToDefinition,
    ShowHover,
    ShowSignatureHelp,
    FindReferences,
    RenameSymbol,
    FormatDocument,
    ShowCodeActions,
    ShowProblems,
    LanguageServerStatus,
    ToggleIndentStyle,
    RestartLanguageServer,
    ShowChanges,
    RefreshGit,
    StageHunk,
    UnstageHunk,
    RevertHunk,
    GitCommit,
    GitBranches,
    GitHistory,
    GitBlame,
    GitFetch,
    GitPull,
    GitPush,
    GitCancel,
    GitStageFile,
    GitUnstageFile,
    GitConflicts,
    GitMarkResolved,
    KeepOursConflict,
    KeepTheirsConflict,
    KeepBothConflict,
    GoToSymbol,
    FixProblemWithAi,
    AskAiShell,
    CancelSelection,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    Home,
    End,
    SelectHome,
    SelectEnd,
    PageUp,
    PageDown,
    SelectPageUp,
    SelectPageDown,
    Insert(char),
    Newline,
    Backspace,
    Delete,
    Tab,
    Outdent,
    Find,
    FindNext,
    FindPrevious,
    Replace,
    ProjectSearch,
    GoToLine,
    MoveWordLeft,
    MoveWordRight,
    SelectWordLeft,
    SelectWordRight,
    DeleteWordLeft,
    DeleteWordRight,
    DocumentStart,
    DocumentEnd,
    SelectDocumentStart,
    SelectDocumentEnd,
}

impl Command {
    /// Workbench actions that must work whichever pane has focus, because
    /// they do not edit the text behind it.
    pub fn works_from_any_pane(self) -> bool {
        matches!(
            self,
            Self::Save
                | Self::SaveAs
                | Self::NewFile
                | Self::OpenFile
                | Self::CloseTab
                | Self::NextTab
                | Self::PreviousTab
                | Self::ToggleTerminal
                | Self::NewTerminal
                | Self::Quit
                | Self::ShowPalette
                | Self::ShowHelp
                | Self::ShowSettings
                | Self::AiSetup
                | Self::CycleTheme
                | Self::ProjectSearch
                | Self::ShowProblems
                | Self::LanguageServerStatus
                | Self::ShowChanges
                | Self::GitCommit
                | Self::GitBranches
                | Self::FocusNextPane
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyBinding {
    CtrlChar(char),
    CtrlShiftChar(char),
    Function(u8),
    ShiftFunction(u8),
    CtrlTab,
    CtrlShiftTab,
    AltUp,
    AltDown,
    AltShiftDown,
    CtrlAltUp,
    CtrlAltDown,
    AltChar(char),
    CtrlBackslash,
    CtrlSpace,
    CtrlPageDown,
    CtrlPageUp,
}

impl KeyBinding {
    /// Human-readable label, e.g. `Ctrl+Shift+F`.
    pub fn label(self) -> String {
        fn key_name(ch: char) -> String {
            match ch {
                '`' => "`".to_owned(),
                ' ' => "Space".to_owned(),
                other => other.to_ascii_uppercase().to_string(),
            }
        }
        match self {
            Self::CtrlChar(ch) => format!("Ctrl+{}", key_name(ch)),
            Self::CtrlShiftChar(ch) => format!("Ctrl+Shift+{}", key_name(ch)),
            Self::Function(n) => format!("F{n}"),
            Self::ShiftFunction(n) => format!("Shift+F{n}"),
            Self::CtrlTab => "Ctrl+Tab".to_owned(),
            Self::CtrlShiftTab => "Ctrl+Shift+Tab".to_owned(),
            Self::AltUp => "Alt+Up".to_owned(),
            Self::AltDown => "Alt+Down".to_owned(),
            Self::AltShiftDown => "Alt+Shift+Down".to_owned(),
            Self::CtrlAltUp => "Ctrl+Alt+Up".to_owned(),
            Self::CtrlAltDown => "Ctrl+Alt+Down".to_owned(),
            Self::AltChar(ch) => format!("Alt+{}", key_name(ch)),
            Self::CtrlBackslash => "Ctrl+\\".to_owned(),
            Self::CtrlSpace => "Ctrl+Space".to_owned(),
            Self::CtrlPageDown => "Ctrl+PgDn".to_owned(),
            Self::CtrlPageUp => "Ctrl+PgUp".to_owned(),
        }
    }

    /// True when a terminal without the kitty keyboard protocol sends the
    /// same bytes for this chord as for a different, plainer key. Such
    /// bindings only work after Mellow enables enhanced keyboard reporting.
    pub fn needs_enhanced_keyboard(self) -> bool {
        matches!(
            self,
            Self::CtrlShiftChar(_) | Self::CtrlTab | Self::CtrlShiftTab | Self::CtrlChar('`')
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub command: Command,
    pub id: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub help: &'static str,
    pub bindings: &'static [KeyBinding],
}

pub const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        command: Command::Save,
        id: "file.save",
        label: "Save file",
        category: "File",
        help: "Save the current file",
        bindings: &[KeyBinding::CtrlChar('s')],
    },
    CommandSpec {
        command: Command::SaveAs,
        id: "file.save_as",
        label: "Save file as",
        category: "File",
        help: "Save under a new name or folder",
        bindings: &[KeyBinding::CtrlShiftChar('s')],
    },
    CommandSpec {
        command: Command::NewFile,
        id: "file.new",
        label: "New file",
        category: "File",
        help: "Start a new file; you choose where to save it",
        bindings: &[KeyBinding::CtrlChar('n')],
    },
    CommandSpec {
        command: Command::OpenFile,
        id: "file.open",
        label: "Open file",
        category: "File",
        help: "Find and open any file by name or path",
        bindings: &[KeyBinding::CtrlChar('o')],
    },
    CommandSpec {
        command: Command::CloseTab,
        id: "file.close_tab",
        label: "Close active tab",
        category: "File",
        help: "Close the active file safely",
        bindings: &[KeyBinding::CtrlChar('w')],
    },
    CommandSpec {
        command: Command::NextTab,
        id: "workbench.next_tab",
        label: "Next tab",
        category: "Workbench",
        help: "Switch to the next open file",
        bindings: &[KeyBinding::CtrlTab, KeyBinding::CtrlPageDown],
    },
    CommandSpec {
        command: Command::PreviousTab,
        id: "workbench.previous_tab",
        label: "Previous tab",
        category: "Workbench",
        help: "Switch to the previous open file",
        bindings: &[KeyBinding::CtrlShiftTab, KeyBinding::CtrlPageUp],
    },
    CommandSpec {
        command: Command::ToggleTerminal,
        id: "terminal.toggle",
        label: "Terminal: show or hide",
        category: "Terminal",
        help: "Open a shell below the editor; press again to come back",
        bindings: &[KeyBinding::CtrlChar('`'), KeyBinding::CtrlChar('t')],
    },
    CommandSpec {
        command: Command::HideTerminal,
        id: "terminal.hide",
        label: "Terminal: hide panel",
        category: "Terminal",
        help: "Hide the terminal; the shell keeps running",
        bindings: &[],
    },
    CommandSpec {
        command: Command::NewTerminal,
        id: "terminal.new",
        label: "New terminal session",
        category: "Terminal",
        help: "Open another terminal",
        bindings: &[],
    },
    CommandSpec {
        command: Command::NextTerminal,
        id: "terminal.next",
        label: "Next terminal session",
        category: "Terminal",
        help: "Switch to the next terminal",
        bindings: &[],
    },
    CommandSpec {
        command: Command::CycleTerminalSize,
        id: "terminal.resize",
        label: "Resize terminal panel",
        category: "Terminal",
        help: "Cycle the terminal between normal, tall and maximized",
        bindings: &[],
    },
    CommandSpec {
        command: Command::CopyTerminalScreen,
        id: "terminal.copy_screen",
        label: "Copy terminal screen",
        category: "Terminal",
        help: "Copy the text on the terminal screen (scroll back first with Shift+PgUp)",
        bindings: &[],
    },
    CommandSpec {
        command: Command::CloseTerminal,
        id: "terminal.close",
        label: "Close active terminal session",
        category: "Terminal",
        help: "Close the current terminal",
        bindings: &[],
    },
    CommandSpec {
        command: Command::Find,
        id: "search.find",
        label: "Find in file",
        category: "Search",
        help: "Find in file",
        bindings: &[KeyBinding::CtrlChar('f')],
    },
    CommandSpec {
        command: Command::FindNext,
        id: "search.find_next",
        label: "Find next match",
        category: "Search",
        help: "Find next match",
        bindings: &[KeyBinding::Function(3)],
    },
    CommandSpec {
        command: Command::FindPrevious,
        id: "search.find_previous",
        label: "Find previous match",
        category: "Search",
        help: "Find previous match",
        bindings: &[KeyBinding::ShiftFunction(3)],
    },
    CommandSpec {
        command: Command::Replace,
        id: "search.replace",
        label: "Replace in file",
        category: "Search",
        help: "Replace text in this file, with a preview",
        bindings: &[KeyBinding::CtrlChar('h')],
    },
    CommandSpec {
        command: Command::ProjectSearch,
        id: "search.project",
        label: "Search across project",
        category: "Search",
        help: "Search every file in the project",
        bindings: &[KeyBinding::CtrlShiftChar('f')],
    },
    CommandSpec {
        command: Command::MoveWordLeft,
        id: "editor.word_left",
        label: "Move to previous word",
        category: "Edit",
        help: "Ctrl+Left (Alt+Left on macOS terminals)",
        bindings: &[],
    },
    CommandSpec {
        command: Command::MoveWordRight,
        id: "editor.word_right",
        label: "Move to next word end",
        category: "Edit",
        help: "Ctrl+Right (Alt+Right on macOS terminals)",
        bindings: &[],
    },
    CommandSpec {
        command: Command::SelectWordLeft,
        id: "editor.select_word_left",
        label: "Select to previous word",
        category: "Edit",
        help: "Ctrl+Shift+Left",
        bindings: &[],
    },
    CommandSpec {
        command: Command::SelectWordRight,
        id: "editor.select_word_right",
        label: "Select to next word end",
        category: "Edit",
        help: "Ctrl+Shift+Right",
        bindings: &[],
    },
    CommandSpec {
        command: Command::DeleteWordLeft,
        id: "editor.delete_word_left",
        label: "Delete previous word",
        category: "Edit",
        help: "Ctrl+Backspace or Alt+Backspace",
        bindings: &[],
    },
    CommandSpec {
        command: Command::DeleteWordRight,
        id: "editor.delete_word_right",
        label: "Delete next word",
        category: "Edit",
        help: "Ctrl+Delete",
        bindings: &[],
    },
    CommandSpec {
        command: Command::DocumentStart,
        id: "editor.document_start",
        label: "Go to start of file",
        category: "Edit",
        help: "Ctrl+Home",
        bindings: &[],
    },
    CommandSpec {
        command: Command::DocumentEnd,
        id: "editor.document_end",
        label: "Go to end of file",
        category: "Edit",
        help: "Ctrl+End",
        bindings: &[],
    },
    CommandSpec {
        command: Command::SelectDocumentStart,
        id: "editor.select_document_start",
        label: "Select to start of file",
        category: "Edit",
        help: "Ctrl+Shift+Home",
        bindings: &[],
    },
    CommandSpec {
        command: Command::SelectDocumentEnd,
        id: "editor.select_document_end",
        label: "Select to end of file",
        category: "Edit",
        help: "Ctrl+Shift+End",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GoToLine,
        id: "navigation.go_to_line",
        label: "Go to line",
        category: "Navigation",
        help: "Go to line (line:col)",
        bindings: &[KeyBinding::CtrlChar('g')],
    },
    CommandSpec {
        command: Command::SelectAll,
        id: "selection.select_all",
        label: "Select all",
        category: "Selection",
        help: "Select all text",
        bindings: &[KeyBinding::CtrlChar('a')],
    },
    CommandSpec {
        command: Command::Copy,
        id: "edit.copy",
        label: "Copy selection or line",
        category: "Edit",
        help: "Copy selection (or line)",
        bindings: &[KeyBinding::CtrlChar('c')],
    },
    CommandSpec {
        command: Command::Cut,
        id: "edit.cut",
        label: "Cut selection or line",
        category: "Edit",
        help: "Cut selection (or line)",
        bindings: &[KeyBinding::CtrlChar('x')],
    },
    CommandSpec {
        command: Command::Paste,
        id: "edit.paste",
        label: "Paste",
        category: "Edit",
        help: "Paste",
        bindings: &[KeyBinding::CtrlChar('v')],
    },
    CommandSpec {
        command: Command::ToggleComment,
        id: "edit.toggle_comment",
        label: "Toggle line comment",
        category: "Edit",
        help: "Comment or uncomment selected lines",
        bindings: &[KeyBinding::CtrlChar('/')],
    },
    CommandSpec {
        command: Command::MoveLinesUp,
        id: "edit.move_lines_up",
        label: "Move line(s) up",
        category: "Edit",
        help: "Move current or selected lines up",
        bindings: &[KeyBinding::AltUp],
    },
    CommandSpec {
        command: Command::MoveLinesDown,
        id: "edit.move_lines_down",
        label: "Move line(s) down",
        category: "Edit",
        help: "Move current or selected lines down",
        bindings: &[KeyBinding::AltDown],
    },
    CommandSpec {
        command: Command::DuplicateLines,
        id: "edit.duplicate_lines",
        label: "Duplicate line(s)",
        category: "Edit",
        help: "Duplicate current or selected lines",
        bindings: &[KeyBinding::AltShiftDown],
    },
    CommandSpec {
        command: Command::CycleTheme,
        id: "view.cycle_theme",
        label: "Cycle theme",
        category: "View",
        help: "Cycle Dark, Light, High Contrast, Tokyo Night, Catppuccin Mocha and Gruvbox Dark",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ToggleWhitespace,
        id: "view.toggle_whitespace",
        label: "Toggle whitespace",
        category: "View",
        help: "Show or hide spaces and tabs",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ToggleIndentGuides,
        id: "view.toggle_indent_guides",
        label: "Toggle indent guides",
        category: "View",
        help: "Show or hide indentation guides",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ToggleWordWrap,
        id: "view.toggle_word_wrap",
        label: "Toggle word wrap",
        category: "View",
        help: "Wrap long lines to the window width",
        bindings: &[KeyBinding::AltChar('z')],
    },
    CommandSpec {
        command: Command::AddCursorAbove,
        id: "editor.add_cursor_above",
        label: "Add cursor above",
        category: "Editing",
        help: "Add another cursor on the line above",
        bindings: &[KeyBinding::CtrlAltUp],
    },
    CommandSpec {
        command: Command::AddCursorBelow,
        id: "editor.add_cursor_below",
        label: "Add cursor below",
        category: "Editing",
        help: "Add another cursor on the line below",
        bindings: &[KeyBinding::CtrlAltDown],
    },
    CommandSpec {
        command: Command::ClearSecondaryCursors,
        id: "editor.clear_secondary_cursors",
        label: "Clear extra cursors",
        category: "Editing",
        help: "Return to a single primary cursor",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ShowSettings,
        id: "preferences.settings",
        label: "Open settings",
        category: "Preferences",
        help: "Theme, word wrap, AI and other preferences",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ReloadKeybindings,
        id: "preferences.reload_keybindings",
        label: "Reload keybindings",
        category: "Preferences",
        help: "Re-read keybindings.conf after editing it",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ToggleExplorer,
        id: "workbench.toggle_explorer",
        label: "Files: show or hide",
        category: "Workbench",
        help: "Show or hide the project file explorer",
        bindings: &[KeyBinding::CtrlChar('b')],
    },
    CommandSpec {
        command: Command::SplitEditor,
        id: "workbench.split_editor",
        label: "Split editor",
        category: "Workbench",
        help: "Open a second editor pane",
        bindings: &[KeyBinding::CtrlBackslash],
    },
    CommandSpec {
        command: Command::FocusNextPane,
        id: "workbench.focus_next_pane",
        label: "Focus next editor pane",
        category: "Workbench",
        help: "Move focus between split editor panes",
        bindings: &[KeyBinding::Function(6)],
    },
    CommandSpec {
        command: Command::ToggleSplitOrientation,
        id: "workbench.toggle_split_orientation",
        label: "Toggle split orientation",
        category: "Workbench",
        help: "Switch split editors between side-by-side and top/bottom",
        bindings: &[],
    },
    CommandSpec {
        command: Command::CloseSplit,
        id: "workbench.close_split",
        label: "Close split editor",
        category: "Workbench",
        help: "Return to a single editor pane",
        bindings: &[],
    },
    CommandSpec {
        command: Command::TriggerCompletion,
        id: "editor.trigger_completion",
        label: "Trigger completion",
        category: "Code Intelligence",
        help: "Suggest completions at the cursor",
        bindings: &[KeyBinding::CtrlSpace],
    },
    CommandSpec {
        command: Command::GoToDefinition,
        id: "editor.go_to_definition",
        label: "Go to definition",
        category: "Code Intelligence",
        help: "Jump to the definition under the cursor",
        bindings: &[KeyBinding::Function(12)],
    },
    CommandSpec {
        command: Command::ShowHover,
        id: "editor.hover",
        label: "Show hover",
        category: "Code Intelligence",
        help: "Show details about the name under the cursor",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ShowSignatureHelp,
        id: "editor.signature_help",
        label: "Show signature help",
        category: "Code Intelligence",
        help: "Show call signature help at the cursor",
        bindings: &[],
    },
    CommandSpec {
        command: Command::FindReferences,
        id: "editor.references",
        label: "Find references",
        category: "Code Intelligence",
        help: "Find references to the symbol under the cursor",
        bindings: &[],
    },
    CommandSpec {
        command: Command::RenameSymbol,
        id: "editor.rename",
        label: "Rename symbol",
        category: "Code Intelligence",
        help: "Rename everywhere, with a preview first",
        bindings: &[],
    },
    CommandSpec {
        command: Command::FormatDocument,
        id: "editor.format_document",
        label: "Format document",
        category: "Code Intelligence",
        help: "Tidy the formatting, with a preview first",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ShowCodeActions,
        id: "editor.code_actions",
        label: "Show code actions",
        category: "Code Intelligence",
        help: "Quick fixes for the code at the cursor",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ShowProblems,
        id: "view.problems",
        label: "Show problems",
        category: "Code Intelligence",
        help: "Errors and warnings in your files",
        bindings: &[KeyBinding::CtrlShiftChar('m')],
    },
    CommandSpec {
        command: Command::ToggleIndentStyle,
        id: "editor.toggle_indent_style",
        label: "Indent with tabs or spaces (this file)",
        category: "Edit",
        help: "Switch this file between tab and 4-space indentation; Mellow detects it on open",
        bindings: &[],
    },
    CommandSpec {
        command: Command::LanguageServerStatus,
        id: "editor.language_server_status",
        label: "Language server status",
        category: "Code Intelligence",
        help: "Which server this file uses, whether it is installed and running, and how to set it up",
        bindings: &[],
    },
    CommandSpec {
        command: Command::RestartLanguageServer,
        id: "editor.restart_language_server",
        label: "Restart language server",
        category: "Code Intelligence",
        help: "Restart completions and error checking",
        bindings: &[],
    },
    CommandSpec {
        command: Command::ShowChanges,
        id: "git.show_changes",
        label: "Show Git changes",
        category: "Git",
        help: "Review staged and unstaged repository changes",
        bindings: &[KeyBinding::CtrlShiftChar('g')],
    },
    CommandSpec {
        command: Command::RefreshGit,
        id: "git.refresh",
        label: "Refresh Git status",
        category: "Git",
        help: "Refresh repository status and diff hunks",
        bindings: &[],
    },
    CommandSpec {
        command: Command::StageHunk,
        id: "git.stage_hunk",
        label: "Stage selected hunk",
        category: "Git",
        help: "Stage only the selected unstaged hunk",
        bindings: &[],
    },
    CommandSpec {
        command: Command::UnstageHunk,
        id: "git.unstage_hunk",
        label: "Unstage selected hunk",
        category: "Git",
        help: "Remove only the selected staged hunk from the index",
        bindings: &[],
    },
    CommandSpec {
        command: Command::RevertHunk,
        id: "git.revert_hunk",
        label: "Revert selected hunk",
        category: "Git",
        help: "Discard the selected unstaged hunk after confirmation",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitCommit,
        id: "git.commit",
        label: "Commit staged changes",
        category: "Git",
        help: "Create a commit from already staged changes",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitBranches,
        id: "git.branches",
        label: "Manage Git branches",
        category: "Git",
        help: "List, create, or safely switch local branches",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitHistory,
        id: "git.history",
        label: "Show Git history",
        category: "Git",
        help: "Browse recent repository commits",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitBlame,
        id: "git.blame",
        label: "Show who changed each line (blame)",
        category: "Git",
        help: "See who last changed each line",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitFetch,
        id: "git.fetch",
        label: "Fetch Git remotes",
        category: "Git",
        help: "Fetch and prune configured remotes without changing the working tree",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitPull,
        id: "git.pull_ff",
        label: "Pull (fast-forward only)",
        category: "Git",
        help: "Pull the current branch only when Git can fast-forward without a merge",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitPush,
        id: "git.push",
        label: "Push current branch",
        category: "Git",
        help: "Push using normal Git upstream configuration; never force-push",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitCancel,
        id: "git.cancel",
        label: "Cancel Git operation",
        category: "Git",
        help: "Stop the fetch, pull, push or commit running in the background",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitStageFile,
        id: "git.stage_file",
        label: "Stage this file",
        category: "Git",
        help: "git add the whole active file, ready to commit",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitUnstageFile,
        id: "git.unstage_file",
        label: "Unstage this file",
        category: "Git",
        help: "Take the active file out of the next commit; your edits stay",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitConflicts,
        id: "git.conflicts",
        label: "Show Git conflicts",
        category: "Git",
        help: "List unresolved merge conflicts and open them at conflict markers",
        bindings: &[],
    },
    CommandSpec {
        command: Command::AskAiShell,
        id: "ai.shell_command",
        label: "Ask AI for a shell command",
        category: "AI",
        help: "Describe what you want in plain words; the command is typed into the terminal for you to review and run",
        bindings: &[],
    },
    CommandSpec {
        command: Command::FixProblemWithAi,
        id: "ai.fix_problem",
        label: "Fix problem on this line with AI",
        category: "AI",
        help: "Ask AI for a fix to the problem on the cursor line; the fix is shown as a diff to accept or reject",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GoToSymbol,
        id: "navigation.go_to_symbol",
        label: "Go to symbol",
        category: "Navigation",
        help: "Jump to a function, type or other definition in this file",
        bindings: &[KeyBinding::CtrlShiftChar('o')],
    },
    CommandSpec {
        command: Command::KeepOursConflict,
        id: "edit.keep_ours_conflict",
        label: "Keep ours in conflict",
        category: "Git",
        help: "Keep the current branch's side of the merge conflict under the cursor",
        bindings: &[],
    },
    CommandSpec {
        command: Command::KeepTheirsConflict,
        id: "edit.keep_theirs_conflict",
        label: "Keep theirs in conflict",
        category: "Git",
        help: "Keep the incoming side of the merge conflict under the cursor",
        bindings: &[],
    },
    CommandSpec {
        command: Command::KeepBothConflict,
        id: "edit.keep_both_conflict",
        label: "Keep both sides of conflict",
        category: "Git",
        help: "Keep both sides of the merge conflict under the cursor, ours first",
        bindings: &[],
    },
    CommandSpec {
        command: Command::GitMarkResolved,
        id: "git.mark_resolved",
        label: "Mark active conflict resolved",
        category: "Git",
        help: "Stage the active conflict only after saving it without conflict markers",
        bindings: &[],
    },
    CommandSpec {
        command: Command::Undo,
        id: "edit.undo",
        label: "Undo",
        category: "Edit",
        help: "Undo edits",
        bindings: &[KeyBinding::CtrlChar('z')],
    },
    CommandSpec {
        command: Command::Redo,
        id: "edit.redo",
        label: "Redo",
        category: "Edit",
        help: "Redo edits",
        bindings: &[KeyBinding::CtrlChar('y'), KeyBinding::CtrlShiftChar('z')],
    },
    CommandSpec {
        command: Command::AiSetup,
        id: "ai.setup",
        label: "AI: set up or change provider",
        category: "AI",
        help: "Choose Claude, OpenAI or a local model and add your key",
        bindings: &[],
    },
    CommandSpec {
        command: Command::AiIntent,
        id: "ai.intent",
        label: "Ask AI",
        category: "AI",
        help: "Ask AI about your selection or this file",
        bindings: &[KeyBinding::CtrlChar('k')],
    },
    CommandSpec {
        command: Command::ShowHelp,
        id: "help.shortcuts",
        label: "Help",
        category: "Help",
        help: "Everyday keys and tips",
        bindings: &[KeyBinding::Function(1)],
    },
    CommandSpec {
        command: Command::ShowPalette,
        id: "workbench.commands",
        label: "All commands",
        category: "Workbench",
        help: "Search every command",
        bindings: &[KeyBinding::CtrlChar('p')],
    },
    CommandSpec {
        command: Command::Quit,
        id: "app.quit",
        label: "Quit Mellow",
        category: "File",
        help: "Quit; asks first if anything is unsaved",
        bindings: &[KeyBinding::CtrlChar('q')],
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn command_registry_has_unique_ids() {
        let mut ids = HashSet::new();
        for spec in COMMAND_SPECS {
            assert!(ids.insert(spec.id), "duplicate command id: {}", spec.id);
        }
    }

    #[test]
    fn command_registry_exposes_core_actions() {
        let save = COMMAND_SPECS
            .iter()
            .find(|spec| spec.command == Command::Save)
            .expect("Save must be registered");
        assert_eq!(save.id, "file.save");
        assert_eq!(save.bindings[0].label(), "Ctrl+S");

        let palette = COMMAND_SPECS
            .iter()
            .find(|spec| spec.command == Command::ShowPalette)
            .expect("palette must be registered");
        assert_eq!(palette.id, "workbench.commands");
    }

    #[test]
    fn everyday_navigation_works_without_enhanced_keyboard() {
        // These actions are advertised on first launch, so each needs at least
        // one chord that a plain xterm-compatible terminal can deliver.
        for command in [
            Command::ToggleTerminal,
            Command::NextTab,
            Command::PreviousTab,
            Command::Save,
            Command::OpenFile,
            Command::ShowPalette,
            Command::Find,
            Command::AiIntent,
            Command::Quit,
        ] {
            let spec = COMMAND_SPECS
                .iter()
                .find(|spec| spec.command == command)
                .unwrap();
            assert!(
                spec.bindings
                    .iter()
                    .any(|binding| !binding.needs_enhanced_keyboard()),
                "{} has no legacy-safe binding",
                spec.id
            );
        }
    }

    #[test]
    fn binding_labels_are_readable() {
        assert_eq!(KeyBinding::CtrlShiftChar('f').label(), "Ctrl+Shift+F");
        assert_eq!(KeyBinding::CtrlChar('`').label(), "Ctrl+`");
        assert_eq!(KeyBinding::CtrlBackslash.label(), "Ctrl+\\");
        assert_eq!(KeyBinding::CtrlPageDown.label(), "Ctrl+PgDn");
        assert!(KeyBinding::CtrlChar('`').needs_enhanced_keyboard());
        assert!(!KeyBinding::CtrlChar('t').needs_enhanced_keyboard());
    }
}
