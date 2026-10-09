use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};

use crate::{
    ai::{self, AiProposal, AiRequest},
    buffer::Buffer,
    command::{COMMAND_SPECS, Command},
    cursor::Cursor,
    git::{
        GitBlameLine, GitBranch, GitCancel, GitCommit, GitHunk, GitHunkStage, GitLineChange,
        GitRepository, GitSnapshot,
    },
    input::TextInput,
    keymap,
    lsp::{
        CompletionItem, DefinitionLocation, DiagnosticSeverity, LanguageService,
        LanguageServiceEvent, LspPosition,
    },
    pty::{self, PtySession},
    recovery::{self, RecoveryKey, RecoveryRecord},
    search::{self, MAX_REPLACE_PREVIEW, ProjectSearchReport, ReplacementMatch, SearchOptions},
    session::{self, SessionDocument, SessionState},
    settings::{self, EditorSettings},
    syntax_tree::{SyntaxDocument, SyntaxSeverity},
    terminal::{self, MellowTerminal},
    theme::{Theme, ThemePreset},
    ui,
    visual::VisualLayout,
    workspace,
};

const TAB_WIDTH: usize = 4;
const MOUSE_SCROLL_LINES: usize = 3;
const LAUNCH_BANNER_DURATION: Duration = Duration::from_secs(6);
/// While typing, a dirty buffer's journal is rewritten at most this often.
const JOURNAL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Editing,
    ConfirmQuit,
    ConfirmCloseTab,
    Palette,
    QuickOpen,
    Help,
    AiPrompt,
    AiWaiting,
    AiReview,
    AiSetup,
    Settings,
    Onboarding,
    Find,
    Replace,
    ProjectSearch,
    GoToLine,
    SaveAs,
    SaveConflict,
    Recovery,
    Completion,
    References,
    RenameInput,
    ConfirmLspEdits,
    CodeActions,
    Problems,
    LanguageStatus,
    ConfirmCloseTerminal,
    Changes,
    ConfirmRevertHunk,
    ExplorerCreate,
    ExplorerCreateDirectory,
    ExplorerRename,
    ConfirmExplorerDelete,
    GitCommitInput,
    GitBranches,
    GitBranchCreate,
    GitBranchRename,
    ConfirmGitBranchDelete,
    GitHistory,
    GitBlame,
    GitConflicts,
    Symbols,
}

#[derive(Debug)]
struct DocumentState {
    buffer: Buffer,
    cursor: Cursor,
    selection_anchor: Option<Cursor>,
    secondary_cursors: Vec<Cursor>,
    rectangular_selection: Option<RectangularSelection>,
    scroll_row: usize,
    scroll_col: usize,
    visual_scroll_row: usize,
    preferred_col: Option<usize>,
    preferred_visual_col: Option<usize>,
    should_scroll_to_cursor: bool,
    recovery_candidate: Option<RecoveryRecord>,
    last_journal_revision: Option<u64>,
}

#[derive(Debug)]
struct TabSlot {
    state: Option<DocumentState>,
}

/// The "Set up AI" form. Field order: provider, model, key, address,
/// suggestions while typing, save.
#[derive(Debug, Clone, Default)]
pub struct AiSetupForm {
    pub provider: usize,
    pub field: usize,
    pub model: TextInput,
    pub api_key: TextInput,
    pub endpoint: TextInput,
    pub inline: bool,
    /// A key is already saved; it is kept unless a new one is typed.
    pub key_on_file: bool,
    pub error: Option<String>,
}

pub const AI_SETUP_FIELDS: usize = 6;

impl AiSetupForm {
    pub fn provider(&self) -> ai::AiProvider {
        ai::AiProvider::ALL[self.provider % ai::AiProvider::ALL.len()]
    }

    fn for_config(config: Option<&ai::AiProviderConfig>) -> Self {
        let mut form = Self::default();
        match config {
            Some(config) => {
                form.provider = ai::AiProvider::ALL
                    .iter()
                    .position(|provider| *provider == config.provider)
                    .unwrap_or(0);
                form.model.set(config.model.as_str());
                form.endpoint.set(config.endpoint.as_str());
                form.inline = config.inline_suggestions;
                form.key_on_file = config.api_key.is_some();
                form.field = 2;
            }
            None => form.apply_provider_defaults(),
        }
        form
    }

    fn apply_provider_defaults(&mut self) {
        let provider = self.provider();
        self.model.set(provider.default_model());
        self.endpoint.set(provider.default_endpoint());
        self.api_key.clear();
        self.key_on_file = false;
        self.error = None;
    }

    pub fn key_from_environment(&self) -> Option<&'static str> {
        self.provider()
            .key_env_var()
            .filter(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
    }
}

/// A typing-suggestion answer, tagged with what it was asked for.
struct GhostReply {
    buffer: u64,
    generation: u64,
    revision: u64,
    cursor: Cursor,
    result: Result<String, String>,
}

/// Where a typing suggestion was requested: buffer identity, revision and
/// cursor. Revision and cursor alone can coincide in two tabs.
type GhostAnchor = (u64, u64, Cursor);

/// A grey typing suggestion anchored at a cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostSuggestion {
    pub buffer: u64,
    pub cursor: Cursor,
    pub revision: u64,
    pub text: String,
}

/// What the status line says about the optional AI assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiStatus {
    NotConfigured,
    Ready,
    Working,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSummary {
    pub label: String,
    pub dirty: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct EditorPaneView<'a> {
    pub buffer: &'a Buffer,
    pub cursor: Cursor,
    pub selection_anchor: Option<Cursor>,
    pub secondary_cursors: &'a [Cursor],
    pub scroll_row: usize,
    pub scroll_col: usize,
    pub visual_scroll_row: usize,
    pub focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitOrientation {
    SideBySide,
    Stacked,
}

impl SplitOrientation {
    fn toggled(self) -> Self {
        match self {
            Self::SideBySide => Self::Stacked,
            Self::Stacked => Self::SideBySide,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SideBySide => "side-by-side",
            Self::Stacked => "top/bottom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RectangularSelection {
    pub anchor_row: usize,
    pub active_row: usize,
    pub anchor_display_col: usize,
    pub active_display_col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemItem {
    pub severity: ProblemSeverity,
    pub source: String,
    pub message: String,
    pub cursor: Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PostSaveAction {
    #[default]
    None,
    Quit,
    CloseTab,
}

/// One row of the language-server change preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspPreviewRow {
    File(String),
    Location(usize),
    Removed(String),
    Added(String),
    Note(String),
}

struct BackgroundGitRefresh {
    /// Found by this refresh when the app did not know the repository yet.
    repository: Option<GitRepository>,
    /// `None` outside a repository.
    snapshot: Option<Result<GitSnapshot, String>>,
}

/// A fetch, pull, push or commit running off the input thread, so network
/// waits and commit hooks never freeze typing.
struct GitOperation {
    kind: GitOperationKind,
    started: Instant,
    cancel: GitCancel,
    receiver: Receiver<Result<GitOperationDone, String>>,
    /// The progress line last shown, replaced only while still on screen.
    progress: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitOperationKind {
    Fetch,
    Pull,
    Push,
    Commit,
}

impl GitOperationKind {
    fn name(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Pull => "pull",
            Self::Push => "push",
            Self::Commit => "commit",
        }
    }

    fn progress(self) -> &'static str {
        match self {
            Self::Fetch => "Fetching",
            Self::Pull => "Pulling",
            Self::Push => "Pushing",
            Self::Commit => "Committing",
        }
    }
}

fn network_failure(prefix: &str, error: &anyhow::Error) -> String {
    let error = error.to_string();
    format!("{prefix}: {error}{}", crate::git::credential_hint(&error))
}

enum GitOperationDone {
    Fetched,
    /// Fetched; the fast-forward itself runs on the input thread so it can
    /// re-check for unsaved edits made while the network was busy.
    PullFetched {
        upstream: String,
    },
    Pushed,
    Committed {
        oid: String,
    },
}

#[derive(Debug, Clone)]
struct WorkspaceEditMember {
    path: PathBuf,
    expected_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceEditHistoryState {
    Applied,
    Undone,
}

#[derive(Debug, Clone)]
struct WorkspaceEditHistory {
    label: String,
    members: Vec<WorkspaceEditMember>,
    state: WorkspaceEditHistoryState,
}

#[derive(Debug, Clone)]
struct AiContextSnapshot {
    revision: u64,
    range: Option<(Cursor, Cursor)>,
    before: String,
    request_context: String,
    label: String,
    path: String,
    language: String,
}

#[derive(Debug, Clone)]
struct CompletionRequestState {
    request_id: u64,
    revision: u64,
    cursor: Cursor,
    path: Option<PathBuf>,
    manual: bool,
}

pub struct App {
    pub buffer: Buffer,
    pub cursor: Cursor,
    pub selection_anchor: Option<Cursor>,
    pub secondary_cursors: Vec<Cursor>,
    pub rectangular_selection: Option<RectangularSelection>,
    pub clipboard: String,
    #[cfg(test)]
    clipboard_reader: Option<fn() -> Option<String>>,
    pub scroll_row: usize,
    pub scroll_col: usize,
    pub visual_scroll_row: usize,
    pub word_wrap: bool,
    pub mode: AppMode,
    pub status: Option<String>,
    keymap_config: keymap::KeymapConfig,
    start_page_dismissed: bool,
    pub theme: Theme,
    pub theme_preset: ThemePreset,
    pub show_whitespace: bool,
    pub show_indent_guides: bool,
    pub auto_completion_enabled: bool,
    pub copy_on_select: bool,
    pub format_on_save: bool,
    /// `MELLOW_FORMAT_<LANGUAGE>` overrides, read once at startup.
    formatter_overrides: std::collections::HashMap<String, String>,
    pub settings_selected: usize,
    pub ai_query: TextInput,
    ai_context: Option<AiContextSnapshot>,
    pub ai_proposal: Option<AiProposal>,
    ai_receiver: Option<Receiver<Result<AiProposal, String>>>,
    commit_message_receiver: Option<Receiver<Result<String, String>>>,
    shell_receiver: Option<Receiver<Result<String, String>>>,
    ai_shell_request: bool,
    /// Periodic Git refresh running off the input thread, so a slow `git`
    /// never delays typing or Escape.
    git_refresh_receiver: Option<Receiver<BackgroundGitRefresh>>,
    /// The one fetch, pull, push or commit allowed to run at a time.
    git_operation: Option<GitOperation>,
    ai_config: Option<ai::AiProviderConfig>,
    pub ai_setup: AiSetupForm,
    pub ai_review_scroll: u16,
    ai_setup_then_ask: bool,
    pub ghost: Option<GhostSuggestion>,
    ghost_receiver: Option<Receiver<GhostReply>>,
    ghost_requested: Option<GhostAnchor>,
    /// Bumped when AI settings change, so replies to an older provider or a
    /// switched-off setting are dropped.
    ghost_generation: u64,
    last_edit_at: Instant,
    /// Set by typing; only typing (not AI edits or cursor moves) invites a
    /// suggestion.
    typed_since_suggestion: bool,
    /// Where the editor cursor was last drawn, for anchoring inline popups.
    pub last_cursor_screen: std::cell::Cell<Option<(u16, u16)>>,
    pub palette_query: TextInput,
    pub palette_selected: usize,
    pub preferred_col: Option<usize>,
    preferred_visual_col: Option<usize>,
    last_editor_width: usize,
    pub should_scroll_to_cursor: bool,
    pub find_query: TextInput,
    pub find_matches: Vec<(usize, usize, usize)>,
    pub find_selected: usize,
    pub replace_query: TextInput,
    pub replace_with: TextInput,
    pub replace_options: SearchOptions,
    pub replace_preview: Vec<ReplacementMatch>,
    pub replace_selected: usize,
    pub replace_active_field: u8,
    pub project_search_query: TextInput,
    pub project_search_options: SearchOptions,
    pub project_search_report: ProjectSearchReport,
    /// The project file list hit its cap, so search did not see every file.
    pub project_search_list_truncated: bool,
    pub project_search_selected: usize,
    project_search_files: Vec<PathBuf>,
    pub goto_query: TextInput,
    pub save_as_query: TextInput,
    pub quick_open_query: TextInput,
    pub quick_open_candidates: Vec<PathBuf>,
    pub quick_open_selected: usize,
    pub symbol_query: TextInput,
    pub symbol_selected: usize,
    recent_files: Vec<PathBuf>,
    pub explorer_visible: bool,
    pub explorer_focused: bool,
    pub explorer_files: Vec<workspace::ExplorerEntry>,
    pub explorer_selected: usize,
    explorer_expanded: HashSet<PathBuf>,
    pub explorer_action_query: TextInput,
    explorer_action_target: Option<PathBuf>,
    pub conflict_reason: Option<String>,
    /// The pending save conflict is a read-only file, not an external edit.
    pub conflict_read_only: bool,
    pub recovery_candidate: Option<RecoveryRecord>,
    pub completion_items: Vec<CompletionItem>,
    pub completion_selected: usize,
    completion_source_items: Vec<CompletionItem>,
    completion_request: Option<CompletionRequestState>,
    auto_completion_due: Option<Instant>,
    auto_completion_trigger: Option<String>,
    pub confirmation_selected: usize,
    pub reference_items: Vec<DefinitionLocation>,
    pub reference_selected: usize,
    pub rename_query: TextInput,
    pub pending_lsp_edits: Vec<crate::lsp::LspTextEdit>,
    pub pending_lsp_label: String,
    /// Readable before/after rows for the pending edits, and how far the
    /// preview is scrolled.
    pub pending_lsp_preview: Vec<LspPreviewRow>,
    pub pending_lsp_scroll: usize,
    workspace_edit_history: Option<WorkspaceEditHistory>,
    pub code_action_items: Vec<crate::lsp::CodeActionItem>,
    pub code_action_selected: usize,
    pub problem_selected: usize,
    pub git_snapshot: GitSnapshot,
    pub changes_selected: usize,
    /// First visible line of the selected change's diff.
    pub changes_diff_scroll: usize,
    pub git_commit_query: TextInput,
    pub git_branches: Vec<GitBranch>,
    pub git_branch_selected: usize,
    pub git_branch_query: TextInput,
    pub git_history: Vec<GitCommit>,
    pub git_history_selected: usize,
    pub git_blame: Vec<GitBlameLine>,
    pub git_blame_selected: usize,
    pub git_conflicts: Vec<PathBuf>,
    pub git_conflict_selected: usize,
    git_branch_action_target: Option<String>,
    git_repository: Option<GitRepository>,
    revert_hunk: Option<GitHunk>,
    last_git_refresh: Instant,
    last_external_scan: Instant,
    syntax_document: Option<SyntaxDocument>,
    language_service: LanguageService,
    language_service_active: bool,
    pub terminal_visible: bool,
    /// Size the terminal panel takes when shown (never `Hidden`).
    terminal_size: ui::TerminalPanel,
    pub terminal_focused: bool,
    pub terminal_status: Option<String>,
    terminal_sessions: Vec<PtySession>,
    active_terminal: usize,
    next_terminal_id: usize,
    tabs: Vec<TabSlot>,
    active_tab: usize,
    primary_pane_tab: usize,
    secondary_pane_tab: Option<usize>,
    active_pane: u8,
    split_ratio: u16,
    split_orientation: SplitOrientation,
    is_resizing_split: bool,
    workspace_root: PathBuf,
    page_rows: usize,
    should_quit: bool,
    post_save_action: PostSaveAction,
    last_journal_revision: Option<u64>,
    /// Crash-recovery journals are written off the input thread.
    journal: recovery::JournalWriter,
    last_journal_sent: Instant,
    is_mouse_dragging: bool,
    last_click_at: Option<Instant>,
    last_click_pos: Option<(u16, u16)>,
    click_count: u8,
    /// Welcome line shown in the status bar after launch, until the first key
    /// or a few seconds pass.
    launch_banner_until: Option<Instant>,
}

impl App {
    pub fn startup(path: Option<PathBuf>) -> Result<Self> {
        let mut app = Self::startup_documents(path)?;
        app.offer_orphan_drafts(recovery::orphan_drafts());
        Ok(app)
    }

    fn startup_documents(path: Option<PathBuf>) -> Result<Self> {
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if let Some(path) = path {
            if path.is_dir() {
                return Self::startup_workspace(std::fs::canonicalize(path)?, true);
            }
            let mut app = Self::new(Buffer::open(Some(path))?);
            app.workspace_root = workspace_root;
            app.finish_startup_without_session();
            return Ok(app);
        }
        Self::startup_workspace(workspace_root, false)
    }

    /// Give every unnamed draft left by a crashed session its own tab with a
    /// recover/discard prompt, so no draft hides another.
    fn offer_orphan_drafts(&mut self, drafts: Vec<RecoveryRecord>) {
        if drafts.is_empty() {
            return;
        }
        let count = drafts.len();
        for record in drafts {
            if self.recovery_candidate.is_none() && self.active_tab_is_pristine() {
                self.recovery_candidate = Some(record);
                continue;
            }
            let mut state = Self::blank_state(Buffer::empty(None));
            state.recovery_candidate = Some(record);
            self.tabs.push(TabSlot { state: Some(state) });
        }
        // Unsaved work comes before the first-run tour.
        if self.recovery_candidate.is_some()
            && matches!(self.mode, AppMode::Editing | AppMode::Onboarding)
        {
            self.mode = AppMode::Recovery;
        }
        if count > 1 {
            self.status = Some(format!(
                "Found {count} unsaved drafts from a crash; each has its own tab"
            ));
        }
    }

    fn startup_workspace(workspace_root: PathBuf, reveal_project: bool) -> Result<Self> {
        let mut app = match session::load(&workspace_root) {
            Ok(Some(state)) => match Self::from_session(workspace_root.clone(), state)? {
                Some(mut app) => {
                    if reveal_project {
                        app.explorer_visible = true;
                        app.reload_explorer();
                    }
                    return Ok(app);
                }
                None => Self::new(Buffer::open(None)?),
            },
            Ok(None) => Self::new(Buffer::open(None)?),
            Err(error) => {
                let mut app = Self::new(Buffer::open(None)?);
                app.status = Some(format!("Session restore skipped: {error}"));
                app
            }
        };
        app.workspace_root = workspace_root;
        if reveal_project {
            app.explorer_visible = true;
        }
        app.finish_startup_without_session();
        Ok(app)
    }

    /// The welcome line for the status bar, while it is still showing.
    pub fn launch_banner(&self) -> Option<String> {
        self.launch_banner_until?;
        Some(format!(
            "Welcome to {} {} · {} · Ctrl+P commands · F1 help",
            crate::brand::PRODUCT,
            env!("CARGO_PKG_VERSION"),
            crate::brand::CREDIT,
        ))
    }

    pub fn project_name(&self) -> String {
        self.workspace_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.workspace_root.display().to_string())
    }

    fn finish_startup_without_session(&mut self) {
        if self.explorer_visible {
            self.reload_explorer();
        }
        if !cfg!(test) && self.mode == AppMode::Editing && !settings::onboarding_seen() {
            self.mode = AppMode::Onboarding;
            self.status = None;
        }
    }

    pub fn new(buffer: Buffer) -> Self {
        let recovery_candidate = Self::recovery_candidate_for(&buffer);
        let mode = if recovery_candidate.is_some() {
            AppMode::Recovery
        } else {
            AppMode::Editing
        };
        let syntax_document = if buffer.reduced_intelligence_mode() {
            None
        } else {
            SyntaxDocument::new(buffer.language(), &buffer.contents(), buffer.revision())
                .ok()
                .flatten()
        };
        let (editor_settings, settings_warning) = if cfg!(test) {
            (EditorSettings::default(), None)
        } else {
            match settings::load() {
                Ok(settings) => (settings, None),
                Err(error) => (
                    EditorSettings::default(),
                    Some(format!("Settings ignored: {error}")),
                ),
            }
        };
        let (keymap_config, keymap_warning) = match keymap::KeymapConfig::load() {
            Ok(config) => (config, None),
            Err(error) => (
                keymap::KeymapConfig::default(),
                Some(format!("Keybindings ignored: {error}")),
            ),
        };

        let theme_preset = editor_settings.theme_preset;
        let warning = match (settings_warning, keymap_warning) {
            (Some(settings), Some(keymap)) => Some(format!("{settings} · {keymap}")),
            (Some(settings), None) => Some(settings),
            (None, Some(keymap)) => Some(keymap),
            (None, None) => None,
        };

        Self {
            buffer,
            cursor: Cursor::new(0, 0),
            selection_anchor: None,
            secondary_cursors: Vec::new(),
            rectangular_selection: None,
            clipboard: String::new(),
            #[cfg(test)]
            clipboard_reader: None,
            scroll_row: 0,
            scroll_col: 0,
            visual_scroll_row: 0,
            word_wrap: editor_settings.word_wrap,
            mode,
            status: warning,
            keymap_config,
            start_page_dismissed: false,
            theme: Theme::for_preset(theme_preset),
            theme_preset,
            show_whitespace: editor_settings.show_whitespace,
            show_indent_guides: editor_settings.show_indent_guides,
            auto_completion_enabled: editor_settings.auto_completion,
            copy_on_select: editor_settings.copy_on_select,
            format_on_save: editor_settings.format_on_save,
            formatter_overrides: crate::format::overrides_from_env(),
            settings_selected: 0,
            ai_query: TextInput::default(),
            ai_context: None,
            ai_proposal: None,
            ai_receiver: None,
            commit_message_receiver: None,
            shell_receiver: None,
            ai_shell_request: false,
            git_refresh_receiver: None,
            git_operation: None,
            ai_config: if cfg!(test) {
                None
            } else {
                ai::AiProviderConfig::load().ok().flatten()
            },
            ai_setup: AiSetupForm::default(),
            ai_review_scroll: 0,
            ai_setup_then_ask: false,
            ghost: None,
            ghost_receiver: None,
            ghost_requested: None,
            ghost_generation: 0,
            last_edit_at: Instant::now(),
            typed_since_suggestion: false,
            last_cursor_screen: std::cell::Cell::new(None),
            palette_query: TextInput::default(),
            palette_selected: 0,
            preferred_col: None,
            preferred_visual_col: None,
            last_editor_width: 1,
            should_scroll_to_cursor: true,
            find_query: TextInput::default(),
            find_matches: Vec::new(),
            find_selected: 0,
            replace_query: TextInput::default(),
            replace_with: TextInput::default(),
            replace_options: SearchOptions::default(),
            replace_preview: Vec::new(),
            replace_selected: 0,
            replace_active_field: 0,
            project_search_query: TextInput::default(),
            project_search_options: SearchOptions::default(),
            project_search_report: ProjectSearchReport::default(),
            project_search_list_truncated: false,
            project_search_selected: 0,
            project_search_files: Vec::new(),
            goto_query: TextInput::default(),
            save_as_query: TextInput::default(),
            quick_open_query: TextInput::default(),
            quick_open_candidates: Vec::new(),
            quick_open_selected: 0,
            recent_files: Vec::new(),
            symbol_query: TextInput::default(),
            symbol_selected: 0,
            explorer_visible: editor_settings.explorer_visible,
            explorer_focused: false,
            explorer_files: Vec::new(),
            explorer_selected: 0,
            explorer_expanded: HashSet::new(),
            explorer_action_query: TextInput::default(),
            explorer_action_target: None,
            conflict_reason: None,
            conflict_read_only: false,
            recovery_candidate,
            completion_items: Vec::new(),
            completion_selected: 0,
            completion_source_items: Vec::new(),
            completion_request: None,
            auto_completion_due: None,
            auto_completion_trigger: None,
            confirmation_selected: 0,
            reference_items: Vec::new(),
            reference_selected: 0,
            rename_query: TextInput::default(),
            pending_lsp_edits: Vec::new(),
            pending_lsp_label: String::new(),
            pending_lsp_preview: Vec::new(),
            pending_lsp_scroll: 0,
            workspace_edit_history: None,
            code_action_items: Vec::new(),
            code_action_selected: 0,
            problem_selected: 0,
            git_snapshot: GitSnapshot::default(),
            changes_selected: 0,
            changes_diff_scroll: 0,
            git_commit_query: TextInput::default(),
            git_branches: Vec::new(),
            git_branch_selected: 0,
            git_branch_query: TextInput::default(),
            git_history: Vec::new(),
            git_history_selected: 0,
            git_blame: Vec::new(),
            git_blame_selected: 0,
            git_conflicts: Vec::new(),
            git_conflict_selected: 0,
            git_branch_action_target: None,
            git_repository: None,
            revert_hunk: None,
            last_git_refresh: Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            last_external_scan: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
            syntax_document,
            language_service: LanguageService::default(),
            language_service_active: false,
            terminal_visible: false,
            terminal_size: ui::TerminalPanel::Normal,
            terminal_focused: false,
            terminal_status: None,
            terminal_sessions: Vec::new(),
            active_terminal: 0,
            next_terminal_id: 1,
            tabs: vec![TabSlot { state: None }],
            active_tab: 0,
            primary_pane_tab: 0,
            secondary_pane_tab: None,
            active_pane: 0,
            split_ratio: 50,
            split_orientation: SplitOrientation::SideBySide,
            is_resizing_split: false,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            page_rows: 12,
            should_quit: false,
            post_save_action: PostSaveAction::None,
            last_journal_revision: None,
            journal: recovery::JournalWriter::start(),
            // Already due: the first edit is journaled at once.
            last_journal_sent: Instant::now()
                .checked_sub(JOURNAL_INTERVAL)
                .unwrap_or_else(Instant::now),
            is_mouse_dragging: false,
            last_click_at: None,
            launch_banner_until: None,
            last_click_pos: None,
            click_count: 0,
        }
    }

    pub fn run(&mut self, terminal: &mut MellowTerminal) -> Result<()> {
        self.language_service_active = true;
        self.restart_language_server(false);
        self.refresh_git_state(true);
        self.launch_banner_until = Some(Instant::now() + LAUNCH_BANNER_DURATION);
        while !self.should_quit {
            if self
                .launch_banner_until
                .is_some_and(|until| Instant::now() >= until)
            {
                self.launch_banner_until = None;
            }
            self.poll_terminal_sessions();
            self.poll_auto_completion();
            self.poll_language_service();
            self.poll_ai_request();
            self.poll_commit_message();
            self.poll_shell_command();
            self.poll_inline_suggestion();
            self.poll_git_operation();
            self.refresh_git_state(false);
            self.poll_external_file_changes();
            self.journal_if_due();
            if terminal::termination_requested() {
                self.sync_recovery_journal();
                self.flush_journal();
                self.sync_session_state();
                self.should_quit = true;
                break;
            }

            let size = terminal.size()?;
            let is_too_small = size.width < 30 || size.height < 8;
            self.resize_active_terminal(size.width, size.height);
            self.keep_cursor_visible(size.width, size.height);
            terminal.draw(|frame| ui::render(frame, self))?;

            let poll_delay = if self.terminal_visible {
                Duration::from_millis(30)
            } else {
                Duration::from_millis(100)
            };
            if !event::poll(poll_delay)? {
                continue;
            }

            match event::read()? {
                Event::Key(key) => {
                    if is_too_small {
                        let quit_key = matches!(key.code, KeyCode::Char('q') | KeyCode::Char('c'))
                            && key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL);
                        if quit_key && !self.any_dirty_tabs() {
                            self.should_quit = true;
                        }
                        continue;
                    }
                    self.handle_key(key);
                    self.journal_if_due();
                }
                Event::Paste(text) => {
                    if is_too_small {
                        continue;
                    }
                    self.handle_paste(&text);
                    self.journal_if_due();
                }
                Event::Mouse(mouse) => {
                    if is_too_small {
                        continue;
                    }
                    self.handle_mouse(mouse, size.width, size.height);
                    self.journal_if_due();
                }
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost => {}
            }
        }
        // Queued clears must land, or the next start would offer stale drafts.
        self.flush_journal();
        self.sync_session_state();
        Ok(())
    }

    pub fn selection_range(&self) -> Option<(Cursor, Cursor)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            None
        } else if anchor.row < self.cursor.row
            || (anchor.row == self.cursor.row && anchor.col < self.cursor.col)
        {
            Some((anchor, self.cursor))
        } else {
            Some((self.cursor, anchor))
        }
    }

    pub fn filtered_palette_entries(&self) -> Vec<usize> {
        let query = self.palette_query.as_str().trim().to_ascii_lowercase();
        COMMAND_SPECS
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                query.is_empty()
                    || entry.label.to_ascii_lowercase().contains(&query)
                    || entry.category.to_ascii_lowercase().contains(&query)
                    || entry.id.to_ascii_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }

        self.should_scroll_to_cursor = true;
        self.launch_banner_until = None;

        if self.mode == AppMode::Editing && self.terminal_focused {
            let command = self.keymap_config.resolve(key);
            // Without enhanced keyboard reporting Ctrl+` arrives as Ctrl+Space,
            // so honor it here: leaving the terminal must always be possible.
            let legacy_backtick = !crate::terminal::keyboard_enhanced()
                && command == Some(Command::TriggerCompletion);
            if command == Some(Command::ToggleTerminal) || legacy_backtick {
                self.execute(Command::ToggleTerminal);
            } else {
                self.handle_terminal_key(key);
            }
            return;
        }

        if self.mode == AppMode::Editing && self.explorer_focused {
            self.handle_explorer_key(key);
            return;
        }

        match self.mode {
            AppMode::ConfirmQuit => self.handle_quit_confirmation(key),
            AppMode::ConfirmCloseTab => self.handle_close_tab_confirmation(key),
            AppMode::ConfirmCloseTerminal => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    self.mode = AppMode::Editing;
                    self.close_active_terminal_now();
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    self.mode = AppMode::Editing;
                    self.terminal_status = Some("Terminal kept open".to_owned());
                }
                _ => {}
            },
            AppMode::Palette => self.handle_palette_key(key),
            AppMode::QuickOpen => self.handle_quick_open_key(key),
            AppMode::Help => self.handle_help_key(key),
            AppMode::AiPrompt => self.handle_ai_prompt_key(key),
            AppMode::AiWaiting => self.handle_ai_waiting_key(key),
            AppMode::AiReview => self.handle_ai_review_key(key),
            AppMode::AiSetup => self.handle_ai_setup_key(key),
            AppMode::Settings => self.handle_settings_key(key),
            AppMode::Onboarding => self.handle_onboarding_key(key),
            AppMode::Find => self.handle_find_key(key),
            AppMode::Replace => self.handle_replace_key(key),
            AppMode::ProjectSearch => self.handle_project_search_key(key),
            AppMode::GoToLine => self.handle_goto_line_key(key),
            AppMode::SaveAs => self.handle_save_as_key(key),
            AppMode::SaveConflict => self.handle_save_conflict_key(key),
            AppMode::Recovery => self.handle_recovery_key(key),
            AppMode::Completion => self.handle_completion_key(key),
            AppMode::References => self.handle_references_key(key),
            AppMode::RenameInput => self.handle_rename_input_key(key),
            AppMode::ConfirmLspEdits => self.handle_lsp_edit_confirmation_key(key),
            AppMode::CodeActions => self.handle_code_actions_key(key),
            AppMode::Problems => self.handle_problems_key(key),
            AppMode::LanguageStatus => match key.code {
                KeyCode::Char('r' | 'R') => {
                    self.mode = AppMode::Editing;
                    self.execute(Command::RestartLanguageServer);
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    self.mode = AppMode::Editing;
                }
                _ => {}
            },
            AppMode::Changes => self.handle_changes_key(key),
            AppMode::ConfirmRevertHunk => self.handle_revert_confirmation_key(key),
            AppMode::ExplorerCreate => self.handle_explorer_create_key(key),
            AppMode::ExplorerCreateDirectory => self.handle_explorer_create_directory_key(key),
            AppMode::ExplorerRename => self.handle_explorer_rename_key(key),
            AppMode::ConfirmExplorerDelete => self.handle_explorer_delete_confirmation(key),
            AppMode::GitCommitInput => self.handle_git_commit_key(key),
            AppMode::GitBranches => self.handle_git_branches_key(key),
            AppMode::GitBranchCreate => self.handle_git_branch_create_key(key),
            AppMode::GitBranchRename => self.handle_git_branch_rename_key(key),
            AppMode::ConfirmGitBranchDelete => self.handle_git_branch_delete_confirmation(key),
            AppMode::GitHistory => self.handle_git_history_key(key),
            AppMode::GitBlame => self.handle_git_blame_key(key),
            AppMode::GitConflicts => self.handle_git_conflicts_key(key),
            AppMode::Symbols => self.handle_symbols_key(key),
            AppMode::Editing => {
                if self.ghost_is_visible() {
                    match key.code {
                        KeyCode::Tab if key.modifiers.is_empty() => {
                            self.accept_ghost();
                            return;
                        }
                        KeyCode::Esc => {
                            self.ghost = None;
                            return;
                        }
                        _ => self.ghost = None,
                    }
                }
                let revision_before = self.buffer.revision();
                if let Some(command) = self.keymap_config.resolve(key) {
                    self.execute(command);
                }
                if self.buffer.revision() != revision_before {
                    self.last_edit_at = Instant::now();
                    self.ghost = None;
                    // Only plain typing counts; shortcuts such as undo do not.
                    self.typed_since_suggestion =
                        matches!(key.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab)
                            && !key.modifiers.intersects(
                                crossterm::event::KeyModifiers::CONTROL
                                    | crossterm::event::KeyModifiers::ALT,
                            );
                }
            }
        }
    }

    fn current_editor_settings(&self) -> EditorSettings {
        EditorSettings {
            theme_preset: self.theme_preset,
            word_wrap: self.word_wrap,
            show_whitespace: self.show_whitespace,
            show_indent_guides: self.show_indent_guides,
            explorer_visible: self.explorer_visible,
            auto_completion: self.auto_completion_enabled,
            copy_on_select: self.copy_on_select,
            format_on_save: self.format_on_save,
        }
    }

    fn persist_editor_settings(&mut self) {
        if cfg!(test) {
            return;
        }
        if let Err(error) = settings::write(self.current_editor_settings()) {
            self.status = Some(format!("Settings save failed: {error}"));
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        const COUNT: usize = 9;
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = Some("Settings closed".to_owned());
            }
            KeyCode::Up => self.settings_selected = self.settings_selected.saturating_sub(1),
            KeyCode::Down => self.settings_selected = (self.settings_selected + 1).min(COUNT - 1),
            KeyCode::Tab => self.settings_selected = (self.settings_selected + 1) % COUNT,
            KeyCode::BackTab => {
                self.settings_selected = (self.settings_selected + COUNT - 1) % COUNT
            }
            KeyCode::Home => self.settings_selected = 0,
            KeyCode::End => self.settings_selected = COUNT - 1,
            KeyCode::Enter | KeyCode::Char(' ') => {
                match self.settings_selected {
                    0 => {
                        self.theme_preset = self.theme_preset.next();
                        self.theme = Theme::for_preset(self.theme_preset);
                    }
                    1 => {
                        self.word_wrap = !self.word_wrap;
                        self.scroll_col = 0;
                        self.visual_scroll_row = 0;
                    }
                    2 => self.show_whitespace = !self.show_whitespace,
                    3 => self.show_indent_guides = !self.show_indent_guides,
                    4 => {
                        self.explorer_visible = !self.explorer_visible;
                        if self.explorer_visible {
                            self.reload_explorer();
                        } else {
                            self.explorer_focused = false;
                        }
                    }
                    5 => {
                        self.auto_completion_enabled = !self.auto_completion_enabled;
                        if !self.auto_completion_enabled {
                            self.dismiss_completion();
                        }
                    }
                    6 => self.copy_on_select = !self.copy_on_select,
                    7 => self.format_on_save = !self.format_on_save,
                    8 => {
                        self.open_ai_setup(false);
                        return;
                    }
                    _ => {}
                }
                self.persist_editor_settings();
            }
            _ => {}
        }
    }

    fn handle_onboarding_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::F(1)) {
            let _ = settings::mark_onboarding_seen();
            self.mode = AppMode::Editing;
            self.status = Some("Tip: F1 shows the everyday keys again".to_owned());
        }
    }

    fn build_ai_context(&self) -> AiContextSnapshot {
        let path = self.buffer.display_path();
        let language = self.buffer.language().to_owned();
        let revision = self.buffer.revision();

        let short_path = self.active_short_path();
        if let Some((start, end)) = self.selection_range() {
            let before = self.buffer.get_text_range(start, end);
            let label = if start.row == end.row {
                format!("line {} of {short_path}", start.row + 1)
            } else {
                format!("lines {}–{} of {short_path}", start.row + 1, end.row + 1)
            };
            return AiContextSnapshot {
                revision,
                range: Some((start, end)),
                request_context: before.clone(),
                before,
                label,
                path,
                language,
            };
        }

        let first_row = self.cursor.row.saturating_sub(20);
        let last_row = self
            .cursor
            .row
            .saturating_add(20)
            .min(self.buffer.line_count().saturating_sub(1));
        let mut request_context = String::new();
        for row in first_row..=last_row {
            request_context.push_str(&format!("{}: {}\n", row + 1, self.buffer.line_text(row)));
        }
        AiContextSnapshot {
            revision,
            range: None,
            before: String::new(),
            request_context,
            label: format!("lines {}–{} of {short_path}", first_row + 1, last_row + 1),
            path,
            language,
        }
    }

    fn open_ai_setup(&mut self, then_ask: bool) {
        self.ai_setup = AiSetupForm::for_config(self.ai_config.as_ref());
        self.ai_setup_then_ask = then_ask;
        self.mode = AppMode::AiSetup;
        self.status = None;
    }

    pub fn ai_config_summary(&self) -> Option<String> {
        self.ai_config.as_ref().map(ai::AiProviderConfig::summary)
    }

    pub fn ai_provider_name(&self) -> &'static str {
        self.ai_config
            .as_ref()
            .map_or("AI", |config| config.provider.short_label())
    }

    fn handle_ai_setup_key(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;
        let form = &mut self.ai_setup;
        form.error = None;
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = Some("AI setup closed · nothing changed".to_owned());
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.remove_ai_setup();
            }
            KeyCode::Up | KeyCode::BackTab => {
                form.field = (form.field + AI_SETUP_FIELDS - 1) % AI_SETUP_FIELDS;
            }
            KeyCode::Down | KeyCode::Tab => form.field = (form.field + 1) % AI_SETUP_FIELDS,
            KeyCode::Enter => {
                if form.field == 4 {
                    form.inline = !form.inline;
                } else {
                    self.save_ai_setup();
                }
            }
            KeyCode::Left | KeyCode::Right if form.field == 0 => {
                let count = ai::AiProvider::ALL.len();
                form.provider = if key.code == KeyCode::Left {
                    (form.provider + count - 1) % count
                } else {
                    (form.provider + 1) % count
                };
                form.apply_provider_defaults();
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if form.field == 4 => {
                form.inline = !form.inline;
            }
            _ => {
                let input = match form.field {
                    1 => &mut form.model,
                    2 => &mut form.api_key,
                    3 => &mut form.endpoint,
                    _ => return,
                };
                match key.code {
                    KeyCode::Left => input.move_left(),
                    KeyCode::Right => input.move_right(),
                    KeyCode::Home => input.home(),
                    KeyCode::End => input.end(),
                    KeyCode::Backspace => {
                        input.backspace();
                    }
                    KeyCode::Delete => {
                        input.delete();
                    }
                    KeyCode::Char(ch)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        input.insert_char(ch);
                    }
                    _ => {}
                }
                if form.field == 2 && !form.api_key.is_empty() {
                    form.key_on_file = false;
                }
            }
        }
    }

    fn ai_setup_paste(&mut self, text: &str) {
        let text = text.trim();
        let form = &mut self.ai_setup;
        match form.field {
            1 => form.model.insert_text(text),
            2 => {
                form.api_key.insert_text(text);
                form.key_on_file = false;
            }
            3 => form.endpoint.insert_text(text),
            _ => {
                // Pasting anywhere else most likely means the API key.
                form.field = 2;
                form.api_key.set(text);
                form.key_on_file = false;
            }
        }
    }

    /// Turns AI off and deletes the saved setup and key.
    fn remove_ai_setup(&mut self) {
        if let Err(error) = ai::remove_config() {
            self.ai_setup.error = Some(format!("Could not remove: {error}"));
            return;
        }
        self.ai_config = None;
        self.ghost = None;
        self.ghost_receiver = None;
        self.ghost_generation += 1;
        self.mode = AppMode::Editing;
        self.status = Some(if crate::brand::env_var_os("AI_ENDPOINT").is_some() {
            "Saved AI setup removed · MELLOW_AI_ENDPOINT is still set in your environment"
                .to_owned()
        } else {
            "AI turned off · saved setup and key removed".to_owned()
        });
    }

    fn save_ai_setup(&mut self) {
        let form = &self.ai_setup;
        let provider = form.provider();
        let endpoint = form.endpoint.as_str().trim().to_owned();
        let model = form.model.as_str().trim().to_owned();
        let typed_key = form.api_key.as_str().trim().to_owned();
        let error = if let Some(problem) = ai::endpoint_problem(&endpoint) {
            Some((3, problem))
        } else if model.is_empty() {
            Some((1, "Type the model name to use"))
        } else if provider.needs_api_key()
            && typed_key.is_empty()
            && !form.key_on_file
            && form.key_from_environment().is_none()
        {
            Some((2, "Paste your API key (it stays on this computer)"))
        } else {
            None
        };
        if let Some((field, message)) = error {
            self.ai_setup.field = field;
            self.ai_setup.error = Some(message.to_owned());
            return;
        }

        let api_key = if !typed_key.is_empty() {
            Some(typed_key)
        } else if form.key_on_file {
            self.ai_config
                .as_ref()
                .and_then(|config| config.api_key.clone())
        } else {
            None
        };
        let config = ai::AiProviderConfig {
            provider,
            endpoint,
            model,
            api_key,
            inline_suggestions: form.inline,
        };
        // Keys from the environment are used at runtime but never written.
        let mut stored = config.clone();
        if form.api_key.is_empty() && !form.key_on_file {
            stored.api_key = None;
        }
        if let Err(error) = stored.save_to(&ai::config_path()) {
            self.ai_setup.error = Some(format!("Could not save: {error}"));
            return;
        }
        let mut config = config;
        if config.api_key.is_none() {
            config.api_key = provider
                .key_env_var()
                .and_then(|name| std::env::var(name).ok())
                .filter(|value| !value.trim().is_empty());
        }
        let summary = config.summary();
        self.ai_config = Some(config);
        self.ghost = None;
        self.ghost_receiver = None;
        self.ghost_generation += 1;
        if self.ai_setup_then_ask {
            self.ai_setup_then_ask = false;
            self.begin_ai_intent();
            self.status = Some(format!("AI ready · {summary}"));
        } else {
            self.mode = AppMode::Editing;
            let ask = self
                .shortcut_label(Command::AiIntent)
                .map(|key| format!(" · {key} to ask"))
                .unwrap_or_default();
            self.status = Some(format!("AI ready · {summary}{ask}"));
        }
    }

    pub fn ghost_is_visible(&self) -> bool {
        self.ghost.as_ref().is_some_and(|ghost| {
            ghost.buffer == self.buffer.identity()
                && ghost.revision == self.buffer.revision()
                && ghost.cursor == self.cursor
                && self.selection_range().is_none()
        })
    }

    fn accept_ghost(&mut self) {
        let Some(ghost) = self.ghost.take() else {
            return;
        };
        let pre_cursor = self.cursor;
        self.buffer.insert_text(&mut self.cursor, &ghost.text);
        self.buffer
            .set_last_history_state(pre_cursor, None, self.cursor, None);
        self.last_edit_at = Instant::now();
        self.typed_since_suggestion = false;
        self.status = Some("AI suggestion inserted · Ctrl+Z to undo".to_owned());
        self.sync_code_intelligence_after_edit();
    }

    /// Asks for a typing suggestion after a short pause at the end of a
    /// line, and shows the answer only if nothing moved in the meantime.
    fn poll_inline_suggestion(&mut self) {
        if let Some(receiver) = self.ghost_receiver.as_ref() {
            match receiver.try_recv() {
                Ok(reply) => {
                    self.ghost_receiver = None;
                    let suggestions_on = self
                        .ai_config
                        .as_ref()
                        .is_some_and(|config| config.inline_suggestions);
                    if let Ok(text) = reply.result
                        && !text.trim().is_empty()
                        && suggestions_on
                        && reply.generation == self.ghost_generation
                        && reply.buffer == self.buffer.identity()
                        && reply.revision == self.buffer.revision()
                        && reply.cursor == self.cursor
                    {
                        self.ghost = Some(GhostSuggestion {
                            buffer: reply.buffer,
                            cursor: reply.cursor,
                            revision: reply.revision,
                            text,
                        });
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.ghost_receiver = None,
            }
        }

        let Some(config) = self
            .ai_config
            .as_ref()
            .filter(|config| config.inline_suggestions)
        else {
            return;
        };
        let revision = self.buffer.revision();
        let quiet = self.last_edit_at.elapsed() >= Duration::from_millis(700);
        let at_line_end = self.cursor.col == self.buffer.grapheme_count(self.cursor.row);
        let line = self.buffer.line_text(self.cursor.row);
        if self.mode != AppMode::Editing
            || self.terminal_focused
            || self.explorer_focused
            || !quiet
            || !at_line_end
            || line.trim().is_empty()
            || self.selection_range().is_some()
            || !self.secondary_cursors.is_empty()
            || self.ghost_receiver.is_some()
            || self.ghost.is_some()
            || self.ghost_requested == Some((self.buffer.identity(), revision, self.cursor))
            || !self.typed_since_suggestion
        {
            return;
        }

        self.typed_since_suggestion = false;
        let buffer = self.buffer.identity();
        self.ghost_requested = Some((buffer, revision, self.cursor));
        let first = self.cursor.row.saturating_sub(40);
        let last = (self.cursor.row + 20).min(self.buffer.line_count().saturating_sub(1));
        let mut before = String::new();
        for row in first..self.cursor.row {
            before.push_str(&self.buffer.line_text(row));
            before.push('\n');
        }
        before.push_str(&line);
        let mut after = String::new();
        for row in self.cursor.row + 1..=last {
            after.push('\n');
            after.push_str(&self.buffer.line_text(row));
        }
        let request = ai::InlineRequest {
            path: self.active_short_path(),
            language: self.buffer.language().to_owned(),
            before,
            after,
        };
        let config = config.clone();
        let cursor = self.cursor;
        let generation = self.ghost_generation;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = ai::complete_inline(&config, &request).map_err(|error| error.to_string());
            let _ = sender.send(GhostReply {
                buffer,
                generation,
                revision,
                cursor,
                result,
            });
        });
        self.ghost_receiver = Some(receiver);
    }

    fn begin_ai_intent(&mut self) {
        self.ai_shell_request = false;
        if self.ai_config.is_none() {
            self.open_ai_setup(true);
            return;
        }
        self.ai_query.clear();
        self.ai_proposal = None;
        self.ai_receiver = None;
        self.ai_context = Some(self.build_ai_context());
        self.mode = AppMode::AiPrompt;
        self.status = None;
    }

    /// "lines 13–16" for a selection, otherwise "this file".
    pub fn ai_subject(&self) -> String {
        match self.ai_context.as_ref().and_then(|context| context.range) {
            Some((start, end)) if start.row == end.row => format!("line {}", start.row + 1),
            Some((start, end)) => format!("lines {}–{}", start.row + 1, end.row + 1),
            None => "this file".to_owned(),
        }
    }

    pub fn ai_context_label(&self) -> &str {
        self.ai_context
            .as_ref()
            .map(|context| context.label.as_str())
            .unwrap_or("No AI context prepared")
    }

    pub fn ai_before_text(&self) -> &str {
        self.ai_context
            .as_ref()
            .map(|context| context.before.as_str())
            .unwrap_or("")
    }

    fn start_ai_request(&mut self) {
        let instruction = self.ai_query.as_str().trim().to_owned();
        if instruction.is_empty() {
            self.status = Some("Type a question, or press Tab for a suggestion".to_owned());
            return;
        }
        if self.ai_shell_request {
            self.start_shell_command(instruction);
            return;
        }

        let Some(config) = self.ai_config.clone() else {
            self.open_ai_setup(true);
            return;
        };
        let Some(context) = self.ai_context.clone() else {
            self.status = Some("AI context is no longer available".to_owned());
            return;
        };

        let request = AiRequest {
            instruction,
            path: context.path,
            language: context.language,
            context: context.request_context,
            allow_replacement: context.range.is_some(),
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = ai::request(&config, &request).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.ai_receiver = Some(receiver);
        self.mode = AppMode::AiWaiting;
        self.status = None;
    }

    fn poll_ai_request(&mut self) {
        let result = self
            .ai_receiver
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("AI request worker disconnected".to_owned()))
                }
            });
        let Some(result) = result else {
            return;
        };
        self.ai_receiver = None;

        match result {
            Ok(proposal) => {
                self.ai_proposal = Some(proposal);
                self.ai_review_scroll = 0;
                self.mode = AppMode::AiReview;
                self.status = None;
            }
            Err(error) => {
                self.mode = AppMode::AiPrompt;
                self.status = Some(format!("AI: {error}"));
            }
        }
    }

    fn handle_ai_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.ai_query.clear();
                self.ai_context = None;
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Left => self.ai_query.move_left(),
            KeyCode::Right => self.ai_query.move_right(),
            KeyCode::Home => self.ai_query.home(),
            KeyCode::End => self.ai_query.end(),
            KeyCode::Backspace => {
                self.ai_query.backspace();
            }
            KeyCode::Delete => {
                self.ai_query.delete();
            }
            KeyCode::Enter => self.start_ai_request(),
            KeyCode::Tab | KeyCode::BackTab => {
                let suggestions = ui::ai_suggestions(
                    self.ai_context
                        .as_ref()
                        .is_some_and(|context| context.range.is_some()),
                );
                let current = suggestions
                    .iter()
                    .position(|suggestion| *suggestion == self.ai_query.as_str());
                let next = match (current, key.code) {
                    (Some(index), KeyCode::BackTab) => {
                        (index + suggestions.len() - 1) % suggestions.len()
                    }
                    (Some(index), _) => (index + 1) % suggestions.len(),
                    (None, _) => 0,
                };
                self.ai_query.set(suggestions[next]);
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.ai_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn handle_ai_waiting_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.ai_receiver = None;
            self.ai_context = None;
            self.mode = AppMode::Editing;
            self.status = Some("AI response dismissed".to_owned());
        }
    }

    fn handle_ai_review_key(&mut self, key: KeyEvent) {
        let read_only = self
            .ai_proposal
            .as_ref()
            .is_some_and(|proposal| proposal.replacement.is_none());
        match key.code {
            KeyCode::Up => {
                self.ai_review_scroll = self.ai_review_scroll.saturating_sub(1);
                return;
            }
            KeyCode::Down => {
                self.ai_review_scroll = self.ai_review_scroll.saturating_add(1);
                return;
            }
            KeyCode::PageUp => {
                self.ai_review_scroll = self.ai_review_scroll.saturating_sub(10);
                return;
            }
            KeyCode::PageDown => {
                self.ai_review_scroll = self.ai_review_scroll.saturating_add(10);
                return;
            }
            KeyCode::Char('c' | 'C') if read_only => {
                if let Some(proposal) = self.ai_proposal.as_ref() {
                    let answer = proposal.summary.clone();
                    self.store_clipboard(answer, "Copied answer");
                }
                return;
            }
            KeyCode::Enter if read_only => {
                self.ai_proposal = None;
                self.ai_context = None;
                self.mode = AppMode::Editing;
                self.status = None;
                return;
            }
            _ => {}
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('r' | 'R') => {
                self.ai_proposal = None;
                self.ai_context = None;
                self.mode = AppMode::Editing;
                self.status = Some("Suggestion rejected · nothing changed".to_owned());
            }
            KeyCode::Enter | KeyCode::Char('a' | 'A') => self.apply_ai_proposal(),
            _ => {}
        }
    }

    fn apply_ai_proposal(&mut self) {
        let Some(proposal) = self.ai_proposal.clone() else {
            self.mode = AppMode::Editing;
            return;
        };
        let Some(context) = self.ai_context.clone() else {
            self.mode = AppMode::Editing;
            self.status = Some("AI proposal expired: context is unavailable".to_owned());
            return;
        };

        let Some(replacement) = proposal.replacement else {
            self.ai_proposal = None;
            self.ai_context = None;
            self.mode = AppMode::Editing;
            self.status = Some(format!("AI: {}", proposal.summary));
            return;
        };
        let Some((start, end)) = context.range else {
            self.mode = AppMode::Editing;
            self.status = Some("AI replacement ignored: no selected edit target".to_owned());
            return;
        };
        if self.buffer.display_path() != context.path
            || self.buffer.revision() != context.revision
            || self.buffer.get_text_range(start, end) != context.before
        {
            self.mode = AppMode::Editing;
            self.ai_proposal = None;
            self.ai_context = None;
            self.status = Some("AI proposal expired because the target buffer changed".to_owned());
            return;
        }

        let pre_cursor = self.cursor;
        let pre_selection_anchor = self.selection_anchor;
        self.buffer
            .replace_range(start, end, &replacement, &mut self.cursor);
        self.selection_anchor = None;
        self.secondary_cursors.clear();
        self.rectangular_selection = None;
        self.buffer.set_last_history_state(
            pre_cursor,
            pre_selection_anchor,
            self.cursor,
            self.selection_anchor,
        );
        self.ai_proposal = None;
        self.ai_context = None;
        self.mode = AppMode::Editing;
        self.status = Some("Suggestion applied · Ctrl+Z to undo".to_owned());
        self.sync_code_intelligence_after_edit();
    }

    fn handle_find_key(&mut self, key: KeyEvent) {
        // Pressing Find again widens the search to every project file. This
        // works in terminals that cannot send Ctrl+Shift+F.
        if matches!(
            self.keymap_config.resolve(key),
            Some(Command::Find | Command::ProjectSearch)
        ) {
            let query = self.find_query.as_str().to_owned();
            self.begin_project_search();
            if self.mode == AppMode::ProjectSearch && !query.is_empty() {
                self.project_search_query.set(&query);
                self.recalculate_project_search();
            }
            return;
        }
        let shift = key
            .modifiers
            .contains(crossterm::event::KeyModifiers::SHIFT);
        let mut query_changed = false;
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Enter if shift => self.find_previous(),
            KeyCode::Enter => self.find_next(),
            KeyCode::Up => self.find_previous(),
            KeyCode::Down => self.find_next(),
            KeyCode::F(3) if shift => self.find_previous(),
            KeyCode::F(3) => self.find_next(),
            KeyCode::Left => self.find_query.move_left(),
            KeyCode::Right => self.find_query.move_right(),
            KeyCode::Home => self.find_query.home(),
            KeyCode::End => self.find_query.end(),
            KeyCode::Backspace => query_changed = self.find_query.backspace(),
            KeyCode::Delete => query_changed = self.find_query.delete(),
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.find_query.insert_char(ch);
                query_changed = true;
            }
            _ => {}
        }

        if query_changed {
            self.recalculate_find_matches();
        }
    }

    fn handle_goto_line_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.goto_query.clear();
                self.status = None;
            }
            KeyCode::Left => self.goto_query.move_left(),
            KeyCode::Right => self.goto_query.move_right(),
            KeyCode::Home => self.goto_query.home(),
            KeyCode::End => self.goto_query.end(),
            KeyCode::Backspace => {
                self.goto_query.backspace();
            }
            KeyCode::Delete => {
                self.goto_query.delete();
            }
            KeyCode::Enter => {
                let query = self.goto_query.as_str().trim();
                if !query.is_empty() {
                    let parts: Vec<&str> = query.split(':').collect();
                    let line_num: usize = parts[0].trim().parse().unwrap_or(1);
                    let col_num: usize = parts
                        .get(1)
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(1);
                    let max_line = self.buffer.line_count().saturating_sub(1);
                    let target_row = line_num.saturating_sub(1).min(max_line);
                    let max_col = self.buffer.grapheme_count(target_row);
                    let target_col = col_num.saturating_sub(1).min(max_col);
                    self.cursor = Cursor::new(target_row, target_col);
                    self.selection_anchor = None;
                    self.preferred_col = None;
                    self.preferred_visual_col = None;
                    self.should_scroll_to_cursor = true;
                    self.status = Some(format!(
                        "Jumped to line {}, col {}",
                        target_row + 1,
                        target_col + 1
                    ));
                }
                self.mode = AppMode::Editing;
                self.goto_query.clear();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() || ch == ':' => {
                self.goto_query.insert_char(ch);
            }
            _ => {}
        }
    }

    pub fn recalculate_find_matches(&mut self) {
        self.find_matches.clear();
        if self.find_query.is_empty() {
            return;
        }

        match search::search_text(
            &self.buffer.contents(),
            self.find_query.as_str(),
            SearchOptions::default(),
            usize::MAX,
        ) {
            Ok(matches) => {
                self.find_matches.extend(
                    matches
                        .into_iter()
                        .filter(|matched| matched.row == matched.end_row)
                        .map(|matched| (matched.row, matched.col, matched.end_col)),
                );
            }
            Err(error) => {
                self.status = Some(format!("Find failed: {error}"));
                return;
            }
        }

        if !self.find_matches.is_empty() {
            let anchor = self.selection_anchor.unwrap_or(self.cursor);
            let current = (anchor.row, anchor.col);
            let idx = self
                .find_matches
                .iter()
                .position(|&(row, col, _)| (row, col) >= current)
                .unwrap_or(0);
            self.jump_to_find_match(idx);
        }
    }

    pub fn jump_to_find_match(&mut self, idx: usize) {
        if let Some(&(row, start_col, end_col)) = self.find_matches.get(idx) {
            self.find_selected = idx;
            self.cursor.row = row;
            self.cursor.col = end_col;
            self.selection_anchor = Some(Cursor::new(row, start_col));
            self.preferred_col = None;
            self.preferred_visual_col = None;
            self.should_scroll_to_cursor = true;
        }
    }

    pub fn find_next(&mut self) {
        if self.find_matches.is_empty() {
            return;
        }
        let next_idx = (self.find_selected + 1) % self.find_matches.len();
        self.jump_to_find_match(next_idx);
    }

    pub fn find_previous(&mut self) {
        if self.find_matches.is_empty() {
            return;
        }
        let prev_idx = (self.find_selected + self.find_matches.len() - 1) % self.find_matches.len();
        self.jump_to_find_match(prev_idx);
    }

    fn begin_replace(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            let selected = self.buffer.get_text_range(start, end);
            if !selected.is_empty() && !selected.contains('\n') {
                self.replace_query.set(selected);
            }
        } else if self.replace_query.is_empty() && !self.find_query.is_empty() {
            self.replace_query.set(self.find_query.as_str());
        }

        self.replace_active_field = 0;
        self.replace_selected = 0;
        self.mode = AppMode::Replace;
        self.recalculate_replace_preview();
    }

    fn recalculate_replace_preview(&mut self) {
        if self.replace_query.is_empty() {
            self.replace_preview.clear();
            self.replace_selected = 0;
            self.status = None;
            return;
        }

        match search::replacement_matches(
            &self.buffer.contents(),
            self.replace_query.as_str(),
            self.replace_with.as_str(),
            self.replace_options,
            MAX_REPLACE_PREVIEW,
        ) {
            Ok(preview) => {
                self.replace_preview = preview;
                self.replace_selected = self
                    .replace_selected
                    .min(self.replace_preview.len().saturating_sub(1));
                self.status = None;
            }
            Err(error) => {
                self.replace_preview.clear();
                self.replace_selected = 0;
                self.status = Some(format!("Replace pattern: {error}"));
            }
        }
    }

    fn handle_replace_key(&mut self, key: KeyEvent) {
        let ctrl = key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(crossterm::event::KeyModifiers::ALT);

        if alt {
            match key.code {
                KeyCode::Char('c' | 'C') => {
                    self.replace_options.case_sensitive = !self.replace_options.case_sensitive;
                    self.recalculate_replace_preview();
                    return;
                }
                KeyCode::Char('r' | 'R') => {
                    self.replace_options.regex = !self.replace_options.regex;
                    self.recalculate_replace_preview();
                    return;
                }
                KeyCode::Char('w' | 'W') => {
                    self.replace_options.whole_word = !self.replace_options.whole_word;
                    self.recalculate_replace_preview();
                    return;
                }
                _ => {}
            }
        }

        let mut changed = false;
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.replace_active_field = if self.replace_active_field == 0 { 1 } else { 0 };
            }
            KeyCode::Up => {
                self.replace_selected = self.replace_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if !self.replace_preview.is_empty() {
                    self.replace_selected =
                        (self.replace_selected + 1).min(self.replace_preview.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.replace_selected = self.replace_selected.saturating_sub(8);
            }
            KeyCode::PageDown => {
                if !self.replace_preview.is_empty() {
                    self.replace_selected = self
                        .replace_selected
                        .saturating_add(8)
                        .min(self.replace_preview.len() - 1);
                }
            }
            KeyCode::Enter if ctrl => self.replace_all_matches(),
            KeyCode::Enter => self.replace_selected_match(),
            KeyCode::Left => {
                if self.replace_active_field == 0 {
                    self.replace_query.move_left();
                } else {
                    self.replace_with.move_left();
                }
            }
            KeyCode::Right => {
                if self.replace_active_field == 0 {
                    self.replace_query.move_right();
                } else {
                    self.replace_with.move_right();
                }
            }
            KeyCode::Home => {
                if self.replace_active_field == 0 {
                    self.replace_query.home();
                } else {
                    self.replace_with.home();
                }
            }
            KeyCode::End => {
                if self.replace_active_field == 0 {
                    self.replace_query.end();
                } else {
                    self.replace_with.end();
                }
            }
            KeyCode::Backspace => {
                changed = if self.replace_active_field == 0 {
                    self.replace_query.backspace()
                } else {
                    self.replace_with.backspace()
                };
            }
            KeyCode::Delete => {
                changed = if self.replace_active_field == 0 {
                    self.replace_query.delete()
                } else {
                    self.replace_with.delete()
                };
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                if self.replace_active_field == 0 {
                    self.replace_query.insert_char(ch);
                } else {
                    self.replace_with.insert_char(ch);
                }
                changed = true;
            }
            _ => {}
        }

        if changed {
            self.replace_selected = 0;
            self.recalculate_replace_preview();
        }
    }

    fn replace_selected_match(&mut self) {
        let Some(replacement) = self.replace_preview.get(self.replace_selected).cloned() else {
            self.status = Some("No replacement match selected".to_owned());
            return;
        };
        if replacement.search.start_byte == replacement.search.end_byte {
            self.status = Some("Zero-width matches are preview-only".to_owned());
            return;
        }

        let start = Cursor::new(replacement.search.row, replacement.search.col);
        let end = Cursor::new(replacement.search.end_row, replacement.search.end_col);
        let pre_cursor = self.cursor;
        let pre_selection_anchor = self.selection_anchor;
        self.cursor = start;
        self.selection_anchor = None;
        self.buffer
            .replace_range(start, end, &replacement.replacement, &mut self.cursor);
        self.buffer.set_last_history_state(
            pre_cursor,
            pre_selection_anchor,
            self.cursor,
            self.selection_anchor,
        );
        self.sync_code_intelligence_after_edit();
        self.recalculate_replace_preview();
        self.status = Some("Replaced 1 match".to_owned());
    }

    fn replace_all_matches(&mut self) {
        if self.replace_query.is_empty() {
            self.status = Some("Replace All: enter a search pattern".to_owned());
            return;
        }

        let source = self.buffer.contents();
        let replacements = match search::replacement_matches(
            &source,
            self.replace_query.as_str(),
            self.replace_with.as_str(),
            self.replace_options,
            usize::MAX,
        ) {
            Ok(replacements) => replacements,
            Err(error) => {
                self.status = Some(format!("Replace pattern: {error}"));
                return;
            }
        };

        let ranges: Vec<(Cursor, Cursor, String)> = replacements
            .iter()
            .filter(|replacement| replacement.search.start_byte < replacement.search.end_byte)
            .map(|replacement| {
                (
                    Cursor::new(replacement.search.row, replacement.search.col),
                    Cursor::new(replacement.search.end_row, replacement.search.end_col),
                    replacement.replacement.clone(),
                )
            })
            .collect();

        let pre_cursor = self.cursor;
        let pre_selection_anchor = self.selection_anchor;
        self.selection_anchor = None;
        let count = self.buffer.replace_ranges(&ranges, self.cursor);
        if count == 0 {
            self.status = Some("Replace All: no replaceable matches".to_owned());
            return;
        }

        self.clamp_cursor();
        self.buffer.set_last_history_state(
            pre_cursor,
            pre_selection_anchor,
            self.cursor,
            self.selection_anchor,
        );
        self.sync_code_intelligence_after_edit();
        self.recalculate_replace_preview();
        self.status = Some(format!("Replaced {count} matches"));
    }

    fn begin_project_search(&mut self) {
        if let Some((start, end)) = self.selection_range() {
            let selected = self.buffer.get_text_range(start, end);
            if !selected.is_empty() && !selected.contains('\n') {
                self.project_search_query.set(selected);
            }
        } else if self.project_search_query.is_empty() && !self.find_query.is_empty() {
            self.project_search_query.set(self.find_query.as_str());
        }

        match workspace::discover_file_list(&self.workspace_root) {
            Ok(list) => {
                self.project_search_files = list.files;
                self.project_search_list_truncated = list.truncated;
                self.project_search_selected = 0;
                self.mode = AppMode::ProjectSearch;
                self.recalculate_project_search();
            }
            Err(error) => {
                self.status = Some(format!("Project search failed: {error}"));
            }
        }
    }

    fn recalculate_project_search(&mut self) {
        if self.project_search_query.is_empty() {
            self.project_search_report = ProjectSearchReport::default();
            self.project_search_selected = 0;
            self.status = None;
            return;
        }

        let unsaved = self.unsaved_workspace_texts();
        match search::search_project(
            &self.workspace_root,
            &self.project_search_files,
            self.project_search_query.as_str(),
            self.project_search_options,
            &unsaved,
        ) {
            Ok(report) => {
                self.project_search_report = report;
                self.project_search_selected = self
                    .project_search_selected
                    .min(self.project_search_report.results.len().saturating_sub(1));
                self.status = None;
            }
            Err(error) => {
                self.project_search_report = ProjectSearchReport::default();
                self.project_search_selected = 0;
                self.status = Some(format!("Project search pattern: {error}"));
            }
        }
    }

    /// Unsaved text of open project files, keyed by path relative to the
    /// workspace, so project search sees what the editor shows.
    fn unsaved_workspace_texts(&self) -> std::collections::HashMap<PathBuf, String> {
        (0..self.tabs.len())
            .filter_map(|index| self.tab_buffer(index))
            .filter(|buffer| buffer.is_dirty())
            .filter_map(|buffer| {
                let relative = self.workspace_relative(buffer.path()?);
                relative
                    .is_relative()
                    .then(|| (relative, buffer.contents()))
            })
            .collect()
    }

    fn handle_project_search_key(&mut self, key: KeyEvent) {
        let alt = key.modifiers.contains(crossterm::event::KeyModifiers::ALT);
        if alt {
            match key.code {
                KeyCode::Char('c' | 'C') => {
                    self.project_search_options.case_sensitive =
                        !self.project_search_options.case_sensitive;
                    self.recalculate_project_search();
                    return;
                }
                KeyCode::Char('r' | 'R') => {
                    self.project_search_options.regex = !self.project_search_options.regex;
                    self.recalculate_project_search();
                    return;
                }
                KeyCode::Char('w' | 'W') => {
                    self.project_search_options.whole_word =
                        !self.project_search_options.whole_word;
                    self.recalculate_project_search();
                    return;
                }
                _ => {}
            }
        }

        let mut changed = false;
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => {
                self.project_search_selected = self.project_search_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if !self.project_search_report.results.is_empty() {
                    self.project_search_selected = (self.project_search_selected + 1)
                        .min(self.project_search_report.results.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.project_search_selected = self.project_search_selected.saturating_sub(10);
            }
            KeyCode::PageDown => {
                if !self.project_search_report.results.is_empty() {
                    self.project_search_selected = self
                        .project_search_selected
                        .saturating_add(10)
                        .min(self.project_search_report.results.len() - 1);
                }
            }
            KeyCode::Home => self.project_search_query.home(),
            KeyCode::End => self.project_search_query.end(),
            KeyCode::Left => self.project_search_query.move_left(),
            KeyCode::Right => self.project_search_query.move_right(),
            KeyCode::Backspace => changed = self.project_search_query.backspace(),
            KeyCode::Delete => changed = self.project_search_query.delete(),
            KeyCode::Enter => self.open_selected_project_search_result(),
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.project_search_query.insert_char(ch);
                changed = true;
            }
            _ => {}
        }

        if changed {
            self.project_search_selected = 0;
            self.recalculate_project_search();
        }
    }

    fn open_selected_project_search_result(&mut self) {
        let Some(result) = self
            .project_search_report
            .results
            .get(self.project_search_selected)
            .cloned()
        else {
            self.status = Some("Project search: no result selected".to_owned());
            return;
        };

        let path = self.workspace_root.join(&result.path);
        match self.open_path_in_tab(path) {
            Ok(()) => {
                let start = Cursor::new(
                    result.row.min(self.buffer.line_count().saturating_sub(1)),
                    result.col,
                );
                let end = Cursor::new(
                    result
                        .end_row
                        .min(self.buffer.line_count().saturating_sub(1)),
                    result.end_col,
                );
                self.selection_anchor = Some(start);
                self.cursor = end;
                self.preferred_col = None;
                self.preferred_visual_col = None;
                self.should_scroll_to_cursor = true;
                self.mode = AppMode::Editing;
                self.status = Some(format!(
                    "{} · Ln {}, Col {}",
                    result.path.display(),
                    start.row + 1,
                    start.col + 1
                ));
            }
            Err(error) => {
                self.status = Some(format!("Project search open failed: {error}"));
            }
        }
    }

    fn handle_paste(&mut self, text: &str) {
        if self.mode == AppMode::Editing && self.terminal_focused {
            if let Some(session) = self.terminal_sessions.get_mut(self.active_terminal)
                && let Err(error) = session.write_paste(text)
            {
                self.terminal_status = Some(format!("Terminal paste failed: {error}"));
            }
            return;
        }

        let revision_before = self.buffer.revision();
        match self.mode {
            AppMode::Editing => {
                self.status = None;
                self.preferred_col = None;
                self.should_scroll_to_cursor = true;
                if let Some((start, end)) = self.selection_range() {
                    let pre_cursor = self.cursor;
                    let pre_selection_anchor = self.selection_anchor;
                    self.buffer
                        .replace_range(start, end, text, &mut self.cursor);
                    self.selection_anchor = None;
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                } else {
                    self.buffer.insert_text(&mut self.cursor, text);
                }
            }
            AppMode::Palette => {
                self.palette_query.insert_text(text);
                self.palette_selected = 0;
            }
            AppMode::QuickOpen => {
                self.quick_open_query.insert_text(text);
                self.quick_open_selected = 0;
                self.refresh_quick_open_candidates();
            }
            AppMode::Find => {
                self.find_query.insert_text(text);
                self.recalculate_find_matches();
            }
            AppMode::Replace => {
                if self.replace_active_field == 0 {
                    self.replace_query.insert_text(text);
                } else {
                    self.replace_with.insert_text(text);
                }
                self.recalculate_replace_preview();
            }
            AppMode::ProjectSearch => {
                self.project_search_query.insert_text(text);
                self.recalculate_project_search();
            }
            AppMode::GoToLine => {
                let filtered: String = text
                    .chars()
                    .filter(|ch| ch.is_ascii_digit() || *ch == ':')
                    .collect();
                self.goto_query.insert_text(&filtered);
            }
            AppMode::SaveAs => self.save_as_query.insert_text(text),
            AppMode::AiPrompt => self.ai_query.insert_text(text),
            AppMode::AiSetup => self.ai_setup_paste(text),
            AppMode::ExplorerCreate
            | AppMode::ExplorerCreateDirectory
            | AppMode::ExplorerRename => {
                self.explorer_action_query.insert_text(text);
            }
            AppMode::GitCommitInput => self.git_commit_query.insert_text(text),
            AppMode::GitBranchCreate | AppMode::GitBranchRename => {
                self.git_branch_query.insert_text(text)
            }
            AppMode::ConfirmQuit
            | AppMode::ConfirmCloseTab
            | AppMode::Help
            | AppMode::AiWaiting
            | AppMode::AiReview
            | AppMode::Settings
            | AppMode::Onboarding
            | AppMode::SaveConflict
            | AppMode::Recovery
            | AppMode::Completion
            | AppMode::References
            | AppMode::RenameInput
            | AppMode::ConfirmLspEdits
            | AppMode::CodeActions
            | AppMode::Problems
            | AppMode::LanguageStatus
            | AppMode::ConfirmCloseTerminal
            | AppMode::Changes
            | AppMode::ConfirmRevertHunk
            | AppMode::ConfirmExplorerDelete
            | AppMode::GitBranches
            | AppMode::ConfirmGitBranchDelete
            | AppMode::GitHistory
            | AppMode::GitBlame
            | AppMode::GitConflicts
            | AppMode::Symbols => {}
        }

        if self.buffer.revision() != revision_before {
            self.sync_code_intelligence_after_edit();
        }
    }

    fn handle_save_as_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.post_save_action = PostSaveAction::None;
                self.save_as_query.clear();
                self.mode = AppMode::Editing;
                self.status = Some("Save As cancelled".to_owned());
            }
            KeyCode::Left => self.save_as_query.move_left(),
            KeyCode::Right => self.save_as_query.move_right(),
            KeyCode::Home => self.save_as_query.home(),
            KeyCode::End => self.save_as_query.end(),
            KeyCode::Backspace => {
                self.save_as_query.backspace();
            }
            KeyCode::Delete => {
                self.save_as_query.delete();
            }
            KeyCode::Enter => {
                let raw = self.save_as_query.as_str().trim();
                if raw.is_empty() {
                    self.status = Some("Save As failed: enter a file path".to_owned());
                    return;
                }

                let mut path = expand_user_path(raw);
                if path.is_relative() {
                    path = self.workspace_root.join(path);
                }
                let prior_key = self.buffer.recovery_key();
                let note = self.format_before_save(&path);
                match self.buffer.save_as(path.clone()) {
                    Ok(()) => {
                        let _ = self.journal.clear_now(&prior_key);
                        let _ = self.journal.clear_now(&self.buffer.recovery_key());
                        let post_action = self.post_save_action;
                        self.post_save_action = PostSaveAction::None;
                        self.save_as_query.clear();
                        self.mode = AppMode::Editing;
                        self.status = Some(match note {
                            Some(note) => format!("Saved as {} · {note}", self.short_path(&path)),
                            None => format!("Saved as {}", self.short_path(&path)),
                        });
                        self.reset_active_code_intelligence();
                        self.finish_post_save(post_action);
                    }
                    Err(error) => {
                        self.status = Some(format!("Save As failed: {error}"));
                    }
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.save_as_query.insert_char(ch);
            }
            _ => {}
        }
    }

    fn request_save(&mut self, post_action: PostSaveAction) {
        if self.buffer.path().is_none() {
            self.begin_save_as(post_action);
            return;
        }

        match self.buffer.external_conflict() {
            Ok(Some(reason)) => {
                self.conflict_reason = Some(reason);
                self.conflict_read_only = false;
                self.post_save_action = post_action;
                self.mode = AppMode::SaveConflict;
                self.status = None;
            }
            // Saving replaces the file, which succeeds even when it is
            // read-only; a chmod 444 file was overwritten without a word.
            Ok(None) if self.buffer.read_only_on_disk() => {
                self.conflict_reason = Some(
                    "It is marked read-only or you lack write permission. Overwrite replaces it \
                     anyway; Save As leaves it untouched."
                        .to_owned(),
                );
                self.conflict_read_only = true;
                self.post_save_action = post_action;
                self.mode = AppMode::SaveConflict;
                self.status = None;
            }
            Ok(None) => {
                let note = self
                    .buffer
                    .path()
                    .cloned()
                    .and_then(|path| self.format_before_save(&path));
                match self.buffer.save() {
                    Ok(()) => {
                        let _ = self.journal.clear_now(&self.buffer.recovery_key());
                        self.last_journal_revision = None;
                        self.status = Some(match note {
                            Some(note) => format!("Saved · {note}"),
                            None => "Saved".to_owned(),
                        });
                        self.finish_post_save(post_action);
                    }
                    Err(error) => {
                        self.status = Some(format!("Save failed: {error}"));
                    }
                }
            }
            Err(error) => {
                self.status = Some(format!("Save failed: {error}"));
            }
        }
    }

    /// Enter keeps the line's indentation and steps in after a line that
    /// opens a block: `{`, `(` or `[` in any file, `:` in Python and YAML.
    /// Between a bracket pair the closer moves to its own line. One undo
    /// removes it all.
    fn smart_newline(&mut self) {
        use unicode_segmentation::UnicodeSegmentation;
        let line = self.buffer.line_text(self.cursor.row);
        let graphemes: Vec<&str> = line.graphemes(true).collect();
        let col = self.cursor.col.min(graphemes.len());
        let before = graphemes[..col].concat();
        let after = graphemes[col..].concat();
        let base: String = before
            .chars()
            .take_while(|ch| matches!(ch, ' ' | '\t'))
            .collect();
        let trimmed = before.trim_end();
        let colon_opens = matches!(self.buffer.language(), "Python" | "YAML");
        let opener = trimmed
            .chars()
            .last()
            .filter(|ch| matches!(ch, '{' | '(' | '[') || (colon_opens && *ch == ':'));
        let Some(opener) = opener else {
            self.buffer
                .insert_text(&mut self.cursor, &format!("\n{base}"));
            return;
        };
        let inner = format!("{base}{}", self.buffer.indent_unit());
        let closer = match opener {
            '{' => Some('}'),
            '(' => Some(')'),
            '[' => Some(']'),
            _ => None,
        };
        if closer.is_some() && after.trim_start().starts_with(closer.unwrap_or_default()) {
            self.buffer
                .insert_text(&mut self.cursor, &format!("\n{inner}\n{base}"));
            self.cursor.row -= 1;
            self.cursor.col = inner.chars().count();
        } else {
            self.buffer
                .insert_text(&mut self.cursor, &format!("\n{inner}"));
        }
    }

    /// With format on save on, runs the file's formatter over the buffer as
    /// one undoable edit. Returns a note for the status line; any problem
    /// leaves the text exactly as typed.
    fn format_before_save(&mut self, path: &std::path::Path) -> Option<String> {
        if !self.format_on_save {
            return None;
        }
        let language = crate::buffer::language_for_path(Some(path));
        let command = match crate::format::formatter_for(language, path, &self.formatter_overrides)
        {
            Ok(command) => command,
            Err(reason) => return Some(format!("not formatted: {reason}")),
        };
        let program = std::path::Path::new(&command[0]).file_name().map_or_else(
            || command[0].clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let original = self.buffer.contents();
        let formatted = match crate::format::run(&command, &original, path) {
            Ok(formatted) => formatted,
            Err(error) => return Some(format!("not formatted: {error}")),
        };
        let crlf = matches!(self.buffer.line_ending(), crate::buffer::LineEnding::CrLf);
        if crate::format::match_line_endings(&formatted, crlf) == original {
            return Some(format!("already tidy ({program})"));
        }
        let cursor = self.cursor;
        let last_row = self.buffer.line_count().saturating_sub(1);
        let end = Cursor::new(last_row, self.buffer.grapheme_count(last_row));
        self.buffer
            .replace_range(Cursor::new(0, 0), end, &formatted, &mut self.cursor);
        // Stay on the same line; formatting moves text, not the reader.
        self.cursor.row = cursor.row.min(self.buffer.line_count().saturating_sub(1));
        self.cursor.col = cursor.col.min(self.buffer.grapheme_count(self.cursor.row));
        self.selection_anchor = None;
        self.sync_code_intelligence_after_edit();
        Some(format!("formatted with {program}"))
    }

    fn handle_save_conflict_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.post_save_action = PostSaveAction::None;
                self.conflict_reason = None;
                self.mode = AppMode::Editing;
                self.status = Some(if self.conflict_read_only {
                    "Save cancelled; the read-only file is unchanged and your edits are kept"
                        .to_owned()
                } else {
                    "Save conflict cancelled; unsaved edits preserved".to_owned()
                });
            }
            KeyCode::Char('s' | 'S') => {
                let post_action = self.post_save_action;
                self.post_save_action = PostSaveAction::None;
                self.conflict_reason = None;
                self.begin_save_as(post_action);
            }
            KeyCode::Char('o' | 'O') => {
                let post_action = self.post_save_action;
                match self.buffer.force_save() {
                    Ok(()) => {
                        let _ = self.journal.clear_now(&self.buffer.recovery_key());
                        self.last_journal_revision = None;
                        self.post_save_action = PostSaveAction::None;
                        self.conflict_reason = None;
                        self.mode = AppMode::Editing;
                        self.status = Some(if self.conflict_read_only {
                            "Replaced the read-only file by explicit choice".to_owned()
                        } else {
                            "Overwrote external version by explicit choice".to_owned()
                        });
                        self.finish_post_save(post_action);
                    }
                    Err(error) => {
                        self.status = Some(format!("Overwrite failed: {error}"));
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_recovery_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('r' | 'R') | KeyCode::Enter => {
                if let Some(record) = self.recovery_candidate.take() {
                    self.buffer.restore_recovery_text(&record.content);
                    self.cursor = Cursor::new(0, 0);
                    self.selection_anchor = None;
                    self.mode = AppMode::Editing;
                    self.status = Some("Recovered unsaved edits; save to persist them".to_owned());
                    self.last_journal_revision = None;
                    self.sync_code_intelligence_after_edit();
                    // Journal the draft under this buffer before letting go of the
                    // earlier session's copy, so a crash here loses nothing.
                    self.sync_recovery_journal();
                    let journaled = self.flush_journal();
                    if let Some(journal) = record.orphan_journal
                        && journaled
                        && let Err(error) = recovery::remove_journal(&journal)
                    {
                        self.status = Some(format!("Recovery cleanup failed: {error}"));
                    }
                }
            }
            KeyCode::Char('d' | 'D') => {
                let _ = self.journal.clear_now(&self.buffer.recovery_key());
                if let Some(journal) = self
                    .recovery_candidate
                    .as_ref()
                    .and_then(|record| record.orphan_journal.as_ref())
                {
                    let _ = recovery::remove_journal(journal);
                }
                self.recovery_candidate = None;
                self.mode = AppMode::Editing;
                self.status = Some("Recovery journal discarded".to_owned());
            }
            _ => {}
        }
    }

    /// Queues the active buffer's journal: its content while dirty, removal
    /// once clean. The disk work happens on the journal thread.
    fn sync_recovery_journal(&mut self) {
        self.last_journal_sent = Instant::now();
        if self.buffer.is_dirty() {
            let revision = self.buffer.revision();
            if self.last_journal_revision == Some(revision) {
                return;
            }
            self.journal
                .write(self.buffer.recovery_key(), self.buffer.contents());
            self.last_journal_revision = Some(revision);
        } else {
            self.journal.clear(self.buffer.recovery_key());
            self.last_journal_revision = None;
        }
    }

    /// Called after every input event and loop tick: journals a changed
    /// buffer at most once per `JOURNAL_INTERVAL`, so typing never waits on
    /// the disk and copying a large buffer per keystroke is avoided.
    fn journal_if_due(&mut self) {
        self.report_journal_failures();
        let changed = if self.buffer.is_dirty() {
            self.last_journal_revision != Some(self.buffer.revision())
        } else {
            self.last_journal_revision.is_some()
        };
        if changed && self.last_journal_sent.elapsed() >= JOURNAL_INTERVAL {
            self.sync_recovery_journal();
        }
    }

    /// Waits for queued journal work; false if any of it failed.
    fn flush_journal(&mut self) -> bool {
        self.journal.wait_idle();
        self.report_journal_failures()
    }

    fn report_journal_failures(&mut self) -> bool {
        let Some(error) = self.journal.take_failures().pop() else {
            return true;
        };
        self.status = Some(format!("Recovery journal failed: {error}"));
        // Write the active buffer again on the next tick.
        self.last_journal_revision = None;
        false
    }

    fn begin_save_as(&mut self, post_action: PostSaveAction) {
        self.post_save_action = post_action;
        self.status = None;
        if let Some(path) = self.buffer.path() {
            self.save_as_query.set(path.to_string_lossy().into_owned());
        } else {
            // Suggest a free name in the project so Enter alone saves.
            let suggested = self.suggested_untitled_path();
            self.save_as_query.set(self.short_path(&suggested));
        }
        self.mode = AppMode::SaveAs;
    }

    fn suggested_untitled_path(&self) -> PathBuf {
        let first = self.workspace_root.join("untitled.txt");
        if !first.exists() {
            return first;
        }
        (2..)
            .map(|n| self.workspace_root.join(format!("untitled-{n}.txt")))
            .find(|candidate| !candidate.exists())
            .unwrap_or(first)
    }

    fn active_terminal_ref(&self) -> Option<&PtySession> {
        self.terminal_sessions.get(self.active_terminal)
    }

    fn active_terminal_mut(&mut self) -> Option<&mut PtySession> {
        self.terminal_sessions.get_mut(self.active_terminal)
    }

    fn ensure_terminal_session(&mut self) {
        if self.terminal_sessions.is_empty() {
            self.create_terminal_session();
        }
    }

    fn create_terminal_session(&mut self) {
        let id = self.next_terminal_id;
        match PtySession::spawn_shell(id, &self.workspace_root, 80, 20) {
            Ok(session) => {
                self.next_terminal_id = self.next_terminal_id.saturating_add(1);
                self.terminal_sessions.push(session);
                self.active_terminal = self.terminal_sessions.len() - 1;
                self.terminal_visible = true;
                self.terminal_focused = true;
                self.terminal_status = Some(format!("Terminal {id} started"));
            }
            Err(error) => {
                self.terminal_status = Some(format!("Terminal start failed: {error}"));
                self.status = self.terminal_status.clone();
            }
        }
    }

    /// Move between the editor and the terminal, opening it when needed. The
    /// same key always takes you back, so terminal focus is never a trap.
    fn toggle_terminal(&mut self) {
        if self.terminal_visible && self.terminal_focused {
            self.terminal_focused = false;
            self.status = Some("Back in the editor · terminal still running".to_owned());
            return;
        }
        if !self.terminal_visible {
            self.ensure_terminal_session();
            if self.terminal_sessions.is_empty() {
                return;
            }
            self.terminal_visible = true;
        }
        self.terminal_focused = true;
        self.explorer_focused = false;
    }

    fn hide_terminal(&mut self) {
        if self.terminal_visible {
            self.terminal_visible = false;
            self.terminal_focused = false;
            self.status = Some("Terminal hidden · the shell keeps running".to_owned());
        }
    }

    fn next_terminal_session(&mut self) {
        if self.terminal_sessions.is_empty() {
            self.create_terminal_session();
            return;
        }
        self.active_terminal = (self.active_terminal + 1) % self.terminal_sessions.len();
        self.terminal_visible = true;
        self.terminal_focused = true;
        let id = self.terminal_sessions[self.active_terminal].id();
        self.terminal_status = Some(format!("Terminal {id} active"));
    }

    /// Closes the active terminal, asking first if a job is still running.
    fn close_active_terminal(&mut self) {
        if self
            .active_terminal_ref()
            .is_some_and(PtySession::has_running_job)
        {
            self.mode = AppMode::ConfirmCloseTerminal;
            return;
        }
        self.close_active_terminal_now();
    }

    fn close_active_terminal_now(&mut self) {
        if self.terminal_sessions.is_empty() {
            self.terminal_visible = false;
            self.terminal_focused = false;
            return;
        }

        let id = self.terminal_sessions[self.active_terminal].id();
        self.terminal_sessions.remove(self.active_terminal);
        if self.terminal_sessions.is_empty() {
            self.active_terminal = 0;
            self.terminal_visible = false;
            self.terminal_focused = false;
            self.terminal_status = Some(format!("Terminal {id} closed"));
        } else {
            self.active_terminal = self.active_terminal.min(self.terminal_sessions.len() - 1);
            self.terminal_status = Some(format!(
                "Terminal {id} closed · Terminal {} active",
                self.terminal_sessions[self.active_terminal].id()
            ));
        }
    }

    pub fn terminal_session_count(&self) -> usize {
        self.terminal_sessions.len()
    }

    pub fn active_terminal_index(&self) -> usize {
        self.active_terminal
    }

    pub fn active_terminal_label(&self) -> Option<&str> {
        self.active_terminal_ref().map(PtySession::label)
    }

    pub fn terminal_screen_snapshot(
        &self,
        max_rows: u16,
        max_cols: u16,
    ) -> Option<pty::TerminalScreenSnapshot> {
        self.active_terminal_ref()
            .map(|session| session.screen_snapshot(max_rows, max_cols))
    }

    fn handle_terminal_key(&mut self, key: KeyEvent) {
        // Shift+PgUp/PgDn read back through the output; any other key goes
        // to the shell and returns the view to live output.
        if key
            .modifiers
            .contains(crossterm::event::KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        {
            let page = self.terminal_page_rows();
            let delta = if key.code == KeyCode::PageUp {
                page
            } else {
                -page
            };
            if let Some(session) = self.active_terminal_mut() {
                session.scroll_view(delta);
            }
            return;
        }
        if let Some(session) = self.active_terminal_mut() {
            session.follow_output();
        }
        if let Some(session) = self.active_terminal_mut()
            && let Err(error) = session.write_key(key)
        {
            self.terminal_status = Some(format!("Terminal input failed: {error}"));
        }
    }

    fn poll_terminal_sessions(&mut self) {
        let mut exited_id = None;
        for session in &mut self.terminal_sessions {
            if let Err(error) = session.read_available() {
                self.terminal_status = Some(format!("Terminal read failed: {error}"));
            }
            if session.is_exited() {
                exited_id = Some(session.id());
            }
        }

        if let Some(id) = exited_id {
            self.terminal_status = Some(format!(
                "Terminal {id} exited · create a new session to restart"
            ));
        }
    }

    /// Lines in the terminal's content area, for page-sized scrolling.
    fn terminal_page_rows(&self) -> isize {
        let (_, height) = crossterm::terminal::size().unwrap_or((80, 24));
        ui::terminal_content_size(80, height, self.terminal_panel())
            .map_or(10, |(_, rows)| rows.saturating_sub(1).max(1) as isize)
    }

    /// How many lines back the active terminal's view is (0 = live).
    pub fn terminal_scroll_offset(&self) -> usize {
        self.active_terminal_ref()
            .map_or(0, PtySession::scroll_offset)
    }

    /// The terminal panel as drawn right now.
    pub fn terminal_panel(&self) -> ui::TerminalPanel {
        if self.terminal_visible {
            self.terminal_size
        } else {
            ui::TerminalPanel::Hidden
        }
    }

    fn resize_active_terminal(&mut self, width: u16, height: u16) {
        if !self.terminal_visible {
            return;
        }
        let Some((cols, rows)) = ui::terminal_content_size(width, height, self.terminal_panel())
        else {
            return;
        };
        if let Some(session) = self.active_terminal_mut() {
            let _ = session.resize(cols, rows);
        }
    }

    fn from_session(workspace_root: PathBuf, state: SessionState) -> Result<Option<Self>> {
        let mut loaded: Vec<(usize, Option<DocumentState>)> = Vec::new();
        for (original_index, document) in state.documents.iter().enumerate() {
            if !document.path.is_file() {
                continue;
            }

            let buffer = match Buffer::open(Some(document.path.clone())) {
                Ok(buffer) => buffer,
                Err(_) => continue,
            };
            let mut document_state = Self::blank_state(buffer);
            document_state.cursor.row = document
                .cursor_row
                .min(document_state.buffer.line_count().saturating_sub(1));
            document_state.cursor.col = document.cursor_col.min(
                document_state
                    .buffer
                    .grapheme_count(document_state.cursor.row),
            );
            document_state.scroll_row = document.scroll_row;
            document_state.scroll_col = document.scroll_col;
            document_state.visual_scroll_row = document.visual_scroll_row;
            loaded.push((original_index, Some(document_state)));
        }

        if loaded.is_empty() {
            return Ok(None);
        }

        let active_index = loaded
            .iter()
            .position(|(original, _)| *original == state.active_document)
            .unwrap_or(0);
        let primary_index = loaded
            .iter()
            .position(|(original, _)| *original == state.primary_document)
            .unwrap_or(active_index);
        let secondary_index = state
            .secondary_document
            .and_then(|wanted| loaded.iter().position(|(original, _)| *original == wanted));

        let active_state = loaded[active_index]
            .1
            .take()
            .expect("active restored state must exist");
        let DocumentState {
            buffer,
            cursor,
            selection_anchor,
            secondary_cursors,
            rectangular_selection,
            scroll_row,
            scroll_col,
            visual_scroll_row,
            preferred_col,
            preferred_visual_col,
            should_scroll_to_cursor,
            recovery_candidate,
            last_journal_revision,
        } = active_state;

        let mut app = Self::new(buffer);
        app.cursor = cursor;
        app.selection_anchor = selection_anchor;
        app.secondary_cursors = secondary_cursors;
        app.rectangular_selection = rectangular_selection;
        app.scroll_row = scroll_row;
        app.scroll_col = scroll_col;
        app.visual_scroll_row = visual_scroll_row;
        app.preferred_col = preferred_col;
        app.preferred_visual_col = preferred_visual_col;
        app.should_scroll_to_cursor = should_scroll_to_cursor;
        app.recovery_candidate = recovery_candidate;
        app.last_journal_revision = last_journal_revision;
        app.tabs = loaded
            .into_iter()
            .map(|(_, state)| TabSlot { state })
            .collect();
        app.active_tab = active_index;
        app.primary_pane_tab = primary_index;
        app.secondary_pane_tab = secondary_index;
        app.active_pane = if state.active_pane == 1 && secondary_index.is_some() {
            1
        } else {
            0
        };
        app.split_ratio = state.split_ratio.clamp(20, 80);
        app.split_orientation = if state.split_orientation == 1 {
            SplitOrientation::Stacked
        } else {
            SplitOrientation::SideBySide
        };

        let active_pane_tab = if app.active_pane == 1 {
            app.secondary_pane_tab.unwrap_or(app.primary_pane_tab)
        } else {
            app.primary_pane_tab
        };
        if active_pane_tab != app.active_tab {
            if app.secondary_pane_tab == Some(app.active_tab) {
                app.active_pane = 1;
            } else {
                app.primary_pane_tab = app.active_tab;
                app.active_pane = 0;
            }
        }

        app.workspace_root = workspace_root;
        app.word_wrap = state.word_wrap;
        // The theme is a global preference (Settings: "Saved for every
        // project"), so a project's session never overrides it.
        app.show_whitespace = state.show_whitespace;
        app.show_indent_guides = state.show_indent_guides;
        app.explorer_visible = state.explorer_visible;
        app.explorer_selected = state.explorer_selected;
        if app.explorer_visible {
            app.reload_explorer();
        }
        app.mode = if app.recovery_candidate.is_some() {
            AppMode::Recovery
        } else {
            AppMode::Editing
        };
        app.status = Some(format!("Restored {} file session", app.tabs.len()));
        Ok(Some(app))
    }

    fn session_document_for_tab(&self, index: usize) -> Option<SessionDocument> {
        let (buffer, cursor, scroll_row, scroll_col, visual_scroll_row) =
            if index == self.active_tab {
                (
                    &self.buffer,
                    self.cursor,
                    self.scroll_row,
                    self.scroll_col,
                    self.visual_scroll_row,
                )
            } else {
                let state = self.tabs.get(index)?.state.as_ref()?;
                (
                    &state.buffer,
                    state.cursor,
                    state.scroll_row,
                    state.scroll_col,
                    state.visual_scroll_row,
                )
            };

        let path = buffer.path()?.clone();
        if !buffer.is_persisted() || !path.is_file() {
            return None;
        }

        Some(SessionDocument {
            path,
            cursor_row: cursor.row,
            cursor_col: cursor.col,
            scroll_row,
            scroll_col,
            visual_scroll_row,
        })
    }

    fn session_snapshot(&self) -> SessionState {
        let mut documents = Vec::new();
        let mut tab_to_document = vec![None; self.tabs.len()];

        for (tab_index, slot) in tab_to_document.iter_mut().enumerate() {
            if let Some(document) = self.session_document_for_tab(tab_index) {
                *slot = Some(documents.len());
                documents.push(document);
            }
        }

        let active_document = tab_to_document
            .get(self.active_tab)
            .and_then(|value| *value)
            .unwrap_or(0);
        let primary_document = tab_to_document
            .get(self.primary_pane_tab)
            .and_then(|value| *value)
            .unwrap_or(active_document);
        let secondary_document = self
            .secondary_pane_tab
            .and_then(|index| tab_to_document.get(index).and_then(|value| *value));

        SessionState {
            documents,
            active_document,
            primary_document,
            secondary_document,
            active_pane: self.active_pane,
            split_ratio: self.split_ratio,
            split_orientation: match self.split_orientation {
                SplitOrientation::SideBySide => 0,
                SplitOrientation::Stacked => 1,
            },
            word_wrap: self.word_wrap,
            theme_preset: self.theme_preset.index(),
            show_whitespace: self.show_whitespace,
            show_indent_guides: self.show_indent_guides,
            explorer_visible: self.explorer_visible,
            explorer_selected: self.explorer_selected,
        }
    }

    fn sync_session_state(&mut self) {
        if cfg!(test) {
            return;
        }
        let snapshot = self.session_snapshot();
        if let Err(error) = session::write(&self.workspace_root, &snapshot)
            && !self.should_quit
        {
            self.status = Some(format!("Session save failed: {error}"));
        }
    }

    fn sync_syntax_document(&mut self) {
        if self.buffer.reduced_intelligence_mode() {
            self.syntax_document = None;
            return;
        }
        let language = self.buffer.language().to_owned();
        let source = self.buffer.contents();
        let revision = self.buffer.revision();

        let needs_new = self
            .syntax_document
            .as_ref()
            .map(|document| document.language_name() != language)
            .unwrap_or(true);

        if needs_new {
            self.syntax_document = SyntaxDocument::new(&language, &source, revision)
                .ok()
                .flatten();
            return;
        }

        if let Some(document) = self.syntax_document.as_mut()
            && let Err(error) = document.sync(&language, &source, revision)
        {
            self.syntax_document = None;
            self.status = Some(format!("Tree-sitter update failed: {error}"));
        }
    }

    fn restart_language_server(&mut self, user_initiated: bool) {
        if self.buffer.reduced_intelligence_mode() {
            self.language_service.stop();
            if user_initiated {
                self.status = Some(format!(
                    "Code intelligence paused for large file ({:.1} MiB); editing remains available",
                    self.buffer.byte_len() as f64 / (1024.0 * 1024.0)
                ));
            }
            return;
        }
        self.language_service.open_document(
            &self.workspace_root,
            self.buffer.path().map(PathBuf::as_path),
            self.buffer.language(),
            &self.buffer.contents(),
            self.buffer.revision(),
        );

        if !user_initiated {
            return;
        }

        if let Some(error) = self.language_service.last_error() {
            self.status = Some(format!("Code intelligence unavailable: {error}"));
        } else if !self.language_service.supported_for_current_language() {
            self.status = Some(format!(
                "No language server configured for {}",
                self.buffer.language()
            ));
        } else {
            self.status = Some(format!(
                "Starting {}…",
                self.language_service
                    .server_label()
                    .unwrap_or("language server")
            ));
        }
    }

    fn reset_active_code_intelligence(&mut self) {
        self.sync_syntax_document();
        self.dismiss_completion();
        self.problem_selected = 0;
        if self.language_service_active {
            self.restart_language_server(false);
        } else {
            self.language_service.stop();
        }
    }

    fn sync_code_intelligence_after_edit(&mut self) {
        self.sync_syntax_document();
        if self.buffer.reduced_intelligence_mode() {
            self.language_service.stop();
            return;
        }
        if self.language_service_active {
            self.language_service.sync_document(
                self.buffer.path().map(PathBuf::as_path),
                &self.buffer.contents(),
                self.buffer.revision(),
            );
        }
    }

    fn lsp_cursor_position(&self) -> LspPosition {
        LspPosition {
            line: self.cursor.row,
            character: self
                .buffer
                .utf16_col_for_grapheme(self.cursor.row, self.cursor.col),
        }
    }

    fn request_completion(&mut self) {
        self.request_completion_with_context(true, None);
    }

    fn request_completion_with_context(&mut self, manual: bool, trigger_character: Option<String>) {
        self.auto_completion_due = None;
        self.auto_completion_trigger = None;

        if !self.language_service_active {
            if !manual {
                return;
            }
            self.language_service_active = true;
            self.restart_language_server(true);
        }

        let cursor = self.cursor;
        let revision = self.buffer.revision();
        let path = self.buffer.path().cloned();
        match self
            .language_service
            .request_completion(self.lsp_cursor_position(), trigger_character.as_deref())
        {
            Ok(Some(request_id)) => {
                self.completion_request = Some(CompletionRequestState {
                    request_id,
                    revision,
                    cursor,
                    path,
                    manual,
                });
                if manual {
                    self.status = Some("Completion requested…".to_owned());
                }
            }
            Ok(None) => {
                self.completion_request = None;
                // No language helper: still help by offering words from this
                // file, as most editors do.
                if manual && self.offer_word_completions() {
                    return;
                }
                if manual {
                    if let Some(error) = self.language_service.last_error() {
                        self.status = Some(format!("Completion unavailable: {error}"));
                    } else if self.language_service.supported_for_current_language() {
                        self.status = Some("Language server is still starting".to_owned());
                    } else {
                        self.status = Some(format!(
                            "No completion server configured for {}",
                            self.buffer.language()
                        ));
                    }
                }
            }
            Err(error) => {
                self.completion_request = None;
                if manual {
                    self.status = Some(format!("Completion request failed: {error}"));
                }
            }
        }
    }

    /// Fills the completion menu with words already used in this file,
    /// nearest first. Returns false when nothing matches.
    fn offer_word_completions(&mut self) -> bool {
        let prefix = self.completion_prefix();
        let prefix_len = prefix.chars().count();
        let start_col = self.cursor.col.saturating_sub(
            unicode_segmentation::UnicodeSegmentation::graphemes(prefix.as_str(), true).count(),
        );
        let row = self.cursor.row;
        let range = crate::lsp::LspRange {
            start: LspPosition {
                line: row,
                character: self.buffer.utf16_col_for_grapheme(row, start_col),
            },
            end: self.lsp_cursor_position(),
        };
        let mut nearest: HashMap<String, usize> = HashMap::new();
        let total = self.buffer.line_count();
        let first = row.saturating_sub(2_000);
        let last = (row + 2_000).min(total.saturating_sub(1));
        for line_row in first..=last {
            let line = self.buffer.line_text(line_row);
            let mut word = String::new();
            for ch in line.chars().chain(std::iter::once(' ')) {
                if is_completion_identifier_char(ch) {
                    word.push(ch);
                    continue;
                }
                if word.chars().count() >= 3
                    && word.chars().count() > prefix_len
                    && word.starts_with(prefix.as_str())
                    && !word.chars().next().is_some_and(|ch| ch.is_ascii_digit())
                {
                    let distance = line_row.abs_diff(row);
                    let entry = nearest.entry(std::mem::take(&mut word)).or_insert(distance);
                    *entry = (*entry).min(distance);
                }
                word.clear();
            }
        }
        if nearest.is_empty() {
            return false;
        }
        let mut words: Vec<(String, usize)> = nearest.into_iter().collect();
        words.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        self.completion_source_items = words
            .into_iter()
            .take(50)
            .enumerate()
            .map(|(rank, (word, _))| CompletionItem {
                label: word.clone(),
                detail: Some("word in this file".to_owned()),
                filter_text: None,
                sort_text: Some(format!("{rank:04}")),
                insert_text: word,
                edit_range: Some(range),
            })
            .collect();
        self.completion_selected = 0;
        self.refresh_completion_filter();
        if self.completion_items.is_empty() {
            return false;
        }
        self.status = Some("Words from this file · no language helper installed".to_owned());
        true
    }

    fn completion_prefix(&self) -> String {
        let before = self
            .buffer
            .get_text_range(Cursor::new(self.cursor.row, 0), self.cursor);
        let mut chars: Vec<char> = before
            .chars()
            .rev()
            .take_while(|ch| is_completion_identifier_char(*ch))
            .collect();
        chars.reverse();
        chars.into_iter().collect()
    }

    fn schedule_auto_completion_after_edit(&mut self, inserted: Option<char>) {
        self.auto_completion_due = None;
        self.auto_completion_trigger = None;
        self.completion_request = None;

        if !self.auto_completion_enabled
            || self.buffer.reduced_intelligence_mode()
            || self.buffer.path().is_none()
            || !self.language_service_active
            || !self.language_service.is_ready()
            || self.selection_anchor.is_some()
            || self.rectangular_selection.is_some()
            || !self.secondary_cursors.is_empty()
        {
            return;
        }

        if let Some(ch) = inserted {
            let trigger = ch.to_string();
            if self
                .language_service
                .completion_trigger_characters()
                .iter()
                .any(|candidate| candidate == &trigger)
            {
                self.auto_completion_trigger = Some(trigger);
                self.auto_completion_due = Some(Instant::now());
                return;
            }

            if !is_completion_identifier_char(ch) {
                self.completion_source_items.clear();
                self.completion_items.clear();
                if self.mode == AppMode::Completion {
                    self.mode = AppMode::Editing;
                }
                return;
            }
        }

        if self.completion_prefix().chars().count() < 2 {
            self.completion_source_items.clear();
            self.completion_items.clear();
            if self.mode == AppMode::Completion {
                self.mode = AppMode::Editing;
            }
            return;
        }

        self.auto_completion_due = Some(Instant::now() + Duration::from_millis(120));
    }

    fn poll_auto_completion(&mut self) {
        let Some(due) = self.auto_completion_due else {
            return;
        };
        if Instant::now() < due {
            return;
        }
        if !matches!(self.mode, AppMode::Editing | AppMode::Completion) {
            self.auto_completion_due = None;
            self.auto_completion_trigger = None;
            return;
        }

        let trigger = self.auto_completion_trigger.take();
        self.auto_completion_due = None;
        self.request_completion_with_context(false, trigger);
    }

    fn completion_request_is_current(&self, request: &CompletionRequestState) -> bool {
        self.buffer.revision() == request.revision
            && self.cursor == request.cursor
            && self.buffer.path().cloned() == request.path
            && matches!(self.mode, AppMode::Editing | AppMode::Completion)
    }

    fn take_matching_completion_request(
        &mut self,
        request_id: u64,
    ) -> Option<CompletionRequestState> {
        if self
            .completion_request
            .as_ref()
            .is_none_or(|request| request.request_id != request_id)
        {
            return None;
        }
        self.completion_request.take()
    }

    fn refresh_completion_filter(&mut self) {
        let prefix = self.completion_prefix();
        let prefix_lower = prefix.to_lowercase();
        let mut items: Vec<CompletionItem> = self
            .completion_source_items
            .iter()
            .filter(|item| {
                if prefix.is_empty() {
                    return true;
                }
                let candidate = item.filter_text.as_deref().unwrap_or(&item.label);
                let candidate_lower = candidate.to_lowercase();
                (candidate.starts_with(&prefix) || candidate_lower.starts_with(&prefix_lower))
                    && item.insert_text != prefix
            })
            .cloned()
            .collect();

        items.sort_by(|left, right| {
            let left_candidate = left.filter_text.as_deref().unwrap_or(&left.label);
            let right_candidate = right.filter_text.as_deref().unwrap_or(&right.label);
            let left_exact_case = !prefix.is_empty() && left_candidate.starts_with(&prefix);
            let right_exact_case = !prefix.is_empty() && right_candidate.starts_with(&prefix);
            right_exact_case
                .cmp(&left_exact_case)
                .then_with(|| {
                    left.sort_text
                        .as_deref()
                        .unwrap_or(&left.label)
                        .cmp(right.sort_text.as_deref().unwrap_or(&right.label))
                })
                .then_with(|| left.label.cmp(&right.label))
        });

        self.completion_items = items;
        self.completion_selected = self
            .completion_selected
            .min(self.completion_items.len().saturating_sub(1));
        if self.completion_items.is_empty() {
            if self.mode == AppMode::Completion {
                self.mode = AppMode::Editing;
            }
        } else {
            self.mode = AppMode::Completion;
        }
    }

    fn dismiss_completion(&mut self) {
        self.auto_completion_due = None;
        self.auto_completion_trigger = None;
        self.completion_request = None;
        self.completion_source_items.clear();
        self.completion_items.clear();
        self.completion_selected = 0;
        if self.mode == AppMode::Completion {
            self.mode = AppMode::Editing;
        }
    }

    fn request_definition(&mut self) {
        if !self.language_service_active {
            self.language_service_active = true;
            self.restart_language_server(true);
        }

        match self
            .language_service
            .request_definition(self.lsp_cursor_position())
        {
            Ok(true) => {
                self.status = Some("Looking up definition…".to_owned());
            }
            Ok(false) => {
                if let Some(error) = self.language_service.last_error() {
                    self.status = Some(format!("Definition unavailable: {error}"));
                } else if self.language_service.supported_for_current_language() {
                    self.status = Some("Language server is still starting".to_owned());
                } else {
                    self.status = Some(format!(
                        "No definition server configured for {}",
                        self.buffer.language()
                    ));
                }
            }
            Err(error) => {
                self.status = Some(format!("Definition request failed: {error}"));
            }
        }
    }

    fn ensure_language_service(&mut self) {
        if !self.language_service_active {
            self.language_service_active = true;
            self.restart_language_server(true);
        }
    }

    fn request_hover(&mut self) {
        self.ensure_language_service();
        match self
            .language_service
            .request_hover(self.lsp_cursor_position())
        {
            Ok(true) => self.status = Some("Hover requested…".to_owned()),
            Ok(false) => {
                self.status = Some(
                    "Hover unavailable while language server starts or is not configured"
                        .to_owned(),
                )
            }
            Err(error) => self.status = Some(format!("Hover request failed: {error}")),
        }
    }

    fn request_signature_help(&mut self) {
        self.ensure_language_service();
        match self
            .language_service
            .request_signature_help(self.lsp_cursor_position())
        {
            Ok(true) => self.status = Some("Signature help requested…".to_owned()),
            Ok(false) => self.status = Some("Signature help unavailable".to_owned()),
            Err(error) => self.status = Some(format!("Signature request failed: {error}")),
        }
    }

    fn request_references(&mut self) {
        self.ensure_language_service();
        match self
            .language_service
            .request_references(self.lsp_cursor_position())
        {
            Ok(true) => self.status = Some("Finding references…".to_owned()),
            Ok(false) => self.status = Some("References unavailable".to_owned()),
            Err(error) => self.status = Some(format!("References request failed: {error}")),
        }
    }

    fn request_formatting(&mut self) {
        self.ensure_language_service();
        match self
            .language_service
            .request_formatting(TAB_WIDTH, !self.buffer.uses_tab_indent())
        {
            Ok(true) => self.status = Some("Formatting preview requested…".to_owned()),
            Ok(false) => self.status = Some("Formatting unavailable".to_owned()),
            Err(error) => self.status = Some(format!("Formatting request failed: {error}")),
        }
    }

    fn request_code_actions(&mut self) {
        self.ensure_language_service();
        let pos = self.lsp_cursor_position();
        let range = crate::lsp::LspRange {
            start: pos,
            end: pos,
        };
        match self.language_service.request_code_actions(range) {
            Ok(true) => self.status = Some("Code actions requested…".to_owned()),
            Ok(false) => self.status = Some("Code actions unavailable".to_owned()),
            Err(error) => self.status = Some(format!("Code action request failed: {error}")),
        }
    }

    fn request_rename_preview(&mut self) {
        self.ensure_language_service();
        let current_name = self
            .buffer
            .word_range(self.cursor.row, self.cursor.col)
            .map(|(start, end)| self.buffer.get_text_range(start, end));
        let Some(current_name) = current_name else {
            self.status = Some("Rename unavailable: place the cursor on a symbol".to_owned());
            return;
        };
        self.rename_query.set(&current_name);
        self.mode = AppMode::RenameInput;
        self.status =
            Some("Type the new symbol name · Enter requests preview · Esc cancels".to_owned());
    }

    fn poll_language_service(&mut self) {
        let events = self.language_service.poll();
        for event in events {
            match event {
                LanguageServiceEvent::Ready(label) => {
                    self.status = Some(format!("{label} ready"));
                }
                LanguageServiceEvent::DiagnosticsChanged => {
                    let count = self.problem_items().len();
                    if count > 0 && self.mode == AppMode::Problems {
                        self.problem_selected = self.problem_selected.min(count.saturating_sub(1));
                    }
                }
                LanguageServiceEvent::Completions { request_id, items } => {
                    let Some(request) = self.take_matching_completion_request(request_id) else {
                        continue;
                    };
                    if !self.completion_request_is_current(&request) {
                        continue;
                    }

                    self.completion_source_items = items;
                    self.completion_selected = 0;
                    self.refresh_completion_filter();
                    if self.completion_items.is_empty() {
                        if request.manual {
                            self.status = Some("No completions".to_owned());
                        }
                    } else {
                        self.status = None;
                    }
                }
                LanguageServiceEvent::CompletionError { request_id, error } => {
                    let Some(request) = self.take_matching_completion_request(request_id) else {
                        continue;
                    };
                    if !self.completion_request_is_current(&request) {
                        continue;
                    }
                    if request.manual {
                        self.mode = AppMode::Editing;
                        self.status = Some(format!("Completion failed: {error}"));
                    }
                }
                LanguageServiceEvent::Definition(location) => {
                    self.mode = AppMode::Editing;
                    if let Some(location) = location {
                        self.apply_definition_location(location);
                    } else {
                        self.status = Some("No definition found".to_owned());
                    }
                }
                LanguageServiceEvent::Hover(contents) => {
                    self.status =
                        Some(contents.unwrap_or_else(|| "No hover information".to_owned()));
                }
                LanguageServiceEvent::SignatureHelp(signatures) => {
                    self.status = Some(
                        signatures
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "No signature information".to_owned()),
                    );
                }
                LanguageServiceEvent::References(locations) => {
                    self.reference_items = locations;
                    self.reference_selected = 0;
                    if self.reference_items.is_empty() {
                        self.status = Some("No references found".to_owned());
                    } else {
                        self.mode = AppMode::References;
                        self.status = None;
                    }
                }
                LanguageServiceEvent::RenamePreview(preview) => {
                    if let Some(reason) = preview.rejected {
                        self.mode = AppMode::Editing;
                        self.status =
                            Some(format!("Rename refused: {reason}; nothing was changed"));
                    } else if preview.has_resource_operations {
                        self.mode = AppMode::Editing;
                        self.status = Some(
                            "Rename blocked: language server requested file create/rename/delete operations that Mellow will not execute automatically"
                                .to_owned(),
                        );
                    } else {
                        self.begin_lsp_edit_preview("Rename", preview.edits);
                    }
                }
                LanguageServiceEvent::Formatting(edits) => {
                    self.begin_lsp_edit_preview("Format document", edits)
                }
                LanguageServiceEvent::CodeActions(actions) => {
                    self.code_action_items = actions;
                    self.code_action_selected = 0;
                    if self.code_action_items.is_empty() {
                        self.status = Some("No code actions available".to_owned());
                    } else {
                        self.mode = AppMode::CodeActions;
                        self.status = None;
                    }
                }
                LanguageServiceEvent::Exited(label) => {
                    self.status = Some(format!("{label} exited"));
                }
                LanguageServiceEvent::Error(error) => {
                    self.status = Some(format!("Language server: {error}"));
                }
            }
        }
    }

    fn apply_definition_location(&mut self, location: DefinitionLocation) {
        if self.buffer.path() != Some(&location.path)
            && let Err(error) = self.open_path_in_tab(location.path.clone())
        {
            self.status = Some(format!("Definition open failed: {error}"));
            return;
        }

        let row = location
            .range
            .start
            .line
            .min(self.buffer.line_count().saturating_sub(1));
        let col = self
            .buffer
            .grapheme_col_for_utf16(row, location.range.start.character);
        self.cursor = Cursor::new(row, col);
        self.selection_anchor = None;
        self.should_scroll_to_cursor = true;
        self.status = Some(format!("Definition · Ln {}, Col {}", row + 1, col + 1));
    }

    fn apply_selected_completion(&mut self) {
        let Some(item) = self.completion_items.get(self.completion_selected).cloned() else {
            self.mode = AppMode::Editing;
            return;
        };

        let pre_cursor = self.cursor;
        let pre_selection_anchor = self.selection_anchor;
        if let Some(range) = item.edit_range {
            let start_row = range
                .start
                .line
                .min(self.buffer.line_count().saturating_sub(1));
            let end_row = range
                .end
                .line
                .min(self.buffer.line_count().saturating_sub(1));
            let start = Cursor::new(
                start_row,
                self.buffer
                    .grapheme_col_for_utf16(start_row, range.start.character),
            );
            let end = Cursor::new(
                end_row,
                self.buffer
                    .grapheme_col_for_utf16(end_row, range.end.character),
            );

            self.cursor = start;
            self.selection_anchor = None;
            if start == end {
                self.buffer.insert_text(&mut self.cursor, &item.insert_text);
            } else {
                self.buffer
                    .replace_range(start, end, &item.insert_text, &mut self.cursor);
                self.buffer.set_last_history_state(
                    pre_cursor,
                    pre_selection_anchor,
                    self.cursor,
                    self.selection_anchor,
                );
            }
        } else {
            self.selection_anchor = None;
            self.buffer.insert_text(&mut self.cursor, &item.insert_text);
        }

        self.dismiss_completion();
        self.status = Some(format!("Inserted {}", item.label));
        self.sync_code_intelligence_after_edit();
    }

    fn handle_completion_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.dismiss_completion();
                self.status = None;
            }
            KeyCode::Up => {
                self.completion_selected = self.completion_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if !self.completion_items.is_empty() {
                    self.completion_selected =
                        (self.completion_selected + 1).min(self.completion_items.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.completion_selected = self.completion_selected.saturating_sub(8);
            }
            KeyCode::PageDown => {
                if !self.completion_items.is_empty() {
                    self.completion_selected = self
                        .completion_selected
                        .saturating_add(8)
                        .min(self.completion_items.len() - 1);
                }
            }
            KeyCode::Enter | KeyCode::Tab => self.apply_selected_completion(),
            KeyCode::Char(' ')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.mode = AppMode::Editing;
                self.request_completion();
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                let keep_filtering = is_completion_identifier_char(ch);
                self.mode = AppMode::Editing;
                self.completion_items.clear();
                self.execute(Command::Insert(ch));
                if keep_filtering && !self.completion_source_items.is_empty() {
                    self.refresh_completion_filter();
                } else if !keep_filtering {
                    self.completion_source_items.clear();
                }
            }
            KeyCode::Backspace => {
                self.mode = AppMode::Editing;
                self.completion_items.clear();
                self.execute(Command::Backspace);
                if !self.completion_source_items.is_empty() {
                    self.refresh_completion_filter();
                }
            }
            KeyCode::Left => {
                self.dismiss_completion();
                self.execute(Command::MoveLeft);
            }
            KeyCode::Right => {
                self.dismiss_completion();
                self.execute(Command::MoveRight);
            }
            KeyCode::Home => {
                self.dismiss_completion();
                self.execute(Command::Home);
            }
            KeyCode::End => {
                self.dismiss_completion();
                self.execute(Command::End);
            }
            KeyCode::Delete => {
                self.dismiss_completion();
                self.execute(Command::Delete);
            }
            _ => {}
        }
    }

    fn handle_references_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => self.reference_selected = self.reference_selected.saturating_sub(1),
            KeyCode::Down => {
                if !self.reference_items.is_empty() {
                    self.reference_selected =
                        (self.reference_selected + 1).min(self.reference_items.len() - 1);
                }
            }
            KeyCode::PageUp => self.reference_selected = self.reference_selected.saturating_sub(8),
            KeyCode::PageDown => {
                if !self.reference_items.is_empty() {
                    self.reference_selected = self
                        .reference_selected
                        .saturating_add(8)
                        .min(self.reference_items.len() - 1);
                }
            }
            KeyCode::Home => self.reference_selected = 0,
            KeyCode::End => self.reference_selected = self.reference_items.len().saturating_sub(1),
            KeyCode::Enter => {
                if let Some(location) = self.reference_items.get(self.reference_selected).cloned() {
                    self.mode = AppMode::Editing;
                    self.apply_definition_location(location);
                }
            }
            _ => {}
        }
    }

    fn begin_lsp_edit_preview(&mut self, label: &str, edits: Vec<crate::lsp::LspTextEdit>) {
        if edits.is_empty() {
            self.status = Some(format!("{label}: no edits returned"));
            self.mode = AppMode::Editing;
            return;
        }
        self.pending_lsp_label = label.to_owned();
        self.pending_lsp_preview = self.lsp_edit_preview(&edits);
        self.pending_lsp_scroll = 0;
        self.pending_lsp_edits = edits;
        self.mode = AppMode::ConfirmLspEdits;
        self.status = None;
    }

    /// Before/after lines for each edit, grouped by file, using the same
    /// position mapping as applying them.
    fn lsp_edit_preview(&self, edits: &[crate::lsp::LspTextEdit]) -> Vec<LspPreviewRow> {
        use unicode_segmentation::UnicodeSegmentation;
        let mut paths: Vec<&PathBuf> = edits.iter().map(|edit| &edit.path).collect();
        paths.sort();
        paths.dedup();
        let mut rows = Vec::new();
        for path in paths {
            rows.push(LspPreviewRow::File(self.short_path(path)));
            let file_edits: Vec<_> = edits
                .iter()
                .filter(|edit| &edit.path == path)
                .cloned()
                .collect();
            let opened;
            let buffer = match (0..self.tabs.len()).find_map(|index| {
                self.tab_buffer(index)
                    .filter(|buffer| buffer.path().is_some_and(|open| self.same_path(open, path)))
            }) {
                Some(buffer) => buffer,
                None => match Buffer::open(Some(path.clone())) {
                    Ok(buffer) => {
                        opened = buffer;
                        &opened
                    }
                    Err(error) => {
                        rows.push(LspPreviewRow::Note(format!("cannot read file: {error}")));
                        continue;
                    }
                },
            };
            let replacements = match Self::lsp_replacements_for_buffer(buffer, &file_edits) {
                Ok(replacements) => replacements,
                Err(error) => {
                    rows.push(LspPreviewRow::Note(error));
                    continue;
                }
            };
            for (start, end, text) in replacements {
                let first = buffer.line_text(start.row);
                let last = buffer.line_text(end.row);
                let head: String = first.graphemes(true).take(start.col).collect();
                let tail: String = last.graphemes(true).skip(end.col).collect();
                rows.push(LspPreviewRow::Location(start.row + 1));
                for row in start.row..=end.row {
                    rows.push(LspPreviewRow::Removed(buffer.line_text(row)));
                }
                for line in format!("{head}{text}{tail}").split('\n') {
                    rows.push(LspPreviewRow::Added(line.to_owned()));
                }
            }
        }
        rows
    }

    fn same_path(&self, left: &std::path::Path, right: &std::path::Path) -> bool {
        left == right || self.path_identity(left) == self.path_identity(right)
    }

    fn lsp_replacements_for_buffer(
        buffer: &Buffer,
        edits: &[crate::lsp::LspTextEdit],
    ) -> Result<Vec<(Cursor, Cursor, String)>, String> {
        let line_count = buffer.line_count();
        let mut ordered: Vec<&crate::lsp::LspTextEdit> = edits.iter().collect();
        ordered.sort_by_key(|edit| {
            (
                edit.range.start.line,
                edit.range.start.character,
                edit.range.end.line,
                edit.range.end.character,
            )
        });

        let mut previous: Option<((usize, usize), (usize, usize))> = None;
        let mut replacements = Vec::with_capacity(ordered.len());
        for edit in ordered {
            let start = (edit.range.start.line, edit.range.start.character);
            let end = (edit.range.end.line, edit.range.end.character);
            if start > end {
                return Err("language server returned a reversed text edit range".to_owned());
            }
            if start.0 >= line_count || end.0 >= line_count {
                return Err(format!(
                    "language server edit references line outside document ({}..{})",
                    start.0 + 1,
                    end.0 + 1
                ));
            }
            let start_utf16 = buffer.line_text(start.0).encode_utf16().count();
            let end_utf16 = buffer.line_text(end.0).encode_utf16().count();
            if start.1 > start_utf16 || end.1 > end_utf16 {
                return Err("language server edit contains a stale UTF-16 column".to_owned());
            }
            if let Some((previous_start, previous_end)) = previous
                && (start < previous_end
                    || (previous_start == previous_end && start == previous_start))
            {
                return Err("language server returned overlapping text edits".to_owned());
            }

            let start_cursor =
                Cursor::new(start.0, buffer.grapheme_col_for_utf16(start.0, start.1));
            let end_cursor = Cursor::new(end.0, buffer.grapheme_col_for_utf16(end.0, end.1));
            replacements.push((start_cursor, end_cursor, edit.new_text.clone()));
            previous = Some((start, end));
        }
        Ok(replacements)
    }

    fn validate_lsp_edits(&self) -> Result<(), String> {
        const MAX_WORKSPACE_EDIT_FILES: usize = 32;
        let mut grouped: BTreeMap<PathBuf, Vec<crate::lsp::LspTextEdit>> = BTreeMap::new();
        for edit in &self.pending_lsp_edits {
            let path = self.path_identity(&edit.path);
            grouped.entry(path).or_default().push(edit.clone());
        }
        if grouped.len() > MAX_WORKSPACE_EDIT_FILES {
            return Err(format!(
                "WorkspaceEdit touches {} files; safety limit is {MAX_WORKSPACE_EDIT_FILES}",
                grouped.len()
            ));
        }

        let workspace = self.path_identity(&self.workspace_root);
        let active_path = self.buffer.path().map(|path| self.path_identity(path));
        for (path, edits) in grouped {
            if active_path.as_ref() != Some(&path) && !path.starts_with(&workspace) {
                return Err(format!(
                    "WorkspaceEdit target is outside the active workspace: {}",
                    path.display()
                ));
            }
            if let Some(index) = self.find_open_tab(&path) {
                let Some(buffer) = self.tab_buffer(index) else {
                    return Err(format!(
                        "open buffer state is unavailable: {}",
                        path.display()
                    ));
                };
                if index != self.active_tab && buffer.is_dirty() {
                    return Err(format!(
                        "{} has unsaved edits in another Mellow tab",
                        path.display()
                    ));
                }
                if index != self.active_tab
                    && self
                        .tabs
                        .get(index)
                        .and_then(|slot| slot.state.as_ref())
                        .and_then(|state| state.recovery_candidate.as_ref())
                        .is_some()
                {
                    return Err(format!(
                        "{} has pending recovery data; restore/discard it first",
                        path.display()
                    ));
                }
                match buffer.external_conflict() {
                    Ok(Some(reason)) => {
                        return Err(format!("{} changed externally: {reason}", path.display()));
                    }
                    Err(error) => {
                        return Err(format!(
                            "external-change check failed for {}: {error}",
                            path.display()
                        ));
                    }
                    Ok(None) => {}
                }
                Self::lsp_replacements_for_buffer(buffer, &edits)?;
            } else {
                let buffer = Buffer::open(Some(path.clone()))
                    .map_err(|error| format!("cannot load {}: {error}", path.display()))?;
                if let Some(record) =
                    recovery::load(&RecoveryKey::File(path.clone())).map_err(|error| {
                        format!(
                            "cannot inspect recovery state for {}: {error}",
                            path.display()
                        )
                    })?
                    && record.content != buffer.contents()
                {
                    return Err(format!(
                        "{} has unsaved recovery data; open and resolve it first",
                        path.display()
                    ));
                }
                Self::lsp_replacements_for_buffer(&buffer, &edits)?;
            }
        }
        Ok(())
    }

    fn apply_pending_lsp_edits(&mut self) {
        if let Err(reason) = self.validate_lsp_edits() {
            self.status = Some(format!("{} blocked: {reason}", self.pending_lsp_label));
            self.mode = AppMode::Editing;
            return;
        }

        let mut grouped: BTreeMap<PathBuf, Vec<crate::lsp::LspTextEdit>> = BTreeMap::new();
        for edit in self.pending_lsp_edits.clone() {
            let path = self.path_identity(&edit.path);
            grouped.entry(path).or_default().push(edit);
        }

        let mut open_plans = Vec::new();
        let mut closed_plans = Vec::new();
        for (path, edits) in grouped {
            if let Some(index) = self.find_open_tab(&path) {
                let Some(buffer) = self.tab_buffer(index) else {
                    self.status = Some(format!(
                        "{} blocked: open buffer state disappeared for {}",
                        self.pending_lsp_label,
                        path.display()
                    ));
                    self.mode = AppMode::Editing;
                    return;
                };
                let replacements = match Self::lsp_replacements_for_buffer(buffer, &edits) {
                    Ok(replacements) => replacements,
                    Err(reason) => {
                        self.status = Some(format!(
                            "{} blocked while preparing {}: {reason}",
                            self.pending_lsp_label,
                            path.display()
                        ));
                        self.mode = AppMode::Editing;
                        return;
                    }
                };
                open_plans.push((index, path, replacements));
            } else {
                let buffer = match Buffer::open(Some(path.clone())) {
                    Ok(buffer) => buffer,
                    Err(error) => {
                        self.status = Some(format!(
                            "{} blocked: {} changed or became unreadable during preview: {error}",
                            self.pending_lsp_label,
                            path.display()
                        ));
                        self.mode = AppMode::Editing;
                        return;
                    }
                };
                let replacements = match Self::lsp_replacements_for_buffer(&buffer, &edits) {
                    Ok(replacements) => replacements,
                    Err(reason) => {
                        self.status = Some(format!(
                            "{} blocked while preparing {}: {reason}",
                            self.pending_lsp_label,
                            path.display()
                        ));
                        self.mode = AppMode::Editing;
                        return;
                    }
                };
                closed_plans.push((path, Self::blank_state(buffer), replacements));
            }
        }

        self.workspace_edit_history = None;
        let mut applied_edits = 0usize;
        let mut affected_files = 0usize;
        let mut recovery_warnings = Vec::new();
        let mut workspace_members = Vec::new();

        for (index, path, replacements) in open_plans {
            let expected = replacements.len();
            let count = if index == self.active_tab {
                self.buffer.replace_ranges(&replacements, self.cursor)
            } else {
                let state = self.tabs[index]
                    .state
                    .as_mut()
                    .expect("validated inactive tab state");
                state.buffer.replace_ranges(&replacements, state.cursor)
            };
            if count > 0 {
                affected_files += 1;
                applied_edits += count;
                if let Some(buffer) = self.tab_buffer(index) {
                    workspace_members.push(WorkspaceEditMember {
                        path: path.clone(),
                        expected_revision: buffer.revision(),
                    });
                }
            }
            if count != expected {
                recovery_warnings.push(format!(
                    "{} applied {count}/{expected} validated edit(s)",
                    path.display()
                ));
            }

            if index == self.active_tab {
                if let Err(error) = self
                    .journal
                    .write_now(&self.buffer.recovery_key(), &self.buffer.contents())
                {
                    recovery_warnings.push(format!(
                        "recovery journal failed for {}: {error}",
                        path.display()
                    ));
                } else {
                    self.last_journal_revision = Some(self.buffer.revision());
                }
            } else if let Some(state) = self.tabs[index].state.as_mut() {
                if let Err(error) = self
                    .journal
                    .write_now(&state.buffer.recovery_key(), &state.buffer.contents())
                {
                    recovery_warnings.push(format!(
                        "recovery journal failed for {}: {error}",
                        path.display()
                    ));
                } else {
                    state.last_journal_revision = Some(state.buffer.revision());
                }
            }
        }

        for (path, mut state, replacements) in closed_plans {
            let expected = replacements.len();
            let count = state.buffer.replace_ranges(&replacements, state.cursor);
            if count > 0 {
                affected_files += 1;
                applied_edits += count;
                workspace_members.push(WorkspaceEditMember {
                    path: path.clone(),
                    expected_revision: state.buffer.revision(),
                });
            }
            if count != expected {
                recovery_warnings.push(format!(
                    "{} applied {count}/{expected} validated edit(s)",
                    path.display()
                ));
            }
            if let Err(error) = self
                .journal
                .write_now(&state.buffer.recovery_key(), &state.buffer.contents())
            {
                recovery_warnings.push(format!(
                    "recovery journal failed for {}: {error}",
                    path.display()
                ));
            } else {
                state.last_journal_revision = Some(state.buffer.revision());
            }
            self.tabs.push(TabSlot { state: Some(state) });
        }

        if workspace_members.len() > 1 {
            self.workspace_edit_history = Some(WorkspaceEditHistory {
                label: self.pending_lsp_label.clone(),
                members: workspace_members,
                state: WorkspaceEditHistoryState::Applied,
            });
        }

        self.pending_lsp_edits.clear();
        self.mode = AppMode::Editing;
        self.sync_code_intelligence_after_edit();
        self.sync_session_state();
        self.refresh_git_state(true);

        let base = format!(
            "{} applied to {affected_files} buffer(s): {applied_edits} edit(s) · review/save each dirty tab",
            self.pending_lsp_label
        );
        self.status = if recovery_warnings.is_empty() {
            Some(base)
        } else {
            Some(format!("{base} · {}", recovery_warnings.join("; ")))
        };
    }

    fn workspace_history_targets_active(&self, history: &WorkspaceEditHistory) -> bool {
        let Some(path) = self.buffer.path() else {
            return false;
        };
        let active = self.path_identity(path);
        history.members.iter().any(|member| member.path == active)
    }

    fn run_workspace_history_step(&mut self, redo: bool) -> Option<bool> {
        let mut history = self.workspace_edit_history.clone()?;
        if !self.workspace_history_targets_active(&history) {
            return None;
        }

        let expected_state = if redo {
            WorkspaceEditHistoryState::Undone
        } else {
            WorkspaceEditHistoryState::Applied
        };
        if history.state != expected_state {
            self.workspace_edit_history = None;
            return None;
        }

        let mut targets = Vec::with_capacity(history.members.len());
        for member in &history.members {
            let Some(index) = self.find_open_tab(&member.path) else {
                self.workspace_edit_history = None;
                self.status = Some(format!(
                    "Atomic {} unavailable: {} is no longer open",
                    if redo { "redo" } else { "undo" },
                    member.path.display()
                ));
                return Some(false);
            };
            let Some(buffer) = self.tab_buffer(index) else {
                self.workspace_edit_history = None;
                self.status =
                    Some("Atomic workspace history unavailable: buffer state missing".to_owned());
                return Some(false);
            };
            let history_ready = if redo {
                buffer.can_redo()
            } else {
                buffer.can_undo()
            };
            if buffer.revision() != member.expected_revision || !history_ready {
                self.workspace_edit_history = None;
                self.status = Some(format!(
                    "Atomic {} refused: {} changed after the workspace edit",
                    if redo { "redo" } else { "undo" },
                    member.path.display()
                ));
                return Some(false);
            }
            targets.push((index, member.path.clone()));
        }

        let mut recovery_warnings = Vec::new();
        for (index, path) in &targets {
            let changed = if *index == self.active_tab {
                if redo {
                    self.buffer
                        .redo_with_selection(&mut self.cursor, &mut self.selection_anchor)
                } else {
                    self.buffer
                        .undo_with_selection(&mut self.cursor, &mut self.selection_anchor)
                }
            } else {
                let Some(state) = self.tabs[*index].state.as_mut() else {
                    self.workspace_edit_history = None;
                    self.status =
                        Some("Atomic workspace history unavailable: tab state missing".to_owned());
                    return Some(false);
                };
                if redo {
                    state
                        .buffer
                        .redo_with_selection(&mut state.cursor, &mut state.selection_anchor)
                } else {
                    state
                        .buffer
                        .undo_with_selection(&mut state.cursor, &mut state.selection_anchor)
                }
            };
            if !changed {
                self.workspace_edit_history = None;
                self.status = Some("Atomic workspace history failed before completion".to_owned());
                return Some(false);
            }

            let Some(buffer) = self.tab_buffer(*index) else {
                continue;
            };
            if let Some(member) = history
                .members
                .iter_mut()
                .find(|member| member.path == *path)
            {
                member.expected_revision = buffer.revision();
            }

            if *index == self.active_tab {
                if let Err(error) = self
                    .journal
                    .write_now(&self.buffer.recovery_key(), &self.buffer.contents())
                {
                    recovery_warnings.push(format!("{}: {error}", path.display()));
                } else {
                    self.last_journal_revision = Some(self.buffer.revision());
                }
            } else if let Some(state) = self.tabs[*index].state.as_mut() {
                if let Err(error) = self
                    .journal
                    .write_now(&state.buffer.recovery_key(), &state.buffer.contents())
                {
                    recovery_warnings.push(format!("{}: {error}", path.display()));
                } else {
                    state.last_journal_revision = Some(state.buffer.revision());
                }
            }
        }

        history.state = if redo {
            WorkspaceEditHistoryState::Applied
        } else {
            WorkspaceEditHistoryState::Undone
        };
        let label = history.label.clone();
        let member_count = history.members.len();
        self.workspace_edit_history = Some(history);
        self.secondary_cursors.clear();
        self.rectangular_selection = None;
        self.clamp_cursor();
        self.sync_code_intelligence_after_edit();
        self.sync_session_state();
        self.refresh_git_state(true);
        self.status = Some(if recovery_warnings.is_empty() {
            format!(
                "{} {} across {member_count} buffers",
                if redo { "Redid" } else { "Undid" },
                label
            )
        } else {
            format!(
                "{} {} across {member_count} buffers · recovery warnings: {}",
                if redo { "Redid" } else { "Undid" },
                label,
                recovery_warnings.join("; ")
            )
        });
        Some(true)
    }

    fn handle_lsp_edit_confirmation_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.apply_pending_lsp_edits()
            }
            KeyCode::Up => self.pending_lsp_scroll = self.pending_lsp_scroll.saturating_sub(1),
            KeyCode::Down => self.scroll_lsp_preview(1),
            KeyCode::PageUp => self.pending_lsp_scroll = self.pending_lsp_scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll_lsp_preview(10),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.pending_lsp_edits.clear();
                self.pending_lsp_preview.clear();
                self.mode = AppMode::Editing;
                self.status = Some("LSP edits cancelled".to_owned());
            }
            _ => {}
        }
    }

    fn scroll_lsp_preview(&mut self, by: usize) {
        let last = self.pending_lsp_preview.len().saturating_sub(1);
        self.pending_lsp_scroll = (self.pending_lsp_scroll + by).min(last);
    }

    /// Smart Home: the first non-blank column, or column 0 when already there.
    fn smart_home_col(&self, cursor: Cursor) -> usize {
        let first = self.buffer.first_non_blank(cursor.row);
        if cursor.col == first { 0 } else { first }
    }

    fn word_target(&self, command: Command) -> Cursor {
        match command {
            Command::MoveWordLeft | Command::SelectWordLeft | Command::DeleteWordLeft => {
                self.buffer.word_left(self.cursor)
            }
            _ => self.buffer.word_right(self.cursor),
        }
    }

    fn extend_or_clear_selection(&mut self, extend: bool) {
        self.secondary_cursors.clear();
        self.rectangular_selection = None;
        if !extend {
            self.selection_anchor = None;
        } else if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
    }

    fn handle_rename_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.rename_query.clear();
            }
            KeyCode::Backspace => {
                self.rename_query.backspace();
            }
            KeyCode::Delete => {
                self.rename_query.delete();
            }
            KeyCode::Left => self.rename_query.move_left(),
            KeyCode::Right => self.rename_query.move_right(),
            KeyCode::Enter => {
                let name = self.rename_query.as_str().trim().to_owned();
                if name.is_empty() {
                    self.status = Some("Rename requires a non-empty symbol name".to_owned());
                    return;
                }
                self.mode = AppMode::Editing;
                match self
                    .language_service
                    .request_rename(self.lsp_cursor_position(), &name)
                {
                    Ok(true) => self.status = Some(format!("Rename preview requested → {name}")),
                    Ok(false) => self.status = Some("Rename unavailable".to_owned()),
                    Err(error) => self.status = Some(format!("Rename request failed: {error}")),
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.rename_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn handle_code_actions_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.code_action_items.clear();
            }
            KeyCode::Up => self.code_action_selected = self.code_action_selected.saturating_sub(1),
            KeyCode::Down => {
                if !self.code_action_items.is_empty() {
                    self.code_action_selected =
                        (self.code_action_selected + 1).min(self.code_action_items.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(action) = self
                    .code_action_items
                    .get(self.code_action_selected)
                    .cloned()
                {
                    if let Some(reason) = action.rejected {
                        self.mode = AppMode::Editing;
                        self.status = Some(format!(
                            "Code action refused: {reason}; nothing was changed"
                        ));
                    } else if action.has_resource_operations {
                        self.mode = AppMode::Editing;
                        self.status = Some(
                            "Code action requires file create/rename/delete operations; Mellow will not partially apply it"
                                .to_owned(),
                        );
                    } else if action.edits.is_empty() {
                        self.mode = AppMode::Editing;
                        self.status = Some(if action.has_command {
                            "Code action contains a server command; Mellow will not execute it automatically".to_owned()
                        } else {
                            "Code action has no supported WorkspaceEdit".to_owned()
                        });
                    } else {
                        self.begin_lsp_edit_preview(
                            &format!("Code action · {}", action.title),
                            action.edits,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub fn language_health(&self) -> crate::lsp::LanguageHealth {
        self.language_service.health(self.buffer.language())
    }

    fn handle_problems_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => {
                self.problem_selected = self.problem_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                let len = self.problem_items().len();
                if len > 0 {
                    self.problem_selected = (self.problem_selected + 1).min(len - 1);
                }
            }
            KeyCode::PageUp => {
                self.problem_selected = self.problem_selected.saturating_sub(10);
            }
            KeyCode::PageDown => {
                let len = self.problem_items().len();
                if len > 0 {
                    self.problem_selected = self.problem_selected.saturating_add(10).min(len - 1);
                }
            }
            KeyCode::Enter => {
                let problems = self.problem_items();
                if let Some(problem) = problems.get(self.problem_selected) {
                    self.cursor = problem.cursor;
                    self.selection_anchor = None;
                    self.should_scroll_to_cursor = true;
                    self.mode = AppMode::Editing;
                    self.status = Some(format!(
                        "{} · Ln {}, Col {}",
                        problem.source,
                        problem.cursor.row + 1,
                        problem.cursor.col + 1
                    ));
                }
            }
            _ => {}
        }
    }

    pub fn problem_items(&self) -> Vec<ProblemItem> {
        let mut problems = Vec::new();

        if let Some(document) = self.syntax_document.as_ref() {
            for diagnostic in document.diagnostics() {
                problems.push(ProblemItem {
                    severity: match diagnostic.severity {
                        SyntaxSeverity::Error => ProblemSeverity::Error,
                    },
                    source: "Tree-sitter".to_owned(),
                    message: diagnostic.message.clone(),
                    cursor: Cursor::new(
                        diagnostic
                            .row
                            .min(self.buffer.line_count().saturating_sub(1)),
                        diagnostic.col,
                    ),
                });
            }
        }

        for diagnostic in self.language_service.diagnostics() {
            let row = diagnostic
                .range
                .start
                .line
                .min(self.buffer.line_count().saturating_sub(1));
            let col = self
                .buffer
                .grapheme_col_for_utf16(row, diagnostic.range.start.character);
            problems.push(ProblemItem {
                severity: match diagnostic.severity {
                    DiagnosticSeverity::Error => ProblemSeverity::Error,
                    DiagnosticSeverity::Warning => ProblemSeverity::Warning,
                    DiagnosticSeverity::Information => ProblemSeverity::Information,
                    DiagnosticSeverity::Hint => ProblemSeverity::Hint,
                },
                source: diagnostic
                    .source
                    .clone()
                    .unwrap_or_else(|| "Language Server".to_owned()),
                message: diagnostic.message.clone(),
                cursor: Cursor::new(row, col),
            });
        }

        problems.sort_by_key(|problem| {
            let severity = match problem.severity {
                ProblemSeverity::Error => 0,
                ProblemSeverity::Warning => 1,
                ProblemSeverity::Information => 2,
                ProblemSeverity::Hint => 3,
            };
            (severity, problem.cursor.row, problem.cursor.col)
        });
        problems
    }

    pub fn semantic_highlights_for_row(
        &self,
        row: usize,
    ) -> Vec<crate::syntax_tree::SyntaxHighlightSpan> {
        self.syntax_document
            .as_ref()
            .map(|document| document.highlight_spans_for_row(row))
            .unwrap_or_default()
    }

    pub fn language_server_label(&self) -> Option<&str> {
        self.language_service.server_label()
    }

    pub fn language_server_ready(&self) -> bool {
        self.language_service.is_ready()
    }

    fn poll_external_file_changes(&mut self) {
        if self.last_external_scan.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_external_scan = Instant::now();

        let mut reload = Vec::new();
        let mut dirty_conflicts = Vec::new();
        for index in 0..self.tabs.len() {
            let Some(buffer) = self.tab_buffer(index) else {
                continue;
            };
            if !buffer.is_persisted() {
                continue;
            }
            let Some(path) = buffer.path().cloned() else {
                continue;
            };
            match buffer.external_change() {
                Ok(Some(_)) if buffer.is_dirty() => dirty_conflicts.push(path),
                Ok(Some(_)) => reload.push(path),
                Ok(None) => {}
                Err(error) => {
                    self.status = Some(format!("External-change check failed: {error}"));
                    return;
                }
            }
        }

        for path in &reload {
            self.reload_open_path_from_disk(path);
        }
        if !dirty_conflicts.is_empty() {
            let first = &dirty_conflicts[0];
            self.status = Some(format!(
                "External change detected for dirty buffer {} · save conflict protection is active",
                first.display()
            ));
        } else if !reload.is_empty() {
            self.status = Some(format!(
                "Reloaded {} clean file(s) changed outside Mellow",
                reload.len()
            ));
        }
    }

    fn ensure_git_repository(&mut self) {
        if self.git_repository.is_some() {
            return;
        }

        match GitRepository::discover(&self.workspace_root) {
            Ok(repository) => self.git_repository = repository,
            Err(error) => {
                self.status = Some(format!("Git discovery failed: {error}"));
            }
        }
    }

    /// `force` refreshes now, on this thread: it follows a Git action the user
    /// just asked for. The periodic refresh runs in the background and is
    /// applied by a later call once it finishes.
    fn refresh_git_state(&mut self, force: bool) {
        if !force {
            self.poll_background_git_refresh();
            return;
        }
        // A forced result is newer than anything still in flight.
        self.git_refresh_receiver = None;
        self.last_git_refresh = Instant::now();
        self.ensure_git_repository();

        let Some(repository) = self.git_repository.as_ref() else {
            self.apply_git_snapshot(GitSnapshot::default());
            return;
        };

        match repository.refresh() {
            Ok(snapshot) => self.apply_git_snapshot(snapshot),
            Err(error) => self.status = Some(format!("Git refresh failed: {error}")),
        }
    }

    fn poll_background_git_refresh(&mut self) {
        if let Some(receiver) = &self.git_refresh_receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    self.git_refresh_receiver = None;
                    if self.git_repository.is_none() {
                        self.git_repository = result.repository;
                    }
                    match result.snapshot {
                        Some(Ok(snapshot)) => self.apply_git_snapshot(snapshot),
                        Some(Err(_)) => {}
                        None => self.apply_git_snapshot(GitSnapshot::default()),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.git_refresh_receiver = None,
            }
            return;
        }
        if self.last_git_refresh.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_git_refresh = Instant::now();

        let known = self.git_repository.clone();
        let root = self.workspace_root.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let repository = known.or_else(|| GitRepository::discover(&root).ok().flatten());
            let snapshot = repository
                .as_ref()
                .map(|repository| repository.refresh().map_err(|error| error.to_string()));
            let _ = sender.send(BackgroundGitRefresh {
                repository,
                snapshot,
            });
        });
        self.git_refresh_receiver = Some(receiver);
    }

    fn apply_git_snapshot(&mut self, snapshot: GitSnapshot) {
        self.git_snapshot = snapshot;
        self.changes_selected = self
            .changes_selected
            .min(self.git_snapshot.hunks.len().saturating_sub(1));
    }

    pub fn git_repository_active(&self) -> bool {
        self.git_repository.is_some()
    }

    pub fn git_line_change_for_path(
        &self,
        path: Option<&std::path::Path>,
        row: usize,
    ) -> Option<GitLineChange> {
        let repository = self.git_repository.as_ref()?;
        let path = path?;
        let absolute = self.path_identity(path);
        let relative = absolute.strip_prefix(repository.root()).ok()?;
        self.git_snapshot.line_change(relative, row)
    }

    fn selected_git_hunk(&self) -> Option<GitHunk> {
        self.git_snapshot.hunks.get(self.changes_selected).cloned()
    }

    fn git_hunk_absolute_path(&self, hunk: &GitHunk) -> Option<PathBuf> {
        self.git_repository
            .as_ref()
            .map(|repository| repository.root().join(&hunk.path))
    }

    fn open_buffer_is_dirty_for_path(&self, path: &std::path::Path) -> bool {
        let Some(index) = self.find_open_tab(path) else {
            return false;
        };

        if index == self.active_tab {
            self.buffer.is_dirty()
        } else {
            self.tabs
                .get(index)
                .and_then(|slot| slot.state.as_ref())
                .map(|state| state.buffer.is_dirty())
                .unwrap_or(false)
        }
    }

    fn begin_git_commit(&mut self) {
        if self.git_operation_blocks_start() {
            return;
        }
        self.refresh_git_state(true);
        if self.git_repository.is_none() {
            self.status = Some("No Git repository found".to_owned());
            return;
        }
        // Not cleared: a message from a failed commit stays for the retry.
        self.mode = AppMode::GitCommitInput;
        self.status = Some(
            "Commit staged changes · Enter commit · Ctrl+G draft a message · Esc cancel".to_owned(),
        );
    }

    fn handle_git_commit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.git_commit_query.clear();
                self.status = Some("Commit cancelled".to_owned());
            }
            KeyCode::Left => self.git_commit_query.move_left(),
            KeyCode::Right => self.git_commit_query.move_right(),
            KeyCode::Home => self.git_commit_query.home(),
            KeyCode::End => self.git_commit_query.end(),
            KeyCode::Backspace => {
                self.git_commit_query.backspace();
            }
            KeyCode::Delete => {
                self.git_commit_query.delete();
            }
            KeyCode::Enter => {
                let message = self.git_commit_query.as_str().trim().to_owned();
                if message.is_empty() {
                    self.status = Some("Commit message cannot be empty".to_owned());
                    return;
                }
                self.mode = AppMode::Editing;
                self.start_git_operation(GitOperationKind::Commit, move |repository| {
                    repository
                        .commit_staged(&message)
                        .map(|oid| GitOperationDone::Committed { oid })
                        .map_err(|error| error.to_string())
                });
            }
            KeyCode::Char('g')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.generate_commit_message();
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.git_commit_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn generate_commit_message(&mut self) {
        if self.commit_message_receiver.is_some() {
            self.status = Some("Already writing a commit message".to_owned());
            return;
        }
        let Some(config) = self.ai_config.clone() else {
            self.status =
                Some("AI is not set up yet · open Set up AI from the command palette".to_owned());
            return;
        };
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        let diff = match repository.staged_diff() {
            Ok(diff) => diff,
            Err(error) => {
                self.status = Some(format!("Could not read staged changes: {error}"));
                return;
            }
        };
        if diff.trim().is_empty() {
            self.status = Some("Nothing is staged to write a message for".to_owned());
            return;
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = ai::commit_message(&config, &diff).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.commit_message_receiver = Some(receiver);
        self.status = Some("Writing a commit message…".to_owned());
    }

    fn poll_commit_message(&mut self) {
        let result =
            self.commit_message_receiver
                .as_ref()
                .and_then(|receiver| match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("commit message worker disconnected".to_owned()))
                    }
                });
        let Some(result) = result else {
            return;
        };
        self.commit_message_receiver = None;
        // The user may have left the commit box while the draft was written;
        // then the draft is dropped rather than typed into another screen.
        if self.mode != AppMode::GitCommitInput {
            return;
        }
        match result {
            Ok(message) if message.is_empty() => {
                self.status = Some("AI returned an empty commit message".to_owned());
            }
            Ok(message) => {
                self.git_commit_query.set(message);
                self.status = Some("Message drafted · review it, then Enter to commit".to_owned());
            }
            Err(error) => self.status = Some(format!("AI: {error}")),
        }
    }

    fn begin_git_branches(&mut self) {
        self.refresh_git_state(true);
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        match repository.branches() {
            Ok(branches) => {
                self.git_branches = branches;
                self.git_branch_selected = 0;
                self.mode = AppMode::GitBranches;
                self.status = None;
            }
            Err(error) => self.status = Some(format!("Branch list failed: {error}")),
        }
    }

    fn switch_selected_git_branch(&mut self) {
        let Some(branch) = self.git_branches.get(self.git_branch_selected).cloned() else {
            return;
        };
        if branch.current {
            self.status = Some(format!("Already on {}", branch.name));
            self.mode = AppMode::Editing;
            return;
        }
        if self.any_dirty_tabs() {
            self.status = Some(
                "Branch switch blocked: save/discard all dirty Mellow buffers first".to_owned(),
            );
            self.mode = AppMode::Editing;
            return;
        }
        let Some(repository) = self.git_repository.clone() else {
            self.mode = AppMode::Editing;
            return;
        };
        let open_paths: Vec<PathBuf> = (0..self.tabs.len())
            .filter_map(|index| self.session_document_for_tab(index).map(|doc| doc.path))
            .collect();
        for path in &open_paths {
            match repository.branch_contains_path(&branch.name, path) {
                Ok(true) => {}
                Ok(false) => {
                    self.status = Some(format!(
                        "Branch switch blocked: {} does not exist on {}",
                        path.display(),
                        branch.name
                    ));
                    self.mode = AppMode::Editing;
                    return;
                }
                Err(error) => {
                    self.status = Some(format!("Branch switch preflight failed: {error}"));
                    self.mode = AppMode::Editing;
                    return;
                }
            }
        }
        match repository.switch_branch(&branch.name) {
            Ok(()) => {
                for path in open_paths {
                    self.reload_open_path_from_disk(&path);
                }
                self.mode = AppMode::Editing;
                self.refresh_git_state(true);
                self.status = Some(format!("Switched to {}", branch.name));
            }
            Err(error) => self.status = Some(format!("Branch switch failed: {error}")),
        }
    }

    fn handle_git_branches_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => self.git_branch_selected = self.git_branch_selected.saturating_sub(1),
            KeyCode::Down => {
                if !self.git_branches.is_empty() {
                    self.git_branch_selected =
                        (self.git_branch_selected + 1).min(self.git_branches.len() - 1);
                }
            }
            KeyCode::Home => self.git_branch_selected = 0,
            KeyCode::End => self.git_branch_selected = self.git_branches.len().saturating_sub(1),
            KeyCode::Enter => self.switch_selected_git_branch(),
            KeyCode::Char('n' | 'N') => {
                self.git_branch_query.clear();
                self.mode = AppMode::GitBranchCreate;
                self.status = Some("New local branch · Enter create · Esc cancel".to_owned());
            }
            KeyCode::Char('r' | 'R') => self.begin_git_branch_rename(),
            KeyCode::Char('d' | 'D') | KeyCode::Delete => self.begin_git_branch_delete(),
            _ => {}
        }
    }

    fn handle_git_branch_create_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.git_branch_query.clear();
                self.begin_git_branches();
            }
            KeyCode::Left => self.git_branch_query.move_left(),
            KeyCode::Right => self.git_branch_query.move_right(),
            KeyCode::Home => self.git_branch_query.home(),
            KeyCode::End => self.git_branch_query.end(),
            KeyCode::Backspace => {
                self.git_branch_query.backspace();
            }
            KeyCode::Delete => {
                self.git_branch_query.delete();
            }
            KeyCode::Enter => {
                let name = self.git_branch_query.as_str().to_owned();
                let Some(repository) = self.git_repository.clone() else {
                    self.mode = AppMode::Editing;
                    return;
                };
                match repository.create_branch(&name) {
                    Ok(()) => {
                        self.git_branch_query.clear();
                        self.begin_git_branches();
                        self.status = Some(format!("Created branch {}", name.trim()));
                    }
                    Err(error) => self.status = Some(format!("Create branch failed: {error}")),
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.git_branch_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn begin_git_branch_rename(&mut self) {
        let Some(branch) = self.git_branches.get(self.git_branch_selected).cloned() else {
            return;
        };
        self.git_branch_action_target = Some(branch.name.clone());
        self.git_branch_query.set(branch.name);
        self.mode = AppMode::GitBranchRename;
        self.status = Some("Rename local branch · Enter apply · Esc back".to_owned());
    }

    fn handle_git_branch_rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.git_branch_query.clear();
                self.git_branch_action_target = None;
                self.begin_git_branches();
            }
            KeyCode::Left => self.git_branch_query.move_left(),
            KeyCode::Right => self.git_branch_query.move_right(),
            KeyCode::Home => self.git_branch_query.home(),
            KeyCode::End => self.git_branch_query.end(),
            KeyCode::Backspace => {
                self.git_branch_query.backspace();
            }
            KeyCode::Delete => {
                self.git_branch_query.delete();
            }
            KeyCode::Enter => {
                let Some(old_name) = self.git_branch_action_target.clone() else {
                    self.mode = AppMode::Editing;
                    self.status = Some("Branch rename target is no longer available".to_owned());
                    return;
                };
                let new_name = self.git_branch_query.as_str().to_owned();
                let Some(repository) = self.git_repository.clone() else {
                    self.mode = AppMode::Editing;
                    return;
                };
                match repository.rename_branch(&old_name, &new_name) {
                    Ok(()) => {
                        self.git_branch_query.clear();
                        self.git_branch_action_target = None;
                        self.begin_git_branches();
                        self.refresh_git_state(true);
                        self.status =
                            Some(format!("Renamed branch {} → {}", old_name, new_name.trim()));
                    }
                    Err(error) => self.status = Some(format!("Rename branch failed: {error}")),
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.git_branch_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn begin_git_branch_delete(&mut self) {
        let Some(branch) = self.git_branches.get(self.git_branch_selected).cloned() else {
            return;
        };
        if branch.current {
            self.status = Some("Cannot delete the current branch".to_owned());
            return;
        }
        self.git_branch_action_target = Some(branch.name);
        self.mode = AppMode::ConfirmGitBranchDelete;
        self.status = None;
    }

    fn handle_git_branch_delete_confirmation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                let Some(name) = self.git_branch_action_target.clone() else {
                    self.mode = AppMode::Editing;
                    self.status = Some("Branch delete target is no longer available".to_owned());
                    return;
                };
                let Some(repository) = self.git_repository.clone() else {
                    self.mode = AppMode::Editing;
                    return;
                };
                match repository.delete_branch(&name) {
                    Ok(()) => {
                        self.git_branch_action_target = None;
                        self.begin_git_branches();
                        self.status = Some(format!("Deleted merged branch {name}"));
                    }
                    Err(error) => {
                        self.mode = AppMode::GitBranches;
                        self.git_branch_action_target = None;
                        self.status = Some(format!("Delete branch failed: {error}"));
                    }
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.git_branch_action_target = None;
                self.begin_git_branches();
                self.status = Some("Branch delete cancelled".to_owned());
            }
            _ => {}
        }
    }

    fn begin_git_blame(&mut self) {
        self.refresh_git_state(true);
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        if self.buffer.is_dirty() {
            self.status =
                Some("Blame blocked: save/discard active buffer changes first".to_owned());
            return;
        }
        let Some(path) = self.buffer.path().cloned() else {
            self.status = Some("Blame unavailable for untitled buffer".to_owned());
            return;
        };
        match repository.blame(&path) {
            Ok(lines) => {
                self.git_blame = lines;
                self.git_blame_selected =
                    self.cursor.row.min(self.git_blame.len().saturating_sub(1));
                if self.git_blame.is_empty() {
                    self.status = Some("No blame information for active file".to_owned());
                } else {
                    self.mode = AppMode::GitBlame;
                    self.status = None;
                }
            }
            Err(error) => self.status = Some(format!("Git blame failed: {error}")),
        }
    }

    fn handle_git_blame_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => self.git_blame_selected = self.git_blame_selected.saturating_sub(1),
            KeyCode::Down => {
                if !self.git_blame.is_empty() {
                    self.git_blame_selected =
                        (self.git_blame_selected + 1).min(self.git_blame.len() - 1);
                }
            }
            KeyCode::PageUp => self.git_blame_selected = self.git_blame_selected.saturating_sub(8),
            KeyCode::PageDown => {
                if !self.git_blame.is_empty() {
                    self.git_blame_selected = self
                        .git_blame_selected
                        .saturating_add(8)
                        .min(self.git_blame.len() - 1);
                }
            }
            KeyCode::Home => self.git_blame_selected = 0,
            KeyCode::End => self.git_blame_selected = self.git_blame.len().saturating_sub(1),
            KeyCode::Enter => {
                if let Some(line) = self.git_blame.get(self.git_blame_selected) {
                    self.cursor =
                        Cursor::new(line.row.min(self.buffer.line_count().saturating_sub(1)), 0);
                    self.selection_anchor = None;
                    self.should_scroll_to_cursor = true;
                    self.mode = AppMode::Editing;
                    self.status = Some(format!("Blame · {} · {}", line.short_oid, line.author));
                }
            }
            _ => {}
        }
    }

    /// Keeps one side of the merge-conflict block under the cursor. This is
    /// one undoable edit, and the cursor moves to the start of the result.
    fn resolve_conflict_at_cursor(&mut self, choice: crate::conflict::ConflictChoice) {
        let lines: Vec<String> = (0..self.buffer.line_count())
            .map(|row| self.buffer.line_text(row))
            .collect();
        let Some(block) = crate::conflict::block_at(&lines, self.cursor.row) else {
            self.status = Some("The cursor is not inside a merge conflict".to_owned());
            return;
        };
        let resolved = crate::conflict::resolved_lines(&lines, block, choice);
        let (start, end, replacement) = if resolved.is_empty() {
            // Nothing to keep: remove the marker lines, including one newline.
            if block.end + 1 < lines.len() {
                (
                    Cursor::new(block.start, 0),
                    Cursor::new(block.end + 1, 0),
                    String::new(),
                )
            } else if block.start > 0 {
                let above = block.start - 1;
                (
                    Cursor::new(above, self.buffer.grapheme_count(above)),
                    Cursor::new(block.end, self.buffer.grapheme_count(block.end)),
                    String::new(),
                )
            } else {
                (
                    Cursor::new(block.start, 0),
                    Cursor::new(block.end, self.buffer.grapheme_count(block.end)),
                    String::new(),
                )
            }
        } else {
            (
                Cursor::new(block.start, 0),
                Cursor::new(block.end, self.buffer.grapheme_count(block.end)),
                resolved.join("\n"),
            )
        };
        let mut cursor = self.cursor;
        self.buffer
            .replace_range(start, end, &replacement, &mut cursor);
        self.cursor = Cursor::new(
            block.start.min(self.buffer.line_count().saturating_sub(1)),
            0,
        );
        self.selection_anchor = None;
        self.should_scroll_to_cursor = true;
        self.status = Some("Kept the chosen side · Ctrl+Z undoes it".to_owned());
    }

    /// Asks AI to fix the problem on the cursor line. The line is selected as
    /// the context, so the answer comes back as a reviewable replacement.
    fn fix_problem_with_ai(&mut self) {
        let row = self.cursor.row;
        let Some(problem) = self
            .problem_items()
            .into_iter()
            .find(|problem| problem.cursor.row == row)
        else {
            self.status = Some("No problem on this line".to_owned());
            return;
        };
        if self.ai_config.is_none() {
            self.open_ai_setup(true);
            return;
        }
        self.selection_anchor = Some(Cursor::new(row, 0));
        self.cursor = Cursor::new(row, self.buffer.grapheme_count(row));
        self.ai_context = Some(self.build_ai_context());
        self.ai_query
            .set(format!("Fix this problem: {}", problem.message));
        self.ai_proposal = None;
        self.ai_receiver = None;
        self.start_ai_request();
    }

    /// Asks AI for one shell command. The answer is typed into the terminal
    /// without Enter, so the user reads it before anything runs.
    fn begin_ai_shell(&mut self) {
        if self.ai_config.is_none() {
            self.open_ai_setup(true);
            return;
        }
        self.ai_query.clear();
        self.ai_proposal = None;
        self.ai_receiver = None;
        self.ai_shell_request = true;
        self.mode = AppMode::AiPrompt;
        self.status = Some("Describe the command you need · Enter asks AI".to_owned());
    }

    fn start_shell_command(&mut self, request: String) {
        let Some(config) = self.ai_config.clone() else {
            self.ai_shell_request = false;
            self.status = Some("AI is not set up yet".to_owned());
            return;
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = ai::shell_command(&config, &request).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.shell_receiver = Some(receiver);
        self.mode = AppMode::AiWaiting;
        self.status = None;
    }

    fn poll_shell_command(&mut self) {
        if !self.ai_shell_request {
            self.shell_receiver = None;
            return;
        }
        let result = self
            .shell_receiver
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("AI request worker disconnected".to_owned()))
                }
            });
        let Some(result) = result else {
            return;
        };
        self.shell_receiver = None;
        self.ai_shell_request = false;
        // The user may have stopped waiting; then the answer is dropped.
        if self.mode != AppMode::AiWaiting {
            return;
        }
        self.mode = AppMode::Editing;
        match result {
            Ok(command) => {
                let typed = if self.terminal_visible {
                    self.active_terminal_mut()
                        .map(|terminal| terminal.write_paste(&command))
                } else {
                    None
                };
                match typed {
                    Some(Ok(())) => {
                        self.terminal_focused = true;
                        self.status = Some(format!(
                            "Typed in the terminal, not run yet: {command} · Enter there to run"
                        ));
                    }
                    _ => {
                        self.status = Some(format!(
                            "Suggested: {command} · open the terminal (Ctrl+T) to type it"
                        ));
                    }
                }
            }
            Err(error) => self.status = Some(format!("AI: {error}")),
        }
    }

    fn begin_go_to_symbol(&mut self) {
        let has_symbols = self
            .syntax_document
            .as_ref()
            .is_some_and(|document| !document.symbols().is_empty());
        if !has_symbols {
            self.status = Some("This file has no symbols to jump to".to_owned());
            return;
        }
        self.symbol_query.clear();
        self.symbol_selected = 0;
        self.mode = AppMode::Symbols;
        self.status = None;
    }

    /// Definitions whose names contain the query, in document order.
    pub fn filtered_symbols(&self) -> Vec<crate::syntax_tree::SyntaxSymbol> {
        let query = self.symbol_query.as_str().trim().to_lowercase();
        self.syntax_document
            .as_ref()
            .map(|document| document.symbols())
            .unwrap_or_default()
            .into_iter()
            .filter(|symbol| query.is_empty() || symbol.name.to_lowercase().contains(&query))
            .collect()
    }

    fn handle_symbols_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => self.symbol_selected = self.symbol_selected.saturating_sub(1),
            KeyCode::Down => {
                let len = self.filtered_symbols().len();
                if len > 0 {
                    self.symbol_selected = (self.symbol_selected + 1).min(len - 1);
                }
            }
            KeyCode::Enter => {
                let symbols = self.filtered_symbols();
                if let Some(symbol) =
                    symbols.get(self.symbol_selected.min(symbols.len().saturating_sub(1)))
                {
                    self.cursor = Cursor::new(symbol.start_row, 0);
                    self.selection_anchor = None;
                    self.should_scroll_to_cursor = true;
                    self.mode = AppMode::Editing;
                    self.status = None;
                }
            }
            KeyCode::Backspace => {
                self.symbol_query.backspace();
                self.symbol_selected = 0;
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.symbol_query.insert_char(ch);
                self.symbol_selected = 0;
            }
            _ => {}
        }
    }

    fn begin_git_conflicts(&mut self) {
        self.refresh_git_state(true);
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        match repository.conflict_paths() {
            Ok(paths) if paths.is_empty() => {
                self.mode = AppMode::Editing;
                self.status = Some("No unresolved Git conflicts".to_owned());
            }
            Ok(paths) => {
                self.git_conflicts = paths;
                self.git_conflict_selected = 0;
                self.mode = AppMode::GitConflicts;
                self.status = None;
            }
            Err(error) => self.status = Some(format!("Conflict scan failed: {error}")),
        }
    }

    fn open_selected_git_conflict(&mut self) {
        let Some(relative) = self.git_conflicts.get(self.git_conflict_selected).cloned() else {
            return;
        };
        let Some(repository) = self.git_repository.clone() else {
            self.mode = AppMode::Editing;
            return;
        };
        let path = repository.root().join(&relative);
        match self.open_path_in_tab(path) {
            Ok(()) => {
                let row = (0..self.buffer.line_count())
                    .find(|row| self.buffer.line_text(*row).starts_with("<<<<<<<"))
                    .unwrap_or(0);
                self.cursor = Cursor::new(row, 0);
                self.selection_anchor = None;
                self.should_scroll_to_cursor = true;
                self.mode = AppMode::Editing;
                self.status = Some(format!(
                    "Resolve {} manually, save it, then run Mark active conflict resolved",
                    relative.display()
                ));
            }
            Err(error) => self.status = Some(format!("Open conflict failed: {error}")),
        }
    }

    fn handle_git_conflicts_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => {
                self.git_conflict_selected = self.git_conflict_selected.saturating_sub(1)
            }
            KeyCode::Down => {
                if !self.git_conflicts.is_empty() {
                    self.git_conflict_selected =
                        (self.git_conflict_selected + 1).min(self.git_conflicts.len() - 1);
                }
            }
            KeyCode::Home => self.git_conflict_selected = 0,
            KeyCode::End => self.git_conflict_selected = self.git_conflicts.len().saturating_sub(1),
            KeyCode::Enter => self.open_selected_git_conflict(),
            KeyCode::Char('m' | 'M') => self.mark_active_git_conflict_resolved(),
            KeyCode::Char('r' | 'R') => self.begin_git_conflicts(),
            _ => {}
        }
    }

    fn has_conflict_markers(text: &str) -> bool {
        text.lines().any(|line| {
            line.starts_with("<<<<<<<")
                || line.starts_with("=======")
                || line.starts_with(">>>>>>>")
        })
    }

    fn mark_active_git_conflict_resolved(&mut self) {
        self.refresh_git_state(true);
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        let Some(path) = self.buffer.path().cloned() else {
            self.status = Some("Active buffer has no file path".to_owned());
            return;
        };
        if self.buffer.is_dirty() {
            self.status = Some(
                "Resolve conflict and save the active buffer before marking it resolved".to_owned(),
            );
            return;
        }
        if Self::has_conflict_markers(&self.buffer.contents()) {
            self.status =
                Some("Conflict markers remain in the active file; resolve them first".to_owned());
            return;
        }
        match repository.mark_resolved(&path) {
            Ok(()) => {
                self.refresh_git_state(true);
                match repository.conflict_paths() {
                    Ok(paths) if paths.is_empty() => {
                        self.git_conflicts.clear();
                        self.mode = AppMode::Editing;
                        self.status = Some(
                            "Conflict marked resolved · no unresolved paths remain".to_owned(),
                        );
                    }
                    Ok(paths) => {
                        self.git_conflicts = paths;
                        self.git_conflict_selected = self
                            .git_conflict_selected
                            .min(self.git_conflicts.len().saturating_sub(1));
                        self.mode = AppMode::GitConflicts;
                        self.status = Some("Conflict marked resolved".to_owned());
                    }
                    Err(error) => {
                        self.mode = AppMode::Editing;
                        self.status =
                            Some(format!("Conflict marked resolved; refresh failed: {error}"));
                    }
                }
            }
            Err(error) => self.status = Some(format!("Mark resolved failed: {error}")),
        }
    }

    fn git_fetch(&mut self) {
        if self.git_operation_blocks_start() {
            return;
        }
        self.refresh_git_state(true);
        self.start_git_operation(GitOperationKind::Fetch, |repository| {
            repository
                .fetch()
                .map(|()| GitOperationDone::Fetched)
                .map_err(|error| network_failure("Git fetch failed", &error))
        });
    }

    fn git_pull_fast_forward(&mut self) {
        if self.git_operation_blocks_start() {
            return;
        }
        self.refresh_git_state(true);
        if self.any_dirty_tabs() {
            self.status =
                Some("Pull blocked: save/discard all dirty Mellow buffers first".to_owned());
            return;
        }
        self.start_git_operation(GitOperationKind::Pull, |repository| {
            repository
                .fetch()
                .map_err(|error| network_failure("Git pull fetch failed", &error))?;
            repository
                .upstream_ref()
                .map(|upstream| GitOperationDone::PullFetched { upstream })
                .map_err(|error| format!("Git pull blocked: {error}"))
        });
    }

    /// Second half of a pull, after the background fetch: local and quick.
    fn finish_git_pull(&mut self, upstream: String) {
        if self.any_dirty_tabs() {
            self.status = Some(
                "Pull stopped: a buffer was edited during the fetch; save or discard, then pull again"
                    .to_owned(),
            );
            return;
        }
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        let open_paths: Vec<PathBuf> = (0..self.tabs.len())
            .filter_map(|index| self.session_document_for_tab(index).map(|doc| doc.path))
            .collect();
        for path in &open_paths {
            match repository.branch_contains_path(&upstream, path) {
                Ok(true) => {}
                Ok(false) => {
                    self.status = Some(format!(
                        "Pull blocked: {} does not exist in upstream {}",
                        path.display(),
                        upstream
                    ));
                    return;
                }
                Err(error) => {
                    self.status = Some(format!("Pull preflight failed: {error}"));
                    return;
                }
            }
        }

        match repository.fast_forward_upstream() {
            Ok(()) => {
                for path in open_paths {
                    self.reload_open_path_from_disk(&path);
                }
                self.refresh_git_state(true);
                self.status = Some(format!("Fast-forwarded from {upstream}"));
            }
            Err(error) => self.status = Some(format!("Git pull refused: {error}")),
        }
    }

    fn git_push(&mut self) {
        if self.git_operation_blocks_start() {
            return;
        }
        self.refresh_git_state(true);
        self.start_git_operation(GitOperationKind::Push, |repository| {
            repository
                .push()
                .map(|()| GitOperationDone::Pushed)
                .map_err(|error| network_failure("Git push failed", &error))
        });
    }

    /// Refuses a second fetch, pull, push or commit while one is running.
    fn git_operation_blocks_start(&mut self) -> bool {
        let Some(operation) = &self.git_operation else {
            return false;
        };
        self.status = Some(format!(
            "Git {} is still running · Ctrl+P, Cancel Git operation",
            operation.kind.name()
        ));
        true
    }

    fn start_git_operation(
        &mut self,
        kind: GitOperationKind,
        work: impl FnOnce(&GitRepository) -> Result<GitOperationDone, String> + Send + 'static,
    ) {
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        let cancel = GitCancel::default();
        let repository = repository.cancellable(cancel.clone());
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work(&repository));
        });
        let progress = format!("{}… · Ctrl+P, Cancel Git operation", kind.progress());
        self.status = Some(progress.clone());
        self.git_operation = Some(GitOperation {
            kind,
            started: Instant::now(),
            cancel,
            receiver,
            progress,
        });
    }

    fn cancel_git_operation(&mut self) {
        let Some(operation) = &self.git_operation else {
            self.status = Some("No Git operation is running".to_owned());
            return;
        };
        operation.cancel.cancel();
        self.status = Some(format!("Cancelling Git {}…", operation.kind.name()));
    }

    fn poll_git_operation(&mut self) {
        let Some(operation) = self.git_operation.as_mut() else {
            return;
        };
        let result = match operation.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                let cancelling = operation.cancel.is_cancelled();
                let showing = self.status.as_deref() == Some(operation.progress.as_str());
                if showing && !cancelling {
                    let seconds = operation.started.elapsed().as_secs();
                    if seconds > 0 {
                        operation.progress = format!(
                            "{}… {seconds}s · Ctrl+P, Cancel Git operation",
                            operation.kind.progress()
                        );
                        self.status = Some(operation.progress.clone());
                    }
                }
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => Err("Git worker stopped".to_owned()),
        };
        let Some(operation) = self.git_operation.take() else {
            return;
        };
        if operation.cancel.is_cancelled() {
            self.refresh_git_state(true);
            self.status = Some(format!("Git {} cancelled", operation.kind.name()));
            return;
        }
        match result {
            Ok(GitOperationDone::Fetched) => {
                self.refresh_git_state(true);
                self.status = Some("Git fetch complete".to_owned());
            }
            Ok(GitOperationDone::PullFetched { upstream }) => self.finish_git_pull(upstream),
            Ok(GitOperationDone::Pushed) => {
                self.refresh_git_state(true);
                self.status = Some("Git push complete".to_owned());
            }
            Ok(GitOperationDone::Committed { oid }) => {
                self.git_commit_query.clear();
                self.refresh_git_state(true);
                self.status = Some(format!("Committed {oid}"));
            }
            Err(error) if operation.kind == GitOperationKind::Commit => {
                self.status = Some(format!("Commit failed: {error} · message kept for retry"));
            }
            Err(error) => self.status = Some(error),
        }
    }

    /// Git state of the file in the active tab; `None` when it is clean,
    /// untitled or outside a repository.
    pub fn active_git_file(&self) -> Option<&crate::git::GitFileChange> {
        let repository = self.git_repository.as_ref()?;
        let absolute = self.path_identity(self.buffer.path()?);
        let relative = absolute.strip_prefix(repository.root()).ok()?;
        self.git_snapshot.file(relative)
    }

    fn git_stage_active_file(&mut self) {
        let Some(path) = self.buffer.path().cloned() else {
            self.status = Some("Save this file before staging it".to_owned());
            return;
        };
        if self.buffer.is_dirty() {
            self.status = Some("Save first: staging uses the saved file".to_owned());
            return;
        }
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        match repository.stage_file(&path) {
            Ok(()) => {
                self.refresh_git_state(true);
                self.status = Some(format!("Staged {}", self.short_path(&path)));
            }
            Err(error) => self.status = Some(format!("Stage file failed: {error}")),
        }
    }

    fn git_unstage_active_file(&mut self) {
        let Some(path) = self.buffer.path().cloned() else {
            return;
        };
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        match repository.unstage_file(&path) {
            Ok(()) => {
                self.refresh_git_state(true);
                self.status = Some(format!("Unstaged {}", self.short_path(&path)));
            }
            Err(error) => self.status = Some(format!("Unstage file failed: {error}")),
        }
    }

    fn begin_git_history(&mut self) {
        self.refresh_git_state(true);
        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("No Git repository found".to_owned());
            return;
        };
        match repository.history(100) {
            Ok(history) => {
                self.git_history = history;
                self.git_history_selected = 0;
                self.mode = AppMode::GitHistory;
                self.status = None;
            }
            Err(error) => self.status = Some(format!("Git history failed: {error}")),
        }
    }

    fn handle_git_history_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => self.git_history_selected = self.git_history_selected.saturating_sub(1),
            KeyCode::Down => {
                if !self.git_history.is_empty() {
                    self.git_history_selected =
                        (self.git_history_selected + 1).min(self.git_history.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.git_history_selected = self.git_history_selected.saturating_sub(8)
            }
            KeyCode::PageDown => {
                if !self.git_history.is_empty() {
                    self.git_history_selected = self
                        .git_history_selected
                        .saturating_add(8)
                        .min(self.git_history.len() - 1);
                }
            }
            KeyCode::Home => self.git_history_selected = 0,
            KeyCode::End => self.git_history_selected = self.git_history.len().saturating_sub(1),
            _ => {}
        }
    }

    fn begin_changes_review(&mut self) {
        self.refresh_git_state(true);
        if self.git_repository.is_none() {
            self.status = Some("No Git repository found for this workspace".to_owned());
            self.mode = AppMode::Editing;
            return;
        }

        if self.git_snapshot.hunks.is_empty() {
            self.status = Some("Git working tree is clean".to_owned());
            self.mode = AppMode::Editing;
            return;
        }

        self.changes_selected = self
            .changes_selected
            .min(self.git_snapshot.hunks.len().saturating_sub(1));
        self.mode = AppMode::Changes;
        self.status = None;
    }

    fn handle_changes_key(&mut self, key: KeyEvent) {
        // Shift+arrows scroll the selected change's diff; plain keys choose.
        if key
            .modifiers
            .contains(crossterm::event::KeyModifiers::SHIFT)
        {
            let step = match key.code {
                KeyCode::Up => Some(-1),
                KeyCode::Down => Some(1),
                KeyCode::PageUp => Some(-10),
                KeyCode::PageDown => Some(10),
                _ => None,
            };
            if let Some(step) = step {
                let last = self.selected_hunk_diff_lines().saturating_sub(1);
                self.changes_diff_scroll = self
                    .changes_diff_scroll
                    .saturating_add_signed(step)
                    .min(last);
                return;
            }
        }
        let selected_before = self.changes_selected;
        self.handle_changes_selection_key(key);
        if self.changes_selected != selected_before {
            self.changes_diff_scroll = 0;
        }
    }

    /// Number of diff lines shown for the selected change.
    pub fn selected_hunk_diff_lines(&self) -> usize {
        self.git_snapshot
            .hunks
            .get(self.changes_selected)
            .map_or(0, |hunk| {
                hunk.patch
                    .lines()
                    .skip_while(|line| !line.starts_with("@@ "))
                    .skip(1)
                    .count()
            })
    }

    fn handle_changes_selection_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => {
                self.changes_selected = self.changes_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if !self.git_snapshot.hunks.is_empty() {
                    self.changes_selected =
                        (self.changes_selected + 1).min(self.git_snapshot.hunks.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.changes_selected = self.changes_selected.saturating_sub(8);
            }
            KeyCode::PageDown => {
                if !self.git_snapshot.hunks.is_empty() {
                    self.changes_selected = self
                        .changes_selected
                        .saturating_add(8)
                        .min(self.git_snapshot.hunks.len() - 1);
                }
            }
            KeyCode::Home => self.changes_selected = 0,
            KeyCode::End => {
                self.changes_selected = self.git_snapshot.hunks.len().saturating_sub(1);
            }
            KeyCode::Enter => self.open_selected_git_hunk(),
            KeyCode::Char('s' | 'S') => self.stage_selected_git_hunk(),
            KeyCode::Char('u' | 'U') => self.unstage_selected_git_hunk(),
            KeyCode::Char('r' | 'R') => self.begin_revert_selected_git_hunk(),
            KeyCode::Char('g' | 'G') => {
                self.refresh_git_state(true);
                self.status = Some("Git status refreshed".to_owned());
            }
            _ => {}
        }
    }

    fn selected_hunk_is_safe_to_operate(&mut self, hunk: &GitHunk) -> bool {
        let Some(path) = self.git_hunk_absolute_path(hunk) else {
            self.status = Some("Git repository is unavailable".to_owned());
            return false;
        };

        if self.open_buffer_is_dirty_for_path(&path) {
            self.status = Some(
                "Save or discard editor changes before staging, unstaging, or reverting this hunk"
                    .to_owned(),
            );
            return false;
        }
        true
    }

    fn stage_selected_git_hunk(&mut self) {
        let Some(hunk) = self.selected_git_hunk() else {
            return;
        };
        if hunk.stage != GitHunkStage::Unstaged {
            self.status = Some("Selected hunk is already staged".to_owned());
            return;
        }
        if !self.selected_hunk_is_safe_to_operate(&hunk) {
            return;
        }

        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("Git repository is unavailable".to_owned());
            return;
        };
        match repository.stage_hunk(&hunk) {
            Ok(()) => {
                self.refresh_git_state(true);
                self.mode = AppMode::Changes;
                self.status = Some(format!("Staged {}", hunk.path.display()));
            }
            Err(error) => {
                self.status = Some(format!("Stage hunk failed: {error}"));
            }
        }
    }

    fn unstage_selected_git_hunk(&mut self) {
        let Some(hunk) = self.selected_git_hunk() else {
            return;
        };
        if hunk.stage != GitHunkStage::Staged {
            self.status = Some("Selected hunk is not staged".to_owned());
            return;
        }
        if !self.selected_hunk_is_safe_to_operate(&hunk) {
            return;
        }

        let Some(repository) = self.git_repository.clone() else {
            self.status = Some("Git repository is unavailable".to_owned());
            return;
        };
        match repository.unstage_hunk(&hunk) {
            Ok(()) => {
                self.refresh_git_state(true);
                self.mode = AppMode::Changes;
                self.status = Some(format!("Unstaged {}", hunk.path.display()));
            }
            Err(error) => {
                self.status = Some(format!("Unstage hunk failed: {error}"));
            }
        }
    }

    fn begin_revert_selected_git_hunk(&mut self) {
        let Some(hunk) = self.selected_git_hunk() else {
            return;
        };
        if hunk.stage != GitHunkStage::Unstaged {
            self.status = Some("Only unstaged hunks can be reverted".to_owned());
            return;
        }
        if hunk.untracked {
            self.status =
                Some("Mellow never deletes untracked files through hunk revert".to_owned());
            return;
        }
        if !self.selected_hunk_is_safe_to_operate(&hunk) {
            return;
        }

        self.revert_hunk = Some(hunk);
        self.mode = AppMode::ConfirmRevertHunk;
        self.status = None;
    }

    fn handle_revert_confirmation_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('c' | 'C') => {
                self.revert_hunk = None;
                self.mode = AppMode::Changes;
                self.status = Some("Revert cancelled".to_owned());
            }
            KeyCode::Enter | KeyCode::Char('r' | 'R') => {
                let Some(hunk) = self.revert_hunk.take() else {
                    self.mode = AppMode::Changes;
                    return;
                };
                if !self.selected_hunk_is_safe_to_operate(&hunk) {
                    self.mode = AppMode::Changes;
                    return;
                }

                let Some(repository) = self.git_repository.clone() else {
                    self.mode = AppMode::Changes;
                    self.status = Some("Git repository is unavailable".to_owned());
                    return;
                };
                match repository.revert_hunk(&hunk) {
                    Ok(()) => {
                        if let Some(path) = self.git_hunk_absolute_path(&hunk) {
                            self.reload_open_path_from_disk(&path);
                        }
                        self.refresh_git_state(true);
                        self.mode = if self.git_snapshot.hunks.is_empty() {
                            AppMode::Editing
                        } else {
                            AppMode::Changes
                        };
                        self.status = Some(format!("Reverted hunk in {}", hunk.path.display()));
                    }
                    Err(error) => {
                        self.mode = AppMode::Changes;
                        self.status = Some(format!("Revert hunk failed: {error}"));
                    }
                }
            }
            _ => {}
        }
    }

    fn open_selected_git_hunk(&mut self) {
        let Some(hunk) = self.selected_git_hunk() else {
            return;
        };
        let Some(path) = self.git_hunk_absolute_path(&hunk) else {
            self.status = Some("Git repository is unavailable".to_owned());
            return;
        };
        if !path.is_file() {
            self.status = Some(format!(
                "{} no longer exists in the working tree",
                hunk.path.display()
            ));
            return;
        }

        match self.open_path_in_tab(path) {
            Ok(()) => {
                let row = hunk
                    .target_row()
                    .min(self.buffer.line_count().saturating_sub(1));
                self.cursor = Cursor::new(row, 0);
                self.selection_anchor = None;
                self.should_scroll_to_cursor = true;
                self.mode = AppMode::Editing;
                self.status = Some(format!(
                    "{} · {} · Ln {}",
                    hunk.action_label(),
                    hunk.path.display(),
                    row + 1
                ));
            }
            Err(error) => {
                self.status = Some(format!("Open Git change failed: {error}"));
            }
        }
    }

    fn reload_open_path_from_disk(&mut self, path: &std::path::Path) {
        let Some(index) = self.find_open_tab(path) else {
            return;
        };
        let Ok(buffer) = Buffer::open(Some(path.to_path_buf())) else {
            return;
        };

        if index == self.active_tab {
            let cursor = self.cursor;
            self.buffer = buffer;
            self.cursor.row = cursor.row.min(self.buffer.line_count().saturating_sub(1));
            self.cursor.col = cursor.col.min(self.buffer.grapheme_count(self.cursor.row));
            self.selection_anchor = None;
            self.reset_active_code_intelligence();
            return;
        }

        if let Some(state) = self
            .tabs
            .get_mut(index)
            .and_then(|slot| slot.state.as_mut())
        {
            let cursor = state.cursor;
            state.buffer = buffer;
            state.cursor.row = cursor.row.min(state.buffer.line_count().saturating_sub(1));
            state.cursor.col = cursor
                .col
                .min(state.buffer.grapheme_count(state.cursor.row));
            state.selection_anchor = None;
        }
    }

    fn recovery_candidate_for(buffer: &Buffer) -> Option<RecoveryRecord> {
        // A fresh unnamed buffer has a new draft id, so it never has a journal;
        // earlier drafts are offered by `offer_orphan_drafts`.
        let key = buffer.recovery_key();
        if matches!(key, RecoveryKey::Draft(_)) {
            return None;
        }
        let loaded = recovery::load(&key).ok().flatten();
        let candidate = loaded.filter(|record| record.content != buffer.contents());
        if candidate.is_none() {
            let _ = recovery::clear(&key);
        }
        candidate
    }

    fn active_state_with_replacement(&mut self, replacement: DocumentState) -> DocumentState {
        let DocumentState {
            buffer,
            cursor,
            selection_anchor,
            secondary_cursors,
            rectangular_selection,
            scroll_row,
            scroll_col,
            visual_scroll_row,
            preferred_col,
            preferred_visual_col,
            should_scroll_to_cursor,
            recovery_candidate,
            last_journal_revision,
        } = replacement;

        DocumentState {
            buffer: std::mem::replace(&mut self.buffer, buffer),
            cursor: std::mem::replace(&mut self.cursor, cursor),
            selection_anchor: std::mem::replace(&mut self.selection_anchor, selection_anchor),
            secondary_cursors: std::mem::replace(&mut self.secondary_cursors, secondary_cursors),
            rectangular_selection: std::mem::replace(
                &mut self.rectangular_selection,
                rectangular_selection,
            ),
            scroll_row: std::mem::replace(&mut self.scroll_row, scroll_row),
            scroll_col: std::mem::replace(&mut self.scroll_col, scroll_col),
            visual_scroll_row: std::mem::replace(&mut self.visual_scroll_row, visual_scroll_row),
            preferred_col: std::mem::replace(&mut self.preferred_col, preferred_col),
            preferred_visual_col: std::mem::replace(
                &mut self.preferred_visual_col,
                preferred_visual_col,
            ),
            should_scroll_to_cursor: std::mem::replace(
                &mut self.should_scroll_to_cursor,
                should_scroll_to_cursor,
            ),
            recovery_candidate: std::mem::replace(&mut self.recovery_candidate, recovery_candidate),
            last_journal_revision: std::mem::replace(
                &mut self.last_journal_revision,
                last_journal_revision,
            ),
        }
    }

    fn blank_state(buffer: Buffer) -> DocumentState {
        let recovery_candidate = Self::recovery_candidate_for(&buffer);
        DocumentState {
            buffer,
            cursor: Cursor::new(0, 0),
            selection_anchor: None,
            secondary_cursors: Vec::new(),
            rectangular_selection: None,
            scroll_row: 0,
            scroll_col: 0,
            visual_scroll_row: 0,
            preferred_col: None,
            preferred_visual_col: None,
            should_scroll_to_cursor: true,
            recovery_candidate,
            last_journal_revision: None,
        }
    }

    fn switch_tab(&mut self, target: usize) {
        if target >= self.tabs.len() {
            return;
        }

        self.assign_tab_to_active_pane(target);
        if target == self.active_tab {
            return;
        }

        self.sync_recovery_journal();
        let Some(target_state) = self.tabs[target].state.take() else {
            return;
        };
        let old_state = self.active_state_with_replacement(target_state);
        self.tabs[self.active_tab].state = Some(old_state);
        self.active_tab = target;
        self.mode = if self.recovery_candidate.is_some() {
            AppMode::Recovery
        } else {
            AppMode::Editing
        };
        self.status = Some(format!("Switched to {}", self.active_short_path()));
        self.find_matches.clear();
        self.reset_active_code_intelligence();
    }

    fn next_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.switch_tab((self.active_tab + 1) % self.tabs.len());
        }
    }

    fn previous_tab(&mut self) {
        if self.tabs.len() > 1 {
            let target = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
            self.switch_tab(target);
        }
    }

    fn path_identity(&self, path: &std::path::Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.workspace_root.join(path)
            }
        })
    }

    fn tab_path(&self, index: usize) -> Option<&PathBuf> {
        if index == self.active_tab {
            self.buffer.path()
        } else {
            self.tabs
                .get(index)
                .and_then(|slot| slot.state.as_ref())
                .and_then(|state| state.buffer.path())
        }
    }

    fn tab_buffer(&self, index: usize) -> Option<&Buffer> {
        if index == self.active_tab {
            Some(&self.buffer)
        } else {
            self.tabs
                .get(index)
                .and_then(|slot| slot.state.as_ref())
                .map(|state| &state.buffer)
        }
    }

    fn find_open_tab(&self, path: &std::path::Path) -> Option<usize> {
        let wanted = self.path_identity(path);
        (0..self.tabs.len()).find(|&index| {
            self.tab_path(index)
                .map(|candidate| self.path_identity(candidate) == wanted)
                .unwrap_or(false)
        })
    }

    /// An empty, never-edited "Untitled" buffer. Opening a file replaces it
    /// instead of leaving a stray tab behind.
    pub fn active_tab_is_pristine(&self) -> bool {
        self.buffer.path().is_none() && !self.buffer.is_dirty() && self.buffer.byte_len() == 0
    }

    /// The start page stands in for a lone, untouched Untitled buffer.
    pub fn start_page_visible(&self) -> bool {
        self.tabs.len() == 1
            && !self.start_page_dismissed
            && !self.split_enabled()
            && self.active_tab_is_pristine()
            && self.recovery_candidate.is_none()
    }

    fn new_untitled_tab(&mut self) {
        self.start_page_dismissed = true;
        self.explorer_focused = false;
        self.terminal_focused = false;
        if self.active_tab_is_pristine() {
            self.status = Some("New file · start typing".to_owned());
            return;
        }
        self.sync_recovery_journal();
        let new_state = Self::blank_state(Buffer::empty(None));
        let old_state = self.active_state_with_replacement(new_state);
        self.tabs[self.active_tab].state = Some(old_state);
        self.tabs.push(TabSlot { state: None });
        self.active_tab = self.tabs.len() - 1;
        self.assign_tab_to_active_pane(self.active_tab);
        self.mode = AppMode::Editing;
        self.status = Some("New file · Ctrl+S chooses where to save it".to_owned());
        self.reset_active_code_intelligence();
    }

    pub(crate) fn open_path_in_tab(&mut self, path: PathBuf) -> Result<()> {
        self.note_recent_file(&path);
        if let Some(index) = self.find_open_tab(&path) {
            self.switch_tab(index);
            return Ok(());
        }

        let buffer = Buffer::open(Some(path))?;
        self.sync_recovery_journal();
        let new_state = Self::blank_state(buffer);
        if self.active_tab_is_pristine() {
            // Reuse the empty Untitled tab, like other editors do.
            let _ = self.active_state_with_replacement(new_state);
        } else {
            let old_state = self.active_state_with_replacement(new_state);
            self.tabs[self.active_tab].state = Some(old_state);
            self.tabs.push(TabSlot { state: None });
            self.active_tab = self.tabs.len() - 1;
        }
        self.assign_tab_to_active_pane(self.active_tab);
        self.mode = if self.recovery_candidate.is_some() {
            AppMode::Recovery
        } else {
            AppMode::Editing
        };
        self.status = Some(format!("Opened {}", self.active_short_path()));
        self.reset_active_code_intelligence();
        Ok(())
    }

    fn begin_quick_open(&mut self) {
        self.quick_open_query.clear();
        self.quick_open_selected = 0;
        match workspace::discover_files(&self.workspace_root) {
            Ok(files) => {
                self.quick_open_candidates = files;
                self.mode = AppMode::QuickOpen;
                self.status = None;
            }
            Err(error) => {
                self.status = Some(format!("Quick Open failed: {error}"));
            }
        }
    }

    pub fn filtered_quick_open_entries(&self) -> Vec<usize> {
        let query = self.quick_open_query.as_str();
        if is_explicit_path_query(query) {
            return (0..self.quick_open_candidates.len()).collect();
        }
        if query.trim().is_empty() {
            // Recently opened files first, most recent first; the rest keep their order.
            let mut all: Vec<usize> = (0..self.quick_open_candidates.len()).collect();
            all.sort_by_key(|&index| {
                self.recency_rank(&self.quick_open_candidates[index])
                    .unwrap_or(usize::MAX)
            });
            return all;
        }
        let mut ranked: Vec<(i64, usize)> = self
            .quick_open_candidates
            .iter()
            .enumerate()
            .filter_map(|(index, path)| {
                workspace::fuzzy_score(path, query).map(|score| (score, index))
            })
            .collect();
        // Better matches first; among equal scores, recently opened files win.
        ranked.sort_by_key(|&(score, index)| {
            (
                std::cmp::Reverse(score),
                self.recency_rank(&self.quick_open_candidates[index])
                    .unwrap_or(usize::MAX),
                index,
            )
        });
        ranked.into_iter().map(|(_, index)| index).collect()
    }

    /// Where a candidate sits in the recently-opened list, if it is there.
    fn recency_rank(&self, candidate: &std::path::Path) -> Option<usize> {
        let absolute = self.workspace_root.join(candidate);
        self.recent_files
            .iter()
            .position(|recent| *recent == absolute)
    }

    /// Moves a file to the front of the recently-opened list.
    fn note_recent_file(&mut self, path: &std::path::Path) {
        const RECENT_FILE_LIMIT: usize = 20;
        let absolute = self.workspace_root.join(path);
        self.recent_files.retain(|recent| *recent != absolute);
        self.recent_files.insert(0, absolute);
        self.recent_files.truncate(RECENT_FILE_LIMIT);
    }

    fn handle_quick_open_key(&mut self, key: KeyEvent) {
        let mut query_changed = false;
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.quick_open_query.clear();
                self.status = Some("Quick Open cancelled".to_owned());
            }
            KeyCode::Up => {
                self.quick_open_selected = self.quick_open_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                let len = self.filtered_quick_open_entries().len();
                if len > 0 {
                    self.quick_open_selected = (self.quick_open_selected + 1).min(len - 1);
                }
            }
            KeyCode::Left => self.quick_open_query.move_left(),
            KeyCode::Right => self.quick_open_query.move_right(),
            KeyCode::Home => self.quick_open_query.home(),
            KeyCode::End => self.quick_open_query.end(),
            KeyCode::Backspace => query_changed = self.quick_open_query.backspace(),
            KeyCode::Delete => query_changed = self.quick_open_query.delete(),
            KeyCode::Tab => {
                let raw = self.quick_open_query.as_str().to_owned();
                match workspace::path_completions(&self.workspace_root, &raw, 64) {
                    Ok(completions) if completions.is_empty() => {
                        self.status = Some("Quick Open: no path completion".to_owned());
                    }
                    Ok(completions) => {
                        let common = common_path_prefix(&completions);
                        let completion = if completions.len() == 1 {
                            completions[0].clone()
                        } else if common.chars().count() > raw.trim().chars().count() {
                            common
                        } else {
                            self.status = Some(format!(
                                "Quick Open: {} path completions · keep typing to narrow",
                                completions.len()
                            ));
                            String::new()
                        };
                        if !completion.is_empty() {
                            self.quick_open_query.set(completion);
                            self.quick_open_selected = 0;
                            self.status = Some(if completions.len() == 1 {
                                "Quick Open: path completed".to_owned()
                            } else {
                                format!(
                                    "Quick Open: completed common prefix across {} paths",
                                    completions.len()
                                )
                            });
                            query_changed = true;
                        }
                    }
                    Err(error) => {
                        self.status = Some(format!("Quick Open path completion failed: {error}"));
                    }
                }
            }
            KeyCode::Enter => {
                let matches = self.filtered_quick_open_entries();
                let selected = matches
                    .get(self.quick_open_selected)
                    .and_then(|&index| self.quick_open_candidates.get(index))
                    .map(|path| {
                        self.workspace_root
                            .join(expand_user_path(&path.to_string_lossy()))
                    });

                let direct = if selected.is_none() {
                    let raw = self.quick_open_query.as_str().trim();
                    if raw.contains(std::path::MAIN_SEPARATOR)
                        || raw.starts_with('.')
                        || raw.starts_with('~')
                        || std::path::Path::new(raw).is_absolute()
                    {
                        let path = expand_user_path(raw);
                        Some(if path.is_absolute() {
                            path
                        } else {
                            self.workspace_root.join(path)
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };

                let Some(path) = selected.or(direct) else {
                    self.status = Some("Quick Open: no matching file".to_owned());
                    return;
                };

                if path.is_dir() {
                    let mut query = path.to_string_lossy().into_owned();
                    query.push(std::path::MAIN_SEPARATOR);
                    self.quick_open_query.set(query);
                    self.refresh_quick_open_candidates();
                    self.quick_open_selected = 0;
                    return;
                }

                match self.open_path_in_tab(path) {
                    Ok(()) => {
                        self.quick_open_query.clear();
                        self.quick_open_selected = 0;
                    }
                    Err(error) => {
                        self.status = Some(format!("Open failed: {error}"));
                    }
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.quick_open_query.insert_char(ch);
                query_changed = true;
            }
            _ => {}
        }

        if query_changed {
            self.quick_open_selected = 0;
            self.refresh_quick_open_candidates();
        }
    }

    fn refresh_quick_open_candidates(&mut self) {
        let query = self.quick_open_query.as_str();
        let candidates = if is_explicit_path_query(query) {
            workspace::path_completions(&self.workspace_root, query, 64)
                .map(|paths| paths.into_iter().map(PathBuf::from).collect())
        } else {
            workspace::discover_files(&self.workspace_root)
        };
        match candidates {
            Ok(paths) => self.quick_open_candidates = paths,
            Err(error) => {
                self.quick_open_candidates.clear();
                self.status = Some(format!("Quick Open: {error}"));
            }
        }
    }

    fn reload_explorer(&mut self) {
        match workspace::discover_tree(&self.workspace_root, &self.explorer_expanded) {
            Ok(entries) => {
                self.explorer_files = entries;
                self.explorer_selected = self
                    .explorer_selected
                    .min(self.explorer_files.len().saturating_sub(1));
            }
            Err(error) => {
                self.explorer_files.clear();
                self.explorer_selected = 0;
                self.status = Some(format!("Explorer refresh failed: {error}"));
            }
        }
    }

    fn toggle_selected_explorer_directory(&mut self) -> bool {
        let Some(entry) = self.explorer_files.get(self.explorer_selected).cloned() else {
            return false;
        };
        if !entry.is_dir {
            return false;
        }
        if !self.explorer_expanded.remove(&entry.path) {
            self.explorer_expanded.insert(entry.path);
        }
        self.reload_explorer();
        true
    }

    fn toggle_explorer(&mut self) {
        if self.explorer_visible {
            self.explorer_visible = false;
            self.explorer_focused = false;
            self.status = Some("Explorer hidden".to_owned());
        } else {
            self.reload_explorer();
            self.explorer_visible = true;
            self.explorer_focused = true;
            self.terminal_focused = false;
            self.status = Some("Explorer focused · Esc returns to editor".to_owned());
        }
        self.persist_editor_settings();
    }

    fn handle_explorer_key(&mut self, key: KeyEvent) {
        let command = self.keymap_config.resolve(key);
        if command == Some(Command::ToggleExplorer) {
            self.toggle_explorer();
            return;
        }
        // Workbench shortcuts (save, quit, open, palette, help, ...) work from
        // Files too; the tree keeps its own plain keys.
        if let Some(command) = command.filter(|command| command.works_from_any_pane()) {
            self.execute(command);
            return;
        }

        match key.code {
            KeyCode::Esc => {
                self.explorer_focused = false;
                self.status = Some("Editor focused".to_owned());
            }
            KeyCode::Up => {
                self.explorer_selected = self.explorer_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if !self.explorer_files.is_empty() {
                    self.explorer_selected =
                        (self.explorer_selected + 1).min(self.explorer_files.len() - 1);
                }
            }
            KeyCode::Home => self.explorer_selected = 0,
            KeyCode::End => {
                self.explorer_selected = self.explorer_files.len().saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Right => {
                if self.toggle_selected_explorer_directory() {
                    return;
                }
                if let Some(entry) = self.explorer_files.get(self.explorer_selected).cloned() {
                    let path = self.workspace_root.join(entry.path);
                    match self.open_path_in_tab(path) {
                        Ok(()) => {
                            self.explorer_focused = false;
                        }
                        Err(error) => {
                            self.status = Some(format!("Open failed: {error}"));
                        }
                    }
                }
            }
            KeyCode::Left => {
                let Some(entry) = self.explorer_files.get(self.explorer_selected).cloned() else {
                    return;
                };
                if entry.is_dir && self.explorer_expanded.remove(&entry.path) {
                    self.reload_explorer();
                } else if let Some(parent) = entry.path.parent() {
                    let parent = parent.to_path_buf();
                    if let Some(index) = self
                        .explorer_files
                        .iter()
                        .position(|item| item.path == parent)
                    {
                        self.explorer_selected = index;
                    }
                }
            }
            KeyCode::Char('r' | 'R') => self.reload_explorer(),
            KeyCode::Char('n') => self.begin_explorer_create(),
            KeyCode::Char('N') => self.begin_explorer_create_directory(),
            KeyCode::F(2) => self.begin_explorer_rename(),
            KeyCode::Delete => self.begin_explorer_delete(),
            _ => {}
        }
    }

    fn selected_explorer_entry(&self) -> Option<workspace::ExplorerEntry> {
        self.explorer_files.get(self.explorer_selected).cloned()
    }

    fn open_buffer_under_path(&self, directory: &std::path::Path) -> Option<PathBuf> {
        let directory = self.path_identity(directory);
        (0..self.tabs.len()).find_map(|index| {
            let buffer = self.tab_buffer(index)?;
            let path = buffer.path()?;
            let identity = self.path_identity(path);
            identity.starts_with(&directory).then_some(identity)
        })
    }

    fn safe_workspace_relative(&self, raw: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(raw.trim());
        if path.as_os_str().is_empty() {
            return Err("path cannot be empty".to_owned());
        }
        if path.is_absolute() {
            return Err("path must stay inside the current workspace".to_owned());
        }
        for component in path.components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err(
                    "path may not contain '.', '..', roots, or platform prefixes".to_owned(),
                );
            }
        }
        Ok(path)
    }

    fn expand_explorer_ancestors(&mut self, path: &std::path::Path) {
        let mut current = path.parent();
        while let Some(parent) = current {
            if !parent.as_os_str().is_empty() {
                self.explorer_expanded.insert(parent.to_path_buf());
            }
            current = parent.parent();
        }
    }

    fn begin_explorer_create(&mut self) {
        let prefix = self.selected_explorer_entry().and_then(|entry| {
            if entry.is_dir {
                Some(entry.path)
            } else {
                entry.path.parent().map(std::path::Path::to_path_buf)
            }
        });
        let initial = prefix
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| format!("{}/", path.display()))
            .unwrap_or_default();
        self.explorer_action_query.set(&initial);
        self.explorer_action_target = None;
        self.mode = AppMode::ExplorerCreate;
        self.status = Some("Create file inside workspace · Enter create · Esc cancel".to_owned());
    }

    fn begin_explorer_create_directory(&mut self) {
        let prefix = self.selected_explorer_entry().and_then(|entry| {
            if entry.is_dir {
                Some(entry.path)
            } else {
                entry.path.parent().map(std::path::Path::to_path_buf)
            }
        });
        let initial = prefix
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| format!("{}/", path.display()))
            .unwrap_or_default();
        self.explorer_action_query.set(&initial);
        self.explorer_action_target = None;
        self.mode = AppMode::ExplorerCreateDirectory;
        self.status =
            Some("Create directory inside workspace · Enter create · Esc cancel".to_owned());
    }

    fn begin_explorer_rename(&mut self) {
        let Some(entry) = self.selected_explorer_entry() else {
            self.status = Some("Explorer has no selected file".to_owned());
            return;
        };
        let absolute = self.workspace_root.join(&entry.path);
        if entry.is_dir {
            if let Some(open_path) = self.open_buffer_under_path(&absolute) {
                self.status = Some(format!(
                    "Close open file {} before moving this directory",
                    open_path.display()
                ));
                return;
            }
        } else if self.find_open_tab(&absolute).is_some() {
            self.status = Some("Close the file before renaming or moving it".to_owned());
            return;
        }
        self.explorer_action_query
            .set(entry.path.to_string_lossy().into_owned());
        self.explorer_action_target = Some(entry.path);
        self.mode = AppMode::ExplorerRename;
        self.status =
            Some("Rename/move item inside workspace · Enter apply · Esc cancel".to_owned());
    }

    fn begin_explorer_delete(&mut self) {
        let Some(entry) = self.selected_explorer_entry() else {
            self.status = Some("Explorer has no selected file".to_owned());
            return;
        };
        let absolute = self.workspace_root.join(&entry.path);
        if entry.is_dir {
            if let Some(open_path) = self.open_buffer_under_path(&absolute) {
                self.status = Some(format!(
                    "Close open file {} before deleting this directory",
                    open_path.display()
                ));
                return;
            }
            match std::fs::read_dir(&absolute) {
                Ok(entries) => {
                    if entries.count() != 0 {
                        self.status = Some(
                            "Delete blocked: only empty directories can be removed".to_owned(),
                        );
                        return;
                    }
                }
                Err(error) => {
                    self.status = Some(format!("Delete blocked: {error}"));
                    return;
                }
            }
        } else if self.find_open_tab(&absolute).is_some() {
            self.status = Some("Close the file before deleting it".to_owned());
            return;
        }
        self.explorer_action_target = Some(entry.path);
        self.mode = AppMode::ConfirmExplorerDelete;
        self.status = None;
    }

    fn handle_explorer_create_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.explorer_action_query.clear();
                self.explorer_focused = true;
                self.status = Some("Create cancelled".to_owned());
            }
            KeyCode::Left => self.explorer_action_query.move_left(),
            KeyCode::Right => self.explorer_action_query.move_right(),
            KeyCode::Home => self.explorer_action_query.home(),
            KeyCode::End => self.explorer_action_query.end(),
            KeyCode::Backspace => {
                self.explorer_action_query.backspace();
            }
            KeyCode::Delete => {
                self.explorer_action_query.delete();
            }
            KeyCode::Enter => {
                let query = self.explorer_action_query.as_str().to_owned();
                let relative = match self.safe_workspace_relative(&query) {
                    Ok(path) => path,
                    Err(reason) => {
                        self.status = Some(format!("Create blocked: {reason}"));
                        return;
                    }
                };
                let absolute = self.workspace_root.join(&relative);
                if absolute.exists() {
                    self.status = Some("Create blocked: destination already exists".to_owned());
                    return;
                }
                let Some(parent) = absolute.parent() else {
                    self.status = Some("Create blocked: destination has no parent".to_owned());
                    return;
                };
                if !parent.is_dir() {
                    self.status =
                        Some("Create blocked: parent directory does not exist".to_owned());
                    return;
                }
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&absolute)
                {
                    Ok(_) => {
                        self.expand_explorer_ancestors(&relative);
                        self.reload_explorer();
                        if let Some(index) = self
                            .explorer_files
                            .iter()
                            .position(|entry| entry.path == relative)
                        {
                            self.explorer_selected = index;
                        }
                        self.explorer_action_query.clear();
                        self.mode = AppMode::Editing;
                        self.explorer_focused = true;
                        match self.open_path_in_tab(absolute) {
                            Ok(()) => self.status = Some(format!("Created {}", relative.display())),
                            Err(error) => {
                                self.status = Some(format!("Created file but open failed: {error}"))
                            }
                        }
                    }
                    Err(error) => self.status = Some(format!("Create failed: {error}")),
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.explorer_action_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn handle_explorer_create_directory_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.explorer_action_query.clear();
                self.explorer_focused = true;
                self.status = Some("Create directory cancelled".to_owned());
            }
            KeyCode::Left => self.explorer_action_query.move_left(),
            KeyCode::Right => self.explorer_action_query.move_right(),
            KeyCode::Home => self.explorer_action_query.home(),
            KeyCode::End => self.explorer_action_query.end(),
            KeyCode::Backspace => {
                self.explorer_action_query.backspace();
            }
            KeyCode::Delete => {
                self.explorer_action_query.delete();
            }
            KeyCode::Enter => {
                let query = self.explorer_action_query.as_str().to_owned();
                let relative = match self.safe_workspace_relative(&query) {
                    Ok(path) => path,
                    Err(reason) => {
                        self.status = Some(format!("Create directory blocked: {reason}"));
                        return;
                    }
                };
                let absolute = self.workspace_root.join(&relative);
                if absolute.exists() {
                    self.status =
                        Some("Create directory blocked: destination already exists".to_owned());
                    return;
                }
                let Some(parent) = absolute.parent() else {
                    self.status =
                        Some("Create directory blocked: destination has no parent".to_owned());
                    return;
                };
                if !parent.is_dir() {
                    self.status = Some(
                        "Create directory blocked: parent directory does not exist".to_owned(),
                    );
                    return;
                }
                match std::fs::create_dir(&absolute) {
                    Ok(()) => {
                        self.expand_explorer_ancestors(&relative);
                        self.reload_explorer();
                        if let Some(index) = self
                            .explorer_files
                            .iter()
                            .position(|entry| entry.path == relative)
                        {
                            self.explorer_selected = index;
                        }
                        self.explorer_action_query.clear();
                        self.mode = AppMode::Editing;
                        self.explorer_focused = true;
                        self.status = Some(format!("Created directory {}", relative.display()));
                        self.refresh_git_state(true);
                    }
                    Err(error) => self.status = Some(format!("Create directory failed: {error}")),
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.explorer_action_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn handle_explorer_rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.explorer_action_query.clear();
                self.explorer_action_target = None;
                self.explorer_focused = true;
                self.status = Some("Rename cancelled".to_owned());
            }
            KeyCode::Left => self.explorer_action_query.move_left(),
            KeyCode::Right => self.explorer_action_query.move_right(),
            KeyCode::Home => self.explorer_action_query.home(),
            KeyCode::End => self.explorer_action_query.end(),
            KeyCode::Backspace => {
                self.explorer_action_query.backspace();
            }
            KeyCode::Delete => {
                self.explorer_action_query.delete();
            }
            KeyCode::Enter => {
                let Some(source_relative) = self.explorer_action_target.clone() else {
                    self.status = Some("Rename target is no longer available".to_owned());
                    self.mode = AppMode::Editing;
                    return;
                };
                let query = self.explorer_action_query.as_str().to_owned();
                let destination_relative = match self.safe_workspace_relative(&query) {
                    Ok(path) => path,
                    Err(reason) => {
                        self.status = Some(format!("Rename blocked: {reason}"));
                        return;
                    }
                };
                if destination_relative == source_relative {
                    self.mode = AppMode::Editing;
                    self.explorer_focused = true;
                    self.status = Some("Rename unchanged".to_owned());
                    return;
                }
                let source = self.workspace_root.join(&source_relative);
                let destination = self.workspace_root.join(&destination_relative);
                if destination.exists() {
                    self.status = Some("Rename blocked: destination already exists".to_owned());
                    return;
                }
                if !destination.parent().is_some_and(std::path::Path::is_dir) {
                    self.status = Some(
                        "Rename blocked: destination parent directory does not exist".to_owned(),
                    );
                    return;
                }
                if source.is_dir() {
                    if let Some(open_path) = self.open_buffer_under_path(&source) {
                        self.status = Some(format!(
                            "Rename blocked: close open file {} first",
                            open_path.display()
                        ));
                        return;
                    }
                } else if self.find_open_tab(&source).is_some() {
                    self.status = Some("Rename blocked: close the source file first".to_owned());
                    return;
                }
                match std::fs::rename(&source, &destination) {
                    Ok(()) => {
                        let moved_expanded: Vec<PathBuf> = self
                            .explorer_expanded
                            .iter()
                            .filter_map(|path| {
                                path.strip_prefix(&source_relative)
                                    .ok()
                                    .map(|suffix| destination_relative.join(suffix))
                            })
                            .collect();
                        self.explorer_expanded
                            .retain(|path| !path.starts_with(&source_relative));
                        self.explorer_expanded.extend(moved_expanded);
                        self.expand_explorer_ancestors(&destination_relative);
                        self.reload_explorer();
                        if let Some(index) = self
                            .explorer_files
                            .iter()
                            .position(|entry| entry.path == destination_relative)
                        {
                            self.explorer_selected = index;
                        }
                        self.explorer_action_query.clear();
                        self.explorer_action_target = None;
                        self.mode = AppMode::Editing;
                        self.explorer_focused = true;
                        self.status = Some(format!(
                            "Moved {} → {}",
                            source_relative.display(),
                            destination_relative.display()
                        ));
                        self.refresh_git_state(true);
                    }
                    Err(error) => self.status = Some(format!("Rename failed: {error}")),
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.explorer_action_query.insert_char(ch)
            }
            _ => {}
        }
    }

    fn handle_explorer_delete_confirmation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                let Some(relative) = self.explorer_action_target.clone() else {
                    self.mode = AppMode::Editing;
                    self.status = Some("Delete target is no longer available".to_owned());
                    return;
                };
                let absolute = self.workspace_root.join(&relative);
                let is_directory = absolute.is_dir();
                if is_directory {
                    if let Some(open_path) = self.open_buffer_under_path(&absolute) {
                        self.mode = AppMode::Editing;
                        self.status = Some(format!(
                            "Delete blocked: close open file {} first",
                            open_path.display()
                        ));
                        return;
                    }
                } else if self.find_open_tab(&absolute).is_some() {
                    self.mode = AppMode::Editing;
                    self.status = Some("Delete blocked: close the file first".to_owned());
                    return;
                }

                let result = if is_directory {
                    std::fs::remove_dir(&absolute)
                } else {
                    std::fs::remove_file(&absolute)
                };
                match result {
                    Ok(()) => {
                        self.explorer_action_target = None;
                        self.explorer_expanded
                            .retain(|path| !path.starts_with(&relative));
                        self.reload_explorer();
                        self.mode = AppMode::Editing;
                        self.explorer_focused = true;
                        self.status = Some(format!(
                            "Deleted {} {}",
                            if is_directory { "directory" } else { "file" },
                            relative.display()
                        ));
                        self.refresh_git_state(true);
                    }
                    Err(error) => self.status = Some(format!("Delete failed: {error}")),
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.explorer_action_target = None;
                self.mode = AppMode::Editing;
                self.explorer_focused = true;
                self.status = Some("Delete cancelled".to_owned());
            }
            _ => {}
        }
    }

    pub fn split_enabled(&self) -> bool {
        self.secondary_pane_tab.is_some()
    }

    fn resize_split_to_column(&mut self, total_width: u16, column: u16) {
        if !ui::rendered_split(total_width, self.explorer_visible, self.split_enabled()) {
            return;
        }
        let explorer = ui::explorer_width(total_width, self.explorer_visible, self.split_enabled());
        let available = total_width.saturating_sub(explorer);
        let content = available.saturating_sub(1).max(2);
        let local = column.saturating_sub(explorer).min(content);
        let ratio = ((u32::from(local) * 100) / u32::from(content)) as u16;
        self.split_ratio = ratio.clamp(20, 80);
        self.should_scroll_to_cursor = true;
    }

    pub fn active_pane_index(&self) -> u8 {
        self.active_pane
    }

    pub fn split_ratio(&self) -> u16 {
        self.split_ratio
    }

    pub fn split_orientation(&self) -> SplitOrientation {
        self.split_orientation
    }

    fn toggle_split_orientation(&mut self) {
        if !self.split_enabled() {
            self.status = Some("Open a split editor before changing orientation".to_owned());
            return;
        }
        self.split_orientation = self.split_orientation.toggled();
        self.is_resizing_split = false;
        self.status = Some(format!(
            "Split orientation: {}",
            self.split_orientation.label()
        ));
        self.sync_session_state();
    }

    fn resize_split_to_row(&mut self, editor_top: u16, editor_bottom: u16, row: u16) {
        let available = editor_bottom.saturating_sub(editor_top);
        let content = available.saturating_sub(1).max(2);
        let local = row.saturating_sub(editor_top).min(content);
        let ratio = ((u32::from(local) * 100) / u32::from(content)) as u16;
        self.split_ratio = ratio.clamp(20, 80);
        self.should_scroll_to_cursor = true;
    }

    fn active_pane_horizontal_geometry(&self, terminal_width: u16) -> (u16, u16) {
        let explorer =
            ui::explorer_width(terminal_width, self.explorer_visible, self.split_enabled());
        let available = terminal_width.saturating_sub(explorer).max(1);
        if self.split_enabled()
            && self.split_orientation == SplitOrientation::Stacked
            && ui::rendered_split(terminal_width, self.explorer_visible, true)
        {
            (explorer, available)
        } else {
            ui::pane_geometry(
                terminal_width,
                self.explorer_visible,
                self.split_enabled(),
                self.split_ratio,
                self.active_pane,
            )
            .unwrap_or((explorer, available))
        }
    }

    fn active_pane_vertical_geometry(
        &self,
        terminal_width: u16,
        editor_top: u16,
        editor_bottom: u16,
    ) -> (u16, u16) {
        if self.split_enabled()
            && self.split_orientation == SplitOrientation::Stacked
            && ui::rendered_split(terminal_width, self.explorer_visible, true)
            && let Some(divider) =
                ui::split_divider_row(editor_top, editor_bottom, true, self.split_ratio)
        {
            if self.active_pane == 0 {
                return (editor_top, divider.saturating_sub(editor_top).max(1));
            }
            let start = divider.saturating_add(1);
            return (start, editor_bottom.saturating_sub(start).max(1));
        }
        (editor_top, editor_bottom.saturating_sub(editor_top).max(1))
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }

    pub fn active_cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn ai_status(&self) -> AiStatus {
        if self.mode == AppMode::AiWaiting {
            AiStatus::Working
        } else if self.ai_config.is_some() {
            AiStatus::Ready
        } else {
            AiStatus::NotConfigured
        }
    }

    /// `path` relative to the project when it lives inside it, otherwise the
    /// path itself (absolute paths stay absolute).
    pub fn workspace_relative(&self, path: &std::path::Path) -> PathBuf {
        if let Ok(relative) = path.strip_prefix(&self.workspace_root) {
            return relative.to_path_buf();
        }
        let root = self.path_identity(&self.workspace_root);
        let absolute = self.path_identity(path);
        absolute
            .strip_prefix(&root)
            .map(std::path::Path::to_path_buf)
            .unwrap_or(absolute)
    }

    /// A short, human path for messages: project-relative, else `~/…`.
    pub fn short_path(&self, path: &std::path::Path) -> String {
        let relative = self.workspace_relative(path);
        if relative.is_relative() {
            return relative.display().to_string();
        }
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
            && let Ok(rest) = relative.strip_prefix(&home)
        {
            return format!("~/{}", rest.display());
        }
        relative.display().to_string()
    }

    pub fn active_display_path(&self) -> String {
        self.active_short_path()
    }

    fn active_short_path(&self) -> String {
        self.buffer
            .path()
            .map(|path| self.short_path(path))
            .unwrap_or_else(|| "Untitled".to_owned())
    }

    pub fn buffer_outside_workspace(&self) -> bool {
        self.buffer
            .path()
            .is_some_and(|path| self.workspace_relative(path).is_absolute())
    }

    /// Git and unsaved-state markers for explorer rows, keyed by
    /// project-relative path. Folders that contain changes get `Contains`.
    pub fn explorer_badges(&self) -> HashMap<PathBuf, ui::ExplorerBadge> {
        let mut badges = HashMap::new();
        let mark_parents = |badges: &mut HashMap<PathBuf, ui::ExplorerBadge>,
                            path: &std::path::Path| {
            for parent in path.ancestors().skip(1) {
                if parent.as_os_str().is_empty() {
                    break;
                }
                badges
                    .entry(parent.to_path_buf())
                    .or_insert(ui::ExplorerBadge::Contains);
            }
        };
        if let Some(repository) = self.git_repository.as_ref() {
            let root = self.path_identity(&self.workspace_root);
            let repo_root = self.path_identity(repository.root());
            if let Ok(prefix) = root.strip_prefix(&repo_root) {
                for file in &self.git_snapshot.files {
                    let Ok(relative) = file.path.strip_prefix(prefix) else {
                        continue;
                    };
                    let badge = if file.index_status == 'U'
                        || file.worktree_status == 'U'
                        || (file.index_status == 'A' && file.worktree_status == 'A')
                    {
                        ui::ExplorerBadge::Conflict
                    } else if file.untracked
                        || file.index_status == 'A'
                        || file.worktree_status == 'A'
                    {
                        ui::ExplorerBadge::Added
                    } else if file.index_status == 'D' || file.worktree_status == 'D' {
                        ui::ExplorerBadge::Deleted
                    } else {
                        ui::ExplorerBadge::Modified
                    };
                    badges.insert(relative.to_path_buf(), badge);
                    mark_parents(&mut badges, relative);
                }
            }
        }
        for index in 0..self.tabs.len() {
            let dirty = if index == self.active_tab {
                self.buffer.is_dirty()
            } else {
                self.tabs[index]
                    .state
                    .as_ref()
                    .is_some_and(|state| state.buffer.is_dirty())
            };
            if let Some(path) = self.tab_path(index).filter(|_| dirty) {
                let relative = self.workspace_relative(path);
                if relative.is_relative() {
                    mark_parents(&mut badges, &relative);
                    badges.insert(relative, ui::ExplorerBadge::Unsaved);
                }
            }
        }
        badges
    }

    pub fn breadcrumb_segments(&self) -> Vec<String> {
        let Some(path) = self.buffer.path() else {
            return vec!["Untitled".to_owned()];
        };
        let relative = self.workspace_relative(path);
        let display_path = relative.as_path();
        let mut segments: Vec<String> = display_path
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        if segments.is_empty() {
            segments.push(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            );
        }
        segments
    }

    /// The file path plus the definition the cursor is in, for the breadcrumb bar.
    pub fn breadcrumb_with_symbol(&self) -> Vec<String> {
        let mut segments = self.breadcrumb_segments();
        if let Some(symbol) = self.enclosing_symbol() {
            segments.push(symbol.name);
        }
        segments
    }

    /// The innermost definition that contains the cursor row, for the breadcrumb.
    pub fn enclosing_symbol(&self) -> Option<crate::syntax_tree::SyntaxSymbol> {
        let row = self.cursor.row;
        self.syntax_document
            .as_ref()?
            .symbols()
            .into_iter()
            .filter(|symbol| symbol.start_row <= row && row <= symbol.end_row)
            .max_by_key(|symbol| symbol.start_row)
    }

    pub fn explorer_entry_expanded(&self, path: &std::path::Path) -> bool {
        self.explorer_expanded.contains(path)
    }

    pub fn explorer_action_target(&self) -> Option<&std::path::Path> {
        self.explorer_action_target.as_deref()
    }

    pub fn git_branch_action_target(&self) -> Option<&str> {
        self.git_branch_action_target.as_deref()
    }

    pub fn pane_tab_index(&self, pane: u8) -> Option<usize> {
        match pane {
            0 => Some(self.primary_pane_tab),
            1 => self.secondary_pane_tab,
            _ => None,
        }
        .filter(|index| *index < self.tabs.len())
    }

    pub fn pane_view(&self, pane: u8) -> Option<EditorPaneView<'_>> {
        let tab_index = self.pane_tab_index(pane)?;
        let focused = pane == self.active_pane;
        if tab_index == self.active_tab {
            return Some(EditorPaneView {
                buffer: &self.buffer,
                cursor: self.cursor,
                selection_anchor: self.selection_anchor,
                secondary_cursors: &self.secondary_cursors,
                scroll_row: self.scroll_row,
                scroll_col: self.scroll_col,
                visual_scroll_row: self.visual_scroll_row,
                focused,
            });
        }

        let state = self.tabs.get(tab_index)?.state.as_ref()?;
        Some(EditorPaneView {
            buffer: &state.buffer,
            cursor: state.cursor,
            selection_anchor: state.selection_anchor,
            secondary_cursors: &state.secondary_cursors,
            scroll_row: state.scroll_row,
            scroll_col: state.scroll_col,
            visual_scroll_row: state.visual_scroll_row,
            focused,
        })
    }

    fn pane_for_tab(&self, tab_index: usize) -> Option<u8> {
        if self.primary_pane_tab == tab_index {
            Some(0)
        } else if self.secondary_pane_tab == Some(tab_index) {
            Some(1)
        } else {
            None
        }
    }

    fn assign_tab_to_active_pane(&mut self, tab_index: usize) {
        if let Some(existing_pane) = self.pane_for_tab(tab_index) {
            self.active_pane = existing_pane;
            return;
        }

        if self.secondary_pane_tab.is_some() && self.active_pane == 1 {
            self.secondary_pane_tab = Some(tab_index);
        } else {
            self.primary_pane_tab = tab_index;
            if self.secondary_pane_tab.is_none() {
                self.active_pane = 0;
            }
        }
    }

    fn split_editor(&mut self) {
        if self.secondary_pane_tab.is_some() {
            self.focus_next_pane();
            return;
        }

        self.primary_pane_tab = self.active_tab;
        let secondary = if self.tabs.len() > 1 {
            (self.active_tab + 1) % self.tabs.len()
        } else {
            self.active_tab
        };
        self.secondary_pane_tab = Some(secondary);
        self.active_pane = 0;
        self.explorer_focused = false;
        self.status = Some("Split editor opened · F6 changes pane focus".to_owned());
    }

    fn focus_pane(&mut self, pane: u8) {
        let Some(target) = self.pane_tab_index(pane) else {
            return;
        };

        self.active_pane = pane;
        self.explorer_focused = false;
        self.terminal_focused = false;
        if target != self.active_tab {
            self.switch_tab(target);
        } else {
            self.status = Some(format!("Pane {} focused", usize::from(pane) + 1));
        }
    }

    fn focus_next_pane(&mut self) {
        if self.secondary_pane_tab.is_none() {
            self.status = Some("No split editor is open".to_owned());
            return;
        }
        self.focus_pane(if self.active_pane == 0 { 1 } else { 0 });
    }

    fn close_split(&mut self) {
        self.primary_pane_tab = self.active_tab;
        self.secondary_pane_tab = None;
        self.active_pane = 0;
        self.status = Some("Split editor closed".to_owned());
    }

    fn repair_panes_after_tab_removal(&mut self, removed: usize) {
        let fallback = self.active_tab;
        self.primary_pane_tab = remap_tab_index(self.primary_pane_tab, removed, fallback);
        self.secondary_pane_tab = self
            .secondary_pane_tab
            .map(|index| remap_tab_index(index, removed, fallback));

        if self.tabs.len() <= 1 {
            self.primary_pane_tab = self.active_tab;
            self.secondary_pane_tab = None;
            self.active_pane = 0;
        } else if self.active_pane == 1 {
            self.secondary_pane_tab = Some(self.active_tab);
        } else {
            self.primary_pane_tab = self.active_tab;
        }
    }

    pub fn tab_summaries(&self) -> Vec<TabSummary> {
        let paths: Vec<Option<String>> = (0..self.tabs.len())
            .map(|index| {
                self.tab_path(index)
                    .map(|path| path.to_string_lossy().into_owned())
            })
            .collect();

        let basenames: Vec<String> = paths
            .iter()
            .map(|path| {
                path.as_ref()
                    .and_then(|path| std::path::Path::new(path).file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_owned())
            })
            .collect();

        (0..self.tabs.len())
            .map(|index| {
                let duplicate = basenames
                    .iter()
                    .enumerate()
                    .any(|(other, name)| other != index && name == &basenames[index]);
                let label = if duplicate {
                    paths[index]
                        .as_ref()
                        .map(|path| {
                            let path = std::path::Path::new(path);
                            let parent = path
                                .parent()
                                .and_then(std::path::Path::file_name)
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            if parent.is_empty() {
                                basenames[index].clone()
                            } else {
                                format!("{parent}/{}", basenames[index])
                            }
                        })
                        .unwrap_or_else(|| basenames[index].clone())
                } else {
                    basenames[index].clone()
                };

                let dirty = if index == self.active_tab {
                    self.buffer.is_dirty()
                } else {
                    self.tabs[index]
                        .state
                        .as_ref()
                        .map(|state| state.buffer.is_dirty())
                        .unwrap_or(false)
                };

                TabSummary {
                    label,
                    dirty,
                    active: index == self.active_tab,
                }
            })
            .collect()
    }

    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    pub fn has_unsaved_tabs(&self) -> bool {
        self.any_dirty_tabs()
    }

    fn any_dirty_tabs(&self) -> bool {
        (0..self.tabs.len()).any(|index| {
            if index == self.active_tab {
                self.buffer.is_dirty()
            } else {
                self.tabs[index]
                    .state
                    .as_ref()
                    .map(|state| state.buffer.is_dirty())
                    .unwrap_or(false)
            }
        })
    }

    fn first_dirty_tab(&self) -> Option<usize> {
        (0..self.tabs.len()).find(|&index| {
            if index == self.active_tab {
                self.buffer.is_dirty()
            } else {
                self.tabs[index]
                    .state
                    .as_ref()
                    .map(|state| state.buffer.is_dirty())
                    .unwrap_or(false)
            }
        })
    }

    pub fn unsaved_count(&self) -> usize {
        self.tab_summaries().iter().filter(|tab| tab.dirty).count()
    }

    #[cfg(test)]
    pub fn focus_label(&self) -> &'static str {
        if !matches!(self.mode, AppMode::Editing | AppMode::Completion) {
            "DIALOG"
        } else if self.terminal_focused {
            "TERMINAL"
        } else if self.explorer_focused {
            "FILES"
        } else {
            "EDITOR"
        }
    }

    fn begin_quit(&mut self) {
        if let Some(index) = self.first_dirty_tab() {
            if index != self.active_tab {
                self.switch_tab(index);
            }
            self.confirmation_selected = 2;
            self.mode = AppMode::ConfirmQuit;
        } else {
            self.should_quit = true;
        }
    }

    fn continue_quit(&mut self) {
        self.begin_quit();
    }

    fn request_close_active_tab(&mut self) {
        if self.buffer.is_dirty() {
            self.confirmation_selected = 0;
            self.mode = AppMode::ConfirmCloseTab;
        } else {
            self.close_active_tab_now(true);
        }
    }

    fn handle_close_tab_confirmation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => {
                self.confirmation_selected = self.confirmation_selected.saturating_sub(1);
            }
            KeyCode::Right => {
                self.confirmation_selected = (self.confirmation_selected + 1).min(2);
            }
            KeyCode::Tab => {
                self.confirmation_selected = (self.confirmation_selected + 1) % 3;
            }
            KeyCode::BackTab => {
                self.confirmation_selected = (self.confirmation_selected + 2) % 3;
            }
            KeyCode::Home => self.confirmation_selected = 0,
            KeyCode::End => self.confirmation_selected = 2,
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = Some("Close tab cancelled".to_owned());
            }
            KeyCode::Char('s' | 'S') => self.request_save(PostSaveAction::CloseTab),
            KeyCode::Char('d' | 'D') => self.close_active_tab_now(true),
            KeyCode::Enter => match self.confirmation_selected {
                0 => self.request_save(PostSaveAction::CloseTab),
                1 => self.close_active_tab_now(true),
                _ => {
                    self.mode = AppMode::Editing;
                    self.status = Some("Close tab cancelled".to_owned());
                }
            },
            _ => {}
        }
    }

    fn close_active_tab_now(&mut self, clear_recovery: bool) {
        let closing_path = self.buffer.path().cloned();
        let closing_key = self.buffer.recovery_key();
        if self.language_service_active {
            self.language_service
                .close_document(closing_path.as_deref());
        }
        if clear_recovery {
            let _ = self.journal.clear_now(&closing_key);
        }

        if self.tabs.len() == 1 {
            self.buffer = Buffer::empty(None);
            self.cursor = Cursor::new(0, 0);
            self.selection_anchor = None;
            self.scroll_row = 0;
            self.scroll_col = 0;
            self.visual_scroll_row = 0;
            self.preferred_col = None;
            self.preferred_visual_col = None;
            self.should_scroll_to_cursor = true;
            self.recovery_candidate = None;
            self.last_journal_revision = None;
            self.tabs[0].state = None;
            self.active_tab = 0;
            self.primary_pane_tab = 0;
            self.secondary_pane_tab = None;
            self.active_pane = 0;
            self.mode = AppMode::Editing;
            self.status = Some("Closed file".to_owned());
            self.reset_active_code_intelligence();
            return;
        }

        let old_index = self.active_tab;
        self.tabs.remove(old_index);
        let target = old_index.min(self.tabs.len() - 1);
        let target_state = self.tabs[target]
            .state
            .take()
            .expect("inactive tab must hold document state");
        let _closed = self.active_state_with_replacement(target_state);
        self.active_tab = target;
        self.repair_panes_after_tab_removal(old_index);
        self.mode = if self.recovery_candidate.is_some() {
            AppMode::Recovery
        } else {
            AppMode::Editing
        };
        self.status = Some(format!("Closed · now showing {}", self.active_short_path()));
        self.reset_active_code_intelligence();
    }

    fn finish_post_save(&mut self, action: PostSaveAction) {
        match action {
            PostSaveAction::None => {}
            PostSaveAction::CloseTab => self.close_active_tab_now(true),
            PostSaveAction::Quit => self.continue_quit(),
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::Enter) {
            self.mode = AppMode::Editing;
            self.status = None;
        } else if self.keymap_config.resolve(key) == Some(Command::ShowPalette) {
            self.mode = AppMode::Editing;
            self.execute(Command::ShowPalette);
        }
    }

    /// Shortcut text for `command` that this terminal can actually deliver.
    pub fn shortcut_label(&self, command: Command) -> Option<String> {
        self.keymap_config
            .shortcut_label(command, crate::terminal::keyboard_enhanced())
    }

    fn handle_palette_key(&mut self, key: KeyEvent) {
        let mut query_changed = false;
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            KeyCode::Up => {
                self.palette_selected = self.palette_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                let len = self.filtered_palette_entries().len();
                if len > 0 {
                    self.palette_selected = (self.palette_selected + 1).min(len - 1);
                }
            }
            KeyCode::Left => self.palette_query.move_left(),
            KeyCode::Right => self.palette_query.move_right(),
            KeyCode::Home => self.palette_query.home(),
            KeyCode::End => self.palette_query.end(),
            KeyCode::Enter => {
                let matches = self.filtered_palette_entries();
                if let Some(index) = matches.get(self.palette_selected).copied() {
                    let command = COMMAND_SPECS[index].command;
                    self.mode = AppMode::Editing;
                    self.palette_query.clear();
                    self.palette_selected = 0;
                    self.execute(command);
                }
            }
            KeyCode::Backspace => query_changed = self.palette_query.backspace(),
            KeyCode::Delete => query_changed = self.palette_query.delete(),
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
                ) =>
            {
                self.palette_query.insert_char(ch);
                query_changed = true;
            }
            _ => {}
        }

        if query_changed {
            self.palette_selected = 0;
        }
    }

    fn discard_current_and_continue_quit(&mut self) {
        if self.tabs.len() == 1 {
            let _ = self.journal.clear_now(&self.buffer.recovery_key());
            self.should_quit = true;
        } else {
            self.close_active_tab_now(true);
            self.continue_quit();
        }
    }

    fn handle_quit_confirmation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => {
                self.confirmation_selected = self.confirmation_selected.saturating_sub(1);
            }
            KeyCode::Right => {
                self.confirmation_selected = (self.confirmation_selected + 1).min(2);
            }
            KeyCode::Tab => {
                self.confirmation_selected = (self.confirmation_selected + 1) % 3;
            }
            KeyCode::BackTab => {
                self.confirmation_selected = (self.confirmation_selected + 2) % 3;
            }
            KeyCode::Home => self.confirmation_selected = 0,
            KeyCode::End => self.confirmation_selected = 2,
            KeyCode::Esc => {
                self.mode = AppMode::Editing;
                self.status = Some("Quit cancelled".to_owned());
            }
            KeyCode::Char('d' | 'D') => self.discard_current_and_continue_quit(),
            KeyCode::Char('s' | 'S') => self.request_save(PostSaveAction::Quit),
            KeyCode::Char('a' | 'A')
                if self
                    .status
                    .as_deref()
                    .is_some_and(|s| s.starts_with("Save failed")) =>
            {
                self.begin_save_as(PostSaveAction::Quit)
            }
            KeyCode::Enter => match self.confirmation_selected {
                0 => self.request_save(PostSaveAction::Quit),
                1 => self.discard_current_and_continue_quit(),
                _ => {
                    self.mode = AppMode::Editing;
                    self.status = Some("Quit cancelled".to_owned());
                }
            },
            _ => {}
        }
    }

    fn register_click(&mut self, column: u16, row: u16) -> u8 {
        let now = Instant::now();
        let repeated = self.last_click_pos == Some((column, row))
            && self
                .last_click_at
                .map(|last| now.duration_since(last) <= Duration::from_millis(450))
                .unwrap_or(false);
        self.click_count = if repeated {
            if self.click_count >= 3 {
                1
            } else {
                self.click_count + 1
            }
        } else {
            1
        };
        self.last_click_at = Some(now);
        self.last_click_pos = Some((column, row));
        self.click_count
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, terminal_width: u16, terminal_height: u16) {
        if self.mode == AppMode::Settings {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let width = 72.min(terminal_width.saturating_sub(2)).max(1);
                let height = 16.min(terminal_height.saturating_sub(2)).max(1);
                let x = (terminal_width - width) / 2;
                let y = (terminal_height - height) / 2 + 3;
                if mouse.column > x
                    && mouse.column < x + width - 1
                    && mouse.row >= y
                    && mouse.row < y + 9
                {
                    self.settings_selected = usize::from(mouse.row - y);
                    self.handle_settings_key(KeyEvent::new(
                        KeyCode::Enter,
                        crossterm::event::KeyModifiers::NONE,
                    ));
                }
            }
            return;
        }
        if self.mode == AppMode::ConfirmQuit {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                let failed = self
                    .status
                    .as_deref()
                    .is_some_and(|s| s.starts_with("Save failed"));
                let popup = ui::quit_dialog_rect(
                    ratatui::layout::Rect::new(0, 0, terminal_width, terminal_height),
                    failed,
                );
                let (popup_x, popup_y, popup_width, popup_height) =
                    (popup.x, popup.y, popup.width, popup.height);
                if failed
                    && mouse.row == popup_y + 7
                    && mouse.column >= popup_x + 2
                    && mouse.column < popup_x + 14
                {
                    self.begin_save_as(PostSaveAction::Quit);
                    return;
                }

                if mouse.row == popup_y + 3 {
                    let inner_x = popup_x + 2;
                    if mouse.column >= inner_x && mouse.column < inner_x + 17 {
                        self.request_save(PostSaveAction::Quit);
                        return;
                    }
                    if mouse.column >= inner_x + 20 && mouse.column < inner_x + 33 {
                        if self.tabs.len() == 1 {
                            let _ = self.journal.clear_now(&self.buffer.recovery_key());
                            self.should_quit = true;
                        } else {
                            self.close_active_tab_now(true);
                            self.continue_quit();
                        }
                        return;
                    }
                    if mouse.column >= inner_x + 36 && mouse.column < inner_x + 50 {
                        self.mode = AppMode::Editing;
                        self.status = Some("Quit cancelled".to_owned());
                        return;
                    }
                }

                if mouse.column < popup_x
                    || mouse.column >= popup_x + popup_width
                    || mouse.row < popup_y
                    || mouse.row >= popup_y + popup_height
                {
                    self.mode = AppMode::Editing;
                    self.status = Some("Quit cancelled".to_owned());
                }
            }
            return;
        }

        if self.mode == AppMode::ConfirmCloseTab {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                let popup_width = 60.min(terminal_width.saturating_sub(2)).max(1);
                let popup_height = 7.min(terminal_height.saturating_sub(2)).max(1);
                let popup_x = (terminal_width.saturating_sub(popup_width)) / 2;
                let popup_y = (terminal_height.saturating_sub(popup_height)) / 2;
                let inner_x = popup_x + 2;

                if mouse.row == popup_y + 3 {
                    if mouse.column >= inner_x && mouse.column < inner_x + 18 {
                        self.request_save(PostSaveAction::CloseTab);
                    } else if mouse.column >= inner_x + 21 && mouse.column < inner_x + 34 {
                        self.close_active_tab_now(true);
                    } else if mouse.column >= inner_x + 37 && mouse.column < inner_x + 51 {
                        self.mode = AppMode::Editing;
                        self.status = Some("Close tab cancelled".to_owned());
                    }
                }
            }
            return;
        }

        if self.mode == AppMode::SaveConflict
            || self.mode == AppMode::Recovery
            || self.mode == AppMode::ConfirmRevertHunk
        {
            return;
        }

        if self.mode == AppMode::Help
            || self.mode == AppMode::Find
            || self.mode == AppMode::Replace
            || self.mode == AppMode::ProjectSearch
            || self.mode == AppMode::GoToLine
            || self.mode == AppMode::SaveAs
            || self.mode == AppMode::QuickOpen
            || self.mode == AppMode::Completion
            || self.mode == AppMode::Problems
            || self.mode == AppMode::LanguageStatus
            || self.mode == AppMode::Changes
        {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                self.mode = AppMode::Editing;
                self.status = None;
            }
            return;
        }

        if self.mode != AppMode::Editing {
            return;
        }

        if self.terminal_visible
            && ui::terminal_contains_row(terminal_height, self.terminal_panel(), mouse.row)
        {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                self.terminal_focused = true;
                self.explorer_focused = false;
                self.status = Some("Terminal focused · Ctrl+` returns to editor".to_owned());
            }

            if let Some((cols, rows)) =
                ui::terminal_content_size(terminal_width, terminal_height, self.terminal_panel())
            {
                let content_top =
                    ui::editor_bottom_row(terminal_height, self.terminal_panel()).saturating_add(1);
                let content_left = 1u16;
                if mouse.column >= content_left
                    && mouse.column < content_left.saturating_add(cols)
                    && mouse.row >= content_top
                    && mouse.row < content_top.saturating_add(rows)
                {
                    let local_col = mouse.column.saturating_sub(content_left);
                    let local_row = mouse.row.saturating_sub(content_top);
                    let wheel = match mouse.kind {
                        MouseEventKind::ScrollUp => Some(3),
                        MouseEventKind::ScrollDown => Some(-3),
                        _ => None,
                    };
                    if let Some(delta) = wheel
                        && let Some(session) = self.active_terminal_mut()
                        && !session.wants_mouse()
                    {
                        session.scroll_view(delta);
                    } else if let Some(session) = self.active_terminal_mut()
                        && let Err(error) = session.write_mouse_event(mouse, local_col, local_row)
                    {
                        self.terminal_status =
                            Some(format!("Terminal mouse input failed: {error}"));
                    }
                }
            }
            return;
        }

        let editor_top = ui::HEADER_ROWS;
        let footer_h = ui::footer_rows(terminal_height);
        let editor_bottom = ui::editor_bottom_row(terminal_height, self.terminal_panel())
            .max(editor_top)
            .min(terminal_height.saturating_sub(footer_h));
        let explorer_width =
            ui::explorer_width(terminal_width, self.explorer_visible, self.split_enabled());

        if explorer_width > 0
            && mouse.row >= editor_top
            && mouse.row < editor_bottom
            && mouse.column < explorer_width
        {
            // Hit-test against the same inner area the explorer renders into.
            let list_top = editor_top.saturating_add(ui::EXPLORER_TITLE_ROWS);
            let list_height = editor_bottom.saturating_sub(list_top);
            if mouse.row < list_top {
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.explorer_focused = true;
                    self.terminal_focused = false;
                }
                return;
            }
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.explorer_selected =
                        self.explorer_selected.saturating_sub(MOUSE_SCROLL_LINES);
                }
                MouseEventKind::ScrollDown => {
                    if !self.explorer_files.is_empty() {
                        self.explorer_selected = self
                            .explorer_selected
                            .saturating_add(MOUSE_SCROLL_LINES)
                            .min(self.explorer_files.len() - 1);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.terminal_focused = false;
                    self.explorer_focused = true;
                    let start = ui::explorer_scroll_start(self, list_height);
                    let relative_row = usize::from(mouse.row - list_top);
                    let list_height = usize::from(list_height);
                    if relative_row < list_height {
                        let index = start + relative_row;
                        if index < self.explorer_files.len() {
                            self.explorer_selected = index;
                            if let Some(entry) = self.explorer_files.get(index).cloned() {
                                if entry.is_dir {
                                    if !self.explorer_expanded.remove(&entry.path) {
                                        self.explorer_expanded.insert(entry.path);
                                    }
                                    self.reload_explorer();
                                } else {
                                    let path = self.workspace_root.join(entry.path);
                                    match self.open_path_in_tab(path) {
                                        Ok(()) => self.explorer_focused = false,
                                        Err(error) => {
                                            self.status = Some(format!("Open failed: {error}"));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        if ui::rendered_split(terminal_width, self.explorer_visible, self.split_enabled())
            && mouse.row >= editor_top
            && mouse.row < editor_bottom
        {
            match self.split_orientation {
                SplitOrientation::SideBySide => {
                    let divider = ui::split_divider_column(
                        terminal_width,
                        self.explorer_visible,
                        self.split_enabled(),
                        self.split_ratio,
                    );
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left)
                            if divider == Some(mouse.column) =>
                        {
                            self.is_resizing_split = true;
                            self.status =
                                Some("Drag to resize split · release to keep width".to_owned());
                            return;
                        }
                        MouseEventKind::Drag(MouseButton::Left) if self.is_resizing_split => {
                            self.resize_split_to_column(terminal_width, mouse.column);
                            return;
                        }
                        MouseEventKind::Up(MouseButton::Left) if self.is_resizing_split => {
                            self.resize_split_to_column(terminal_width, mouse.column);
                            self.is_resizing_split = false;
                            self.status = Some(format!(
                                "Split width: {}% / {}%",
                                self.split_ratio,
                                100 - self.split_ratio
                            ));
                            self.sync_session_state();
                            return;
                        }
                        _ => {}
                    }
                }
                SplitOrientation::Stacked => {
                    let divider = ui::split_divider_row(
                        editor_top,
                        editor_bottom,
                        self.split_enabled(),
                        self.split_ratio,
                    );
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) if divider == Some(mouse.row) => {
                            self.is_resizing_split = true;
                            self.status =
                                Some("Drag to resize split · release to keep height".to_owned());
                            return;
                        }
                        MouseEventKind::Drag(MouseButton::Left) if self.is_resizing_split => {
                            self.resize_split_to_row(editor_top, editor_bottom, mouse.row);
                            return;
                        }
                        MouseEventKind::Up(MouseButton::Left) if self.is_resizing_split => {
                            self.resize_split_to_row(editor_top, editor_bottom, mouse.row);
                            self.is_resizing_split = false;
                            self.status = Some(format!(
                                "Split height: {}% / {}%",
                                self.split_ratio,
                                100 - self.split_ratio
                            ));
                            self.sync_session_state();
                            return;
                        }
                        _ => {}
                    }
                }
            }
        } else if self.is_resizing_split
            && matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
        {
            self.is_resizing_split = false;
            self.status = Some(format!(
                "Split ratio: {}% / {}%",
                self.split_ratio,
                100 - self.split_ratio
            ));
            self.sync_session_state();
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.terminal_focused = false;
            self.explorer_focused = false;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.word_wrap {
                    self.visual_scroll_row =
                        self.visual_scroll_row.saturating_sub(MOUSE_SCROLL_LINES);
                } else {
                    self.scroll_row = self.scroll_row.saturating_sub(MOUSE_SCROLL_LINES);
                }
                self.should_scroll_to_cursor = false;
            }
            MouseEventKind::ScrollDown => {
                if self.word_wrap {
                    let pane_width = self.active_pane_horizontal_geometry(terminal_width).1;
                    let gutter = ui::gutter_width(self.buffer.line_count());
                    let text_width = pane_width.saturating_sub(gutter).max(1) as usize;
                    let footer_h = ui::footer_rows(terminal_height);
                    let editor_top = ui::HEADER_ROWS;
                    let editor_bottom =
                        ui::editor_bottom_row(terminal_height, self.terminal_panel())
                            .max(editor_top)
                            .min(terminal_height.saturating_sub(footer_h));
                    let (_, pane_height) = self.active_pane_vertical_geometry(
                        terminal_width,
                        editor_top,
                        editor_bottom,
                    );
                    let editor_rows = pane_height.max(1) as usize;
                    let layout = VisualLayout::new(&self.buffer, text_width, TAB_WIDTH, true);
                    self.visual_scroll_row = layout.clamp_scroll(
                        self.visual_scroll_row.saturating_add(MOUSE_SCROLL_LINES),
                        editor_rows,
                    );
                } else {
                    let max_scroll = self.buffer.line_count().saturating_sub(1);
                    self.scroll_row = (self.scroll_row + MOUSE_SCROLL_LINES).min(max_scroll);
                }
                self.should_scroll_to_cursor = false;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.preferred_col = None;
                self.should_scroll_to_cursor = true;
                let chrome = ui::chrome_rows(terminal_height);
                if terminal_height <= chrome {
                    return;
                }

                let editor_top = ui::HEADER_ROWS;
                let footer_h = ui::footer_rows(terminal_height);
                let editor_bottom = ui::editor_bottom_row(terminal_height, self.terminal_panel())
                    .max(editor_top)
                    .min(terminal_height.saturating_sub(footer_h));

                if mouse.row < editor_top {
                    if mouse.row == 0 {
                        match ui::header_hit(self, terminal_width, mouse.column) {
                            Some(ui::HeaderHit::Brand) => self.execute(Command::ToggleExplorer),
                            Some(ui::HeaderHit::Tab(index)) => {
                                self.switch_tab(index);
                                self.explorer_focused = false;
                                self.terminal_focused = false;
                            }
                            Some(ui::HeaderHit::CloseTab(index)) => {
                                self.switch_tab(index);
                                self.execute(Command::CloseTab);
                            }
                            Some(ui::HeaderHit::Action(command)) => self.execute(command),
                            None => {}
                        }
                    } else {
                        let explorer = ui::explorer_width(
                            terminal_width,
                            self.explorer_visible,
                            self.split_enabled(),
                        );
                        if mouse.column < explorer {
                            self.explorer_focused = true;
                            self.terminal_focused = false;
                        } else {
                            // The breadcrumb is a doorway to every other file.
                            self.execute(Command::OpenFile);
                        }
                    }
                    return;
                }

                if mouse.row >= editor_bottom {
                    if mouse.row == terminal_height.saturating_sub(1)
                        && let Some(command) =
                            ui::status_command_at(self, terminal_width, mouse.column)
                    {
                        self.execute(command);
                    }
                    return;
                }

                if self.start_page_visible() {
                    let explorer = ui::explorer_width(
                        terminal_width,
                        self.explorer_visible,
                        self.split_enabled(),
                    );
                    let area = ratatui::layout::Rect::new(
                        explorer,
                        editor_top,
                        terminal_width.saturating_sub(explorer),
                        editor_bottom.saturating_sub(editor_top),
                    );
                    self.explorer_focused = false;
                    self.terminal_focused = false;
                    match ui::start_page_hit(self, area, mouse.column, mouse.row) {
                        Some(ui::StartAction::Run(command)) => self.execute(command),
                        Some(ui::StartAction::Open(path)) => {
                            if let Err(error) = self.open_path_in_tab(path) {
                                self.status = Some(format!("Open failed: {error}"));
                            }
                        }
                        None => {}
                    }
                    return;
                }

                let split_rendered =
                    ui::rendered_split(terminal_width, self.explorer_visible, self.split_enabled());
                let (hit_pane, local_column, pane_width, local_row) =
                    if split_rendered && self.split_orientation == SplitOrientation::Stacked {
                        let explorer = ui::explorer_width(
                            terminal_width,
                            self.explorer_visible,
                            self.split_enabled(),
                        );
                        if mouse.column < explorer {
                            return;
                        }
                        let Some((pane, pane_row, _pane_height)) = ui::stacked_pane_hit(
                            editor_top,
                            editor_bottom,
                            self.split_ratio,
                            mouse.row,
                        ) else {
                            return;
                        };
                        (
                            pane,
                            mouse.column.saturating_sub(explorer),
                            terminal_width.saturating_sub(explorer).max(1),
                            pane_row,
                        )
                    } else {
                        let Some((pane, column, width)) = ui::editor_pane_hit(
                            terminal_width,
                            self.explorer_visible,
                            self.split_enabled(),
                            self.split_ratio,
                            mouse.column,
                        ) else {
                            return;
                        };
                        (pane, column, width, mouse.row.saturating_sub(editor_top))
                    };
                let pane = if split_rendered {
                    hit_pane
                } else if self.split_enabled() {
                    self.active_pane
                } else {
                    0
                };
                if pane != self.active_pane {
                    self.focus_pane(pane);
                }

                let gutter = ui::gutter_width(self.buffer.line_count());
                let screen_col = local_column.saturating_sub(gutter) as usize;
                let (row, col) = if self.word_wrap {
                    let text_width = pane_width.saturating_sub(gutter).max(1) as usize;
                    let layout = VisualLayout::new(&self.buffer, text_width, TAB_WIDTH, true);
                    let visual_row = self.visual_scroll_row + usize::from(local_row);
                    let cursor =
                        layout.cursor_for_visual_position(&self.buffer, visual_row, screen_col);
                    (cursor.row, cursor.col)
                } else {
                    let row = self.scroll_row + usize::from(local_row);
                    if row >= self.buffer.line_count() {
                        return;
                    }
                    let display_col = if local_column <= gutter {
                        0
                    } else {
                        self.scroll_col + screen_col
                    };
                    (
                        row,
                        self.buffer
                            .grapheme_col_at_display_column(row, display_col, TAB_WIDTH),
                    )
                };
                self.cursor = Cursor::new(row, col);

                let rectangular_gesture = !self.word_wrap
                    && mouse
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::ALT)
                    && mouse
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::SHIFT);
                if rectangular_gesture {
                    let display_col = if local_column <= gutter {
                        0
                    } else {
                        self.scroll_col + screen_col
                    };
                    self.secondary_cursors.clear();
                    self.selection_anchor = None;
                    self.rectangular_selection = Some(RectangularSelection {
                        anchor_row: row,
                        active_row: row,
                        anchor_display_col: display_col,
                        active_display_col: display_col,
                    });
                    self.is_mouse_dragging = true;
                    self.status =
                        Some("Rectangular selection · Alt+Shift drag · release to keep".to_owned());
                    return;
                }

                self.rectangular_selection = None;
                let click_count = self.register_click(mouse.column, mouse.row);

                match click_count {
                    2 => {
                        if let Some((start, end)) =
                            self.buffer.word_range(self.cursor.row, self.cursor.col)
                        {
                            self.selection_anchor = Some(start);
                            self.cursor = end;
                            self.status = Some("Word selected".to_owned());
                        } else {
                            self.selection_anchor = Some(self.cursor);
                        }
                    }
                    3 => {
                        self.selection_anchor = Some(Cursor::new(row, 0));
                        self.cursor = if row + 1 < self.buffer.line_count() {
                            Cursor::new(row + 1, 0)
                        } else {
                            Cursor::new(row, self.buffer.grapheme_count(row))
                        };
                        self.status = Some("Line selected".to_owned());
                    }
                    _ => {
                        self.selection_anchor = Some(self.cursor);
                    }
                }

                self.is_mouse_dragging = true;
                self.keep_cursor_visible(terminal_width, terminal_height);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if !self.is_mouse_dragging {
                    return;
                }
                self.preferred_col = None;
                self.should_scroll_to_cursor = false;
                let chrome = ui::chrome_rows(terminal_height);
                if terminal_height <= chrome {
                    return;
                }
                let editor_top = ui::HEADER_ROWS;
                let footer_h = ui::footer_rows(terminal_height);
                let editor_bottom = ui::editor_bottom_row(terminal_height, self.terminal_panel())
                    .max(editor_top)
                    .min(terminal_height.saturating_sub(footer_h));
                let (pane_top, pane_height) =
                    self.active_pane_vertical_geometry(terminal_width, editor_top, editor_bottom);
                let pane_bottom = pane_top.saturating_add(pane_height);
                let last_row = self.buffer.line_count().saturating_sub(1);
                let editor_rows = pane_height.max(1) as usize;
                let (pane_x, pane_width) = self.active_pane_horizontal_geometry(terminal_width);
                let gutter = ui::gutter_width(self.buffer.line_count());
                let local_column = mouse.column.saturating_sub(pane_x);
                let screen_col = local_column.saturating_sub(gutter) as usize;

                if self.rectangular_selection.is_some() && !self.word_wrap {
                    let row = if mouse.row < pane_top {
                        self.scroll_row = self.scroll_row.saturating_sub(1);
                        self.scroll_row
                    } else if mouse.row >= pane_bottom {
                        self.scroll_row = (self.scroll_row + 1).min(last_row);
                        (self.scroll_row + editor_rows.saturating_sub(1)).min(last_row)
                    } else {
                        (self.scroll_row + usize::from(mouse.row - pane_top)).min(last_row)
                    };
                    let display_col = if local_column <= gutter {
                        0
                    } else {
                        self.scroll_col + screen_col
                    };
                    if let Some(selection) = self.rectangular_selection.as_mut() {
                        selection.active_row = row;
                        selection.active_display_col = display_col;
                    }
                    self.cursor = Cursor::new(
                        row,
                        self.buffer
                            .grapheme_col_at_display_column(row, display_col, TAB_WIDTH),
                    );
                    return;
                }

                if self.word_wrap {
                    let text_width = pane_width.saturating_sub(gutter).max(1) as usize;
                    let layout = VisualLayout::new(&self.buffer, text_width, TAB_WIDTH, true);
                    let visual_row = if mouse.row < pane_top {
                        self.visual_scroll_row = self.visual_scroll_row.saturating_sub(1);
                        self.visual_scroll_row
                    } else if mouse.row >= pane_bottom {
                        self.visual_scroll_row = layout
                            .clamp_scroll(self.visual_scroll_row.saturating_add(1), editor_rows);
                        (self.visual_scroll_row + editor_rows.saturating_sub(1))
                            .min(layout.len().saturating_sub(1))
                    } else {
                        (self.visual_scroll_row + usize::from(mouse.row - pane_top))
                            .min(layout.len().saturating_sub(1))
                    };
                    self.cursor =
                        layout.cursor_for_visual_position(&self.buffer, visual_row, screen_col);
                } else {
                    let row = if mouse.row < pane_top {
                        self.scroll_row = self.scroll_row.saturating_sub(1);
                        self.scroll_row
                    } else if mouse.row >= pane_bottom {
                        self.scroll_row = (self.scroll_row + 1).min(last_row);
                        (self.scroll_row + editor_rows.saturating_sub(1)).min(last_row)
                    } else {
                        (self.scroll_row + usize::from(mouse.row - pane_top)).min(last_row)
                    };

                    let display_col = if local_column <= gutter {
                        0
                    } else {
                        self.scroll_col + screen_col
                    };
                    self.cursor.row = row;
                    self.cursor.col =
                        self.buffer
                            .grapheme_col_at_display_column(row, display_col, TAB_WIDTH);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let was_selecting = self.is_mouse_dragging;
                self.is_mouse_dragging = false;
                if self.click_count == 1 && self.selection_anchor == Some(self.cursor) {
                    self.selection_anchor = None;
                }
                if was_selecting && self.copy_on_select {
                    self.copy_active_selection_to_clipboard();
                }
            }
            _ => {}
        }
    }

    fn all_edit_cursors(&self) -> Vec<Cursor> {
        let mut cursors = Vec::with_capacity(self.secondary_cursors.len() + 1);
        cursors.push(self.cursor);
        cursors.extend(self.secondary_cursors.iter().copied());
        cursors.sort();
        cursors.dedup();
        cursors
    }

    fn add_cursor_vertical(&mut self, direction: isize) {
        if self.selection_anchor.is_some() {
            self.selection_anchor = None;
        }
        let cursors = self.all_edit_cursors();
        let source = if direction < 0 {
            cursors.first().copied().unwrap_or(self.cursor)
        } else {
            cursors.last().copied().unwrap_or(self.cursor)
        };
        let target_row = if direction < 0 {
            source.row.checked_sub(1)
        } else if source.row + 1 < self.buffer.line_count() {
            Some(source.row + 1)
        } else {
            None
        };
        let Some(row) = target_row else {
            self.status = Some("No additional line available for another cursor".to_owned());
            return;
        };
        let target = Cursor::new(row, source.col.min(self.buffer.grapheme_count(row)));
        if target == self.cursor || self.secondary_cursors.contains(&target) {
            return;
        }
        self.secondary_cursors.push(target);
        self.secondary_cursors.sort();
        self.secondary_cursors.dedup();
        self.status = Some(format!(
            "{} cursors active",
            self.secondary_cursors.len() + 1
        ));
    }

    fn set_multi_cursor_results(&mut self, ordered_sources: &[Cursor], results: Vec<Cursor>) {
        if results.is_empty() {
            return;
        }
        let primary_index = ordered_sources
            .iter()
            .position(|cursor| *cursor == self.cursor)
            .unwrap_or(0)
            .min(results.len().saturating_sub(1));
        self.cursor = results[primary_index];
        self.secondary_cursors = results
            .into_iter()
            .enumerate()
            .filter_map(|(index, cursor)| (index != primary_index).then_some(cursor))
            .collect();
        self.secondary_cursors.sort();
        self.secondary_cursors.dedup();
    }

    fn apply_text_to_all_cursors(&mut self, text: &str, pair_backtrack: bool) -> bool {
        let cursors = self.all_edit_cursors();
        if cursors.len() <= 1 {
            return false;
        }
        let replacements: Vec<_> = cursors
            .iter()
            .map(|cursor| (*cursor, *cursor, text.to_owned()))
            .collect();
        let pre_cursor = self.cursor;
        let results = self
            .buffer
            .replace_ranges_with_cursors(&replacements, self.cursor);
        if results.is_empty() {
            return true;
        }
        self.set_multi_cursor_results(&cursors, results);
        if pair_backtrack {
            self.cursor.col = self.cursor.col.saturating_sub(1);
            for cursor in &mut self.secondary_cursors {
                cursor.col = cursor.col.saturating_sub(1);
            }
        }
        self.selection_anchor = None;
        self.buffer
            .set_last_history_state(pre_cursor, None, self.cursor, None);
        true
    }

    fn multi_cursor_backspace(&mut self) -> bool {
        let cursors = self.all_edit_cursors();
        if cursors.len() <= 1 {
            return false;
        }
        let mut tagged = Vec::new();
        for cursor in cursors {
            let range = if cursor.col > 0 {
                Some((Cursor::new(cursor.row, cursor.col - 1), cursor))
            } else if cursor.row > 0 {
                Some((
                    Cursor::new(cursor.row - 1, self.buffer.grapheme_count(cursor.row - 1)),
                    cursor,
                ))
            } else {
                None
            };
            if let Some((start, end)) = range {
                tagged.push((start, end, String::new(), cursor));
            }
        }
        if tagged.is_empty() {
            return true;
        }
        tagged.sort_by_key(|(start, end, _, _)| (*start, *end));
        let ordered_sources: Vec<Cursor> = tagged.iter().map(|(_, _, _, source)| *source).collect();
        let replacements: Vec<_> = tagged
            .iter()
            .map(|(start, end, replacement, _)| (*start, *end, replacement.clone()))
            .collect();
        let pre_cursor = self.cursor;
        let results = self
            .buffer
            .replace_ranges_with_cursors(&replacements, self.cursor);
        self.set_multi_cursor_results(&ordered_sources, results);
        self.selection_anchor = None;
        self.buffer
            .set_last_history_state(pre_cursor, None, self.cursor, None);
        true
    }

    fn multi_cursor_delete(&mut self) -> bool {
        let cursors = self.all_edit_cursors();
        if cursors.len() <= 1 {
            return false;
        }
        let mut tagged = Vec::new();
        for cursor in cursors {
            let line_len = self.buffer.grapheme_count(cursor.row);
            let range = if cursor.col < line_len {
                Some((cursor, Cursor::new(cursor.row, cursor.col + 1)))
            } else if cursor.row + 1 < self.buffer.line_count() {
                Some((cursor, Cursor::new(cursor.row + 1, 0)))
            } else {
                None
            };
            if let Some((start, end)) = range {
                tagged.push((start, end, String::new(), cursor));
            }
        }
        if tagged.is_empty() {
            return true;
        }
        tagged.sort_by_key(|(start, end, _, _)| (*start, *end));
        let ordered_sources: Vec<Cursor> = tagged.iter().map(|(_, _, _, source)| *source).collect();
        let replacements: Vec<_> = tagged
            .iter()
            .map(|(start, end, replacement, _)| (*start, *end, replacement.clone()))
            .collect();
        let pre_cursor = self.cursor;
        let results = self
            .buffer
            .replace_ranges_with_cursors(&replacements, self.cursor);
        self.set_multi_cursor_results(&ordered_sources, results);
        self.selection_anchor = None;
        self.buffer
            .set_last_history_state(pre_cursor, None, self.cursor, None);
        true
    }

    pub fn rectangular_row_range(&self, row: usize) -> Option<(usize, usize)> {
        let selection = self.rectangular_selection?;
        if self.word_wrap {
            return None;
        }
        let start_row = selection.anchor_row.min(selection.active_row);
        let end_row = selection.anchor_row.max(selection.active_row);
        if row < start_row || row > end_row {
            return None;
        }
        let start_display = selection
            .anchor_display_col
            .min(selection.active_display_col);
        let end_display = selection
            .anchor_display_col
            .max(selection.active_display_col);
        if start_display == end_display {
            return None;
        }
        let mut start = self
            .buffer
            .grapheme_col_at_display_column(row, start_display, TAB_WIDTH);
        let mut end = self
            .buffer
            .grapheme_col_at_display_column(row, end_display, TAB_WIDTH);
        let count = self.buffer.grapheme_count(row);
        start = start.min(count);
        end = end.min(count);
        if end <= start && start < count {
            end = start + 1;
        }
        (start < end).then_some((start, end))
    }

    fn rectangular_ranges(&self) -> Vec<(Cursor, Cursor)> {
        let Some(selection) = self.rectangular_selection else {
            return Vec::new();
        };
        let start_row = selection.anchor_row.min(selection.active_row);
        let end_row = selection
            .anchor_row
            .max(selection.active_row)
            .min(self.buffer.line_count().saturating_sub(1));
        (start_row..=end_row)
            .filter_map(|row| {
                self.rectangular_row_range(row)
                    .map(|(start, end)| (Cursor::new(row, start), Cursor::new(row, end)))
            })
            .collect()
    }

    fn rectangular_text(&self) -> Option<String> {
        let ranges = self.rectangular_ranges();
        if ranges.is_empty() {
            return None;
        }
        Some(
            ranges
                .into_iter()
                .map(|(start, end)| self.buffer.get_text_range(start, end))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    fn store_clipboard(&mut self, text: String, verb: &str) {
        let count = text.chars().count();
        let delivery = copy_to_system_clipboard(&text);
        self.clipboard = text;
        self.status = Some(delivery.message(verb, count));
    }

    fn copy_active_selection_to_clipboard(&mut self) -> bool {
        let text = if let Some(text) = self.rectangular_text() {
            text
        } else if let Some((start, end)) = self.selection_range() {
            self.buffer.get_text_range(start, end)
        } else {
            return false;
        };

        if text.is_empty() {
            return false;
        }

        self.store_clipboard(text, "Copied");
        true
    }

    fn replace_rectangular_selection(&mut self, replacement: &str) -> bool {
        let ranges = self.rectangular_ranges();
        if ranges.is_empty() {
            return false;
        }
        let replacements: Vec<_> = ranges
            .iter()
            .map(|(start, end)| (*start, *end, replacement.to_owned()))
            .collect();
        let pre_cursor = self.cursor;
        let results = self
            .buffer
            .replace_ranges_with_cursors(&replacements, self.cursor);
        if results.is_empty() {
            return false;
        }
        self.cursor = results[0];
        self.secondary_cursors = results.into_iter().skip(1).collect();
        self.selection_anchor = None;
        self.rectangular_selection = None;
        self.buffer
            .set_last_history_state(pre_cursor, None, self.cursor, None);
        true
    }

    fn selected_line_rows(&self) -> (usize, usize) {
        if let Some((start, end)) = self.selection_range() {
            let end_row = if end.row > start.row && end.col == 0 {
                end.row - 1
            } else {
                end.row
            };
            (start.row, end_row)
        } else {
            (self.cursor.row, self.cursor.row)
        }
    }

    fn execute(&mut self, command: Command) {
        let revision_before = self.buffer.revision();
        let completion_edit = match command {
            Command::Insert(ch) => Some(Some(ch)),
            Command::Backspace => Some(None),
            _ => None,
        };
        if completion_edit.is_none() && command != Command::TriggerCompletion {
            self.auto_completion_due = None;
            self.auto_completion_trigger = None;
            self.completion_request = None;
        }
        self.status = None;
        self.should_scroll_to_cursor = true;
        match command {
            Command::MoveUp
            | Command::MoveDown
            | Command::PageUp
            | Command::PageDown
            | Command::SelectUp
            | Command::SelectDown
            | Command::SelectPageUp
            | Command::SelectPageDown => {}
            _ => {
                self.preferred_col = None;
                self.preferred_visual_col = None;
            }
        }

        match command {
            Command::Save => self.request_save(PostSaveAction::None),
            Command::SaveAs => self.begin_save_as(PostSaveAction::None),
            Command::NewFile => self.new_untitled_tab(),
            Command::OpenFile => self.begin_quick_open(),
            Command::CloseTab => self.request_close_active_tab(),
            Command::NextTab => self.next_tab(),
            Command::PreviousTab => self.previous_tab(),
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::HideTerminal => self.hide_terminal(),
            Command::NewTerminal => self.create_terminal_session(),
            Command::NextTerminal => self.next_terminal_session(),
            Command::CloseTerminal => self.close_active_terminal(),
            Command::CycleTerminalSize => {
                use ui::TerminalPanel::{Maximized, Normal, Tall};
                self.terminal_size = match self.terminal_size {
                    Normal => Tall,
                    Tall => Maximized,
                    _ => Normal,
                };
                if !self.terminal_visible {
                    self.toggle_terminal();
                }
                self.status = Some(format!(
                    "Terminal size: {}",
                    match self.terminal_size {
                        Tall => "tall",
                        Maximized => "maximized",
                        _ => "normal",
                    }
                ));
            }
            Command::CopyTerminalScreen => match self.active_terminal_ref() {
                Some(session) => {
                    let text = session.screen_text();
                    self.store_clipboard(text, "Copied terminal screen");
                }
                None => self.status = Some("No terminal is open".to_owned()),
            },
            Command::Quit => self.begin_quit(),
            Command::Undo => {
                if self.run_workspace_history_step(false).is_some() {
                    return;
                }
                if self
                    .buffer
                    .undo_with_selection(&mut self.cursor, &mut self.selection_anchor)
                {
                    self.secondary_cursors.clear();
                    self.rectangular_selection = None;
                    self.clamp_cursor();
                    self.workspace_edit_history = None;
                    self.status = Some("Undo".to_owned());
                } else {
                    self.status = Some("Nothing to undo".to_owned());
                }
            }
            Command::Redo => {
                if self.run_workspace_history_step(true).is_some() {
                    return;
                }
                if self
                    .buffer
                    .redo_with_selection(&mut self.cursor, &mut self.selection_anchor)
                {
                    self.secondary_cursors.clear();
                    self.rectangular_selection = None;
                    self.clamp_cursor();
                    self.workspace_edit_history = None;
                    self.status = Some("Redo".to_owned());
                } else {
                    self.status = Some("Nothing to redo".to_owned());
                }
            }
            Command::ShowPalette => {
                self.palette_query.clear();
                self.palette_selected = 0;
                self.mode = AppMode::Palette;
            }
            Command::ShowHelp => {
                self.mode = AppMode::Help;
            }
            Command::ShowSettings => {
                self.settings_selected = 0;
                self.mode = AppMode::Settings;
                self.status = None;
            }
            Command::Find => {
                self.mode = AppMode::Find;
                if let Some((start, end)) = self.selection_range() {
                    let selected_text = self.buffer.get_text_range(start, end);
                    if !selected_text.contains('\n') && !selected_text.is_empty() {
                        self.find_query.set(selected_text);
                    }
                }
                self.recalculate_find_matches();
            }
            Command::FindNext => {
                self.find_next();
            }
            Command::FindPrevious => {
                self.find_previous();
            }
            Command::Replace => self.begin_replace(),
            Command::ProjectSearch => self.begin_project_search(),
            Command::GoToLine => {
                self.mode = AppMode::GoToLine;
                self.goto_query.clear();
            }
            Command::AiIntent => self.begin_ai_intent(),
            Command::AiSetup => self.open_ai_setup(false),
            Command::SelectAll => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                let last_row = self.buffer.line_count().saturating_sub(1);
                let last_col = self.buffer.grapheme_count(last_row);
                self.selection_anchor = Some(Cursor::new(0, 0));
                self.cursor = Cursor::new(last_row, last_col);
                self.status = Some("All selected".to_owned());
            }
            Command::Copy => {
                if let Some(text) = self.rectangular_text() {
                    self.store_clipboard(text, "Copied");
                } else if let Some((start, end)) = self.selection_range() {
                    let text = self.buffer.get_text_range(start, end);
                    self.store_clipboard(text, "Copied");
                } else {
                    let mut line = self.buffer.line_text(self.cursor.row);
                    line.push('\n');
                    self.store_clipboard(line, "Copied");
                }
            }
            Command::Cut => {
                if let Some(text) = self.rectangular_text() {
                    self.store_clipboard(text, "Cut");
                    let _ = self.replace_rectangular_selection("");
                } else if let Some((start, end)) = self.selection_range() {
                    let text = self.buffer.get_text_range(start, end);
                    self.store_clipboard(text, "Cut");
                    let pre_cursor = self.cursor;
                    let pre_selection_anchor = self.selection_anchor;
                    self.buffer.replace_range(start, end, "", &mut self.cursor);
                    self.selection_anchor = None;
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                } else {
                    let mut line = self.buffer.line_text(self.cursor.row);
                    line.push('\n');
                    self.store_clipboard(line, "Cut");
                    let row = self.cursor.row;
                    if self.buffer.line_count() > 1 && row + 1 < self.buffer.line_count() {
                        self.buffer
                            .delete_range(Cursor::new(row, 0), Cursor::new(row + 1, 0));
                    } else {
                        self.buffer.delete_range(
                            Cursor::new(row, 0),
                            Cursor::new(row, self.buffer.grapheme_count(row)),
                        );
                    }
                    self.clamp_cursor();
                }
            }
            Command::Paste => {
                let (text, from_system) =
                    paste_clipboard_text(self.system_clipboard_text(), &self.clipboard);
                self.clipboard = text;
                if !self.clipboard.is_empty() {
                    let text = self.clipboard.clone();
                    if self.rectangular_selection.is_some() {
                        let _ = self.replace_rectangular_selection(&text);
                    } else if !self.secondary_cursors.is_empty() && self.selection_anchor.is_none()
                    {
                        let _ = self.apply_text_to_all_cursors(&text, false);
                    } else if let Some((start, end)) = self.selection_range() {
                        let pre_cursor = self.cursor;
                        let pre_selection_anchor = self.selection_anchor;
                        self.buffer
                            .replace_range(start, end, &text, &mut self.cursor);
                        self.selection_anchor = None;
                        self.buffer.set_last_history_state(
                            pre_cursor,
                            pre_selection_anchor,
                            self.cursor,
                            self.selection_anchor,
                        );
                    } else {
                        self.buffer.insert_text(&mut self.cursor, &text);
                    }
                    self.status = Some(
                        if from_system {
                            "Pasted from system clipboard"
                        } else {
                            "Pasted from Mellow clipboard"
                        }
                        .to_owned(),
                    );
                } else {
                    self.status = Some(
                        if from_system {
                            "System clipboard is empty"
                        } else {
                            "Mellow clipboard is empty; system clipboard unavailable"
                        }
                        .to_owned(),
                    );
                }
            }
            Command::ToggleComment => {
                let (start_row, end_row) = self.selected_line_rows();
                let pre_cursor = self.cursor;
                let pre_selection_anchor = self.selection_anchor;
                if self.buffer.toggle_comment_lines(start_row, end_row) {
                    self.clamp_cursor();
                    if let Some(anchor) = self.selection_anchor.as_mut() {
                        anchor.col = anchor.col.min(self.buffer.grapheme_count(anchor.row));
                    }
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                    self.status = Some("Toggled line comment".to_owned());
                } else {
                    self.status = Some(format!(
                        "No line-comment syntax for {}",
                        self.buffer.language()
                    ));
                }
            }
            Command::MoveLinesUp => {
                let (start_row, end_row) = self.selected_line_rows();
                let pre_cursor = self.cursor;
                let pre_selection_anchor = self.selection_anchor;
                if self.buffer.move_lines_up(start_row, end_row) {
                    self.cursor.row = self.cursor.row.saturating_sub(1);
                    if let Some(anchor) = self.selection_anchor.as_mut() {
                        anchor.row = anchor.row.saturating_sub(1);
                    }
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                    self.status = Some("Moved line(s) up".to_owned());
                }
            }
            Command::MoveLinesDown => {
                let (start_row, end_row) = self.selected_line_rows();
                let pre_cursor = self.cursor;
                let pre_selection_anchor = self.selection_anchor;
                if self.buffer.move_lines_down(start_row, end_row) {
                    let last_row = self.buffer.line_count().saturating_sub(1);
                    self.cursor.row = self.cursor.row.saturating_add(1).min(last_row);
                    if let Some(anchor) = self.selection_anchor.as_mut() {
                        anchor.row = anchor.row.saturating_add(1).min(last_row);
                    }
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                    self.status = Some("Moved line(s) down".to_owned());
                }
            }
            Command::DuplicateLines => {
                let (start_row, end_row) = self.selected_line_rows();
                let pre_cursor = self.cursor;
                let pre_selection_anchor = self.selection_anchor;
                if self.buffer.duplicate_lines(start_row, end_row) {
                    let height = end_row.saturating_sub(start_row) + 1;
                    let last_row = self.buffer.line_count().saturating_sub(1);
                    self.cursor.row = self.cursor.row.saturating_add(height).min(last_row);
                    if let Some(anchor) = self.selection_anchor.as_mut() {
                        anchor.row = anchor.row.saturating_add(height).min(last_row);
                    }
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                    self.status = Some("Duplicated line(s)".to_owned());
                }
            }
            Command::CycleTheme => {
                self.theme_preset = self.theme_preset.next();
                self.theme = Theme::for_preset(self.theme_preset);
                self.status = Some(format!("Theme: {}", self.theme_preset.label()));
                self.persist_editor_settings();
            }
            Command::ToggleWhitespace => {
                self.show_whitespace = !self.show_whitespace;
                self.status = Some(format!(
                    "Whitespace: {}",
                    if self.show_whitespace {
                        "shown"
                    } else {
                        "hidden"
                    }
                ));
                self.persist_editor_settings();
            }
            Command::ToggleIndentGuides => {
                self.show_indent_guides = !self.show_indent_guides;
                self.status = Some(format!(
                    "Indent guides: {}",
                    if self.show_indent_guides {
                        "shown"
                    } else {
                        "hidden"
                    }
                ));
                self.persist_editor_settings();
            }
            Command::ToggleWordWrap => {
                self.word_wrap = !self.word_wrap;
                self.preferred_col = None;
                self.preferred_visual_col = None;
                self.visual_scroll_row = 0;
                self.scroll_col = 0;
                self.should_scroll_to_cursor = true;
                self.status = Some(format!(
                    "Word wrap: {}",
                    if self.word_wrap { "on" } else { "off" }
                ));
                self.persist_editor_settings();
            }
            Command::AddCursorAbove => self.add_cursor_vertical(-1),
            Command::AddCursorBelow => self.add_cursor_vertical(1),
            Command::ClearSecondaryCursors => {
                self.secondary_cursors.clear();
                self.status = Some("Extra cursors cleared".to_owned());
            }
            Command::ReloadKeybindings => match keymap::KeymapConfig::load() {
                Ok(config) => {
                    let source = config
                        .source()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "defaults".to_owned());
                    self.keymap_config = config;
                    self.status = Some(format!("Keybindings reloaded · {source}"));
                }
                Err(error) => {
                    self.status = Some(format!("Keybinding reload blocked: {error}"));
                }
            },
            Command::ToggleExplorer => self.toggle_explorer(),
            Command::SplitEditor => self.split_editor(),
            Command::FocusNextPane => self.focus_next_pane(),
            Command::ToggleSplitOrientation => self.toggle_split_orientation(),
            Command::CloseSplit => self.close_split(),
            Command::TriggerCompletion => self.request_completion(),
            Command::GoToDefinition => self.request_definition(),
            Command::ShowHover => self.request_hover(),
            Command::ShowSignatureHelp => self.request_signature_help(),
            Command::FindReferences => self.request_references(),
            Command::RenameSymbol => self.request_rename_preview(),
            Command::FormatDocument => self.request_formatting(),
            Command::ShowCodeActions => self.request_code_actions(),
            Command::ToggleIndentStyle => {
                let tabs = !self.buffer.uses_tab_indent();
                self.buffer.set_tab_indent(tabs);
                self.status = Some(if tabs {
                    "This file now indents with tabs".to_owned()
                } else {
                    "This file now indents with 4 spaces".to_owned()
                });
            }
            Command::LanguageServerStatus => {
                self.mode = AppMode::LanguageStatus;
                self.status = None;
            }
            Command::ShowProblems => {
                let count = self.problem_items().len();
                if count == 0 {
                    self.status = Some("No problems in active file".to_owned());
                } else {
                    self.problem_selected = self.problem_selected.min(count - 1);
                    self.mode = AppMode::Problems;
                }
            }
            Command::RestartLanguageServer => {
                self.language_service_active = true;
                self.restart_language_server(true);
            }
            Command::ShowChanges => self.begin_changes_review(),
            Command::RefreshGit => {
                self.refresh_git_state(true);
                if self.git_repository.is_some() {
                    self.status = Some("Git status refreshed".to_owned());
                } else {
                    self.status = Some("No Git repository found for this workspace".to_owned());
                }
            }
            Command::StageHunk => self.stage_selected_git_hunk(),
            Command::UnstageHunk => self.unstage_selected_git_hunk(),
            Command::RevertHunk => self.begin_revert_selected_git_hunk(),
            Command::GitCommit => self.begin_git_commit(),
            Command::GitBranches => self.begin_git_branches(),
            Command::GitHistory => self.begin_git_history(),
            Command::GitBlame => self.begin_git_blame(),
            Command::GoToSymbol => self.begin_go_to_symbol(),
            Command::FixProblemWithAi => self.fix_problem_with_ai(),
            Command::AskAiShell => self.begin_ai_shell(),
            Command::KeepOursConflict => {
                self.resolve_conflict_at_cursor(crate::conflict::ConflictChoice::Ours)
            }
            Command::KeepTheirsConflict => {
                self.resolve_conflict_at_cursor(crate::conflict::ConflictChoice::Theirs)
            }
            Command::KeepBothConflict => {
                self.resolve_conflict_at_cursor(crate::conflict::ConflictChoice::Both)
            }
            Command::GitFetch => self.git_fetch(),
            Command::GitPull => self.git_pull_fast_forward(),
            Command::GitPush => self.git_push(),
            Command::GitCancel => self.cancel_git_operation(),
            Command::GitStageFile => self.git_stage_active_file(),
            Command::GitUnstageFile => self.git_unstage_active_file(),
            Command::GitConflicts => self.begin_git_conflicts(),
            Command::GitMarkResolved => self.mark_active_git_conflict_resolved(),
            Command::CancelSelection => {
                self.selection_anchor = None;
                self.rectangular_selection = None;
                if !self.secondary_cursors.is_empty() {
                    self.secondary_cursors.clear();
                    self.status = Some("Extra cursors cleared".to_owned());
                }
            }
            Command::MoveLeft => {
                self.rectangular_selection = None;
                if let Some((start, _)) = self.selection_range() {
                    self.cursor = start;
                    self.selection_anchor = None;
                } else {
                    self.selection_anchor = None;
                    self.move_left();
                    self.move_secondary_left();
                }
            }
            Command::MoveRight => {
                self.rectangular_selection = None;
                if let Some((_, end)) = self.selection_range() {
                    self.cursor = end;
                    self.selection_anchor = None;
                } else {
                    self.selection_anchor = None;
                    self.move_right();
                    self.move_secondary_right();
                }
            }
            Command::MoveUp => {
                self.rectangular_selection = None;
                self.selection_anchor = None;
                self.move_up(1);
                self.move_secondary_vertical(-1);
            }
            Command::MoveDown => {
                self.rectangular_selection = None;
                self.selection_anchor = None;
                self.move_down(1);
                self.move_secondary_vertical(1);
            }
            Command::Home => {
                self.rectangular_selection = None;
                self.selection_anchor = None;
                self.cursor.col = self.smart_home_col(self.cursor);
                let targets: Vec<usize> = self
                    .secondary_cursors
                    .iter()
                    .map(|cursor| self.smart_home_col(*cursor))
                    .collect();
                for (cursor, col) in self.secondary_cursors.iter_mut().zip(targets) {
                    cursor.col = col;
                }
            }
            Command::MoveWordLeft | Command::MoveWordRight => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                self.selection_anchor = None;
                self.cursor = self.word_target(command);
            }
            Command::SelectWordLeft | Command::SelectWordRight => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.cursor = self.word_target(command);
            }
            Command::DocumentStart | Command::SelectDocumentStart => {
                self.extend_or_clear_selection(command == Command::SelectDocumentStart);
                self.cursor = Cursor::new(0, 0);
            }
            Command::DocumentEnd | Command::SelectDocumentEnd => {
                self.extend_or_clear_selection(command == Command::SelectDocumentEnd);
                let row = self.buffer.line_count().saturating_sub(1);
                self.cursor = Cursor::new(row, self.buffer.grapheme_count(row));
            }
            Command::DeleteWordLeft | Command::DeleteWordRight => {
                if self.selection_range().is_some() || self.rectangular_selection.is_some() {
                    self.execute(Command::Backspace);
                    return;
                }
                self.secondary_cursors.clear();
                let target = self.word_target(command);
                if target != self.cursor {
                    let pre_cursor = self.cursor;
                    let start = if command == Command::DeleteWordLeft {
                        target
                    } else {
                        self.cursor
                    };
                    self.buffer.delete_range(self.cursor, target);
                    self.cursor = start;
                    self.buffer
                        .set_last_history_state(pre_cursor, None, self.cursor, None);
                }
            }
            Command::End => {
                self.rectangular_selection = None;
                self.selection_anchor = None;
                self.cursor.col = self.buffer.grapheme_count(self.cursor.row);
                for cursor in &mut self.secondary_cursors {
                    cursor.col = self.buffer.grapheme_count(cursor.row);
                }
            }
            Command::PageUp => {
                self.rectangular_selection = None;
                self.selection_anchor = None;
                self.move_up(self.page_rows);
                self.move_secondary_vertical(-(self.page_rows as isize));
            }
            Command::PageDown => {
                self.rectangular_selection = None;
                self.selection_anchor = None;
                self.move_down(self.page_rows);
                self.move_secondary_vertical(self.page_rows as isize);
            }
            Command::SelectLeft => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.move_left();
            }
            Command::SelectRight => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.move_right();
            }
            Command::SelectUp => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.move_up(1);
            }
            Command::SelectDown => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.move_down(1);
            }
            Command::SelectHome => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.cursor.col = self.smart_home_col(self.cursor);
            }
            Command::SelectEnd => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.cursor.col = self.buffer.grapheme_count(self.cursor.row);
            }
            Command::SelectPageUp => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.move_up(self.page_rows);
            }
            Command::SelectPageDown => {
                self.secondary_cursors.clear();
                self.rectangular_selection = None;
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(self.cursor);
                }
                self.move_down(self.page_rows);
            }
            Command::Insert(ch) => {
                if self.rectangular_selection.is_some() {
                    if let Some(close) = matching_close(ch) {
                        if self.replace_rectangular_selection(&format!("{ch}{close}")) {
                            self.cursor.col = self.cursor.col.saturating_sub(1);
                            for cursor in &mut self.secondary_cursors {
                                cursor.col = cursor.col.saturating_sub(1);
                            }
                        }
                    } else {
                        let _ = self.replace_rectangular_selection(&ch.to_string());
                    }
                } else if !self.secondary_cursors.is_empty() && self.selection_anchor.is_none() {
                    if let Some(close) = matching_close(ch) {
                        let _ = self.apply_text_to_all_cursors(&format!("{ch}{close}"), true);
                    } else {
                        let _ = self.apply_text_to_all_cursors(&ch.to_string(), false);
                    }
                } else if let Some((start, end)) = self.selection_range() {
                    let pre_cursor = self.cursor;
                    let pre_selection_anchor = self.selection_anchor;
                    let selected = self.buffer.get_text_range(start, end);
                    let replacement = if let Some(close) = matching_close(ch) {
                        format!("{ch}{selected}{close}")
                    } else {
                        ch.to_string()
                    };
                    self.buffer
                        .replace_range(start, end, &replacement, &mut self.cursor);
                    self.selection_anchor = None;
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                } else if is_closing_delimiter(ch)
                    && self
                        .buffer
                        .grapheme_at(self.cursor.row, self.cursor.col)
                        .is_some_and(|grapheme| grapheme == ch.to_string())
                {
                    self.cursor.col += 1;
                } else if let Some(close) = matching_close(ch) {
                    self.buffer
                        .insert_text(&mut self.cursor, &format!("{ch}{close}"));
                    self.cursor.col = self.cursor.col.saturating_sub(1);
                } else {
                    self.buffer.insert_char(&mut self.cursor, ch);
                }
            }
            Command::Newline => {
                if self.rectangular_selection.is_some() {
                    let _ = self.replace_rectangular_selection("\n");
                } else if !self.secondary_cursors.is_empty() && self.selection_anchor.is_none() {
                    let _ = self.apply_text_to_all_cursors("\n", false);
                } else if let Some((start, end)) = self.selection_range() {
                    let pre_cursor = self.cursor;
                    let pre_selection_anchor = self.selection_anchor;
                    self.buffer
                        .replace_range(start, end, "\n", &mut self.cursor);
                    self.selection_anchor = None;
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                } else {
                    self.smart_newline();
                }
            }
            Command::Backspace => {
                if self.rectangular_selection.is_some() {
                    let _ = self.replace_rectangular_selection("");
                } else if !self.secondary_cursors.is_empty() && self.selection_anchor.is_none() {
                    let _ = self.multi_cursor_backspace();
                } else if let Some((start, end)) = self.selection_range() {
                    let pre_cursor = self.cursor;
                    let pre_selection_anchor = self.selection_anchor;
                    self.buffer.replace_range(start, end, "", &mut self.cursor);
                    self.selection_anchor = None;
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                } else {
                    self.buffer.backspace(&mut self.cursor);
                }
            }
            Command::Delete => {
                if self.rectangular_selection.is_some() {
                    let _ = self.replace_rectangular_selection("");
                } else if !self.secondary_cursors.is_empty() && self.selection_anchor.is_none() {
                    let _ = self.multi_cursor_delete();
                } else if let Some((start, end)) = self.selection_range() {
                    let pre_cursor = self.cursor;
                    let pre_selection_anchor = self.selection_anchor;
                    self.buffer.replace_range(start, end, "", &mut self.cursor);
                    self.selection_anchor = None;
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                } else {
                    self.buffer.delete(&self.cursor);
                }
            }
            Command::Tab => {
                if let Some((start, end)) = self.selection_range() {
                    let pre_cursor = self.cursor;
                    let pre_selection_anchor = self.selection_anchor;
                    let end_row = if end.row > start.row && end.col == 0 {
                        end.row - 1
                    } else {
                        end.row
                    };
                    let step = self.buffer.indent_unit().chars().count();
                    self.buffer.indent_lines(start.row, end_row);
                    if let Some(anchor) = self.selection_anchor.as_mut()
                        && anchor.row >= start.row
                        && anchor.row <= end_row
                    {
                        anchor.col += step;
                    }
                    if self.cursor.row >= start.row && self.cursor.row <= end_row {
                        self.cursor.col += step;
                    }
                    self.buffer.set_last_history_state(
                        pre_cursor,
                        pre_selection_anchor,
                        self.cursor,
                        self.selection_anchor,
                    );
                } else if self.buffer.uses_tab_indent() {
                    self.buffer.insert_text(&mut self.cursor, "\t");
                } else {
                    let display_col = self.buffer.display_width_before(
                        self.cursor.row,
                        self.cursor.col,
                        TAB_WIDTH,
                    );
                    let spaces = TAB_WIDTH - (display_col % TAB_WIDTH);
                    self.buffer
                        .insert_text(&mut self.cursor, &" ".repeat(spaces));
                }
            }
            Command::Outdent => {
                let step = self.buffer.indent_unit().chars().count();
                let pre_cursor = self.cursor;
                let pre_selection_anchor = self.selection_anchor;
                if let Some((start, end)) = self.selection_range() {
                    let end_row = if end.row > start.row && end.col == 0 {
                        end.row - 1
                    } else {
                        end.row
                    };
                    self.buffer.outdent_lines(start.row, end_row);
                    if let Some(anchor) = self.selection_anchor.as_mut()
                        && anchor.row >= start.row
                        && anchor.row <= end_row
                    {
                        anchor.col = anchor.col.saturating_sub(step);
                        anchor.col = anchor.col.min(self.buffer.grapheme_count(anchor.row));
                    }
                    if self.cursor.row >= start.row && self.cursor.row <= end_row {
                        self.cursor.col = self.cursor.col.saturating_sub(step);
                    }
                    self.clamp_cursor();
                } else {
                    self.buffer.outdent_lines(self.cursor.row, self.cursor.row);
                    self.cursor.col = self.cursor.col.saturating_sub(step);
                    self.clamp_cursor();
                }
                self.buffer.set_last_history_state(
                    pre_cursor,
                    pre_selection_anchor,
                    self.cursor,
                    self.selection_anchor,
                );
            }
        }

        if self.buffer.revision() != revision_before {
            self.sync_code_intelligence_after_edit();
            if let Some(inserted) = completion_edit {
                self.schedule_auto_completion_after_edit(inserted);
            }
        } else if completion_edit.is_some() {
            self.auto_completion_due = None;
            self.auto_completion_trigger = None;
            self.completion_request = None;
        }
    }

    fn clamp_cursor(&mut self) {
        self.cursor.row = self
            .cursor
            .row
            .min(self.buffer.line_count().saturating_sub(1));
        self.cursor.col = self
            .cursor
            .col
            .min(self.buffer.grapheme_count(self.cursor.row));
    }

    fn cursor_left_in(buffer: &Buffer, mut cursor: Cursor) -> Cursor {
        if cursor.col > 0 {
            cursor.col -= 1;
        } else if cursor.row > 0 {
            cursor.row -= 1;
            cursor.col = buffer.grapheme_count(cursor.row);
        }
        cursor
    }

    fn cursor_right_in(buffer: &Buffer, mut cursor: Cursor) -> Cursor {
        let line_len = buffer.grapheme_count(cursor.row);
        if cursor.col < line_len {
            cursor.col += 1;
        } else if cursor.row + 1 < buffer.line_count() {
            cursor.row += 1;
            cursor.col = 0;
        }
        cursor
    }

    fn cursor_vertical_in(buffer: &Buffer, mut cursor: Cursor, rows: isize) -> Cursor {
        if rows < 0 {
            cursor.row = cursor.row.saturating_sub(rows.unsigned_abs());
        } else {
            cursor.row = cursor
                .row
                .saturating_add(rows as usize)
                .min(buffer.line_count().saturating_sub(1));
        }
        cursor.col = cursor.col.min(buffer.grapheme_count(cursor.row));
        cursor
    }

    fn move_secondary_left(&mut self) {
        let buffer = &self.buffer;
        for cursor in &mut self.secondary_cursors {
            *cursor = Self::cursor_left_in(buffer, *cursor);
        }
        self.secondary_cursors.sort();
        self.secondary_cursors.dedup();
        self.secondary_cursors
            .retain(|cursor| *cursor != self.cursor);
    }

    fn move_secondary_right(&mut self) {
        let buffer = &self.buffer;
        for cursor in &mut self.secondary_cursors {
            *cursor = Self::cursor_right_in(buffer, *cursor);
        }
        self.secondary_cursors.sort();
        self.secondary_cursors.dedup();
        self.secondary_cursors
            .retain(|cursor| *cursor != self.cursor);
    }

    fn move_secondary_vertical(&mut self, rows: isize) {
        let buffer = &self.buffer;
        for cursor in &mut self.secondary_cursors {
            *cursor = Self::cursor_vertical_in(buffer, *cursor, rows);
        }
        self.secondary_cursors.sort();
        self.secondary_cursors.dedup();
        self.secondary_cursors
            .retain(|cursor| *cursor != self.cursor);
    }

    fn move_left(&mut self) {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
            self.cursor.col = self.buffer.grapheme_count(self.cursor.row);
        }
    }

    fn move_right(&mut self) {
        let line_len = self.buffer.grapheme_count(self.cursor.row);
        if self.cursor.col < line_len {
            self.cursor.col += 1;
        } else if self.cursor.row + 1 < self.buffer.line_count() {
            self.cursor.row += 1;
            self.cursor.col = 0;
        }
    }

    fn move_up(&mut self, rows: usize) {
        if self.word_wrap {
            let layout = VisualLayout::new(&self.buffer, self.last_editor_width, TAB_WIDTH, true);
            let position = layout.visual_position_for_cursor(&self.buffer, self.cursor);
            let pref = *self.preferred_visual_col.get_or_insert(position.column);
            let target_row = position.row.saturating_sub(rows);
            self.cursor = layout.cursor_for_visual_position(&self.buffer, target_row, pref);
            return;
        }

        let pref = *self.preferred_col.get_or_insert(self.cursor.col);
        self.cursor.row = self.cursor.row.saturating_sub(rows);
        self.cursor.col = pref.min(self.buffer.grapheme_count(self.cursor.row));
    }

    fn move_down(&mut self, rows: usize) {
        if self.word_wrap {
            let layout = VisualLayout::new(&self.buffer, self.last_editor_width, TAB_WIDTH, true);
            let position = layout.visual_position_for_cursor(&self.buffer, self.cursor);
            let pref = *self.preferred_visual_col.get_or_insert(position.column);
            let target_row = position
                .row
                .saturating_add(rows)
                .min(layout.len().saturating_sub(1));
            self.cursor = layout.cursor_for_visual_position(&self.buffer, target_row, pref);
            return;
        }

        let pref = *self.preferred_col.get_or_insert(self.cursor.col);
        let last_row = self.buffer.line_count().saturating_sub(1);
        self.cursor.row = self.cursor.row.saturating_add(rows).min(last_row);
        self.cursor.col = pref.min(self.buffer.grapheme_count(self.cursor.row));
    }

    fn system_clipboard_text(&self) -> Option<String> {
        #[cfg(test)]
        if let Some(reader) = self.clipboard_reader {
            return reader();
        }
        read_system_clipboard()
    }

    fn keep_cursor_visible(&mut self, terminal_width: u16, terminal_height: u16) {
        let editor_top = ui::HEADER_ROWS;
        let footer_h = ui::footer_rows(terminal_height);
        let editor_bottom = ui::editor_bottom_row(terminal_height, self.terminal_panel())
            .max(editor_top)
            .min(terminal_height.saturating_sub(footer_h));
        let (_, pane_height) =
            self.active_pane_vertical_geometry(terminal_width, editor_top, editor_bottom);
        let editor_height = pane_height.max(1) as usize;
        self.page_rows = editor_height.saturating_sub(1).max(1);

        let pane_width = self.active_pane_horizontal_geometry(terminal_width).1;
        let gutter = ui::gutter_width(self.buffer.line_count()) as usize;
        let editor_width = pane_width.saturating_sub(gutter as u16).max(1) as usize;
        self.last_editor_width = editor_width;

        if self.word_wrap {
            self.scroll_col = 0;
            let layout = VisualLayout::new(&self.buffer, editor_width, TAB_WIDTH, true);
            self.visual_scroll_row = layout.clamp_scroll(self.visual_scroll_row, editor_height);

            if !self.should_scroll_to_cursor {
                return;
            }

            let position = layout.visual_position_for_cursor(&self.buffer, self.cursor);
            if position.row < self.visual_scroll_row {
                self.visual_scroll_row = position.row;
            } else if position.row >= self.visual_scroll_row + editor_height {
                self.visual_scroll_row = position.row + 1 - editor_height;
            }
            self.visual_scroll_row = layout.clamp_scroll(self.visual_scroll_row, editor_height);
            return;
        }

        self.scroll_row = self
            .scroll_row
            .min(self.buffer.line_count().saturating_sub(1));

        if !self.should_scroll_to_cursor {
            return;
        }

        if self.cursor.row < self.scroll_row {
            self.scroll_row = self.cursor.row;
        } else if self.cursor.row >= self.scroll_row + editor_height {
            self.scroll_row = self.cursor.row + 1 - editor_height;
        }

        let cursor_display_col =
            self.buffer
                .display_width_before(self.cursor.row, self.cursor.col, TAB_WIDTH);
        if cursor_display_col < self.scroll_col {
            self.scroll_col = cursor_display_col;
        } else if cursor_display_col >= self.scroll_col + editor_width {
            self.scroll_col = cursor_display_col + 1 - editor_width;
        }
    }
}

fn remap_tab_index(index: usize, removed: usize, fallback: usize) -> usize {
    if index == removed {
        fallback
    } else if index > removed {
        index - 1
    } else {
        index
    }
}

fn is_completion_identifier_char(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_alphanumeric()
}

fn matching_close(ch: char) -> Option<char> {
    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

fn is_closing_delimiter(ch: char) -> bool {
    matches!(ch, ')' | ']' | '}' | '"' | '\'' | '`')
}

fn common_path_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.chars().collect();
    for value in &values[1..] {
        let chars: Vec<char> = value.chars().collect();
        let shared = prefix
            .iter()
            .zip(chars.iter())
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(shared);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.into_iter().collect()
}

fn is_explicit_path_query(input: &str) -> bool {
    input.contains(std::path::MAIN_SEPARATOR) || input.starts_with('.') || input.starts_with('~')
}

fn expand_user_path(input: &str) -> PathBuf {
    if input == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }

    if let Some(rest) = input.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }

    PathBuf::from(input)
}

const MAX_SYSTEM_CLIPBOARD_BYTES: usize = 8 * 1024 * 1024;

fn read_clipboard_command(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_SYSTEM_CLIPBOARD_BYTES {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn paste_clipboard_text(system: Option<String>, fallback: &str) -> (String, bool) {
    match system {
        Some(text) => (text, true),
        None => (fallback.to_owned(), false),
    }
}

fn read_system_clipboard() -> Option<String> {
    if cfg!(test) {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        read_clipboard_command("pbpaste", &[])
    }

    #[cfg(target_os = "linux")]
    {
        for (program, args) in [
            ("wl-paste", &["--no-newline"][..]),
            ("xclip", &["-selection", "clipboard", "-o"][..]),
            ("xsel", &["--clipboard", "--output"][..]),
        ] {
            if let Some(text) = read_clipboard_command(program, args) {
                return Some(text);
            }
        }
        None
    }

    #[cfg(target_os = "windows")]
    {
        read_clipboard_command(
            "powershell",
            &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
        )
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn write_clipboard_command(program: &str, args: &[&str], text: &str) -> bool {
    use std::io::Write;

    if text.len() > MAX_SYSTEM_CLIPBOARD_BYTES {
        return false;
    }

    let mut child = match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        let _ = child.kill();
        return false;
    }
    drop(stdin);

    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return false,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    false
}

fn write_native_system_clipboard(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        write_clipboard_command("pbcopy", &[], text)
    }

    #[cfg(target_os = "linux")]
    {
        for (program, args) in [
            ("wl-copy", &[][..]),
            ("xclip", &["-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--input"][..]),
        ] {
            if write_clipboard_command(program, args, text) {
                return true;
            }
        }
        false
    }

    #[cfg(target_os = "windows")]
    {
        write_clipboard_command(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "Set-Clipboard -Value ([Console]::In.ReadToEnd())",
            ],
            text,
        )
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = text;
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardDelivery {
    Native,
    TerminalSent,
    InternalOnly,
}

impl ClipboardDelivery {
    fn message(self, verb: &str, count: usize) -> String {
        match self {
            Self::Native => format!("{verb} {count} chars · system clipboard"),
            Self::TerminalSent => format!("Unconfirmed copy · {verb} {count} chars"),
            Self::InternalOnly => format!("Mellow only · {verb} {count} chars"),
        }
    }
}

fn copy_to_system_clipboard(text: &str) -> ClipboardDelivery {
    use std::io::Write;

    if cfg!(test) || text.len() > MAX_SYSTEM_CLIPBOARD_BYTES {
        return ClipboardDelivery::InternalOnly;
    }
    if write_native_system_clipboard(text) {
        return ClipboardDelivery::Native;
    }
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut base64 = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        base64.push(TABLE[(b0 >> 2) as usize] as char);
        base64.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            base64.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            base64.push('=');
        }
        if chunk.len() > 2 {
            base64.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            base64.push('=');
        }
    }
    let osc = format!("\x1b]52;c;{}\x07", base64);
    let mut stdout = std::io::stdout();
    if stdout
        .write_all(osc.as_bytes())
        .and_then(|_| stdout.flush())
        .is_ok()
    {
        ClipboardDelivery::TerminalSent
    } else {
        ClipboardDelivery::InternalOnly
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::git::GitFileState;

    #[test]
    fn design_lock_empty_system_clipboard_does_not_paste_stale_internal_text() {
        assert_eq!(
            paste_clipboard_text(Some(String::new()), "stale"),
            (String::new(), true)
        );
        assert_eq!(
            paste_clipboard_text(None, "fallback"),
            ("fallback".to_owned(), false)
        );
    }

    #[test]
    fn design_lock_failed_quit_save_as_mouse_hit_follows_resized_dialog() {
        for (width, height) in [(80, 24), (120, 34), (160, 45)] {
            let mut app = App::new(Buffer::empty(None));
            app.execute(Command::Insert('x'));
            app.mode = AppMode::ConfirmQuit;
            app.status = Some("Save failed: disposable fixture".to_owned());
            let rect = ui::quit_dialog_rect(ratatui::layout::Rect::new(0, 0, width, height), true);
            app.handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x + 4,
                    row: rect.y + 7,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
                width,
                height,
            );
            assert_eq!(app.mode, AppMode::SaveAs);
            assert!(!app.should_quit);
            assert!(app.buffer.is_dirty());
        }
    }

    #[test]
    fn design_lock_mouse_copy_uses_visible_intent_action() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "alpha");
        app.selection_anchor = Some(Cursor::new(0, 0));
        // The status line offers Copy next to the selection for mouse users.
        let (column, _) = ui::status_layout(&app, 80)
            .into_iter()
            .find(|(_, item)| item.text == "Copy")
            .expect("Copy action is visible while text is selected");
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: 23,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            80,
            24,
        );
        assert_eq!(app.clipboard, "alpha");
        assert!(app.buffer.is_dirty());
        assert_eq!(app.mode, AppMode::Editing);
    }

    #[test]
    fn design_lock_copy_preference_is_mouse_reachable_without_global_writes() {
        let mut app = App::new(Buffer::empty(None));
        app.copy_on_select = true;
        app.mode = AppMode::Settings;
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row: 13,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            80,
            24,
        );
        assert!(!app.copy_on_select);
        assert_eq!(app.settings_selected, 6);
    }

    #[test]
    fn design_lock_failed_quit_save_keeps_text_and_offers_save_as() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-parent").join("note.txt");
        let mut app = App::new(Buffer::open(Some(path)).unwrap());
        app.execute(Command::Insert('x'));
        app.begin_quit();
        assert_eq!(app.confirmation_selected, 2);
        app.request_save(PostSaveAction::Quit);
        assert_eq!(app.mode, AppMode::ConfirmQuit);
        assert!(!app.should_quit);
        assert_eq!(app.buffer.line_text(0), "x");
        assert!(app.status.as_deref().unwrap().starts_with("Save failed"));
        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.mode, AppMode::SaveAs);
        assert!(app.buffer.is_dirty());
    }

    #[test]
    fn design_lock_open_editors_click_preserves_dirty_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.py");
        let b = dir.path().join("B.py");
        std::fs::write(&a, "alpha").unwrap();
        std::fs::write(&b, "beta").unwrap();
        let mut app = App::new(Buffer::open(Some(a.clone())).unwrap());
        app.execute(Command::Insert('!'));
        app.open_path_in_tab(b).unwrap();
        app.explorer_visible = true;
        // Click the A.py tab in the title bar.
        let column = (0..120)
            .find(|&column| ui::header_hit(&app, 120, column) == Some(ui::HeaderHit::Tab(0)))
            .expect("A.py tab is visible");
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            120,
            34,
        );
        assert_eq!(app.buffer.path(), Some(&a));
        assert_eq!(app.buffer.line_text(0), "!alpha");
        assert!(app.buffer.is_dirty());
        assert_eq!(app.focus_label(), "EDITOR");
    }

    #[test]
    fn design_lock_copy_on_select_off_preserves_clipboard_but_explicit_copy_works() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "alpha beta");
        app.selection_anchor = Some(Cursor::new(0, 0));
        app.cursor = Cursor::new(0, 5);
        app.copy_on_select = false;
        app.is_mouse_dragging = true;
        app.clipboard = "previous fixture".to_owned();
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 10,
                row: 3,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            80,
            24,
        );
        assert_eq!(app.clipboard, "previous fixture");
        app.execute(Command::Copy);
        assert_eq!(app.clipboard, "alpha");
        assert_eq!(app.status.as_deref(), Some("Mellow only · Copied 5 chars"));
    }

    #[test]
    fn design_lock_clipboard_feedback_distinguishes_confirmed_and_unconfirmed_delivery() {
        assert!(
            ClipboardDelivery::Native
                .message("Copied", 3)
                .contains("system clipboard")
        );
        assert!(
            ClipboardDelivery::TerminalSent
                .message("Copied", 3)
                .contains("Unconfirmed")
        );
        assert!(
            ClipboardDelivery::InternalOnly
                .message("Cut", 3)
                .contains("Mellow only")
        );
    }

    #[test]
    fn design_lock_directory_launch_reveals_its_own_project_without_opening_a_directory_buffer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.py"), "print('A')\n").unwrap();
        std::fs::write(dir.path().join("B.json"), "{}\n").unwrap();
        let app = App::startup(Some(dir.path().to_path_buf())).unwrap();
        assert_eq!(
            app.workspace_root,
            std::fs::canonicalize(dir.path()).unwrap()
        );
        assert!(app.explorer_visible);
        assert!(
            app.explorer_files
                .iter()
                .any(|entry| entry.path.ends_with("A.py"))
        );
        assert!(!app.buffer.is_dirty());
    }

    #[test]
    fn design_lock_header_quit_reaches_existing_dirty_guard() {
        let mut app = App::new(Buffer::open(None).unwrap());
        app.handle_key(KeyEvent::new(
            KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 78,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            80,
            24,
        );
        assert_eq!(app.mode, AppMode::ConfirmQuit);
        assert!(!app.should_quit);
        assert!(app.buffer.is_dirty());
    }

    #[test]
    fn stage_and_unstage_active_file_from_the_editor() {
        let dir = git_workspace();
        let path = dir.path().join("demo.txt");
        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.refresh_git_state(true);
        assert!(app.active_git_file().is_none(), "clean file has no badge");

        app.buffer.insert_text(&mut app.cursor, "edit ");
        app.execute(Command::GitStageFile);
        assert_eq!(
            app.status.as_deref(),
            Some("Save first: staging uses the saved file")
        );

        app.execute(Command::Save);
        app.refresh_git_state(true);
        let state = |app: &App| app.active_git_file().map(|file| file.state());
        assert_eq!(state(&app), Some(GitFileState::Modified));

        app.execute(Command::GitStageFile);
        assert_eq!(state(&app), Some(GitFileState::Staged));
        assert_eq!(app.status.as_deref(), Some("Staged demo.txt"));

        app.execute(Command::GitUnstageFile);
        assert_eq!(state(&app), Some(GitFileState::Modified));
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("edit "));
    }

    /// Audit F2: dirty edit → Ctrl+B (Files focused) → workbench shortcuts
    /// must still act, while the tree keeps its own plain keys.
    #[test]
    fn audit_f2_workbench_shortcuts_work_while_files_has_focus() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "base").unwrap();
        let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        let plain = |code| KeyEvent::new(code, KeyModifiers::empty());

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.buffer.insert_text(&mut app.cursor, "edit ");
        let focus_files = |app: &mut App| {
            app.mode = AppMode::Editing;
            app.explorer_visible = false;
            app.handle_key(ctrl('b'));
            assert!(app.explorer_focused, "Ctrl+B focuses Files");
        };

        focus_files(&mut app);
        app.handle_key(ctrl('s'));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edit base");

        for (key, mode) in [
            (ctrl('p'), AppMode::Palette),
            (plain(KeyCode::F(1)), AppMode::Help),
            (ctrl('o'), AppMode::QuickOpen),
        ] {
            focus_files(&mut app);
            app.handle_key(key);
            assert_eq!(app.mode, mode, "{key:?} from Files");
        }

        focus_files(&mut app);
        app.buffer.insert_text(&mut app.cursor, "x");
        app.handle_key(ctrl('q'));
        assert_eq!(app.mode, AppMode::ConfirmQuit, "dirty quit still asks");

        // The tree keeps its plain keys.
        focus_files(&mut app);
        app.handle_key(plain(KeyCode::Char('n')));
        assert_eq!(app.mode, AppMode::ExplorerCreate);
        let _ = recovery::clear(&RecoveryKey::File(path));
    }

    /// Audit F5: the periodic Git refresh must not run on the input thread.
    #[test]
    fn audit_f5_periodic_git_refresh_runs_in_background_and_applies_later() {
        let dir = git_workspace();
        std::fs::write(dir.path().join("demo.txt"), "changed\n").unwrap();
        let mut app = App::new(Buffer::open(Some(dir.path().join("demo.txt"))).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.last_git_refresh = Instant::now() - Duration::from_secs(1);

        app.refresh_git_state(false);
        assert!(
            app.git_refresh_receiver.is_some(),
            "periodic refresh is handed to a worker"
        );
        assert!(
            app.git_snapshot.files.is_empty(),
            "nothing applied synchronously"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while app.git_refresh_receiver.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            app.refresh_git_state(false);
        }
        assert!(
            app.git_repository_active(),
            "worker discovered the repository"
        );
        assert_eq!(app.git_snapshot.files.len(), 1);
    }

    #[test]
    fn forced_git_refresh_supersedes_an_in_flight_background_refresh() {
        let dir = git_workspace();
        let mut app = App::new(Buffer::open(Some(dir.path().join("demo.txt"))).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.last_git_refresh = Instant::now() - Duration::from_secs(1);
        app.refresh_git_state(false);
        assert!(app.git_refresh_receiver.is_some());

        std::fs::write(dir.path().join("demo.txt"), "changed\n").unwrap();
        app.refresh_git_state(true);
        assert!(
            app.git_refresh_receiver.is_none(),
            "stale worker result dropped"
        );
        assert_eq!(app.git_snapshot.files.len(), 1);
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
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
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[cfg(unix)]
    fn install_git_hook(dir: &std::path::Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let hook = dir.join(".git/hooks").join(name);
        std::fs::write(&hook, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn wait_for_git_operation(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.git_operation.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            app.poll_git_operation();
        }
        assert!(app.git_operation.is_none(), "Git operation never finished");
    }

    fn type_commit_message(app: &mut App, message: &str) {
        let key = |code| KeyEvent::new(code, crossterm::event::KeyModifiers::empty());
        app.execute(Command::GitCommit);
        assert_eq!(app.mode, AppMode::GitCommitInput);
        for ch in message.chars() {
            app.handle_git_commit_key(key(KeyCode::Char(ch)));
        }
        app.handle_git_commit_key(key(KeyCode::Enter));
    }

    /// A workspace whose branch tracks a bare `remote` beside it.
    fn git_workspace_with_remote() -> (tempfile::TempDir, tempfile::TempDir) {
        let dir = git_workspace();
        let remote = tempfile::tempdir().unwrap();
        git_in(remote.path(), &["init", "-q", "--bare"]);
        let remote_path = remote.path().to_str().unwrap();
        git_in(dir.path(), &["remote", "add", "origin", remote_path]);
        git_in(dir.path(), &["push", "-q", "-u", "origin", "HEAD"]);
        (dir, remote)
    }

    /// Step 2 regression: a slow pre-commit hook froze the whole editor.
    #[cfg(unix)]
    #[test]
    fn git_commit_runs_hooks_in_the_background_and_the_editor_stays_usable() {
        let dir = git_workspace();
        std::fs::write(dir.path().join("demo.txt"), "changed\n").unwrap();
        git_in(dir.path(), &["add", "demo.txt"]);
        install_git_hook(dir.path(), "pre-commit", "sleep 2");
        let mut app = App::new(Buffer::open(Some(dir.path().join("demo.txt"))).unwrap());
        app.workspace_root = dir.path().to_path_buf();

        let started = Instant::now();
        type_commit_message(&mut app, "slow hook");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "Enter returned while the hook still runs"
        );
        assert_eq!(app.mode, AppMode::Editing);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.starts_with("Committing…"))
        );

        app.execute(Command::Insert('!'));
        assert!(app.any_dirty_tabs(), "typing works during the commit");
        app.execute(Command::GitPush);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.starts_with("Git commit is still running"))
        );

        wait_for_git_operation(&mut app);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.starts_with("Committed ")),
            "{:?}",
            app.status
        );
        assert_eq!(
            git_in(dir.path(), &["log", "-1", "--format=%s"]),
            "slow hook"
        );
        assert!(app.git_commit_query.as_str().is_empty());
    }

    #[test]
    fn go_to_symbol_filters_by_name_and_jumps_to_the_definition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.rs");
        std::fs::write(&path, "fn main() {}\n\nfn helper() {}\n").unwrap();
        let mut app = App::new(crate::buffer::Buffer::open(Some(path)).unwrap());
        app.execute(Command::GoToSymbol);
        assert_eq!(app.mode, AppMode::Symbols);
        for ch in "help".chars() {
            app.handle_symbols_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(app.filtered_symbols().len(), 1);
        app.handle_symbols_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(app.cursor.row, 2);
    }

    #[test]
    fn breadcrumb_ends_with_the_definition_under_the_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.rs");
        std::fs::write(&path, "fn main() {\n    let x = 1;\n}\n\nfn other() {}\n").unwrap();
        let mut app = App::new(crate::buffer::Buffer::open(Some(path)).unwrap());
        app.cursor = Cursor::new(1, 4);
        assert_eq!(
            app.breadcrumb_with_symbol().last().map(String::as_str),
            Some("main")
        );
        app.cursor = Cursor::new(4, 0);
        assert_eq!(
            app.breadcrumb_with_symbol().last().map(String::as_str),
            Some("other")
        );
    }

    #[test]
    fn asking_for_a_shell_command_without_ai_opens_setup() {
        let mut app = App::new(crate::buffer::Buffer::empty(None));
        app.execute(Command::AskAiShell);
        assert_ne!(app.mode, AppMode::AiPrompt);
        assert!(!app.ai_shell_request);
    }

    #[test]
    fn fix_problem_says_when_the_line_has_no_problem() {
        let mut app = App::new(crate::buffer::Buffer::empty(None));
        app.buffer
            .insert_text(&mut Cursor::default(), "let fine = 1;");
        app.execute(Command::FixProblemWithAi);
        assert_eq!(app.status.as_deref(), Some("No problem on this line"));
        assert_eq!(app.mode, AppMode::Editing);
    }

    #[test]
    fn fix_problem_without_ai_opens_setup_instead_of_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.rs");
        std::fs::write(&path, "fn main( {\n").unwrap();
        let mut app = App::new(crate::buffer::Buffer::open(Some(path)).unwrap());
        assert!(
            app.problem_items()
                .iter()
                .any(|problem| problem.cursor.row == 0),
            "expected a parse problem on the first line"
        );
        app.cursor = Cursor::new(0, 0);
        app.execute(Command::FixProblemWithAi);
        assert_ne!(app.mode, AppMode::AiReview);
        assert!(app.ai_proposal.is_none());
    }

    #[test]
    fn go_to_symbol_explains_when_a_file_has_none() {
        let mut app = App::new(crate::buffer::Buffer::empty(None));
        app.execute(Command::GoToSymbol);
        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(
            app.status.as_deref(),
            Some("This file has no symbols to jump to")
        );
    }

    #[test]
    fn quick_open_lists_recently_opened_files_first() {
        let mut app = App::new(crate::buffer::Buffer::empty(None));
        app.workspace_root = PathBuf::from("/work");
        app.quick_open_candidates = vec![
            PathBuf::from("a.txt"),
            PathBuf::from("b.txt"),
            PathBuf::from("c.txt"),
        ];
        app.recent_files = vec![PathBuf::from("/work/c.txt"), PathBuf::from("/work/a.txt")];
        assert_eq!(app.filtered_quick_open_entries(), vec![2, 0, 1]);
    }

    #[test]
    fn opening_a_file_moves_it_to_the_front_of_recents() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["one.txt", "two.txt"] {
            std::fs::write(dir.path().join(name), "x\n").unwrap();
        }
        let mut app = App::new(crate::buffer::Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.open_path_in_tab(dir.path().join("one.txt")).unwrap();
        app.open_path_in_tab(dir.path().join("two.txt")).unwrap();
        app.open_path_in_tab(dir.path().join("one.txt")).unwrap();
        assert_eq!(
            app.recent_files,
            vec![dir.path().join("one.txt"), dir.path().join("two.txt")]
        );
    }

    #[test]
    fn recent_files_break_ties_in_fuzzy_matches() {
        let mut app = App::new(crate::buffer::Buffer::empty(None));
        app.workspace_root = PathBuf::from("/work");
        app.quick_open_candidates = vec![PathBuf::from("util_a.rs"), PathBuf::from("util_b.rs")];
        app.quick_open_query.set("util");
        app.recent_files = vec![PathBuf::from("/work/util_b.rs")];
        assert_eq!(app.filtered_quick_open_entries(), vec![1, 0]);
    }

    #[test]
    fn keeping_a_conflict_side_is_one_undoable_edit() {
        use crate::{buffer::Buffer, cursor::Cursor};
        let text = "keep\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> topic\nend";
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut Cursor::default(), text);
        app.cursor = Cursor::new(2, 0);
        app.execute(Command::KeepTheirsConflict);
        assert_eq!(app.buffer.contents(), "keep\ntheirs\nend");
        assert!(app.buffer.can_undo());
        let mut cursor = app.cursor;
        assert!(app.buffer.undo(&mut cursor));
        assert_eq!(app.buffer.contents(), text);
    }

    #[test]
    fn keeping_an_empty_side_removes_the_marker_lines() {
        use crate::{buffer::Buffer, cursor::Cursor};
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(
            &mut Cursor::default(),
            "a\n<<<<<<< HEAD\n=======\ntheirs\n>>>>>>> topic\nb",
        );
        app.cursor = Cursor::new(1, 0);
        app.execute(Command::KeepOursConflict);
        assert_eq!(app.buffer.contents(), "a\nb");
    }

    #[test]
    fn keep_both_puts_ours_first_and_outside_a_conflict_says_so() {
        use crate::{buffer::Buffer, cursor::Cursor};
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(
            &mut Cursor::default(),
            "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> topic",
        );
        app.cursor = Cursor::new(3, 0);
        app.execute(Command::KeepBothConflict);
        assert_eq!(app.buffer.contents(), "ours\ntheirs");

        let mut plain = App::new(Buffer::empty(None));
        plain
            .buffer
            .insert_text(&mut Cursor::default(), "no conflict here");
        plain.execute(Command::KeepOursConflict);
        assert_eq!(
            plain.status.as_deref(),
            Some("The cursor is not inside a merge conflict")
        );
    }

    #[test]
    fn ctrl_g_says_when_ai_is_not_set_up_and_keeps_the_message() {
        let dir = git_workspace();
        std::fs::write(dir.path().join("demo.txt"), "changed\n").unwrap();
        git_in(dir.path(), &["add", "demo.txt"]);
        let mut app = App::new(Buffer::open(Some(dir.path().join("demo.txt"))).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.begin_git_commit();
        app.handle_git_commit_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(app.mode, AppMode::GitCommitInput);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("AI is not set up"),
            "{:?}",
            app.status
        );
        assert!(app.git_commit_query.is_empty());
    }

    #[test]
    fn staged_diff_is_read_only_and_shows_only_staged_changes() {
        let dir = git_workspace();
        std::fs::write(dir.path().join("demo.txt"), "changed\n").unwrap();
        git_in(dir.path(), &["add", "demo.txt"]);
        std::fs::write(dir.path().join("other.txt"), "not staged\n").unwrap();
        let repository = crate::git::GitRepository::discover(dir.path())
            .unwrap()
            .unwrap();
        let diff = repository.staged_diff().unwrap();
        assert!(diff.contains("demo.txt"), "{diff}");
        assert!(!diff.contains("other.txt"), "{diff}");
        // The index is unchanged: the file is still staged, nothing committed.
        assert!(git_in(dir.path(), &["diff", "--cached", "--name-only"]).contains("demo.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_git_commit_kills_its_hook_and_keeps_the_message() {
        let dir = git_workspace();
        std::fs::write(dir.path().join("demo.txt"), "changed\n").unwrap();
        git_in(dir.path(), &["add", "demo.txt"]);
        install_git_hook(dir.path(), "pre-commit", "sleep 30");
        let before = git_in(dir.path(), &["rev-parse", "HEAD"]);
        let mut app = App::new(Buffer::open(Some(dir.path().join("demo.txt"))).unwrap());
        app.workspace_root = dir.path().to_path_buf();

        type_commit_message(&mut app, "never lands");
        std::thread::sleep(Duration::from_millis(200));
        app.execute(Command::GitCancel);
        let started = Instant::now();
        wait_for_git_operation(&mut app);

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "hook was killed"
        );
        assert_eq!(app.status.as_deref(), Some("Git commit cancelled"));
        assert_eq!(git_in(dir.path(), &["rev-parse", "HEAD"]), before);
        assert_eq!(app.git_commit_query.as_str(), "never lands");
    }

    #[cfg(unix)]
    #[test]
    fn git_push_runs_in_the_background() {
        let (dir, remote) = git_workspace_with_remote();
        std::fs::write(dir.path().join("demo.txt"), "pushed\n").unwrap();
        git_in(dir.path(), &["commit", "-q", "-am", "to push"]);
        install_git_hook(dir.path(), "pre-push", "sleep 2");
        let mut app = App::new(Buffer::open(Some(dir.path().join("demo.txt"))).unwrap());
        app.workspace_root = dir.path().to_path_buf();

        let started = Instant::now();
        app.execute(Command::GitPush);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(app.git_operation.is_some());

        wait_for_git_operation(&mut app);
        assert_eq!(app.status.as_deref(), Some("Git push complete"));
        assert_eq!(
            git_in(remote.path(), &["log", "-1", "--format=%s"]),
            "to push"
        );
    }

    /// The fast-forward reloads open buffers from disk, so edits made while
    /// the fetch ran in the background must stop it rather than be replaced.
    #[test]
    fn git_pull_stops_when_a_buffer_was_edited_during_the_fetch() {
        let (dir, remote) = git_workspace_with_remote();
        let other = tempfile::tempdir().unwrap();
        git_in(
            other.path(),
            &["clone", "-q", remote.path().to_str().unwrap(), "."],
        );
        git_in(
            other.path(),
            &["config", "user.email", "mellow@example.invalid"],
        );
        git_in(other.path(), &["config", "user.name", "Mellow Tests"]);
        std::fs::write(other.path().join("demo.txt"), "upstream\n").unwrap();
        git_in(other.path(), &["commit", "-q", "-am", "upstream"]);
        git_in(other.path(), &["push", "-q"]);

        let path = dir.path().join("demo.txt");
        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::GitPull);
        app.execute(Command::Insert('!'));
        wait_for_git_operation(&mut app);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.starts_with("Pull stopped")),
            "{:?}",
            app.status
        );
        assert!(app.any_dirty_tabs(), "the edit survived");
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "upstream\n");

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::GitPull);
        wait_for_git_operation(&mut app);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.starts_with("Fast-forwarded from origin/")),
            "{:?}",
            app.status
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "upstream\n");
        assert_eq!(app.buffer.line_text(0), "upstream");
    }

    /// Audit: the LSP preview must show the actual change, not edit counts.
    #[test]
    fn lsp_edit_preview_shows_before_and_after_lines() {
        use crate::lsp::{LspPosition, LspRange, LspTextEdit};
        let dir = tempfile::tempdir().unwrap();
        let open = dir.path().join("open.rs");
        let closed = dir.path().join("closed.rs");
        std::fs::write(&open, "let old = 1;\nkeep();\n").unwrap();
        std::fs::write(&closed, "use old;\n").unwrap();
        let mut app = App::new(Buffer::open(Some(open.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        let edit = |path: &PathBuf, line, start, end| LspTextEdit {
            path: path.clone(),
            range: LspRange {
                start: LspPosition {
                    line,
                    character: start,
                },
                end: LspPosition {
                    line,
                    character: end,
                },
            },
            new_text: "new".to_owned(),
        };
        app.begin_lsp_edit_preview("Rename", vec![edit(&open, 0, 4, 7), edit(&closed, 0, 4, 7)]);

        assert_eq!(app.mode, AppMode::ConfirmLspEdits);
        assert_eq!(
            app.pending_lsp_preview,
            vec![
                LspPreviewRow::File("closed.rs".to_owned()),
                LspPreviewRow::Location(1),
                LspPreviewRow::Removed("use old;".to_owned()),
                LspPreviewRow::Added("use new;".to_owned()),
                LspPreviewRow::File("open.rs".to_owned()),
                LspPreviewRow::Location(1),
                LspPreviewRow::Removed("let old = 1;".to_owned()),
                LspPreviewRow::Added("let new = 1;".to_owned()),
            ]
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(app.pending_lsp_preview.is_empty());
    }

    #[test]
    fn ctrl_arrows_backspace_and_home_edit_by_word_and_line() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "    call(foo_bar, baz)");
        app.cursor = Cursor::new(0, 22);

        app.handle_key(key(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(app.cursor.col, 21, "before ')'");
        app.handle_key(key(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.cursor.col, 18, "Alt+Left works too (macOS)");

        app.handle_key(key(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(
            app.buffer.contents(),
            "    call(foo_barbaz)",
            "deletes ', '"
        );
        app.execute(Command::Undo);
        app.cursor = Cursor::new(0, 16);
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(app.buffer.contents(), "    call(, baz)");
        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "    call(foo_bar, baz)");

        app.cursor = Cursor::new(0, 4);
        app.handle_key(key(
            KeyCode::Right,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let (start, end) = app.selection_range().unwrap();
        assert_eq!(app.buffer.get_text_range(start, end), "call");

        app.handle_key(key(KeyCode::Home, KeyModifiers::empty()));
        assert_eq!(app.cursor.col, 4, "smart Home goes to the first non-blank");
        app.handle_key(key(KeyCode::Home, KeyModifiers::empty()));
        assert_eq!(app.cursor.col, 0, "pressed again: column 0");
        app.handle_key(key(KeyCode::Home, KeyModifiers::empty()));
        assert_eq!(app.cursor.col, 4, "and back");

        app.handle_key(key(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(app.cursor, Cursor::new(0, 22));
        app.handle_key(key(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(app.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn shift_arrows_scroll_the_diff_and_choosing_another_change_resets_it() {
        let dir = git_workspace();
        let lines: String = (1..=40).map(|n| format!("new {n}\n")).collect();
        std::fs::write(dir.path().join("demo.txt"), &lines).unwrap();
        std::fs::write(dir.path().join("other.txt"), "x\n").unwrap();
        let mut app = App::new(Buffer::open(Some(dir.path().join("demo.txt"))).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.refresh_git_state(true);
        app.mode = AppMode::Changes;
        let shift = |code| KeyEvent::new(code, KeyModifiers::SHIFT);

        app.handle_key(shift(KeyCode::Down));
        app.handle_key(shift(KeyCode::PageDown));
        assert_eq!(app.changes_diff_scroll, 11);
        app.handle_key(shift(KeyCode::Up));
        assert_eq!(app.changes_diff_scroll, 10);

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(app.changes_selected, 1);
        assert_eq!(app.changes_diff_scroll, 0, "new change starts at its top");
    }

    #[test]
    fn terminal_resizes_and_asks_before_closing_a_running_job() {
        use ui::TerminalPanel;
        assert_eq!(ui::terminal_panel_height(40, TerminalPanel::Hidden), 0);
        let normal = ui::terminal_panel_height(40, TerminalPanel::Normal);
        let tall = ui::terminal_panel_height(40, TerminalPanel::Tall);
        let max = ui::terminal_panel_height(40, TerminalPanel::Maximized);
        assert!(normal < tall && tall < max, "{normal} {tall} {max}");
        assert_eq!(ui::editor_content_height(40, TerminalPanel::Maximized), 1);
        assert_eq!(ui::terminal_panel_height(40, true), normal);

        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::CycleTerminalSize);
        assert!(app.terminal_visible, "resizing shows the terminal");
        assert_eq!(app.terminal_panel(), TerminalPanel::Tall);
        app.execute(Command::CycleTerminalSize);
        assert_eq!(app.terminal_panel(), TerminalPanel::Maximized);
        app.execute(Command::CycleTerminalSize);
        assert_eq!(app.terminal_panel(), TerminalPanel::Normal);

        let session = app.active_terminal_mut().unwrap();
        session.write_bytes(b"sleep 30\n").unwrap();
        // The terminal runs the developer's own $SHELL; a heavy zsh setup can
        // take seconds before it reads the typed command.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !app.active_terminal_ref().unwrap().has_running_job() && Instant::now() < deadline {
            app.poll_terminal_sessions();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            app.active_terminal_ref().unwrap().has_running_job(),
            "the shell never started `sleep 30`"
        );
        app.execute(Command::CloseTerminal);
        assert_eq!(app.mode, AppMode::ConfirmCloseTerminal);
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
        assert_eq!(app.terminal_session_count(), 1, "kept");
        app.execute(Command::CloseTerminal);
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
        assert_eq!(app.terminal_session_count(), 0, "closed after confirming");
    }

    #[test]
    fn indentation_follows_the_file_and_can_be_switched() {
        let dir = tempfile::tempdir().unwrap();
        let makefile = dir.path().join("Makefile");
        std::fs::write(&makefile, "all:\n\tcc main.c\n").unwrap();
        let mut app = App::new(Buffer::open(Some(makefile)).unwrap());
        assert!(app.buffer.uses_tab_indent(), "Makefiles need tabs");
        app.cursor = Cursor::new(1, 0);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        assert!(app.buffer.line_text(1).starts_with("\t\t"));

        let spaced = dir.path().join("lib.py");
        std::fs::write(&spaced, "def f():\n    return 1\n").unwrap();
        let mut app = App::new(Buffer::open(Some(spaced)).unwrap());
        assert!(!app.buffer.uses_tab_indent());
        app.execute(Command::ToggleIndentStyle);
        assert!(app.buffer.uses_tab_indent());
        app.cursor = Cursor::new(0, 0);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(app.buffer.line_text(0), "\tdef f():");
        app.execute(Command::Outdent);
        assert_eq!(app.buffer.line_text(0), "def f():");
    }

    fn git_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run(&["init", "-q"]);
        run(&["config", "user.email", "mellow@example.invalid"]);
        run(&["config", "user.name", "Mellow Tests"]);
        std::fs::write(
            dir.path().join("demo.txt"),
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
        )
        .unwrap();
        run(&["add", "demo.txt"]);
        run(&["commit", "-q", "-m", "base"]);
        dir
    }

    #[test]
    fn goal15_multi_cursor_insert_and_newline_are_single_undo_transactions() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "aa\nbb\ncc");
        app.cursor = Cursor::new(0, 1);
        app.execute(Command::AddCursorBelow);
        app.execute(Command::AddCursorBelow);
        assert_eq!(
            app.secondary_cursors,
            vec![Cursor::new(1, 1), Cursor::new(2, 1)]
        );

        app.execute(Command::Insert('X'));
        assert_eq!(app.buffer.contents(), "aXa\nbXb\ncXc");
        assert_eq!(app.cursor, Cursor::new(0, 2));
        assert_eq!(
            app.secondary_cursors,
            vec![Cursor::new(1, 2), Cursor::new(2, 2)]
        );

        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "aa\nbb\ncc");
        assert!(app.secondary_cursors.is_empty());

        app.cursor = Cursor::new(0, 1);
        app.execute(Command::AddCursorBelow);
        app.execute(Command::Newline);
        assert_eq!(app.buffer.contents(), "a\na\nb\nb\ncc");
        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "aa\nbb\ncc");
    }

    #[test]
    fn goal15_multi_cursor_backspace_and_delete_edit_all_locations() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "abc\ndef\nghi");
        app.cursor = Cursor::new(0, 2);
        app.execute(Command::AddCursorBelow);
        app.execute(Command::AddCursorBelow);
        app.execute(Command::Backspace);
        assert_eq!(app.buffer.contents(), "ac\ndf\ngi");
        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "abc\ndef\nghi");

        app.cursor = Cursor::new(0, 1);
        app.execute(Command::AddCursorBelow);
        app.execute(Command::AddCursorBelow);
        app.execute(Command::Delete);
        assert_eq!(app.buffer.contents(), "ac\ndf\ngi");
    }

    #[test]
    fn goal15_alt_shift_mouse_drag_creates_rectangular_selection_and_cut_is_undoable() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "abcd\nwxyz");
        let gutter = ui::gutter_width(app.buffer.line_count());

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: gutter + 1,
                row: ui::HEADER_ROWS,
                modifiers: crossterm::event::KeyModifiers::ALT
                    | crossterm::event::KeyModifiers::SHIFT,
            },
            80,
            24,
        );
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: gutter + 3,
                row: ui::HEADER_ROWS + 1,
                modifiers: crossterm::event::KeyModifiers::ALT
                    | crossterm::event::KeyModifiers::SHIFT,
            },
            80,
            24,
        );
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: gutter + 3,
                row: ui::HEADER_ROWS + 1,
                modifiers: crossterm::event::KeyModifiers::ALT
                    | crossterm::event::KeyModifiers::SHIFT,
            },
            80,
            24,
        );

        assert_eq!(app.rectangular_text().as_deref(), Some("bc\nxy"));
        app.execute(Command::Cut);
        assert_eq!(app.buffer.contents(), "ad\nwz");
        assert!(app.rectangular_selection.is_none());
        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "abcd\nwxyz");
    }

    #[cfg(unix)]
    #[test]
    fn goal15_clipboard_reader_accepts_bounded_utf8_process_output() {
        let text = read_clipboard_command("/bin/sh", &["-c", "printf clipboard-value"]).unwrap();
        assert_eq!(text, "clipboard-value");
    }

    #[test]
    fn select_copy_and_paste_flow() {
        let mut app = App::new(Buffer::empty(None));
        app.clipboard_reader = Some(|| None);
        app.execute(Command::Insert('a'));
        app.execute(Command::Insert('b'));
        app.execute(Command::Insert('c'));
        assert_eq!(app.buffer.line_text(0), "abc");

        // Select All
        app.execute(Command::SelectAll);
        assert!(app.selection_range().is_some());

        // Copy
        app.execute(Command::Copy);
        assert_eq!(app.clipboard, "abc");

        // Moving cursor cancels selection
        app.execute(Command::Home);
        assert_eq!(app.selection_range(), None);

        // Paste
        app.execute(Command::Paste);
        assert_eq!(app.buffer.line_text(0), "abcabc");
    }

    #[test]
    fn typing_replaces_active_selection() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "hello world");
        app.cursor = Cursor::new(0, 5);
        app.selection_anchor = Some(Cursor::new(0, 0));
        assert!(app.selection_range().is_some());

        app.execute(Command::Insert('H'));
        assert_eq!(app.buffer.line_text(0), "H world");
        assert_eq!(app.selection_range(), None);
    }

    #[test]
    fn backspace_deletes_active_selection() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "hello world");
        app.cursor = Cursor::new(0, 5);
        app.selection_anchor = Some(Cursor::new(0, 0));

        app.execute(Command::Backspace);
        assert_eq!(app.buffer.line_text(0), " world");
        assert_eq!(app.selection_range(), None);
    }

    #[test]
    fn cut_copies_and_removes_selection() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "cut-me and keep-me");
        app.cursor = Cursor::new(0, 6);
        app.selection_anchor = Some(Cursor::new(0, 0));

        app.execute(Command::Cut);
        assert_eq!(app.clipboard, "cut-me");
        assert_eq!(app.buffer.line_text(0), " and keep-me");
        assert_eq!(app.selection_range(), None);
    }

    #[test]
    fn myn_e07_selection_replacement_is_one_undo_transaction() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "hello world");
        app.cursor = Cursor::new(0, 5);
        app.selection_anchor = Some(Cursor::new(0, 0));

        app.execute(Command::Insert('H'));
        assert_eq!(app.buffer.line_text(0), "H world");
        assert_eq!(app.cursor, Cursor::new(0, 1));
        assert_eq!(app.selection_anchor, None);

        app.execute(Command::Undo);
        assert_eq!(app.buffer.line_text(0), "hello world");
        assert_eq!(app.cursor, Cursor::new(0, 5));
        assert_eq!(app.selection_anchor, Some(Cursor::new(0, 0)));

        app.execute(Command::Redo);
        assert_eq!(app.buffer.line_text(0), "H world");
        assert_eq!(app.cursor, Cursor::new(0, 1));
        assert_eq!(app.selection_anchor, None);
    }

    #[test]
    fn myn_e07_paste_over_selection_restores_selection_on_undo() {
        let mut app = App::new(Buffer::empty(None));
        app.clipboard_reader = Some(|| None);
        app.buffer.insert_text(&mut app.cursor, "hello world");
        app.cursor = Cursor::new(0, 5);
        app.selection_anchor = Some(Cursor::new(0, 0));
        app.clipboard = "hi".to_owned();

        app.execute(Command::Paste);
        assert_eq!(app.buffer.line_text(0), "hi world");

        app.execute(Command::Undo);
        assert_eq!(app.buffer.line_text(0), "hello world");
        assert_eq!(app.cursor, Cursor::new(0, 5));
        assert_eq!(app.selection_anchor, Some(Cursor::new(0, 0)));
    }

    #[test]
    fn paste_prefers_system_clipboard_and_falls_back_when_unavailable() {
        let mut app = App::new(Buffer::empty(None));
        app.clipboard = "internal".into();
        app.clipboard_reader = Some(|| Some("system 日本語".into()));
        app.execute(Command::Paste);
        assert_eq!(app.buffer.contents(), "system 日本語");
        assert_eq!(app.clipboard, "system 日本語");
        app.clipboard_reader = Some(|| None);
        app.execute(Command::Paste);
        assert_eq!(app.buffer.contents(), "system 日本語system 日本語");
    }

    #[test]
    fn myn_e07_indent_undo_restores_text_and_selection() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "line 1\nline 2\nline 3");
        app.selection_anchor = Some(Cursor::new(0, 0));
        app.cursor = Cursor::new(1, 4);

        app.execute(Command::Tab);
        assert_eq!(app.buffer.line_text(0), "    line 1");
        assert_eq!(app.buffer.line_text(1), "    line 2");

        app.execute(Command::Undo);
        assert_eq!(app.buffer.line_text(0), "line 1");
        assert_eq!(app.buffer.line_text(1), "line 2");
        assert_eq!(app.cursor, Cursor::new(1, 4));
        assert_eq!(app.selection_anchor, Some(Cursor::new(0, 0)));
    }

    #[test]
    fn myn_ux03_preferred_column_retained_across_short_lines() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "0123456789\nabc\n0123456789");
        app.cursor = Cursor::new(0, 8);

        app.execute(Command::MoveDown);
        assert_eq!(app.cursor, Cursor::new(1, 3));
        assert_eq!(app.preferred_col, Some(8));

        app.execute(Command::MoveDown);
        assert_eq!(app.cursor, Cursor::new(2, 8));
        assert_eq!(app.preferred_col, Some(8));

        app.execute(Command::MoveLeft);
        assert_eq!(app.cursor, Cursor::new(2, 7));
        assert_eq!(app.preferred_col, None);

        app.execute(Command::MoveUp);
        assert_eq!(app.cursor, Cursor::new(1, 3));
        assert_eq!(app.preferred_col, Some(7));

        app.execute(Command::MoveUp);
        assert_eq!(app.cursor, Cursor::new(0, 7));
    }

    #[test]
    fn myn_ux17_move_left_right_collapses_selection() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "hello world");

        // Set selection from (0, 2) to (0, 7)
        app.cursor = Cursor::new(0, 7);
        app.selection_anchor = Some(Cursor::new(0, 2));
        assert!(app.selection_range().is_some());

        app.execute(Command::MoveLeft);
        assert_eq!(app.cursor, Cursor::new(0, 2));
        assert_eq!(app.selection_range(), None);

        // Reset selection
        app.cursor = Cursor::new(0, 7);
        app.selection_anchor = Some(Cursor::new(0, 2));
        assert!(app.selection_range().is_some());

        app.execute(Command::MoveRight);
        assert_eq!(app.cursor, Cursor::new(0, 7));
        assert_eq!(app.selection_range(), None);
    }

    #[test]
    fn myn_ux16_tab_and_outdent_lines_with_selection() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "line 1\nline 2\nline 3");

        // Select line 0 and line 1
        app.selection_anchor = Some(Cursor::new(0, 0));
        app.cursor = Cursor::new(1, 4);

        app.execute(Command::Tab);
        assert_eq!(app.buffer.line_text(0), "    line 1");
        assert_eq!(app.buffer.line_text(1), "    line 2");
        assert_eq!(app.buffer.line_text(2), "line 3");
        assert!(app.selection_range().is_some());

        app.execute(Command::Outdent);
        assert_eq!(app.buffer.line_text(0), "line 1");
        assert_eq!(app.buffer.line_text(1), "line 2");
        assert_eq!(app.buffer.line_text(2), "line 3");
    }

    #[test]
    fn myn_ux02_mouse_scroll_does_not_relocate_cursor() {
        let mut app = App::new(Buffer::empty(None));
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        app.buffer.insert_text(&mut app.cursor, &lines.join("\n"));
        app.cursor = Cursor::new(0, 0);

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );

        assert_eq!(app.scroll_row, 3);
        assert_eq!(app.cursor.row, 0);
        assert!(!app.should_scroll_to_cursor);

        // keep_cursor_visible should not snap back while scrolling
        app.keep_cursor_visible(80, 24);
        assert_eq!(app.scroll_row, 3);

        // Key movement resumes cursor tracking
        app.execute(Command::MoveDown);
        assert!(app.should_scroll_to_cursor);
        app.keep_cursor_visible(80, 24);
        assert_eq!(app.scroll_row, 1);
    }

    #[test]
    fn myn_ux04_mouse_click_handles_quit_confirmation() {
        let mut app = App::new(Buffer::empty(None));
        app.mode = AppMode::ConfirmQuit;

        // Popup centered for 80x24: width=60, height=7 -> popup_x=10, popup_y=8
        // [D] Discard button is at inner_x + 20..33, row = popup_y + 3 = 11
        // inner_x = 10 + 2 = 12 -> col 32..45
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 34,
                row: 11,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );
        assert!(app.should_quit);

        // Reset and test clicking outside popup to cancel
        let mut app2 = App::new(Buffer::empty(None));
        app2.mode = AppMode::ConfirmQuit;
        app2.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );
        assert_eq!(app2.mode, AppMode::Editing);
        assert_eq!(app2.status, Some("Quit cancelled".to_owned()));
    }

    #[test]
    fn polish_quit_dialog_supports_focus_navigation_and_enter_activation() {
        let mut app = App::new(Buffer::empty(None));
        app.execute(Command::Insert('x'));
        app.begin_quit();

        assert_eq!(app.mode, AppMode::ConfirmQuit);
        assert_eq!(app.confirmation_selected, 2);

        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.confirmation_selected, 1);

        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Tab,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.confirmation_selected, 2);

        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::BackTab,
            crossterm::event::KeyModifiers::SHIFT,
        ));
        assert_eq!(app.confirmation_selected, 1);

        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.confirmation_selected, 0);

        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::End,
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Editing);
        assert!(!app.should_quit);

        app.begin_quit();
        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(app.should_quit);
    }

    #[test]
    fn myn_ux13_mouse_drag_creates_active_selection() {
        let mut app = App::new(Buffer::empty(None));
        app.copy_on_select = true;
        app.buffer.insert_text(&mut app.cursor, "hello world");
        let gutter = ui::gutter_width(app.buffer.line_count());

        // Mouse down on the first editor row below the two-row header.
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: gutter + 2,
                row: ui::HEADER_ROWS,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );
        assert_eq!(app.cursor, Cursor::new(0, 2));
        assert_eq!(app.selection_anchor, Some(Cursor::new(0, 2)));

        // Mouse drag to col = gutter + 5
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: gutter + 5,
                row: ui::HEADER_ROWS,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );
        assert_eq!(app.cursor, Cursor::new(0, 5));
        assert_eq!(
            app.selection_range(),
            Some((Cursor::new(0, 2), Cursor::new(0, 5)))
        );

        // Mouse up completes drag selection
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: gutter + 5,
                row: ui::HEADER_ROWS,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );
        assert_eq!(
            app.selection_range(),
            Some((Cursor::new(0, 2), Cursor::new(0, 5)))
        );
        assert_eq!(app.clipboard, "llo");
        assert_eq!(app.status.as_deref(), Some("Mellow only · Copied 3 chars"));
    }

    #[test]
    fn goal17_branch_switch_blocks_dirty_mellow_buffers() {
        let dir = git_workspace();
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["branch", "feature/demo"])
            .output()
            .unwrap();
        assert!(output.status.success());

        let path = dir.path().join("demo.txt");
        let mut app = App::new(Buffer::open(Some(path)).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::GitBranches);
        app.execute(Command::Insert('!'));
        app.git_branch_selected = app
            .git_branches
            .iter()
            .position(|branch| branch.name == "feature/demo")
            .unwrap();
        app.switch_selected_git_branch();

        assert!(app.status.as_deref().unwrap_or("").contains("blocked"));
        assert_ne!(app.git_snapshot.branch, "feature/demo");
    }

    #[test]
    fn goal17_clean_branch_switch_reloads_open_file_from_target_branch() {
        let dir = git_workspace();
        let initial = String::from_utf8(
            std::process::Command::new("git")
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

        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["switch", "-c", "feature/demo"]);
        std::fs::write(dir.path().join("demo.txt"), "feature branch\n").unwrap();
        run(&["add", "demo.txt"]);
        run(&["commit", "-q", "-m", "feature content"]);
        run(&["switch", &initial]);

        let path = dir.path().join("demo.txt");
        let mut app = App::new(Buffer::open(Some(path)).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::GitBranches);
        app.git_branch_selected = app
            .git_branches
            .iter()
            .position(|branch| branch.name == "feature/demo")
            .unwrap();
        app.switch_selected_git_branch();

        assert_eq!(app.git_snapshot.branch, "feature/demo");
        assert_eq!(app.buffer.contents(), "feature branch\n");
        assert!(!app.buffer.is_dirty());
    }

    #[test]
    fn goal_git_changes_review_jumps_to_exact_hunk_and_blocks_dirty_stage() {
        let dir = git_workspace();
        let path = dir.path().join("demo.txt");
        std::fs::write(
            &path,
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nTWELVE\n",
        )
        .unwrap();

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::ShowChanges);

        assert_eq!(app.mode, AppMode::Changes);
        assert_eq!(app.git_snapshot.hunks.len(), 1);
        app.handle_changes_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(app.cursor.row, 11);

        app.execute(Command::Insert('!'));
        app.execute(Command::ShowChanges);
        app.handle_changes_key(KeyEvent::new(
            KeyCode::Char('s'),
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(app.git_snapshot.staged_count(), 0);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.starts_with("Save or discard editor changes"))
        );
    }

    #[test]
    fn goal_git_revert_requires_confirmation_and_reloads_clean_buffer() {
        let dir = git_workspace();
        let path = dir.path().join("demo.txt");
        std::fs::write(
            &path,
            "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
        )
        .unwrap();

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::ShowChanges);
        app.handle_changes_key(KeyEvent::new(
            KeyCode::Char('r'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::ConfirmRevertHunk);

        app.handle_revert_confirmation_key(KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Changes);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("ONE\n"));

        app.handle_changes_key(KeyEvent::new(
            KeyCode::Char('r'),
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_revert_confirmation_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert!(std::fs::read_to_string(&path).unwrap().starts_with("one\n"));
        assert!(app.buffer.contents().starts_with("one\n"));
        assert!(!app.buffer.is_dirty());
    }

    #[test]
    fn goal_replace_preview_and_replace_all_are_one_undo_transaction() {
        let mut app = App::new(Buffer::empty(Some(PathBuf::from("demo.txt"))));
        app.buffer
            .insert_text(&mut app.cursor, "foo1 foo2\nFOO3 untouched");
        let original = app.buffer.contents();
        app.cursor = Cursor::new(0, 0);
        app.replace_query.set(r"foo(\d)");
        app.replace_with.set("bar$1");
        app.replace_options.regex = true;

        app.execute(Command::Replace);

        assert_eq!(app.mode, AppMode::Replace);
        assert_eq!(app.replace_preview.len(), 3);
        assert_eq!(app.buffer.contents(), original);
        assert_eq!(app.replace_preview[0].replacement, "bar1");

        app.handle_replace_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::CONTROL,
        ));

        assert_eq!(app.buffer.contents(), "bar1 bar2\nbar3 untouched");
        assert_eq!(app.status, Some("Replaced 3 matches".to_owned()));

        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), original);
        assert_eq!(app.cursor, Cursor::new(0, 0));
    }

    #[test]
    fn goal_replace_literal_dollar_and_whole_word_preview_are_safe() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "cat scatter cat42 cat");
        app.cursor = Cursor::new(0, 0);
        app.replace_query.set("cat");
        app.replace_with.set("$1");
        app.replace_options.whole_word = true;

        app.execute(Command::Replace);

        assert_eq!(app.replace_preview.len(), 2);
        assert!(
            app.replace_preview
                .iter()
                .all(|preview| preview.replacement == "$1")
        );

        app.replace_selected = 1;
        app.handle_replace_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.buffer.contents(), "cat scatter cat42 $1");
    }

    #[test]
    fn goal_project_search_regex_opens_and_selects_exact_unicode_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let target = dir.path().join("src/main.rs");
        std::fs::write(
            &target,
            "fn main() {\n    let label = \"🙂 Needle42\";\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "No target here\n").unwrap();

        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.project_search_query.set(r"Needle\d+");
        app.project_search_options.regex = true;
        app.project_search_options.case_sensitive = true;

        app.execute(Command::ProjectSearch);

        assert_eq!(app.mode, AppMode::ProjectSearch);
        assert_eq!(app.project_search_report.results.len(), 1);
        let result = &app.project_search_report.results[0];
        assert_eq!(result.path, PathBuf::from("src/main.rs"));
        assert_eq!(result.row, 1);

        app.handle_project_search_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(app.buffer.path(), Some(&target));
        let (start, end) = app.selection_range().unwrap();
        assert_eq!(app.buffer.get_text_range(start, end), "Needle42");
    }

    #[test]
    fn goal_project_search_option_shortcuts_recalculate_results() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("words.txt"), "Cat cat scatter cat42\n").unwrap();

        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.project_search_query.set("cat");
        app.execute(Command::ProjectSearch);
        assert_eq!(app.project_search_report.results.len(), 4);

        app.handle_project_search_key(KeyEvent::new(
            KeyCode::Char('c'),
            crossterm::event::KeyModifiers::ALT,
        ));
        assert_eq!(app.project_search_report.results.len(), 3);

        app.handle_project_search_key(KeyEvent::new(
            KeyCode::Char('w'),
            crossterm::event::KeyModifiers::ALT,
        ));
        assert_eq!(app.project_search_report.results.len(), 1);
    }

    #[test]
    fn goal_code_intel_tree_sitter_updates_incrementally_and_feeds_problems() {
        let mut app = App::new(Buffer::empty(Some(PathBuf::from("demo.rs"))));
        let initial_generation = app
            .syntax_document
            .as_ref()
            .map(|document| document.parse_generation())
            .unwrap();

        app.handle_paste("fn main( {");

        let syntax = app.syntax_document.as_ref().unwrap();
        assert!(syntax.parse_generation() > initial_generation);
        assert!(syntax.incremental_reparse_count() >= 1);
        let problems = app.problem_items();
        assert!(
            problems
                .iter()
                .any(|problem| problem.source == "Tree-sitter")
        );

        app.execute(Command::ShowProblems);
        assert_eq!(app.mode, AppMode::Problems);
    }

    #[test]
    fn polish_completion_filters_sorts_and_rejects_stale_request_state() {
        let mut app = App::new(Buffer::empty(Some(PathBuf::from("demo.rs"))));
        app.handle_paste("pri");

        app.completion_source_items = vec![
            CompletionItem {
                label: "private".to_owned(),
                detail: Some("keyword".to_owned()),
                filter_text: Some("private".to_owned()),
                sort_text: Some("002".to_owned()),
                insert_text: "private".to_owned(),
                edit_range: None,
            },
            CompletionItem {
                label: "println!".to_owned(),
                detail: Some("macro".to_owned()),
                filter_text: Some("println".to_owned()),
                sort_text: Some("001".to_owned()),
                insert_text: "println!".to_owned(),
                edit_range: None,
            },
            CompletionItem {
                label: "Vec".to_owned(),
                detail: Some("struct".to_owned()),
                filter_text: None,
                sort_text: Some("000".to_owned()),
                insert_text: "Vec".to_owned(),
                edit_range: None,
            },
        ];
        app.refresh_completion_filter();

        assert_eq!(
            app.completion_items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["println!", "private"]
        );
        assert_eq!(app.mode, AppMode::Completion);

        let request = CompletionRequestState {
            request_id: 42,
            revision: app.buffer.revision(),
            cursor: app.cursor,
            path: app.buffer.path().cloned(),
            manual: false,
        };
        assert!(app.completion_request_is_current(&request));

        app.completion_request = Some(request.clone());
        assert!(app.take_matching_completion_request(41).is_none());
        assert_eq!(
            app.completion_request
                .as_ref()
                .map(|state| state.request_id),
            Some(42)
        );
        assert!(app.take_matching_completion_request(42).is_some());
        assert!(app.completion_request.is_none());

        app.cursor.col = app.cursor.col.saturating_sub(1);
        assert!(!app.completion_request_is_current(&request));
    }

    #[test]
    fn goal_code_intel_completion_text_edit_is_utf16_aware_and_undoable() {
        let mut app = App::new(Buffer::empty(Some(PathBuf::from("demo.rs"))));
        app.handle_paste("let pri🙂 = 1;");
        app.completion_items = vec![CompletionItem {
            label: "println!".to_owned(),
            detail: Some("macro".to_owned()),
            filter_text: None,
            sort_text: None,
            insert_text: "println!".to_owned(),
            edit_range: Some(crate::lsp::LspRange {
                start: LspPosition {
                    line: 0,
                    character: 4,
                },
                end: LspPosition {
                    line: 0,
                    character: 7,
                },
            }),
        }];
        app.completion_selected = 0;
        app.mode = AppMode::Completion;

        app.handle_completion_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(app.buffer.contents(), "let println!🙂 = 1;");
        assert_eq!(app.mode, AppMode::Editing);

        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "let pri🙂 = 1;");
    }

    #[test]
    fn goal_code_intel_definition_jump_opens_target_and_maps_utf16_column() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.rs");
        let two = dir.path().join("two.rs");
        std::fs::write(&one, "fn main() {}").unwrap();
        std::fs::write(&two, "🙂target\n").unwrap();

        let mut app = App::new(Buffer::open(Some(one)).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.apply_definition_location(DefinitionLocation {
            path: two.clone(),
            range: crate::lsp::LspRange {
                start: LspPosition {
                    line: 0,
                    character: 2,
                },
                end: LspPosition {
                    line: 0,
                    character: 8,
                },
            },
        });

        assert_eq!(app.buffer.path(), Some(&two));
        assert_eq!(app.cursor, Cursor::new(0, 1));
    }

    #[test]
    fn goal_session_restore_recovers_tabs_views_split_and_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one\nline").unwrap();
        std::fs::write(&two, "two\nline").unwrap();

        let state = SessionState {
            documents: vec![
                SessionDocument {
                    path: one.clone(),
                    cursor_row: 1,
                    cursor_col: 2,
                    scroll_row: 1,
                    scroll_col: 0,
                    visual_scroll_row: 1,
                },
                SessionDocument {
                    path: two.clone(),
                    cursor_row: 0,
                    cursor_col: 1,
                    scroll_row: 0,
                    scroll_col: 0,
                    visual_scroll_row: 0,
                },
            ],
            active_document: 1,
            primary_document: 0,
            secondary_document: Some(1),
            active_pane: 1,
            split_ratio: 64,
            split_orientation: 1,
            word_wrap: true,
            theme_preset: 1,
            show_whitespace: true,
            show_indent_guides: false,
            explorer_visible: true,
            explorer_selected: 0,
        };

        let app = App::from_session(dir.path().to_path_buf(), state)
            .unwrap()
            .unwrap();

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.buffer.path(), Some(&two));
        assert!(app.word_wrap);
        // Theme is global: the session's Light does not replace the setting.
        assert_eq!(app.theme_preset, ThemePreset::Dark);
        assert!(app.show_whitespace);
        assert!(!app.show_indent_guides);
        assert!(app.explorer_visible);
        assert!(app.split_enabled());
        assert_eq!(app.active_pane_index(), 1);
        assert_eq!(app.split_ratio(), 64);
        assert_eq!(app.split_orientation(), SplitOrientation::Stacked);
        assert_eq!(app.pane_view(0).unwrap().buffer.path(), Some(&one));
        assert_eq!(app.pane_view(1).unwrap().buffer.path(), Some(&two));
    }

    #[test]
    fn goal_explorer_loads_project_files_and_opens_selected_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let target = dir.path().join("src/main.rs");
        std::fs::write(&target, "fn main() {}").unwrap();

        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::ToggleExplorer);

        assert!(app.explorer_visible);
        assert!(app.explorer_focused);
        assert!(
            app.explorer_files
                .iter()
                .any(|entry| entry.path == std::path::Path::new("src") && entry.is_dir)
        );
        assert!(
            !app.explorer_files
                .iter()
                .any(|entry| entry.path == std::path::Path::new("src/main.rs"))
        );

        app.explorer_selected = app
            .explorer_files
            .iter()
            .position(|entry| entry.path == std::path::Path::new("src"))
            .unwrap();
        app.handle_explorer_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(
            app.explorer_files
                .iter()
                .any(|entry| entry.path == std::path::Path::new("src/main.rs"))
        );

        app.explorer_selected = app
            .explorer_files
            .iter()
            .position(|entry| entry.path == std::path::Path::new("src/main.rs"))
            .unwrap();
        app.handle_explorer_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(app.buffer.path(), Some(&target));
        assert!(!app.explorer_focused);
    }

    #[test]
    fn workspace_directory_create_move_and_empty_delete_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();

        app.explorer_action_query.set("src");
        app.mode = AppMode::ExplorerCreateDirectory;
        app.handle_explorer_create_directory_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(dir.path().join("src").is_dir());

        app.explorer_action_target = Some(PathBuf::from("src"));
        app.explorer_action_query.set("lib");
        app.mode = AppMode::ExplorerRename;
        app.handle_explorer_rename_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(!dir.path().join("src").exists());
        assert!(dir.path().join("lib").is_dir());

        app.explorer_action_target = Some(PathBuf::from("lib"));
        app.mode = AppMode::ConfirmExplorerDelete;
        app.handle_explorer_delete_confirmation(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(!dir.path().join("lib").exists());
    }

    #[test]
    fn workspace_directory_mutation_blocks_nonempty_delete_and_open_descendants() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let file = dir.path().join("src/main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let mut app = App::new(Buffer::open(Some(file.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.reload_explorer();
        app.explorer_selected = app
            .explorer_files
            .iter()
            .position(|entry| entry.path == std::path::Path::new("src"))
            .unwrap();

        app.begin_explorer_rename();
        assert_eq!(app.mode, AppMode::Editing);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("Close open file")
        );

        app.begin_explorer_delete();
        assert_eq!(app.mode, AppMode::Editing);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("Close open file")
                || app
                    .status
                    .as_deref()
                    .unwrap_or("")
                    .contains("only empty directories")
        );
        assert!(dir.path().join("src").exists());
    }

    #[test]
    fn conflict_marker_detection_requires_standard_marker_lines() {
        assert!(App::has_conflict_markers(
            "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> side\n"
        ));
        assert!(!App::has_conflict_markers(
            "ordinary ===== text\nnot a marker >>> value\n"
        ));
    }

    #[test]
    fn goal14_explorer_create_rename_move_delete_are_guarded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::ToggleExplorer);

        app.explorer_action_query.set("new.txt");
        app.mode = AppMode::ExplorerCreate;
        app.handle_explorer_create_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        let created = dir.path().join("new.txt");
        assert!(created.exists());
        assert_eq!(app.buffer.path(), Some(&created));

        app.close_active_tab_now(true);
        app.reload_explorer();
        app.explorer_selected = app
            .explorer_files
            .iter()
            .position(|entry| entry.path == std::path::Path::new("new.txt"))
            .unwrap();
        app.begin_explorer_rename();
        assert_eq!(app.mode, AppMode::ExplorerRename);
        app.explorer_action_query.set("src/moved.txt");
        app.handle_explorer_rename_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        let moved = dir.path().join("src/moved.txt");
        assert!(!created.exists());
        assert!(moved.exists());

        app.reload_explorer();
        app.explorer_expanded.insert(PathBuf::from("src"));
        app.reload_explorer();
        app.explorer_selected = app
            .explorer_files
            .iter()
            .position(|entry| entry.path == std::path::Path::new("src/moved.txt"))
            .unwrap();
        app.begin_explorer_delete();
        assert_eq!(app.mode, AppMode::ConfirmExplorerDelete);
        app.handle_explorer_delete_confirmation(KeyEvent::new(
            KeyCode::Char('y'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(!moved.exists());
    }

    #[test]
    fn goal14_explorer_blocks_escape_and_open_file_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("open.txt");
        std::fs::write(&target, "hello").unwrap();

        let mut app = App::new(Buffer::open(Some(target.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::ToggleExplorer);
        app.explorer_selected = app
            .explorer_files
            .iter()
            .position(|entry| entry.path == std::path::Path::new("open.txt"))
            .unwrap();

        app.begin_explorer_rename();
        assert_eq!(app.mode, AppMode::Editing);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("Close the file")
        );

        app.explorer_action_query.set("../escape.txt");
        app.mode = AppMode::ExplorerCreate;
        app.handle_explorer_create_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
        assert!(app.status.as_deref().unwrap_or("").contains("blocked"));
    }

    #[test]
    fn final_core_stacked_split_toggles_maps_mouse_rows_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one\nline\nmore\n").unwrap();
        std::fs::write(&two, "two\nline\nmore\n").unwrap();

        let mut app = App::new(Buffer::open(Some(one)).unwrap());
        app.open_path_in_tab(two).unwrap();
        app.switch_tab(0);
        app.execute(Command::SplitEditor);
        assert_eq!(app.split_orientation(), SplitOrientation::SideBySide);

        app.execute(Command::ToggleSplitOrientation);
        assert_eq!(app.split_orientation(), SplitOrientation::Stacked);
        assert_eq!(app.session_snapshot().split_orientation, 1);

        let editor_top = ui::HEADER_ROWS;
        let editor_bottom = 20;
        let divider =
            ui::split_divider_row(editor_top, editor_bottom, true, app.split_ratio()).unwrap();
        let (top_pane, top_local, _) =
            ui::stacked_pane_hit(editor_top, editor_bottom, app.split_ratio(), editor_top).unwrap();
        let (bottom_pane, bottom_local, _) =
            ui::stacked_pane_hit(editor_top, editor_bottom, app.split_ratio(), divider + 1)
                .unwrap();
        assert_eq!((top_pane, top_local), (0, 0));
        assert_eq!((bottom_pane, bottom_local), (1, 0));
        assert!(
            ui::stacked_pane_hit(editor_top, editor_bottom, app.split_ratio(), divider).is_none()
        );

        app.resize_split_to_row(editor_top, editor_bottom, editor_top + 12);
        assert!(app.split_ratio() > 50);
    }

    #[test]
    fn goal14_split_resize_clamps_and_changes_pane_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one").unwrap();
        std::fs::write(&two, "two").unwrap();

        let mut app = App::new(Buffer::open(Some(one)).unwrap());
        app.open_path_in_tab(two).unwrap();
        app.switch_tab(0);
        app.execute(Command::SplitEditor);

        app.resize_split_to_column(100, 70);
        assert_eq!(app.split_ratio(), 70);
        let left = ui::pane_geometry(100, false, true, app.split_ratio(), 0)
            .unwrap()
            .1;
        let right = ui::pane_geometry(100, false, true, app.split_ratio(), 1)
            .unwrap()
            .1;
        assert!(left > right);

        app.resize_split_to_column(100, 2);
        assert_eq!(app.split_ratio(), 20);
        app.resize_split_to_column(100, 99);
        assert_eq!(app.split_ratio(), 80);
    }

    #[test]
    fn goal14_breadcrumbs_are_workspace_relative_and_unicode_safe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/తెలుగు")).unwrap();
        let file = dir.path().join("src/తెలుగు/main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let mut app = App::new(Buffer::open(Some(file)).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        assert_eq!(
            app.breadcrumb_segments(),
            vec!["src".to_owned(), "తెలుగు".to_owned(), "main.rs".to_owned()]
        );
    }

    #[test]
    fn goal_split_editor_shows_two_tabs_and_f6_moves_real_edit_focus() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one").unwrap();
        std::fs::write(&two, "two").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.open_path_in_tab(two.clone()).unwrap();
        app.switch_tab(0);
        app.execute(Command::SplitEditor);

        assert!(app.split_enabled());
        assert_eq!(app.pane_view(0).unwrap().buffer.path(), Some(&one));
        assert_eq!(app.pane_view(1).unwrap().buffer.path(), Some(&two));
        assert_eq!(app.active_pane_index(), 0);

        app.execute(Command::FocusNextPane);
        assert_eq!(app.active_pane_index(), 1);
        assert_eq!(app.buffer.path(), Some(&two));

        app.execute(Command::Insert('!'));
        assert_eq!(app.buffer.contents(), "!two");

        app.execute(Command::FocusNextPane);
        assert_eq!(app.buffer.path(), Some(&one));
        assert_eq!(app.buffer.contents(), "one");
    }

    #[test]
    fn goal_split_close_and_tab_close_repair_pane_assignments() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one").unwrap();
        std::fs::write(&two, "two").unwrap();

        let mut app = App::new(Buffer::open(Some(one)).unwrap());
        app.open_path_in_tab(two).unwrap();
        app.switch_tab(0);
        app.execute(Command::SplitEditor);
        app.execute(Command::FocusNextPane);
        app.execute(Command::CloseTab);

        assert_eq!(app.tabs.len(), 1);
        assert!(!app.split_enabled());
        assert_eq!(app.active_pane_index(), 0);

        app.execute(Command::SplitEditor);
        assert!(app.split_enabled());
        app.execute(Command::CloseSplit);
        assert!(!app.split_enabled());
    }

    #[test]
    fn goal_wrap_toggle_preserves_source_and_disables_horizontal_scroll() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "alpha beta gamma delta epsilon zeta");
        let original = app.buffer.contents();
        app.scroll_col = 12;

        app.execute(Command::ToggleWordWrap);

        assert!(app.word_wrap);
        assert_eq!(app.scroll_col, 0);
        assert_eq!(app.buffer.contents(), original);
        assert_eq!(app.status, Some("Word wrap: on".to_owned()));
    }

    #[test]
    fn goal_wrap_vertical_navigation_moves_through_visual_rows() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "alpha beta gamma delta epsilon");
        app.cursor = Cursor::new(0, 0);
        app.word_wrap = true;
        app.keep_cursor_visible(14, 20);

        app.execute(Command::MoveDown);
        assert_eq!(app.cursor.row, 0);
        assert!(app.cursor.col > 0);
        let wrapped_col = app.cursor.col;

        app.execute(Command::MoveUp);
        assert_eq!(app.cursor, Cursor::new(0, 0));

        app.execute(Command::MoveDown);
        assert_eq!(app.cursor.col, wrapped_col);
    }

    #[test]
    fn goal_wrap_mouse_click_on_continuation_maps_back_to_source() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "alpha beta gamma delta epsilon");
        app.cursor = Cursor::new(0, 0);
        app.word_wrap = true;
        app.keep_cursor_visible(14, 20);
        let gutter = ui::gutter_width(app.buffer.line_count());

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: gutter + 1,
                row: ui::HEADER_ROWS + 1,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            14,
            20,
        );

        assert_eq!(app.cursor.row, 0);
        assert!(app.cursor.col > 0);
    }

    #[test]
    fn goal_pointer_double_and_triple_click_select_word_and_line() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "hello world\nnext line");
        let gutter = ui::gutter_width(app.buffer.line_count());
        let click = |kind| MouseEvent {
            kind,
            column: gutter + 2,
            row: ui::HEADER_ROWS,
            modifiers: crossterm::event::KeyModifiers::empty(),
        };

        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)), 80, 24);
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)), 80, 24);
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)), 80, 24);

        let (start, end) = app.selection_range().unwrap();
        assert_eq!(app.buffer.get_text_range(start, end), "hello");

        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)), 80, 24);
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)), 80, 24);
        assert_eq!(
            app.selection_range(),
            Some((Cursor::new(0, 0), Cursor::new(1, 0)))
        );
    }

    #[test]
    fn goal_pointer_drag_beyond_editor_autoscrolls_selection() {
        let mut app = App::new(Buffer::empty(None));
        let lines: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        app.buffer.insert_text(&mut app.cursor, &lines.join("\n"));
        app.scroll_row = 5;
        let gutter = ui::gutter_width(app.buffer.line_count());

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: gutter + 1,
                row: ui::HEADER_ROWS + 1,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );
        let anchor = app.selection_anchor.unwrap();

        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: gutter + 3,
                row: 23,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
            80,
            24,
        );

        assert_eq!(app.scroll_row, 6);
        assert!(app.cursor.row > anchor.row);
        assert!(app.selection_range().is_some());
    }

    #[test]
    fn goal_editing_bracket_pairing_and_line_commands_are_semantic() {
        let mut app = App::new(Buffer::empty(Some(PathBuf::from("demo.rs"))));
        app.execute(Command::Insert('('));
        assert_eq!(app.buffer.contents(), "()");
        assert_eq!(app.cursor, Cursor::new(0, 1));

        app.execute(Command::Insert(')'));
        assert_eq!(app.buffer.contents(), "()");
        assert_eq!(app.cursor, Cursor::new(0, 2));

        app.buffer.insert_text(&mut app.cursor, "\none\ntwo");
        app.cursor = Cursor::new(1, 1);
        app.execute(Command::ToggleComment);
        assert_eq!(app.buffer.line_text(1), "// one");

        app.execute(Command::DuplicateLines);
        assert_eq!(app.buffer.line_text(2), "// one");

        app.execute(Command::MoveLinesDown);
        assert_eq!(app.buffer.line_text(3), "// one");
    }

    #[test]
    fn goal_visual_theme_and_whitespace_controls_change_real_state() {
        let mut app = App::new(Buffer::empty(None));
        assert_eq!(app.theme_preset, ThemePreset::Dark);
        assert!(!app.show_whitespace);
        assert!(app.show_indent_guides);

        app.execute(Command::CycleTheme);
        assert_eq!(app.theme_preset, ThemePreset::Light);
        for expected in [
            ThemePreset::HighContrast,
            ThemePreset::TokyoNight,
            ThemePreset::CatppuccinMocha,
            ThemePreset::GruvboxDark,
            ThemePreset::Dark,
        ] {
            app.execute(Command::CycleTheme);
            assert_eq!(app.theme_preset, expected);
            assert_eq!(app.theme.canvas, Theme::for_preset(expected).canvas);
        }

        app.execute(Command::ToggleWhitespace);
        assert!(app.show_whitespace);

        app.execute(Command::ToggleIndentGuides);
        assert!(!app.show_indent_guides);
    }

    #[test]
    fn goal_terminal_focus_isolates_normal_typing_from_editor_buffer() {
        let mut app = App::new(Buffer::empty(None));
        app.execute(Command::Insert('a'));
        app.execute(Command::ToggleTerminal);
        assert!(app.terminal_visible);
        assert!(app.terminal_focused);

        app.handle_key(KeyEvent::new(
            KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.buffer.contents(), "a");
    }

    fn rendered_rows(app: &App, width: u16, height: u16) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    fn click(app: &mut App, column: u16, row: u16, width: u16, height: u16) {
        use crossterm::event::KeyModifiers;
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.handle_mouse(
                MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::empty(),
                },
                width,
                height,
            );
        }
    }

    #[test]
    fn explorer_click_opens_the_row_under_the_pointer() {
        let dir = tempfile::tempdir().unwrap();
        for folder in ["infra", "scripts", "src"] {
            std::fs::create_dir(dir.path().join(folder)).unwrap();
        }
        std::fs::write(dir.path().join("scripts/deploy.sh"), "echo hi\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# demo\n").unwrap();
        std::fs::write(dir.path().join("config.yaml"), "a: 1\n").unwrap();

        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.explorer_visible = true;
        app.reload_explorer();
        let (width, height) = (120, 34);

        let locate = |app: &App, name: &str| -> (u16, u16) {
            let rows = rendered_rows(app, width, height);
            rows.iter()
                .enumerate()
                .find_map(|(y, row)| {
                    let explorer: String = row.chars().take(30).collect();
                    explorer.find(name).map(|x| (x as u16, y as u16))
                })
                .unwrap_or_else(|| panic!("{name} not rendered:\n{}", rows.join("\n")))
        };

        let (x, y) = locate(&app, "scripts");
        click(&mut app, x + 1, y, width, height);
        assert!(
            app.explorer_entry_expanded(std::path::Path::new("scripts")),
            "clicking 'scripts' must expand scripts"
        );
        assert!(!app.explorer_entry_expanded(std::path::Path::new("src")));

        let (x, y) = locate(&app, "deploy.sh");
        click(&mut app, x + 1, y, width, height);
        assert!(
            app.buffer
                .path()
                .is_some_and(|path| path.ends_with("scripts/deploy.sh")),
            "clicking deploy.sh opened {:?}",
            app.buffer.path()
        );

        let (x, y) = locate(&app, "config.yaml");
        click(&mut app, x + 1, y, width, height);
        assert!(
            app.buffer
                .path()
                .is_some_and(|path| path.ends_with("config.yaml")),
            "clicking config.yaml opened {:?}",
            app.buffer.path()
        );
    }

    #[test]
    fn terminal_focus_always_returns_to_editor_from_the_keyboard() {
        use crossterm::event::KeyModifiers;
        let mut app = App::new(Buffer::empty(None));
        app.execute(Command::ToggleTerminal);
        assert!(app.terminal_focused);

        // Ctrl+T works in every terminal.
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(!app.terminal_focused);
        assert!(
            app.terminal_visible,
            "returning to the editor keeps the shell"
        );

        // The same key goes back into the still-visible terminal.
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(app.terminal_focused);

        // Legacy terminals deliver Ctrl+` as Ctrl+Space (NUL).
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(!app.terminal_focused);

        app.execute(Command::HideTerminal);
        assert!(!app.terminal_visible);
    }

    #[test]
    fn goal_terminal_multiple_sessions_switch_and_close_independently() {
        let mut app = App::new(Buffer::empty(None));
        app.execute(Command::NewTerminal);
        app.execute(Command::NewTerminal);
        assert_eq!(app.terminal_session_count(), 2);

        let before = app.active_terminal_index();
        app.execute(Command::NextTerminal);
        assert_ne!(app.active_terminal_index(), before);

        app.execute(Command::CloseTerminal);
        assert_eq!(app.terminal_session_count(), 1);
        assert!(app.terminal_visible);
    }

    #[test]
    fn goal_workspace_switching_preserves_each_buffer_view_and_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        let three = dir.path().join("three.txt");
        std::fs::write(&one, "one line").unwrap();
        std::fs::write(&two, "two line").unwrap();
        std::fs::write(&three, "three line").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.cursor = Cursor::new(0, 3);
        app.execute(Command::Insert('!'));
        app.cursor = Cursor::new(0, 4);
        app.selection_anchor = Some(Cursor::new(0, 1));
        app.scroll_col = 2;
        let one_text = app.buffer.contents();

        app.open_path_in_tab(two.clone()).unwrap();
        app.cursor = Cursor::new(0, 2);
        app.execute(Command::Insert('?'));
        let two_text = app.buffer.contents();

        app.open_path_in_tab(three.clone()).unwrap();
        assert_eq!(app.tabs.len(), 3);
        assert_eq!(app.active_tab_index(), 2);

        app.switch_tab(0);
        assert_eq!(app.buffer.path(), Some(&one));
        assert_eq!(app.buffer.contents(), one_text);
        assert_eq!(app.cursor, Cursor::new(0, 4));
        assert_eq!(app.selection_anchor, Some(Cursor::new(0, 1)));
        assert_eq!(app.scroll_col, 2);
        assert!(app.buffer.is_dirty());

        app.switch_tab(1);
        assert_eq!(app.buffer.path(), Some(&two));
        assert_eq!(app.buffer.contents(), two_text);
        assert!(app.buffer.is_dirty());

        app.switch_tab(2);
        assert_eq!(app.buffer.path(), Some(&three));
        assert!(!app.buffer.is_dirty());
    }

    #[test]
    fn goal_workspace_quick_open_filters_and_opens_selected_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let target = src.join("two.rs");
        std::fs::write(&target, "fn two() {}").unwrap();

        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::OpenFile);
        assert_eq!(app.mode, AppMode::QuickOpen);

        app.quick_open_query.set("two");
        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(app.buffer.path(), Some(&target));
        // The empty Untitled tab is reused rather than left behind.
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn release_readiness_quick_open_tab_completes_workspace_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
        let target = dir.path().join("src/components/editor.rs");
        std::fs::write(&target, "fn editor() {}").unwrap();

        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::OpenFile);

        app.quick_open_query.set("s");
        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Tab,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.quick_open_query.as_str(), "src/");

        app.quick_open_query.set("src/c");
        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Tab,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.quick_open_query.as_str(), "src/components/");

        app.quick_open_query.set("src/components/e");
        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Tab,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.quick_open_query.as_str(), "src/components/editor.rs");

        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.buffer.path(), Some(&target));
    }

    #[test]
    fn quick_open_pasted_parent_paths_list_and_open_files_outside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let target = dir.path().join("outside.txt");
        std::fs::write(&target, "outside text").unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = root;
        app.execute(Command::OpenFile);
        app.handle_paste("../out");
        assert_eq!(app.filtered_quick_open_entries().len(), 1);
        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Tab,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.quick_open_query.as_str(), "../outside.txt");
        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(
            std::fs::canonicalize(app.buffer.path().unwrap()).unwrap(),
            std::fs::canonicalize(target).unwrap()
        );
        assert_eq!(app.buffer.contents(), "outside text");
    }

    #[test]
    fn quick_open_enter_on_directory_lists_contents_instead_of_opening_a_buffer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/note.txt"), "note").unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.execute(Command::OpenFile);
        app.handle_paste("./do");
        app.handle_quick_open_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::QuickOpen);
        assert_eq!(app.filtered_quick_open_entries().len(), 1);
        assert!(app.quick_open_candidates[0].ends_with("note.txt"));
        assert!(app.buffer.path().is_none());
    }

    #[test]
    fn goal_workspace_opening_same_file_reuses_existing_tab() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one").unwrap();
        std::fs::write(&two, "two").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.open_path_in_tab(two).unwrap();
        assert_eq!(app.tabs.len(), 2);

        app.open_path_in_tab(one.clone()).unwrap();
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.buffer.path(), Some(&one));
    }

    #[test]
    fn goal_workspace_dirty_close_requires_explicit_confirmation() {
        let mut app = App::new(Buffer::empty(None));
        app.execute(Command::Insert('x'));
        app.execute(Command::CloseTab);
        assert_eq!(app.mode, AppMode::ConfirmCloseTab);
        assert_eq!(app.buffer.contents(), "x");

        app.handle_close_tab_confirmation(KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(app.buffer.contents(), "x");
    }

    #[test]
    fn goal_quit_walks_to_inactive_dirty_tab_instead_of_losing_it() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one").unwrap();
        std::fs::write(&two, "two").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.execute(Command::Insert('!'));
        app.open_path_in_tab(two).unwrap();
        assert!(!app.buffer.is_dirty());

        app.execute(Command::Quit);
        assert_eq!(app.mode, AppMode::ConfirmQuit);
        assert_eq!(app.buffer.path(), Some(&one));
        assert!(app.buffer.is_dirty());
        assert!(!app.should_quit);
    }

    #[test]
    fn goal_safety_save_conflict_preserves_disk_and_offers_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.txt");
        std::fs::write(&path, "disk").unwrap();

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.cursor = Cursor::new(0, 4);
        app.execute(Command::Insert('!'));
        std::fs::write(&path, "external").unwrap();

        app.execute(Command::Save);
        assert_eq!(app.mode, AppMode::SaveConflict);
        assert!(app.conflict_reason.is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
        assert!(app.buffer.is_dirty());

        app.handle_save_conflict_key(KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Editing);
        assert!(app.buffer.is_dirty());
    }

    /// Audit: saving a `chmod 444` file replaced it and said "Saved".
    #[test]
    fn saving_a_read_only_file_asks_first_and_overwrite_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.conf");
        std::fs::write(&path, "keep").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.cursor = Cursor::new(0, 4);
        app.execute(Command::Insert('!'));
        app.execute(Command::Save);
        assert_eq!(app.mode, AppMode::SaveConflict);
        assert!(app.conflict_read_only);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");

        let key = |code| KeyEvent::new(code, crossterm::event::KeyModifiers::empty());
        app.handle_save_conflict_key(key(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Editing);
        assert!(app.buffer.is_dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");

        app.execute(Command::Save);
        app.handle_save_conflict_key(key(KeyCode::Char('o')));
        assert_eq!(app.mode, AppMode::Editing);
        assert!(!app.buffer.is_dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep!");
        assert!(
            std::fs::metadata(&path).unwrap().permissions().readonly(),
            "the file keeps its read-only mode"
        );
    }

    #[test]
    fn goal_recovery_startup_can_restore_unsaved_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recover.txt");
        std::fs::write(&path, "disk").unwrap();
        recovery::write(&RecoveryKey::File(path.clone()), "draft from crash").unwrap();

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        assert_eq!(app.mode, AppMode::Recovery);

        app.handle_recovery_key(KeyEvent::new(
            KeyCode::Char('r'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(app.buffer.contents(), "draft from crash");
        assert!(app.buffer.is_dirty());

        recovery::clear(&RecoveryKey::File(path.clone())).unwrap();
    }

    /// Audit F1: a second unnamed tab must never erase the first one's journal,
    /// and after a crash every draft comes back with its own contents.
    #[test]
    fn audit_f1_two_unnamed_drafts_each_keep_and_recover_their_journal() {
        let recover = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty());
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "FIRST_UNSAVED_DRAFT");
        app.sync_recovery_journal();
        app.flush_journal();
        let first = app.buffer.recovery_key();

        app.execute(Command::NewFile);
        app.sync_recovery_journal();
        app.flush_journal();
        assert_eq!(
            recovery::load(&first).unwrap().unwrap().content,
            "FIRST_UNSAVED_DRAFT",
            "opening a clean Untitled tab must not clear another draft"
        );

        app.buffer
            .insert_text(&mut app.cursor, "SECOND_UNSAVED_DRAFT");
        app.sync_recovery_journal();
        app.flush_journal();
        let second = app.buffer.recovery_key();
        assert_ne!(first, second);
        assert_eq!(
            recovery::load(&first).unwrap().unwrap().content,
            "FIRST_UNSAVED_DRAFT"
        );

        // "Crash": a new session finds both journals as orphans.
        let orphan = |key: &RecoveryKey| {
            let mut record = recovery::load(key).unwrap().unwrap();
            record.orphan_journal = Some(recovery::write(key, &record.content).unwrap());
            record
        };
        let drafts = vec![orphan(&first), orphan(&second)];
        drop(app);

        let mut restarted = App::new(Buffer::empty(None));
        restarted.offer_orphan_drafts(drafts);
        assert_eq!(restarted.tabs.len(), 2);
        assert_eq!(restarted.mode, AppMode::Recovery);

        restarted.handle_recovery_key(recover);
        assert_eq!(restarted.buffer.contents(), "FIRST_UNSAVED_DRAFT");
        assert!(
            recovery::load(&first).unwrap().is_none(),
            "old copy released"
        );
        let restored_first = restarted.buffer.recovery_key();
        assert_eq!(
            recovery::load(&restored_first).unwrap().unwrap().content,
            "FIRST_UNSAVED_DRAFT",
            "restored draft is journaled under its new buffer"
        );

        restarted.execute(Command::NextTab);
        assert_eq!(restarted.mode, AppMode::Recovery);
        restarted.handle_recovery_key(recover);
        assert_eq!(restarted.buffer.contents(), "SECOND_UNSAVED_DRAFT");
        assert!(recovery::load(&second).unwrap().is_none());

        let restored_second = restarted.buffer.recovery_key();
        recovery::clear(&restored_first).unwrap();
        recovery::clear(&restored_second).unwrap();
    }

    #[test]
    fn saving_one_draft_leaves_other_drafts_journaled() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.buffer.insert_text(&mut app.cursor, "keep me");
        app.sync_recovery_journal();
        app.flush_journal();
        let kept = app.buffer.recovery_key();

        app.execute(Command::NewFile);
        app.buffer.insert_text(&mut app.cursor, "save me");
        app.sync_recovery_journal();
        app.flush_journal();
        app.execute(Command::Save);
        app.save_as_query.set("saved.txt".to_owned());
        app.handle_save_as_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        app.sync_recovery_journal();
        app.flush_journal();

        assert_eq!(recovery::load(&kept).unwrap().unwrap().content, "keep me");
        recovery::clear(&kept).unwrap();
    }

    /// Opens `text` as `name` with the cursor at the end of line `row`, then
    /// presses Enter.
    fn press_enter_at_end(name: &str, text: &str, row: usize) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let mut app = App::new(Buffer::open(Some(path)).unwrap());
        app.cursor = Cursor::new(row, app.buffer.grapheme_count(row));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        (dir, app)
    }

    #[test]
    fn enter_keeps_the_indentation_of_the_line_in_every_file_type() {
        for (name, text) in [
            ("main.rs", "fn main() {\n    let x = 1;\n}\n"),
            ("deploy.sh", "if true; then\n    echo hi\nfi\n"),
            ("notes.txt", "list:\n    first item\n"),
        ] {
            let (_dir, app) = press_enter_at_end(name, text, 1);
            assert_eq!(app.buffer.line_text(2), "    ", "{name}");
            assert_eq!(app.cursor, Cursor::new(2, 4), "{name}");
        }
    }

    #[test]
    fn enter_after_a_block_opener_indents_one_more_level() {
        let cases = [
            ("lib.py", "def area(r):\n", "    "),
            ("lib.py", "    if r > 0:\n", "        "),
            ("config.yaml", "services:\n", "    "),
            ("main.rs", "fn main() {\n", "    "),
            ("app.js", "call(\n", "    "),
            ("data.json", "[\n", "    "),
        ];
        for (name, text, expected) in cases {
            let (_dir, app) = press_enter_at_end(name, text, 0);
            assert_eq!(app.buffer.line_text(1), expected, "{name}: {text:?}");
        }
        // `:` opens a block only where the language says so.
        let (_dir, app) = press_enter_at_end("notes.txt", "Note:\n", 0);
        assert_eq!(app.buffer.line_text(1), "");
    }

    #[test]
    fn enter_between_brackets_puts_the_closer_on_its_own_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        std::fs::write(&path, "    if ok {}\n").unwrap();
        let mut app = App::new(Buffer::open(Some(path)).unwrap());
        app.cursor = Cursor::new(0, 11); // between { and }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(app.buffer.contents(), "    if ok {\n        \n    }\n");
        assert_eq!(app.cursor, Cursor::new(1, 8));

        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "    if ok {}\n", "one undo");
    }

    #[test]
    fn enter_indents_with_tabs_in_a_tab_indented_file() {
        let (_dir, app) = press_enter_at_end("Makefile", "all:\n\tcc main.c\n", 1);
        assert_eq!(app.buffer.line_text(2), "\t");
        let (_dir, app) = press_enter_at_end("main.go", "func main() {\n\tx := 1\n}\n", 0);
        assert_eq!(app.buffer.line_text(1), "\t");
    }

    /// A workspace with `name` holding `text` and an executable fake
    /// formatter `fmt.sh` running `script` (stdin to stdout).
    #[cfg(unix)]
    fn format_fixture(name: &str, text: &str, script: &str) -> (tempfile::TempDir, PathBuf, App) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let formatter = dir.path().join("fmt.sh");
        std::fs::write(&formatter, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&formatter, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.formatter_overrides = std::collections::HashMap::from([(
            crate::buffer::language_for_path(Some(&path)).to_owned(),
            formatter.to_string_lossy().into_owned(),
        )]);
        (dir, path, app)
    }

    #[cfg(unix)]
    #[test]
    fn format_on_save_runs_the_formatter_and_one_undo_restores_the_typed_text() {
        let (_dir, path, mut app) = format_fixture("calc.py", "x=1\n", "sed 's/=/ = /'");
        app.format_on_save = true;
        app.execute(Command::Insert('#'));
        app.execute(Command::Save);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "#x = 1\n");
        assert_eq!(app.status.as_deref(), Some("Saved · formatted with fmt.sh"));
        assert!(!app.buffer.is_dirty());
        app.execute(Command::Undo);
        assert_eq!(
            app.buffer.contents(),
            "#x=1\n",
            "one undo removes the formatting"
        );
    }

    #[cfg(unix)]
    #[test]
    fn format_on_save_is_off_unless_chosen() {
        let (_dir, path, mut app) = format_fixture("calc.py", "x=1\n", "sed 's/=/ = /'");
        assert!(!app.format_on_save);
        app.execute(Command::Insert('#'));
        app.execute(Command::Save);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "#x=1\n");
        assert_eq!(app.status.as_deref(), Some("Saved"));
    }

    /// A syntax error or a missing formatter must never cost the save.
    #[cfg(unix)]
    #[test]
    fn a_failing_or_missing_formatter_saves_the_text_unchanged_and_says_why() {
        let (_dir, path, mut app) = format_fixture(
            "deploy.sh",
            "echo hi\n",
            "echo 'deploy.sh:2:1: if statement must end with fi' >&2; exit 1",
        );
        app.format_on_save = true;
        app.execute(Command::Insert('#'));
        app.execute(Command::Save);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "#echo hi\n");
        assert_eq!(
            app.status.as_deref(),
            Some("Saved · not formatted: fmt.sh: deploy.sh:2:1: if statement must end with fi")
        );

        app.formatter_overrides
            .insert("Shell".to_owned(), "/nonexistent/shfmt".to_owned());
        app.execute(Command::Insert('#'));
        app.execute(Command::Save);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "##echo hi\n");
        assert!(
            app.status.as_deref().is_some_and(|status| status
                .starts_with("Saved · not formatted: could not start /nonexistent/shfmt")),
            "{:?}",
            app.status
        );
    }

    #[cfg(unix)]
    #[test]
    fn format_on_save_keeps_windows_line_endings() {
        let (_dir, path, mut app) = format_fixture("a.py", "a=1\r\nb=2\r\n", "sed 's/=/ = /'");
        app.format_on_save = true;
        app.execute(Command::Insert('#'));
        app.execute(Command::Save);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#a = 1\r\nb = 2\r\n"
        );
    }

    /// The real thing, where rustfmt is installed (CI has it).
    #[test]
    fn format_on_save_with_rustfmt_tidies_rust() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        std::fs::write(&path, "fn main(){let x=1;println!(\"{x}\");}\n").unwrap();
        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.formatter_overrides.clear();
        if crate::format::formatter_for("Rust", &path, &app.formatter_overrides).is_err() {
            return; // rustfmt not installed here
        }
        app.format_on_save = true;
        app.execute(Command::Save);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n"
        );
        assert_eq!(
            app.status.as_deref(),
            Some("Saved · formatted with rustfmt")
        );
    }

    /// P1 regression: every keystroke wrote and fsynced the recovery journal
    /// on the input thread, so a slow disk slowed typing.
    #[test]
    fn typing_never_waits_for_the_recovery_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.txt");
        std::fs::write(&path, "base\n").unwrap();
        let key = RecoveryKey::File(path.clone());
        let mut app = App::new(Buffer::open(Some(path)).unwrap());

        let stalled_disk = app.journal.pause();
        for ch in "abcdefghij".chars() {
            app.execute(Command::Insert(ch));
            app.journal_if_due();
        }
        assert!(
            recovery::load(&key).unwrap().is_none(),
            "no journal I/O happens on the input thread"
        );
        drop(stalled_disk);

        app.flush_journal();
        assert_eq!(
            recovery::load(&key).unwrap().unwrap().content,
            "abase\n",
            "the first edit is journaled at once, the rest are throttled"
        );
        std::thread::sleep(JOURNAL_INTERVAL);
        app.journal_if_due();
        app.flush_journal();
        assert_eq!(
            recovery::load(&key).unwrap().unwrap().content,
            app.buffer.contents(),
            "a pause in typing journals the latest text"
        );
        assert_eq!(app.journal.writes_performed(), 2);
        recovery::clear(&key).unwrap();
    }

    #[test]
    fn goal_recovery_dirty_edit_writes_journal_and_save_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.txt");
        std::fs::write(&path, "base").unwrap();

        let mut app = App::new(Buffer::open(Some(path.clone())).unwrap());
        app.cursor = Cursor::new(0, 4);
        app.execute(Command::Insert('!'));
        app.sync_recovery_journal();
        app.flush_journal();

        let record = recovery::load(&RecoveryKey::File(path.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(record.content, "base!");

        app.execute(Command::Save);
        assert!(
            recovery::load(&RecoveryKey::File(path.clone()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn launch_banner_welcomes_then_clears_on_first_key() {
        let mut app = App::new(Buffer::empty(None));
        assert!(app.launch_banner().is_none(), "only shown after run starts");

        app.launch_banner_until = Some(Instant::now() + LAUNCH_BANNER_DURATION);
        let banner = app.launch_banner().unwrap();
        assert!(banner.contains(env!("CARGO_PKG_VERSION")));
        assert!(banner.contains("Ctrl+P"));
        assert!(banner.contains("Ritru Labs"));

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
        assert!(app.launch_banner().is_none());
    }

    /// The welcome line carries the Ritru Labs credit; it must be readable
    /// in full on an 80-column terminal, not cut to "Welcome to Mellow…".
    #[test]
    fn welcome_line_shows_the_full_credit_at_80_columns() {
        let mut app = App::new(Buffer::empty(Some(PathBuf::from("demo.rs"))));
        app.launch_banner_until = Some(Instant::now() + LAUNCH_BANNER_DURATION);
        let banner = app.launch_banner().unwrap();
        let cells = |text: &str| unicode_width::UnicodeWidthStr::width(text) as u16;
        for width in [80u16, 120, 200] {
            let placed = ui::status_layout(&app, width);
            assert!(
                placed.iter().any(|(_, item)| item.text == banner),
                "welcome cut at {width}: {placed:?}"
            );
            let mut end = 0;
            for (x, item) in &placed {
                assert!(*x >= end, "overlap at {width}");
                end = x + cells(&item.text);
            }
            assert!(end <= width, "overflow at {width}");
        }
        let narrow = ui::status_layout(&app, 40);
        assert!(narrow.iter().all(|(x, item)| x + cells(&item.text) <= 40));
    }

    #[test]
    fn save_on_untitled_suggests_free_project_name_and_enter_saves() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("untitled.txt"), "taken").unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.workspace_root = dir.path().to_path_buf();
        app.buffer.insert_text(&mut app.cursor, "hello");

        app.execute(Command::Save);
        assert_eq!(app.mode, AppMode::SaveAs);
        assert_eq!(app.save_as_query.as_str(), "untitled-2.txt");

        app.handle_save_as_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        let target = dir.path().join("untitled-2.txt");
        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(app.buffer.path(), Some(&target));
        assert_eq!(app.status.as_deref(), Some("Saved as untitled-2.txt"));
    }

    #[test]
    fn goal_file_lifecycle_save_on_untitled_opens_save_as_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("untitled.txt");
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "hello");

        app.execute(Command::Save);
        assert_eq!(app.mode, AppMode::SaveAs);

        app.save_as_query.set(target.to_string_lossy().into_owned());
        app.handle_save_as_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(app.buffer.path(), Some(&target));
        assert!(app.buffer.is_persisted());
        assert!(!app.buffer.is_dirty());
    }

    #[test]
    fn goal_file_lifecycle_quit_save_untitled_finishes_after_save_as() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("quit-save.txt");
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "keep me");

        app.execute(Command::Quit);
        assert_eq!(app.mode, AppMode::ConfirmQuit);
        app.handle_quit_confirmation(KeyEvent::new(
            KeyCode::Char('s'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::SaveAs);
        assert_eq!(app.post_save_action, PostSaveAction::Quit);

        app.save_as_query.set(target.to_string_lossy().into_owned());
        app.handle_save_as_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        assert!(app.should_quit);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep me");
    }

    #[test]
    fn goal_overlay_palette_supports_mid_string_editing() {
        let mut app = App::new(Buffer::empty(None));
        app.execute(Command::ShowPalette);

        for ch in "sve".chars() {
            app.handle_palette_key(KeyEvent::new(
                KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ));
        }
        app.handle_palette_key(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_palette_key(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_palette_key(KeyEvent::new(
            KeyCode::Char('a'),
            crossterm::event::KeyModifiers::empty(),
        ));

        assert_eq!(app.palette_query.as_str(), "save");
        assert!(
            app.filtered_palette_entries()
                .iter()
                .any(|&index| COMMAND_SPECS[index].command == Command::Save)
        );
    }

    #[test]
    fn goal_overlay_find_paste_and_unicode_backspace_do_not_touch_buffer() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "alpha e\u{301}x omega");
        let before = app.buffer.line_text(0);
        app.execute(Command::Find);

        app.handle_paste("e\u{301}x");
        assert_eq!(app.find_query.as_str(), "e\u{301}x");
        assert_eq!(app.find_matches.len(), 1);

        app.handle_find_key(KeyEvent::new(
            KeyCode::End,
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_find_key(KeyEvent::new(
            KeyCode::Backspace,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.find_query.as_str(), "e\u{301}");
        assert_eq!(app.buffer.line_text(0), before);
    }

    #[test]
    fn goal_overlay_goto_paste_filters_invalid_text_and_edits_in_place() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "one\ntwo\nthree\nfour");
        app.execute(Command::GoToLine);

        app.handle_paste("3:x");
        assert_eq!(app.goto_query.as_str(), "3:");

        app.handle_goto_line_key(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_goto_line_key(KeyEvent::new(
            KeyCode::Char('2'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.goto_query.as_str(), "32:");
    }

    #[test]
    fn myn_ux12_find_in_file_and_navigation() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "apple orange banana apple pear apple");
        app.cursor = Cursor::new(0, 0);

        // Open Find
        app.execute(Command::Find);
        assert_eq!(app.mode, AppMode::Find);

        // Type query 'apple'
        for ch in "apple".chars() {
            app.handle_find_key(KeyEvent::new(
                KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ));
        }
        assert_eq!(app.find_matches.len(), 3);
        assert_eq!(app.find_selected, 0);
        assert_eq!(app.cursor, Cursor::new(0, 5));
        assert_eq!(app.selection_anchor, Some(Cursor::new(0, 0)));

        // Press Enter -> FindNext (second apple: col 20..25)
        app.handle_find_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.find_selected, 1);
        assert_eq!(app.cursor, Cursor::new(0, 25));
        assert_eq!(app.selection_anchor, Some(Cursor::new(0, 20)));

        // Press Enter again -> FindNext (third apple: col 31..36)
        app.handle_find_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.find_selected, 2);
        assert_eq!(app.cursor, Cursor::new(0, 36));

        // Press Shift+Enter -> FindPrevious (wrap back to second apple)
        app.handle_find_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::SHIFT,
        ));
        assert_eq!(app.find_selected, 1);
        assert_eq!(app.cursor, Cursor::new(0, 25));

        // Esc closes Find
        app.handle_find_key(KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Editing);
    }

    #[test]
    fn goal13_single_file_lsp_edits_apply_as_one_undo_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("goal13.rs");
        let mut buffer = Buffer::empty(Some(path.clone()));
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "alpha beta");
        buffer.force_save().unwrap();
        let mut app = App::new(buffer);
        app.workspace_root = dir.path().to_path_buf();
        app.pending_lsp_label = "Rename".to_owned();
        app.pending_lsp_edits = vec![crate::lsp::LspTextEdit {
            path: path.clone(),
            range: crate::lsp::LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 5,
                },
            },
            new_text: "gamma".to_owned(),
        }];
        app.apply_pending_lsp_edits();
        assert_eq!(app.buffer.contents(), "gamma beta");
        assert!(app.buffer.is_dirty());
        app.buffer.undo(&mut app.cursor);
        assert_eq!(app.buffer.contents(), "alpha beta");
        let _ = recovery::clear(&RecoveryKey::File(path.clone()));
    }

    #[test]
    fn goal16_multifile_lsp_edit_opens_and_updates_all_buffers_without_disk_write() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.rs");
        let two = dir.path().join("two.rs");
        std::fs::write(&one, "alpha one").unwrap();
        std::fs::write(&two, "alpha two").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.pending_lsp_label = "Rename".to_owned();
        app.pending_lsp_edits = vec![
            crate::lsp::LspTextEdit {
                path: one.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
            crate::lsp::LspTextEdit {
                path: two.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
        ];

        app.apply_pending_lsp_edits();

        assert_eq!(app.buffer.contents(), "gamma one");
        assert!(app.buffer.is_dirty());
        let two_tab = app
            .find_open_tab(&two)
            .expect("second file should open as a dirty tab");
        assert_ne!(two_tab, app.active_tab);
        assert_eq!(app.tab_buffer(two_tab).unwrap().contents(), "gamma two");
        assert!(app.tab_buffer(two_tab).unwrap().is_dirty());
        assert_eq!(std::fs::read_to_string(&two).unwrap(), "alpha two");
        assert!(app.status.as_deref().unwrap_or("").contains("2 buffer(s)"));

        let _ = recovery::clear(&RecoveryKey::File(one.clone()));
        let _ = recovery::clear(&RecoveryKey::File(two.clone()));
    }

    #[test]
    fn final_core_multifile_lsp_undo_and_redo_are_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.rs");
        let two = dir.path().join("two.rs");
        std::fs::write(&one, "alpha one").unwrap();
        std::fs::write(&two, "alpha two").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.pending_lsp_label = "Rename".to_owned();
        app.pending_lsp_edits = vec![
            crate::lsp::LspTextEdit {
                path: one.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
            crate::lsp::LspTextEdit {
                path: two.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
        ];

        app.apply_pending_lsp_edits();
        let two_tab = app.find_open_tab(&two).unwrap();
        assert_eq!(app.buffer.contents(), "gamma one");
        assert_eq!(app.tab_buffer(two_tab).unwrap().contents(), "gamma two");

        app.execute(Command::Undo);
        assert_eq!(app.buffer.contents(), "alpha one");
        assert_eq!(app.tab_buffer(two_tab).unwrap().contents(), "alpha two");
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("Undid Rename across 2 buffers")
        );

        app.execute(Command::Redo);
        assert_eq!(app.buffer.contents(), "gamma one");
        assert_eq!(app.tab_buffer(two_tab).unwrap().contents(), "gamma two");
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("Redid Rename across 2 buffers")
        );

        let _ = recovery::clear(&RecoveryKey::File(one.clone()));
        let _ = recovery::clear(&RecoveryKey::File(two.clone()));
    }

    #[test]
    fn final_core_multifile_lsp_atomic_undo_refuses_after_member_changes() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.rs");
        let two = dir.path().join("two.rs");
        std::fs::write(&one, "alpha one").unwrap();
        std::fs::write(&two, "alpha two").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.pending_lsp_label = "Rename".to_owned();
        app.pending_lsp_edits = vec![
            crate::lsp::LspTextEdit {
                path: one.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
            crate::lsp::LspTextEdit {
                path: two.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
        ];
        app.apply_pending_lsp_edits();

        let two_tab = app.find_open_tab(&two).unwrap();
        app.switch_tab(two_tab);
        app.execute(Command::End);
        app.execute(Command::Insert('!'));
        app.switch_tab(0);

        let one_before = app.buffer.contents();
        let two_before = app.tab_buffer(two_tab).unwrap().contents();
        app.execute(Command::Undo);

        assert_eq!(app.buffer.contents(), one_before);
        assert_eq!(app.tab_buffer(two_tab).unwrap().contents(), two_before);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("Atomic undo refused")
        );

        let _ = recovery::clear(&RecoveryKey::File(one.clone()));
        let _ = recovery::clear(&RecoveryKey::File(two.clone()));
    }

    #[test]
    fn goal16_multifile_lsp_edit_blocks_dirty_other_buffer_before_any_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.rs");
        let two = dir.path().join("two.rs");
        std::fs::write(&one, "alpha one").unwrap();
        std::fs::write(&two, "alpha two").unwrap();

        let mut app = App::new(Buffer::open(Some(one.clone())).unwrap());
        app.workspace_root = dir.path().to_path_buf();
        app.open_path_in_tab(two.clone()).unwrap();
        app.execute(Command::Insert('!'));
        app.switch_tab(0);
        let before = app.buffer.contents();

        app.pending_lsp_label = "Rename".to_owned();
        app.pending_lsp_edits = vec![
            crate::lsp::LspTextEdit {
                path: one.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
            crate::lsp::LspTextEdit {
                path: two.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
                new_text: "gamma".to_owned(),
            },
        ];

        app.apply_pending_lsp_edits();
        assert_eq!(app.buffer.contents(), before);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("unsaved edits")
        );
    }

    #[test]
    fn goal16_lsp_edit_rejects_overlapping_or_stale_ranges() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "abc");
        let path = PathBuf::from("demo.rs");
        let overlapping = vec![
            crate::lsp::LspTextEdit {
                path: path.clone(),
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 1,
                    },
                },
                new_text: "a".to_owned(),
            },
            crate::lsp::LspTextEdit {
                path,
                range: crate::lsp::LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
                new_text: "b".to_owned(),
            },
        ];
        assert!(App::lsp_replacements_for_buffer(&buffer, &overlapping).is_err());

        let stale = vec![crate::lsp::LspTextEdit {
            path: PathBuf::from("demo.rs"),
            range: crate::lsp::LspRange {
                start: LspPosition {
                    line: 0,
                    character: 9,
                },
                end: LspPosition {
                    line: 0,
                    character: 9,
                },
            },
            new_text: "x".to_owned(),
        }];
        assert!(App::lsp_replacements_for_buffer(&buffer, &stale).is_err());
    }

    fn test_ai_config(inline: bool) -> ai::AiProviderConfig {
        ai::AiProviderConfig {
            provider: ai::AiProvider::Claude,
            endpoint: "http://127.0.0.1:9/v1/messages".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            api_key: Some("test".to_owned()),
            inline_suggestions: inline,
        }
    }

    #[test]
    fn ask_ai_without_a_provider_opens_setup_and_continues_to_the_question() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(&mut app.cursor, "alpha beta");
        app.execute(Command::AiIntent);
        assert_eq!(app.mode, AppMode::AiSetup);
        assert_eq!(app.ai_setup.provider(), ai::AiProvider::Claude);
        assert_eq!(app.ai_setup.model.as_str(), "claude-opus-5-5");

        // Saving without a key explains what is missing (unless the
        // environment already provides one).
        if app.ai_setup.key_from_environment().is_none() {
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
            assert_eq!(app.mode, AppMode::AiSetup);
            assert!(app.ai_setup.error.is_some());
            assert_eq!(app.ai_setup.field, 2);
        }

        app.handle_paste("  sk-ant-test-key  ");
        app.ai_setup.field = 5;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(app.mode, AppMode::AiPrompt, "{:?}", app.ai_setup.error);
        assert_eq!(app.ai_status(), AiStatus::Ready);
        assert_eq!(app.ai_subject(), "this file");
        let saved = ai::AiProviderConfig::load_from(&ai::config_path())
            .unwrap()
            .unwrap();
        assert_eq!(saved.api_key.as_deref(), Some("sk-ant-test-key"));
        let _ = std::fs::remove_file(ai::config_path());

        // Tab offers ready-made questions.
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(app.ai_query.as_str(), "Explain what this code does");
    }

    #[test]
    fn ai_setup_cycles_providers_and_keeps_settings_reachable() {
        let mut app = App::new(Buffer::empty(None));
        app.execute(Command::AiSetup);
        assert_eq!(app.mode, AppMode::AiSetup);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
        assert_eq!(app.ai_setup.provider(), ai::AiProvider::OpenAi);
        assert!(app.ai_setup.endpoint.as_str().contains("api.openai.com"));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
        assert_eq!(app.ai_setup.provider(), ai::AiProvider::Gemini);
        assert!(
            app.ai_setup
                .endpoint
                .as_str()
                .contains("generativelanguage.googleapis.com")
        );
        assert_eq!(app.ai_setup.model.as_str(), "gemini-2.5-flash");
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
        assert_eq!(app.ai_setup.provider(), ai::AiProvider::Ollama);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(app.mode, AppMode::Editing);
        assert!(app.ai_config.is_none(), "Esc must not save anything");
    }

    #[test]
    fn ctrl_d_in_ai_setup_turns_ai_off_and_deletes_the_saved_key() {
        let path = ai::config_path();
        test_ai_config(true).save_to(&path).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.ai_config = Some(test_ai_config(true));
        app.execute(Command::AiSetup);
        assert_eq!(app.mode, AppMode::AiSetup);

        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.mode, AppMode::Editing);
        assert!(app.ai_config.is_none());
        assert!(!path.exists(), "saved key file removed");
    }

    /// Audit: inline replies need document identity. Revision and cursor can
    /// coincide in two tabs; a reply must only land in the buffer it was for,
    /// and never after AI settings changed or suggestions were turned off.
    #[test]
    fn inline_suggestion_reply_only_lands_in_its_own_buffer_and_settings() {
        let deliver = |app: &mut App, reply: GhostReply| {
            let (sender, receiver) = mpsc::channel();
            sender.send(reply).unwrap();
            app.ghost_receiver = Some(receiver);
            app.poll_inline_suggestion();
        };
        let reply = |buffer, generation, app: &App| GhostReply {
            buffer,
            generation,
            revision: app.buffer.revision(),
            cursor: app.cursor,
            result: Ok(" run();".to_owned()),
        };

        let mut app = App::new(Buffer::empty(None));
        app.ai_config = Some(test_ai_config(true));
        let first_tab = app.buffer.identity();
        app.buffer.insert_text(&mut app.cursor, "x");
        app.execute(Command::NewFile);
        app.buffer.insert_text(&mut app.cursor, "x");
        // Same revision and cursor as the first tab, different buffer.
        let r = reply(first_tab, 0, &app);
        deliver(&mut app, r);
        assert!(
            app.ghost.is_none(),
            "reply for tab 1 must not show in tab 2"
        );

        let this_tab = app.buffer.identity();
        let r = reply(this_tab, 0, &app);
        deliver(&mut app, r);
        assert!(app.ghost_is_visible(), "the matching reply is shown");

        app.ghost = None;
        app.ghost_generation += 1;
        let r = reply(this_tab, 0, &app);
        deliver(&mut app, r);
        assert!(app.ghost.is_none(), "reply from before a settings change");

        app.ai_config = Some(test_ai_config(false));
        let generation = app.ghost_generation;
        let r = reply(this_tab, generation, &app);
        deliver(&mut app, r);
        assert!(app.ghost.is_none(), "suggestions were switched off");
    }

    #[test]
    fn ghost_suggestion_accepts_with_tab_and_disappears_on_other_keys() {
        let mut app = App::new(Buffer::empty(None));
        app.ai_config = Some(test_ai_config(true));
        app.buffer.insert_text(&mut app.cursor, "fn main() {");
        let ghost = GhostSuggestion {
            buffer: app.buffer.identity(),
            cursor: app.cursor,
            revision: app.buffer.revision(),
            text: "\n    run();\n}".to_owned(),
        };
        app.ghost = Some(ghost.clone());
        assert!(app.ghost_is_visible());
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(app.buffer.contents(), "fn main() {\n    run();\n}");
        assert!(app.ghost.is_none());
        assert!(app.buffer.undo(&mut app.cursor));
        assert_eq!(app.buffer.contents(), "fn main() {");

        // Typing anything else dismisses it and types normally.
        app.cursor = Cursor::new(0, 11);
        app.ghost = Some(GhostSuggestion {
            buffer: app.buffer.identity(),
            cursor: app.cursor,
            revision: app.buffer.revision(),
            text: " run();".to_owned(),
        });
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
        assert!(app.ghost.is_none());
        assert_eq!(app.buffer.contents(), "fn main() {x");

        // A stale suggestion (cursor moved) is never shown.
        app.ghost = Some(ghost);
        assert!(!app.ghost_is_visible());

        // Only typing invites the next suggestion; undo and AI edits do not.
        app.ghost = None;
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert!(!app.typed_since_suggestion);
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
        assert!(app.typed_since_suggestion);
    }

    #[test]
    fn completion_without_a_language_helper_offers_words_from_the_file() {
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "subtotal = 1\nsubmit()\nprint(sub");
        app.request_completion_with_context(true, None);
        assert_eq!(app.mode, AppMode::Completion);
        let labels: Vec<_> = app
            .completion_items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, ["submit", "subtotal"], "nearest first");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(app.buffer.line_text(2), "print(submit");
    }

    #[test]
    fn goal19_ai_selection_proposal_is_explicit_and_undoable() {
        let mut app = App::new(Buffer::empty(None));
        app.ai_config = Some(test_ai_config(false));
        app.buffer.insert_text(&mut app.cursor, "alpha beta");
        app.selection_anchor = Some(Cursor::new(0, 6));
        app.cursor = Cursor::new(0, 10);

        app.begin_ai_intent();
        assert_eq!(app.mode, AppMode::AiPrompt);
        assert_eq!(app.ai_subject(), "line 1");
        assert!(app.ai_context_label().contains("line 1 of"));
        assert_eq!(app.ai_before_text(), "beta");

        app.ai_proposal = Some(AiProposal {
            summary: "Rename the selected word.".to_owned(),
            replacement: Some("gamma".to_owned()),
        });
        app.mode = AppMode::AiReview;
        app.apply_ai_proposal();

        assert_eq!(app.buffer.contents(), "alpha gamma");
        assert!(app.buffer.is_dirty());
        assert!(app.buffer.undo(&mut app.cursor));
        assert_eq!(app.buffer.contents(), "alpha beta");
    }

    #[test]
    fn goal19_ai_proposal_expires_when_target_buffer_changes() {
        let mut app = App::new(Buffer::empty(None));
        app.ai_config = Some(test_ai_config(false));
        app.buffer.insert_text(&mut app.cursor, "alpha beta");
        app.selection_anchor = Some(Cursor::new(0, 6));
        app.cursor = Cursor::new(0, 10);
        app.begin_ai_intent();

        app.selection_anchor = None;
        app.cursor = Cursor::new(0, 10);
        app.buffer.insert_text(&mut app.cursor, "!");
        let changed = app.buffer.contents();

        app.ai_proposal = Some(AiProposal {
            summary: "Replace selection.".to_owned(),
            replacement: Some("gamma".to_owned()),
        });
        app.mode = AppMode::AiReview;
        app.apply_ai_proposal();

        assert_eq!(app.buffer.contents(), changed);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("proposal expired")
        );
    }

    #[test]
    fn goal21_external_change_auto_reloads_clean_buffer_but_preserves_dirty_work() {
        let dir = tempfile::tempdir().unwrap();
        let clean_path = dir.path().join("clean.txt");
        std::fs::write(&clean_path, "one\n").unwrap();
        let mut clean = App::new(Buffer::open(Some(clean_path.clone())).unwrap());
        std::fs::write(&clean_path, "two\n").unwrap();
        clean.poll_external_file_changes();
        assert_eq!(clean.buffer.contents(), "two\n");
        assert!(!clean.buffer.is_dirty());

        let dirty_path = dir.path().join("dirty.txt");
        std::fs::write(&dirty_path, "disk\n").unwrap();
        let mut dirty = App::new(Buffer::open(Some(dirty_path.clone())).unwrap());
        dirty.execute(Command::Insert('X'));
        std::fs::write(&dirty_path, "external\n").unwrap();
        dirty.poll_external_file_changes();
        assert_eq!(dirty.buffer.contents(), "Xdisk\n");
        assert!(dirty.buffer.is_dirty());
        assert!(
            dirty
                .status
                .as_deref()
                .unwrap_or("")
                .contains("External change detected")
        );
    }

    #[test]
    fn goal21_large_files_remain_editable_with_reduced_intelligence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.rs");
        std::fs::write(&path, vec![b'a'; 5 * 1024 * 1024 + 1]).unwrap();
        let buffer = Buffer::open(Some(path)).unwrap();
        assert!(buffer.reduced_intelligence_mode());

        let mut app = App::new(buffer);
        assert!(app.syntax_document.is_none());
        app.language_service_active = true;
        app.restart_language_server(true);
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("paused for large file")
        );
        app.execute(Command::End);
        app.execute(Command::Insert('!'));
        assert!(app.buffer.is_dirty());
    }

    #[test]
    fn myn_ux12_goto_line_and_column() {
        let mut app = App::new(Buffer::empty(None));
        let lines = "line 1\nline 2\nline 3 is long\nline 4";
        app.buffer.insert_text(&mut app.cursor, lines);
        app.cursor = Cursor::new(0, 0);

        // Open GoToLine
        app.execute(Command::GoToLine);
        assert_eq!(app.mode, AppMode::GoToLine);

        // Type "3:6"
        for ch in "3:6".chars() {
            app.handle_goto_line_key(KeyEvent::new(
                KeyCode::Char(ch),
                crossterm::event::KeyModifiers::empty(),
            ));
        }
        assert_eq!(app.goto_query.as_str(), "3:6");

        // Press Enter -> jumps to line 3 (row 2), col 6 (col 5)
        app.handle_goto_line_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(app.mode, AppMode::Editing);
        assert_eq!(app.cursor, Cursor::new(2, 5));
    }
}
