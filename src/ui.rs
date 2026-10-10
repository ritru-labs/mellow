use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, AppMode, EditorPaneView, LspPreviewRow, ProblemSeverity, SplitOrientation},
    command::{COMMAND_SPECS, Command},
    git::{GitFileState, GitHunkStage, GitLineChange},
    visual::VisualLayout,
};

const TAB_WIDTH: usize = 4;
pub const HEADER_ROWS: u16 = 2;

/// One quiet status line; contextual hints live inside it.
pub fn footer_rows(_height: u16) -> u16 {
    1
}

pub fn chrome_rows(height: u16) -> u16 {
    HEADER_ROWS + footer_rows(height)
}

pub fn explorer_width(total_width: u16, visible: bool, _split_enabled: bool) -> u16 {
    if !visible || total_width < 72 {
        return 0;
    }
    (total_width / 4).clamp(22, 32)
}

pub fn rendered_split(total_width: u16, explorer_visible: bool, split_enabled: bool) -> bool {
    if !split_enabled {
        return false;
    }
    let explorer = explorer_width(total_width, explorer_visible, split_enabled);
    total_width.saturating_sub(explorer) >= 60
}

fn split_widths(available: u16, split_ratio: u16) -> (u16, u16) {
    let content = available.saturating_sub(1).max(2);
    let ratio = split_ratio.clamp(20, 80);
    let mut left = ((u32::from(content) * u32::from(ratio)) / 100) as u16;
    left = left.clamp(1, content.saturating_sub(1));
    let right = content.saturating_sub(left).max(1);
    (left, right)
}

fn split_heights(available: u16, split_ratio: u16) -> (u16, u16) {
    let content = available.saturating_sub(1).max(2);
    let ratio = split_ratio.clamp(20, 80);
    let mut top = ((u32::from(content) * u32::from(ratio)) / 100) as u16;
    top = top.clamp(1, content.saturating_sub(1));
    let bottom = content.saturating_sub(top).max(1);
    (top, bottom)
}

pub fn pane_geometry(
    total_width: u16,
    explorer_visible: bool,
    split_enabled: bool,
    split_ratio: u16,
    pane: u8,
) -> Option<(u16, u16)> {
    let explorer = explorer_width(total_width, explorer_visible, split_enabled);
    let available = total_width.saturating_sub(explorer).max(1);
    if !rendered_split(total_width, explorer_visible, split_enabled) {
        return (pane <= 1).then_some((explorer, available));
    }

    let (left, right) = split_widths(available, split_ratio);
    match pane {
        0 => Some((explorer, left)),
        1 => Some((explorer.saturating_add(left + 1), right)),
        _ => None,
    }
}

pub fn split_divider_column(
    total_width: u16,
    explorer_visible: bool,
    split_enabled: bool,
    split_ratio: u16,
) -> Option<u16> {
    if !rendered_split(total_width, explorer_visible, split_enabled) {
        return None;
    }
    let explorer = explorer_width(total_width, explorer_visible, split_enabled);
    let available = total_width.saturating_sub(explorer).max(1);
    let (left, _) = split_widths(available, split_ratio);
    Some(explorer.saturating_add(left))
}

pub fn split_divider_row(
    editor_top: u16,
    editor_bottom: u16,
    split_enabled: bool,
    split_ratio: u16,
) -> Option<u16> {
    if !split_enabled || editor_bottom <= editor_top.saturating_add(2) {
        return None;
    }
    let available = editor_bottom.saturating_sub(editor_top);
    let (top, _) = split_heights(available, split_ratio);
    Some(editor_top.saturating_add(top))
}

pub fn stacked_pane_hit(
    editor_top: u16,
    editor_bottom: u16,
    split_ratio: u16,
    row: u16,
) -> Option<(u8, u16, u16)> {
    if row < editor_top || row >= editor_bottom {
        return None;
    }
    let divider = split_divider_row(editor_top, editor_bottom, true, split_ratio)?;
    if row == divider {
        return None;
    }
    if row < divider {
        Some((
            0,
            row.saturating_sub(editor_top),
            divider.saturating_sub(editor_top),
        ))
    } else {
        Some((
            1,
            row.saturating_sub(divider.saturating_add(1)),
            editor_bottom.saturating_sub(divider.saturating_add(1)),
        ))
    }
}

pub fn editor_pane_hit(
    total_width: u16,
    explorer_visible: bool,
    split_enabled: bool,
    split_ratio: u16,
    column: u16,
) -> Option<(u8, u16, u16)> {
    let explorer = explorer_width(total_width, explorer_visible, split_enabled);
    if column < explorer {
        return None;
    }

    let available = total_width.saturating_sub(explorer);
    let local = column.saturating_sub(explorer);
    if !rendered_split(total_width, explorer_visible, split_enabled) {
        return Some((0, local, available.max(1)));
    }

    let (left, right_width) = split_widths(available, split_ratio);
    if local < left {
        Some((0, local, left))
    } else if local == left {
        None
    } else {
        Some((1, local.saturating_sub(left + 1), right_width))
    }
}

/// How much room the integrated terminal takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalPanel {
    #[default]
    Hidden,
    /// About a third of the editor, at most 12 rows.
    Normal,
    /// About two thirds of the editor.
    Tall,
    /// Everything but one editor row.
    Maximized,
}

impl From<bool> for TerminalPanel {
    fn from(visible: bool) -> Self {
        if visible { Self::Normal } else { Self::Hidden }
    }
}

pub fn terminal_panel_height(height: u16, panel: impl Into<TerminalPanel>) -> u16 {
    let available = height.saturating_sub(chrome_rows(height));
    if available <= 2 {
        return 0;
    }
    let rows = match panel.into() {
        TerminalPanel::Hidden => return 0,
        TerminalPanel::Normal => (available / 3).clamp(4, 12),
        TerminalPanel::Tall => (available * 2 / 3).max(4),
        TerminalPanel::Maximized => available,
    };
    rows.min(available.saturating_sub(1))
}

pub fn editor_content_height(height: u16, terminal: impl Into<TerminalPanel>) -> u16 {
    height
        .saturating_sub(chrome_rows(height))
        .saturating_sub(terminal_panel_height(height, terminal))
}

pub fn editor_bottom_row(height: u16, terminal: impl Into<TerminalPanel>) -> u16 {
    HEADER_ROWS.saturating_add(editor_content_height(height, terminal))
}

pub fn terminal_contains_row(height: u16, panel: TerminalPanel, row: u16) -> bool {
    let panel_height = terminal_panel_height(height, panel);
    if panel_height == 0 {
        return false;
    }
    let top = editor_bottom_row(height, panel);
    row >= top && row < top.saturating_add(panel_height)
}

pub fn terminal_content_size(
    width: u16,
    height: u16,
    panel: impl Into<TerminalPanel>,
) -> Option<(u16, u16)> {
    let panel_height = terminal_panel_height(height, panel);
    if panel_height < 3 || width < 3 {
        return None;
    }
    Some((width.saturating_sub(2), panel_height.saturating_sub(2)))
}

pub fn gutter_width(line_count: usize) -> u16 {
    let digits = line_count.max(1).to_string().len();
    (digits + 3) as u16
}

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    app.overlay_area.set(None);
    app.overlay_targets.borrow_mut().clear();
    let theme = app.theme;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.canvas)),
        area,
    );

    if area.width < 30 || area.height < 8 {
        render_too_small(frame, area, app);
        return;
    }

    let terminal_height = terminal_panel_height(area.height, app.terminal_panel());
    let rows = Layout::vertical([
        Constraint::Length(HEADER_ROWS),
        Constraint::Min(1),
        Constraint::Length(terminal_height),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(frame, rows[0], app);
    render_workspace(frame, rows[1], app);
    if terminal_height > 0 {
        render_terminal(frame, rows[2], app);
    }
    render_status(frame, rows[3], app);

    match app.mode {
        AppMode::Editing => {}
        AppMode::ConfirmQuit => render_quit_confirmation(frame, area, app),
        AppMode::ConfirmCloseTab => render_close_tab_confirmation(frame, area, app),
        AppMode::ConfirmCloseTerminal => render_close_terminal_confirmation(frame, area, app),
        AppMode::Palette => render_palette(frame, area, app),
        AppMode::QuickOpen => render_quick_open(frame, area, app),
        AppMode::Help => render_help(frame, area, app),
        AppMode::AiPrompt => render_ai_prompt(frame, area, app),
        AppMode::AiWaiting => render_ai_waiting(frame, area, app),
        AppMode::AiReview => render_ai_review(frame, area, app),
        AppMode::AiSetup => render_ai_setup(frame, area, app),
        AppMode::Settings => render_settings(frame, area, app),
        AppMode::Onboarding => render_onboarding(frame, area, app),
        AppMode::Find => render_find(frame, area, app),
        AppMode::Replace => render_replace(frame, area, app),
        AppMode::ProjectSearch => render_project_search(frame, area, app),
        AppMode::GoToLine => render_goto(frame, area, app),
        AppMode::SaveAs => render_save_as(frame, area, app),
        AppMode::SaveConflict => render_save_conflict(frame, area, app),
        AppMode::Recovery => render_recovery(frame, area, app),
        AppMode::Completion => render_completion(frame, area, app),
        AppMode::References => render_references(frame, area, app),
        AppMode::RenameInput => render_rename_input(frame, area, app),
        AppMode::ConfirmLspEdits => render_lsp_edit_confirmation(frame, area, app),
        AppMode::CodeActions => render_code_actions(frame, area, app),
        AppMode::Problems => render_problems(frame, area, app),
        AppMode::LanguageStatus => render_language_status(frame, area, app),
        AppMode::Changes => render_changes(frame, area, app),
        AppMode::ConfirmRevertHunk => render_revert_confirmation(frame, area, app),
        AppMode::ExplorerCreate => {
            render_explorer_path_action(frame, area, app, " New file ", "Enter create · Esc cancel")
        }
        AppMode::ExplorerCreateDirectory => render_explorer_path_action(
            frame,
            area,
            app,
            " New folder ",
            "Enter create · Esc cancel",
        ),
        AppMode::ExplorerRename => render_explorer_path_action(
            frame,
            area,
            app,
            " Rename or move ",
            "Enter apply · Esc cancel",
        ),
        AppMode::ConfirmExplorerDelete => render_explorer_delete_confirmation(frame, area, app),
        AppMode::GitCommitInput => render_git_commit_input(frame, area, app),
        AppMode::GitBranches => render_git_branches(frame, area, app),
        AppMode::GitBranchCreate => render_git_branch_create(frame, area, app),
        AppMode::GitBranchRename => render_git_branch_rename(frame, area, app),
        AppMode::ConfirmGitBranchDelete => render_git_branch_delete_confirmation(frame, area, app),
        AppMode::GitHistory => render_git_history(frame, area, app),
        AppMode::GitBlame => render_git_blame(frame, area, app),
        AppMode::GitConflicts => render_git_conflicts(frame, area, app),
        AppMode::Symbols => render_symbols(frame, area, app),
    }
    if !app.theme.unicode_symbols {
        asciify_symbols(frame.buffer_mut());
    }
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let message = if area.height >= 3 {
        "Mellow needs a little more room\nResize to at least 30×8"
    } else {
        "Resize terminal"
    };
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.muted).bg(theme.canvas)),
        area,
    );
}

/// What a click on header row 0 means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderHit {
    Brand,
    Tab(usize),
    CloseTab(usize),
    Action(Command),
}

#[derive(Debug, Clone)]
struct HeaderTab {
    index: usize,
    x: u16,
    width: u16,
    label: String,
    marker: &'static str,
    active: bool,
    dirty: bool,
}

#[derive(Debug, Clone)]
struct HeaderLayout {
    project: String,
    branch: Option<String>,
    changed: usize,
    brand_width: u16,
    tabs: Vec<HeaderTab>,
    hidden_before: usize,
    hidden_after: usize,
    actions: Vec<(u16, &'static str, Command)>,
}

fn header_actions(width: u16) -> &'static [(&'static str, Command)] {
    if width >= 100 {
        &[
            ("✦ Ask AI", Command::AiIntent),
            ("Open", Command::OpenFile),
            ("Save", Command::Save),
            ("Quit", Command::Quit),
        ]
    } else if width >= 64 {
        &[
            ("Open", Command::OpenFile),
            ("Save", Command::Save),
            ("Quit", Command::Quit),
        ]
    } else if width >= 44 {
        &[("Save", Command::Save), ("Quit", Command::Quit)]
    } else {
        &[]
    }
}

fn header_layout(app: &App, width: u16) -> HeaderLayout {
    let unicode = app.theme.unicode_symbols;
    let project = display_slice(&app.project_name(), 0, 18, TAB_WIDTH);
    let branch = (app.git_repository_active() && width >= 80)
        .then(|| app.git_snapshot.branch.clone())
        .filter(|branch| !branch.is_empty())
        .map(|branch| display_slice(&branch, 0, 16, TAB_WIDTH));
    let changed = app.git_snapshot.files.len();

    // " ✦ project  branch ±N │"
    let mut brand_width = 3 + UnicodeWidthStr::width(project.as_str()) as u16;
    if let Some(branch) = &branch {
        brand_width += 2 + UnicodeWidthStr::width(branch.as_str()) as u16;
        if changed > 0 {
            brand_width += 2 + changed.to_string().len() as u16;
        }
    }
    brand_width += 2;

    // Actions are right-aligned, two spaces apart, one space from the edge.
    let action_specs = header_actions(width);
    let mut actions = Vec::new();
    let mut x = width.saturating_sub(1);
    for (label, command) in action_specs.iter().rev() {
        x = x.saturating_sub(UnicodeWidthStr::width(*label) as u16);
        actions.push((x, *label, *command));
        x = x.saturating_sub(2);
    }
    actions.reverse();
    let actions_width = actions
        .first()
        .map_or(0, |(x, _, _)| width.saturating_sub(*x) + 2);

    // Tabs fill the space between; keep the active tab visible.
    let summaries = app.tab_summaries();
    let tab_space = width.saturating_sub(brand_width + actions_width);
    let tab_width = |label: &str| (UnicodeWidthStr::width(label) as u16 + 4).min(30);
    let active = app
        .active_tab_index()
        .min(summaries.len().saturating_sub(1));
    let mut first = active;
    let mut used = summaries.get(active).map_or(0, |tab| tab_width(&tab.label));
    while first > 0 {
        let next = tab_width(&summaries[first - 1].label);
        if used + next > tab_space.saturating_sub(4) {
            break;
        }
        used += next;
        first -= 1;
    }
    let mut tabs = Vec::new();
    let mut x = brand_width;
    let mut last = first;
    for (index, summary) in summaries.iter().enumerate().skip(first) {
        let width_needed = tab_width(&summary.label);
        let reserve = if index + 1 < summaries.len() { 4 } else { 0 };
        if index != first && x + width_needed + reserve > brand_width + tab_space {
            break;
        }
        let tab_w = width_needed.min(tab_space.max(6));
        let label = display_slice(
            &summary.label,
            0,
            tab_w.saturating_sub(4) as usize,
            TAB_WIDTH,
        );
        let marker = if summary.dirty {
            if unicode { "●" } else { "*" }
        } else if summary.active {
            if unicode { "×" } else { "x" }
        } else {
            " "
        };
        tabs.push(HeaderTab {
            index,
            x,
            width: tab_w,
            label,
            marker,
            active: summary.active,
            dirty: summary.dirty,
        });
        x += tab_w;
        last = index;
    }
    HeaderLayout {
        project,
        branch,
        changed,
        brand_width,
        hidden_before: first,
        hidden_after: summaries.len().saturating_sub(last + 1),
        tabs,
        actions,
    }
}

pub fn header_hit(app: &App, width: u16, column: u16) -> Option<HeaderHit> {
    let layout = header_layout(app, width);
    if column < layout.brand_width.saturating_sub(2) {
        return Some(HeaderHit::Brand);
    }
    for tab in &layout.tabs {
        if column >= tab.x && column < tab.x + tab.width {
            // The marker cell sits two columns from the tab's right edge.
            if column == tab.x + tab.width - 2 && (tab.active || tab.dirty) {
                return Some(HeaderHit::CloseTab(tab.index));
            }
            return Some(HeaderHit::Tab(tab.index));
        }
    }
    layout
        .actions
        .iter()
        .find(|(x, label, _)| column >= *x && column < *x + UnicodeWidthStr::width(*label) as u16)
        .map(|(_, _, command)| HeaderHit::Action(*command))
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    render_title_bar(frame, rows[0], app);
    render_breadcrumb_bar(frame, rows[1], app);
}

fn render_title_bar(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let layout = header_layout(app, area.width);
    let base = Style::default().bg(theme.surface);
    frame.render_widget(Block::default().style(base), area);
    let buffer = frame.buffer_mut();

    let mut spans = vec![
        Span::styled(
            if theme.unicode_symbols {
                " ✦ "
            } else {
                " * "
            },
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            layout.project.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(branch) = &layout.branch {
        spans.push(Span::styled(
            format!("  {branch}"),
            Style::default().fg(theme.muted),
        ));
        if layout.changed > 0 {
            spans.push(Span::styled(
                format!(" ±{}", layout.changed),
                Style::default().fg(theme.warning),
            ));
        }
    }
    spans.push(Span::styled(
        if app.theme.unicode_symbols {
            " │"
        } else {
            " |"
        },
        Style::default().fg(theme.elevated2),
    ));
    buffer.set_line(area.x, area.y, &Line::from(spans), layout.brand_width);

    for tab in &layout.tabs {
        let style = if tab.active {
            Style::default()
                .fg(theme.text)
                .bg(theme.canvas)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted).bg(theme.surface)
        };
        let marker_style = if tab.dirty {
            style.fg(theme.warning)
        } else {
            style.fg(theme.faint).remove_modifier(Modifier::BOLD)
        };
        let label_width = tab.width.saturating_sub(4) as usize;
        let line = Line::from(vec![
            Span::styled(format!(" {:<label_width$} ", tab.label), style),
            Span::styled(tab.marker, marker_style),
            Span::styled(" ", style),
        ]);
        buffer.set_line(area.x + tab.x, area.y, &line, tab.width);
    }
    if layout.hidden_before + layout.hidden_after > 0 {
        let x = layout
            .tabs
            .last()
            .map_or(layout.brand_width, |tab| tab.x + tab.width);
        let more = format!(" +{}", layout.hidden_before + layout.hidden_after);
        buffer.set_string(
            area.x + x,
            area.y,
            more,
            Style::default().fg(theme.faint).bg(theme.surface),
        );
    }

    for (x, label, command) in &layout.actions {
        let style = if *command == Command::AiIntent {
            Style::default().fg(theme.mint).bg(theme.surface)
        } else {
            Style::default().fg(theme.text).bg(theme.surface)
        };
        buffer.set_string(area.x + x, area.y, label, style);
    }
}

/// Row 1: a "FILES" caption over the explorer, then where you are and
/// whether your work is saved.
fn render_breadcrumb_bar(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let explorer = explorer_width(area.width, app.explorer_visible, app.split_enabled());
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.canvas)),
        area,
    );
    let buffer = frame.buffer_mut();
    if explorer > 0 {
        buffer.set_style(
            Rect::new(area.x, area.y, explorer, 1),
            Style::default().bg(theme.surface),
        );
        buffer.set_string(
            area.x + 1,
            area.y,
            "FILES",
            Style::default()
                .fg(if app.explorer_focused {
                    theme.mint
                } else {
                    theme.faint
                })
                .add_modifier(Modifier::BOLD),
        );
        buffer.set_string(
            area.x + explorer - 1,
            area.y,
            if app.theme.unicode_symbols {
                "│"
            } else {
                "|"
            },
            Style::default()
                .fg(if app.explorer_focused {
                    theme.mint
                } else {
                    theme.elevated2
                })
                .bg(theme.surface),
        );
    }

    let (state_text, state_style) = if app.buffer.is_dirty() {
        (
            if theme.unicode_symbols {
                "● Unsaved"
            } else {
                "* Unsaved"
            },
            Style::default().fg(theme.warning),
        )
    } else if !app.buffer.is_persisted() {
        ("New file", Style::default().fg(theme.faint))
    } else {
        (
            if theme.unicode_symbols {
                "✓ Saved"
            } else {
                "+ Saved"
            },
            Style::default().fg(theme.faint),
        )
    };
    let outside = app.buffer_outside_workspace();
    let right = if outside {
        format!("outside project  {state_text} ")
    } else {
        format!("{state_text} ")
    };
    let right_width = UnicodeWidthStr::width(right.as_str()) as u16;
    let left_x = area.x + explorer + 1;
    let available = area.right().saturating_sub(left_x + right_width + 2) as usize;

    let separator = if theme.unicode_symbols {
        " › "
    } else {
        " / "
    };
    let mut segments = app.breadcrumb_with_symbol();
    let mut truncated = false;
    while segments.len() > 1
        && UnicodeWidthStr::width(segments.join(separator).as_str()) > available.saturating_sub(4)
    {
        segments.remove(0);
        truncated = true;
    }
    let mut spans = Vec::new();
    if truncated {
        spans.push(Span::styled(
            format!("…{separator}"),
            Style::default().fg(theme.faint),
        ));
    }
    let last = segments.len().saturating_sub(1);
    for (index, segment) in segments.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(separator, Style::default().fg(theme.faint)));
        }
        let style = if index == last {
            Style::default().fg(theme.text)
        } else {
            Style::default().fg(theme.faint)
        };
        spans.push(Span::styled(
            display_slice(&segment, 0, available, TAB_WIDTH),
            style,
        ));
    }
    buffer.set_line(left_x, area.y, &Line::from(spans), available as u16);
    let right_x = area.right().saturating_sub(right_width);
    if right_x > left_x {
        if outside {
            buffer.set_string(
                right_x,
                area.y,
                "outside project",
                Style::default().fg(theme.warning),
            );
            buffer.set_string(right_x + 17, area.y, format!("{state_text} "), state_style);
        } else {
            buffer.set_string(right_x, area.y, right, state_style);
        }
    }
}

fn terminal_color(color: crate::pty::TerminalColor, default: Color) -> Color {
    match color {
        crate::pty::TerminalColor::Default => default,
        crate::pty::TerminalColor::Indexed(index) => Color::Indexed(index),
        crate::pty::TerminalColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn render_terminal(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }

    let theme = app.theme;
    let active_number = app.active_terminal_index().saturating_add(1);
    let total = app.terminal_session_count();
    let label = app.active_terminal_label().unwrap_or("shell");
    let accent = if app.terminal_focused {
        theme.mint
    } else {
        theme.muted
    };

    let provisional_inner_width = area.width.saturating_sub(2);
    let provisional_inner_height = area.height.saturating_sub(2);
    let snapshot = app.terminal_screen_snapshot(provisional_inner_height, provisional_inner_width);
    let scrolled = app.terminal_scroll_offset();
    let scroll_note = if scrolled > 0 {
        format!("· ↑ {scrolled} lines back · Shift+PgDn ")
    } else {
        String::new()
    };
    let title = if total > 1 {
        format!(" Terminal {active_number} of {total} · {label} {scroll_note}")
    } else {
        format!(" Terminal · {label} {scroll_note}")
    };
    let toggle = app.shortcut_label(Command::ToggleTerminal);
    let hint = match (&toggle, app.terminal_focused) {
        (Some(key), true) => format!(" {key} back to editor "),
        (Some(key), false) => format!(" {key} to type here "),
        (None, true) => " Click the editor to go back ".to_owned(),
        (None, false) => " Click here to type ".to_owned(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(rounded_border(theme.unicode_symbols))
        .border_style(Style::default().fg(accent))
        .title(Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .title_top(Line::styled(hint, Style::default().fg(accent)).right_aligned())
        .style(Style::default().bg(theme.elevated));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(snapshot) = snapshot else {
        frame.render_widget(
            Paragraph::new("Starting your shell…")
                .style(Style::default().fg(theme.text).bg(theme.elevated)),
            inner,
        );
        return;
    };

    let mut lines = Vec::with_capacity(inner.height as usize);
    for row in snapshot.rows.iter().take(inner.height as usize) {
        let mut spans = Vec::new();
        for cell in row {
            if cell.wide_continuation {
                continue;
            }
            let contents = if cell.contents.is_empty() {
                " ".to_owned()
            } else {
                cell.contents.clone()
            };
            let mut style = Style::default()
                .fg(terminal_color(cell.fg, theme.text))
                .bg(terminal_color(cell.bg, theme.elevated));
            let mut modifiers = Modifier::empty();
            if cell.bold {
                modifiers |= Modifier::BOLD;
            }
            if cell.dim {
                modifiers |= Modifier::DIM;
            }
            if cell.italic {
                modifiers |= Modifier::ITALIC;
            }
            if cell.underline {
                modifiers |= Modifier::UNDERLINED;
            }
            if cell.inverse {
                modifiers |= Modifier::REVERSED;
            }
            style = style.add_modifier(modifiers);
            spans.push(Span::styled(contents, style));
        }
        lines.push(Line::from(spans));
    }
    while lines.len() < inner.height as usize {
        lines.push(Line::raw(""));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.text).bg(theme.elevated)),
        inner,
    );

    if app.terminal_focused
        && let Some((row, col)) = snapshot.cursor
        && row < inner.height
        && col < inner.width
    {
        frame.set_cursor_position((inner.x.saturating_add(col), inner.y.saturating_add(row)));
    }
}

fn render_workspace(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let explorer = explorer_width(area.width, app.explorer_visible, app.split_enabled());
    let mut editor_area = area;

    if explorer > 0 {
        let explorer_area = Rect::new(area.x, area.y, explorer, area.height);
        render_explorer(frame, explorer_area, app);
        editor_area.x = editor_area.x.saturating_add(explorer);
        editor_area.width = editor_area.width.saturating_sub(explorer);
    }

    if rendered_split(area.width, app.explorer_visible, app.split_enabled()) {
        match app.split_orientation() {
            SplitOrientation::SideBySide => {
                let (left_width, right_width) = split_widths(editor_area.width, app.split_ratio());
                let left = Rect::new(editor_area.x, editor_area.y, left_width, editor_area.height);
                let divider = Rect::new(
                    editor_area.x.saturating_add(left_width),
                    editor_area.y,
                    1,
                    editor_area.height,
                );
                let right = Rect::new(
                    divider.x.saturating_add(1),
                    editor_area.y,
                    right_width,
                    editor_area.height,
                );

                if let Some(view) = app.pane_view(0) {
                    render_editor_pane(frame, left, app, view);
                }
                frame.render_widget(
                    Paragraph::new("│\n".repeat(divider.height as usize)).style(
                        Style::default()
                            .fg(app.theme.elevated2)
                            .bg(app.theme.canvas),
                    ),
                    divider,
                );
                if let Some(view) = app.pane_view(1) {
                    render_editor_pane(frame, right, app, view);
                }
            }
            SplitOrientation::Stacked => {
                let (top_height, bottom_height) =
                    split_heights(editor_area.height, app.split_ratio());
                let top = Rect::new(editor_area.x, editor_area.y, editor_area.width, top_height);
                let divider = Rect::new(
                    editor_area.x,
                    editor_area.y.saturating_add(top_height),
                    editor_area.width,
                    1,
                );
                let bottom = Rect::new(
                    editor_area.x,
                    divider.y.saturating_add(1),
                    editor_area.width,
                    bottom_height,
                );

                if let Some(view) = app.pane_view(0) {
                    render_editor_pane(frame, top, app, view);
                }
                frame.render_widget(
                    Paragraph::new(rule_glyph(app).repeat(divider.width as usize)).style(
                        Style::default()
                            .fg(app.theme.elevated2)
                            .bg(app.theme.canvas),
                    ),
                    divider,
                );
                if let Some(view) = app.pane_view(1) {
                    render_editor_pane(frame, bottom, app, view);
                }
            }
        }
    } else {
        let pane = if app.split_enabled() {
            app.active_pane_index()
        } else {
            0
        };
        if let Some(view) = app.pane_view(pane) {
            render_editor_pane(frame, editor_area, app, view);
        }
    }
}

/// Rows the explorer block reserves above its list. The "FILES" caption now
/// lives in the breadcrumb row, so the list starts at the top of the panel.
/// Rendering and mouse hit-testing must both use this offset.
pub const EXPLORER_TITLE_ROWS: u16 = 0;

/// First explorer entry drawn for a list of `height` rows, keeping the
/// selected entry visible.
pub fn explorer_scroll_start(app: &App, height: u16) -> usize {
    let height = height as usize;
    let selected = app
        .explorer_selected
        .min(app.explorer_files.len().saturating_sub(1));
    if height > 0 && selected >= height {
        selected + 1 - height
    } else {
        0
    }
}

fn render_explorer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(if app.explorer_focused {
            theme.mint
        } else {
            theme.elevated2
        }))
        .style(Style::default().bg(theme.surface));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width < 4 {
        return;
    }

    if app.explorer_files.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::styled(" No files yet", Style::default().fg(theme.muted)),
                Line::styled(" Press n for a new file", Style::default().fg(theme.faint)),
            ]),
            inner,
        );
        return;
    }

    let start = explorer_scroll_start(app, inner.height);
    let selected = app
        .explorer_selected
        .min(app.explorer_files.len().saturating_sub(1));
    let active_path = app.buffer.path().map(|path| app.workspace_relative(path));
    let badges = app.explorer_badges();
    let buffer = frame.buffer_mut();
    for (offset, (index, entry)) in app
        .explorer_files
        .iter()
        .enumerate()
        .skip(start)
        .take(inner.height as usize)
        .enumerate()
    {
        let y = inner.y + offset as u16;
        let is_selected = app.explorer_focused && index == selected;
        let is_active = !entry.is_dir && active_path.as_deref() == Some(entry.path.as_path());
        let background = if is_selected {
            theme.elevated2
        } else {
            theme.surface
        };
        buffer.set_style(
            Rect::new(inner.x, y, inner.width, 1),
            Style::default().bg(background),
        );
        if is_active {
            buffer.set_string(
                inner.x,
                y,
                if theme.unicode_symbols { "▎" } else { "|" },
                Style::default().fg(theme.mint).bg(background),
            );
        }

        let kind = if entry.is_dir {
            if app.explorer_entry_expanded(&entry.path) {
                if theme.unicode_symbols { "▾ " } else { "v " }
            } else if theme.unicode_symbols {
                "▸ "
            } else {
                "> "
            }
        } else {
            "  "
        };
        let name = entry
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| entry.path.to_string_lossy().into_owned());
        let badge = badges
            .get(&entry.path)
            .copied()
            .filter(|badge| {
                *badge != ExplorerBadge::Contains || !app.explorer_entry_expanded(&entry.path)
            })
            .map(|badge| (badge.symbol(theme.unicode_symbols), badge));
        let badge_width = if badge.is_some() { 2 } else { 0 };
        let text_width = (inner.width as usize).saturating_sub(2 + badge_width);
        let text = display_slice(
            &format!("{}{kind}{name}", "  ".repeat(entry.depth)),
            0,
            text_width,
            TAB_WIDTH,
        );
        let text_style = if is_active || is_selected {
            Style::default()
                .fg(theme.text)
                .bg(background)
                .add_modifier(Modifier::BOLD)
        } else if entry.is_dir {
            Style::default().fg(theme.text).bg(background)
        } else {
            Style::default().fg(theme.muted).bg(background)
        };
        buffer.set_string(inner.x + 1, y, text, text_style);
        if let Some((symbol, kind)) = badge {
            let color = match kind {
                ExplorerBadge::Unsaved | ExplorerBadge::Modified => theme.warning,
                ExplorerBadge::Added => theme.mint,
                ExplorerBadge::Deleted | ExplorerBadge::Conflict => theme.error,
                ExplorerBadge::Contains => theme.faint,
            };
            buffer.set_string(
                inner.right().saturating_sub(2),
                y,
                symbol,
                Style::default().fg(color).bg(background),
            );
        }
    }
}

/// A one-cell marker at the right edge of an explorer row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerBadge {
    Unsaved,
    Modified,
    Added,
    Deleted,
    Conflict,
    /// A collapsed folder that contains changes.
    Contains,
}

impl ExplorerBadge {
    pub fn symbol(self, unicode: bool) -> &'static str {
        match self {
            Self::Unsaved => {
                if unicode {
                    "●"
                } else {
                    "*"
                }
            }
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Conflict => "!",
            Self::Contains => {
                if unicode {
                    "•"
                } else {
                    "."
                }
            }
        }
    }
}

/// What a click on the start page does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartAction {
    Run(Command),
    Open(std::path::PathBuf),
}

struct StartPage {
    lines: Vec<(u16, u16, Line<'static>)>,
    targets: Vec<(Rect, StartAction)>,
}

/// Start page shown in place of an empty, untouched Untitled file: the few
/// things people do first, what changed in the project, and how to learn.
fn start_page(app: &App, area: Rect) -> StartPage {
    let theme = app.theme;
    let unicode = theme.unicode_symbols;
    let mut lines = Vec::new();
    let mut targets = Vec::new();
    let width = area.width.saturating_sub(4).min(80);
    let two_columns = width >= 66;
    let x0 = area.x + area.width.saturating_sub(width) / 2;
    let content_height = 16u16;
    let y0 = area.y + area.height.saturating_sub(content_height) / 3;
    let left_width = if two_columns { 40 } else { width };
    let right_x = x0 + 46;
    let heading = |text: &'static str| {
        Line::styled(
            text,
            Style::default()
                .fg(theme.faint)
                .add_modifier(Modifier::BOLD),
        )
    };
    let key_or = |command, fallback: &str| {
        app.shortcut_label(command)
            .unwrap_or_else(|| fallback.to_owned())
    };

    let title = vec![
        Span::styled(
            if unicode { "✦ " } else { "* " },
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            crate::brand::PRODUCT,
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", crate::brand::CREDIT),
            Style::default().fg(theme.muted),
        ),
    ];
    let title_width = Line::from(title.clone()).width() as u16;
    lines.push((x0, y0, Line::from(title)));
    // The project (and branch) sit at the right edge of the same row.
    let mut project = app.project_name();
    if app.git_repository_active() && !app.git_snapshot.branch.is_empty() {
        project.push_str(&format!(" · {}", app.git_snapshot.branch));
    }
    let project_width = UnicodeWidthStr::width(project.as_str()) as u16;
    if title_width + 3 + project_width <= width {
        lines.push((
            x0 + width - project_width,
            y0,
            Line::styled(project, Style::default().fg(theme.faint)),
        ));
    }

    lines.push((x0, y0 + 2, heading("START")));
    let start_items = [
        (
            "Open a file…",
            key_or(Command::OpenFile, ""),
            Command::OpenFile,
        ),
        ("New file", key_or(Command::NewFile, ""), Command::NewFile),
        (
            "Search all files…",
            app.shortcut_label(Command::ProjectSearch)
                .unwrap_or_else(|| "Ctrl+F Ctrl+F".to_owned()),
            Command::ProjectSearch,
        ),
        (
            "Open terminal",
            key_or(Command::ToggleTerminal, ""),
            Command::ToggleTerminal,
        ),
    ];
    for (offset, (label, key, command)) in start_items.into_iter().enumerate() {
        let y = y0 + 3 + offset as u16;
        let key_width = UnicodeWidthStr::width(key.as_str());
        let gap = (left_width as usize).saturating_sub(2 + label.len() + key_width);
        lines.push((
            x0,
            y,
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(label, Style::default().fg(theme.text)),
                Span::raw(" ".repeat(gap)),
                Span::styled(key, Style::default().fg(theme.faint)),
            ]),
        ));
        targets.push((Rect::new(x0, y, left_width, 1), StartAction::Run(command)));
    }

    // What changed in this project: the files people usually come back to.
    let mut y = y0 + 8;
    let changed: Vec<(std::path::PathBuf, &'static str)> = {
        let badges = app.explorer_badges();
        let mut files: Vec<_> = badges
            .into_iter()
            .filter(|(path, badge)| {
                *badge != ExplorerBadge::Contains
                    && *badge != ExplorerBadge::Deleted
                    && app.workspace_root().join(path).is_file()
            })
            .map(|(path, badge)| (path, badge.symbol(unicode)))
            .collect();
        files.sort();
        files.truncate(4);
        files
    };
    if !changed.is_empty() {
        lines.push((x0, y, heading("CHANGED FILES")));
        for (path, symbol) in changed {
            y += 1;
            let label = display_slice(
                &path.display().to_string(),
                0,
                left_width.saturating_sub(6) as usize,
                TAB_WIDTH,
            );
            let gap = (left_width as usize)
                .saturating_sub(2 + UnicodeWidthStr::width(label.as_str()) + 1);
            lines.push((
                x0,
                y,
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(label, Style::default().fg(theme.muted)),
                    Span::raw(" ".repeat(gap)),
                    Span::styled(symbol, Style::default().fg(theme.warning)),
                ]),
            ));
            targets.push((
                Rect::new(x0, y, left_width, 1),
                StartAction::Open(app.workspace_root().join(&path)),
            ));
        }
    }

    // Right column (or below, when narrow): how to learn, and AI.
    let (rx, mut ry) = if two_columns {
        (right_x, y0 + 2)
    } else {
        (x0, y + 2)
    };
    lines.push((rx, ry, heading("LEARN")));
    for (command, text) in [
        (Command::ShowHelp, "Everyday keys"),
        (Command::ShowPalette, "Every command"),
        (Command::ToggleExplorer, "Show files"),
    ] {
        if let Some(key) = app.shortcut_label(command) {
            ry += 1;
            lines.push((
                rx,
                ry,
                Line::from(vec![
                    Span::styled(
                        format!("{key:<8}"),
                        Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text, Style::default().fg(theme.muted)),
                ]),
            ));
            targets.push((Rect::new(rx, ry, 30, 1), StartAction::Run(command)));
        }
    }
    ry += 2;
    lines.push((rx, ry, heading("AI")));
    ry += 1;
    match app.ai_status() {
        crate::app::AiStatus::NotConfigured => {
            lines.push((
                rx,
                ry,
                Line::styled("Not set up yet.", Style::default().fg(theme.muted)),
            ));
            ry += 1;
            lines.push((
                rx,
                ry,
                Line::styled(
                    "[ Set up AI… ]",
                    Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
                ),
            ));
            targets.push((
                Rect::new(rx, ry, 14, 1),
                StartAction::Run(Command::AiIntent),
            ));
            ry += 1;
            lines.push((
                rx,
                ry,
                Line::styled(
                    "Optional. Mellow works offline.",
                    Style::default().fg(theme.faint),
                ),
            ));
        }
        _ => {
            lines.push((
                rx,
                ry,
                Line::styled(
                    format!(
                        "Ready. {} asks about your code.",
                        key_or(Command::AiIntent, "Ctrl+K")
                    ),
                    Style::default().fg(theme.muted),
                ),
            ));
        }
    }

    let footer_y = (y.max(ry) + 3).min(area.bottom().saturating_sub(1));
    lines.push((
        x0,
        footer_y,
        Line::styled(
            format!("{} to write a new file.", crate::brand::TAGLINE),
            Style::default()
                .fg(theme.faint)
                .add_modifier(Modifier::ITALIC),
        ),
    ));
    StartPage { lines, targets }
}

fn render_start_page(frame: &mut Frame<'_>, area: Rect, app: &App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.canvas)),
        area,
    );
    let page = start_page(app, area);
    let buffer = frame.buffer_mut();
    for (x, y, line) in page.lines {
        if y < area.bottom() && x < area.right() {
            buffer.set_line(x, y, &line, area.right().saturating_sub(x));
        }
    }
}

pub fn start_page_hit(app: &App, area: Rect, column: u16, row: u16) -> Option<StartAction> {
    start_page(app, area)
        .targets
        .into_iter()
        .find(|(rect, _)| rect.contains(ratatui::layout::Position::new(column, row)))
        .map(|(_, action)| action)
}

fn pane_selection_range(
    view: EditorPaneView<'_>,
) -> Option<(crate::cursor::Cursor, crate::cursor::Cursor)> {
    let anchor = view.selection_anchor?;
    if anchor == view.cursor {
        None
    } else if anchor.row < view.cursor.row
        || (anchor.row == view.cursor.row && anchor.col < view.cursor.col)
    {
        Some((anchor, view.cursor))
    } else {
        Some((view.cursor, anchor))
    }
}

fn render_editor_pane(frame: &mut Frame<'_>, area: Rect, app: &App, view: EditorPaneView<'_>) {
    if view.focused && app.start_page_visible() {
        render_start_page(frame, area, app);
        return;
    }
    let theme = app.theme;
    let gutter = gutter_width(view.buffer.line_count());
    let text_width = area.width.saturating_sub(gutter).max(1) as usize;
    let visible_rows = area.height as usize;
    let number_width = gutter.saturating_sub(3) as usize;
    let lang = view.buffer.language();
    let selection_range = pane_selection_range(view);
    // Only wrapped panes read the layout, and building one walks every
    // line of the buffer. Skip it when wrap is off: this ran on every frame
    // (10 a second when idle) and made a 10 MiB file cost ~80% of a core.
    let layout = if app.word_wrap {
        VisualLayout::new(view.buffer, text_width, TAB_WIDTH, true)
    } else {
        VisualLayout::empty(TAB_WIDTH)
    };
    let problems = if view.focused {
        app.problem_items()
    } else {
        Vec::new()
    };

    let mut lines = Vec::with_capacity(visible_rows);
    for offset in 0..visible_rows {
        let (row, source_range, skip_columns, continuation) = if app.word_wrap {
            let visual_index = view.visual_scroll_row + offset;
            let Some(visual_row) = layout.row(visual_index) else {
                lines.push(Line::raw(""));
                continue;
            };
            (
                visual_row.source_row,
                Some((visual_row.start_col, visual_row.end_col)),
                visual_row.start_display_col,
                visual_row.is_continuation(),
            )
        } else {
            let row = view.scroll_row + offset;
            if row >= view.buffer.line_count() {
                lines.push(Line::raw(""));
                continue;
            }
            (row, None, view.scroll_col, false)
        };

        let is_current = view.focused && row == view.cursor.row;
        let line_num_style = if is_current {
            Style::default()
                .fg(theme.mint)
                .bg(theme.cur_line)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.faint).bg(theme.canvas)
        };
        let row_problem = problems
            .iter()
            .filter(|problem| problem.cursor.row == row)
            .map(|problem| problem.severity)
            .min_by_key(|severity| match severity {
                ProblemSeverity::Error => 0,
                ProblemSeverity::Warning => 1,
                ProblemSeverity::Information => 2,
                ProblemSeverity::Hint => 3,
            });
        let git_change =
            app.git_line_change_for_path(view.buffer.path().map(|path| path.as_path()), row);
        let background = if is_current {
            theme.cur_line
        } else {
            theme.canvas
        };
        let git_style = Style::default()
            .fg(match git_change {
                Some(GitLineChange::Added) => theme.mint,
                Some(GitLineChange::Modified) => theme.warning,
                Some(GitLineChange::Deleted) => theme.error,
                None => {
                    if is_current {
                        theme.mint
                    } else {
                        theme.elevated2
                    }
                }
            })
            .bg(background);
        let problem_style = Style::default()
            .fg(match row_problem {
                Some(ProblemSeverity::Error) => theme.error,
                Some(ProblemSeverity::Warning) => theme.warning,
                Some(ProblemSeverity::Information | ProblemSeverity::Hint) => theme.mint,
                None => theme.elevated2,
            })
            .bg(background);

        let row_selection = if view.focused {
            app.rectangular_row_range(row)
        } else {
            None
        }
        .or_else(|| {
            selection_range.and_then(|(s, e)| {
                if row < s.row || row > e.row {
                    None
                } else {
                    let start_col = if row == s.row { s.col } else { 0 };
                    let end_col = if row == e.row {
                        e.col
                    } else {
                        view.buffer.grapheme_count(row) + 1
                    };
                    (start_col < end_col).then_some((start_col, end_col))
                }
            })
        });

        let number_text = if continuation {
            " ".repeat(number_width + 1)
        } else {
            format!("{:>number_width$} ", row + 1)
        };
        let git_marker = match git_change {
            Some(GitLineChange::Added) => "+",
            Some(GitLineChange::Modified) => "~",
            Some(GitLineChange::Deleted) => "-",
            None if theme.unicode_symbols => "│",
            None => "|",
        };
        let problem_marker = if row_problem.is_some() {
            if theme.unicode_symbols { "●" } else { "!" }
        } else {
            " "
        };

        let raw = view.buffer.line_text(row);
        let semantic = if view.focused {
            app.semantic_highlights_for_row(row)
        } else {
            Vec::new()
        };
        let highlighted_spans = render_highlighted_slice(
            &raw,
            lang,
            &semantic,
            skip_columns,
            text_width,
            &theme,
            RenderCodeOptions {
                is_current_line: is_current,
                selection: row_selection,
                show_whitespace: app.show_whitespace,
                show_indent_guides: app.show_indent_guides,
                source_range,
            },
        );

        let mut row_spans = Vec::with_capacity(3 + highlighted_spans.len());
        row_spans.push(Span::styled(number_text, line_num_style));
        if continuation {
            row_spans.push(Span::styled(
                if theme.unicode_symbols { "↪ " } else { "> " },
                git_style,
            ));
        } else {
            row_spans.push(Span::styled(git_marker, git_style));
            row_spans.push(Span::styled(problem_marker, problem_style));
        }
        row_spans.extend(highlighted_spans);
        lines.push(Line::from(row_spans));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.canvas)),
        area,
    );

    // Lines that continue past the right edge get a quiet marker, so long
    // commands are never silently cut off.
    if !app.word_wrap && text_width > 1 {
        let buffer = frame.buffer_mut();
        for offset in 0..visible_rows {
            let row = view.scroll_row + offset;
            if row >= view.buffer.line_count() {
                break;
            }
            let line_width =
                view.buffer
                    .display_width_before(row, view.buffer.grapheme_count(row), TAB_WIDTH);
            if line_width > view.scroll_col + text_width {
                buffer.set_string(
                    area.right().saturating_sub(1),
                    area.y + offset as u16,
                    if theme.unicode_symbols { "›" } else { ">" },
                    Style::default().fg(theme.mint).bg(theme.elevated),
                );
            }
        }
    }

    if !view.focused
        || !matches!(
            app.mode,
            AppMode::Editing | AppMode::Find | AppMode::Completion
        )
    {
        return;
    }

    if let Some((origin, partner)) = view.buffer.matching_bracket(view.cursor) {
        for bracket in [origin, partner] {
            let (visual_row, x_in_text) = if app.word_wrap {
                let position = layout.visual_position_for_cursor(view.buffer, bracket);
                (
                    position.row.saturating_sub(view.visual_scroll_row),
                    position.column,
                )
            } else {
                let display_col =
                    view.buffer
                        .display_width_before(bracket.row, bracket.col, TAB_WIDTH);
                (
                    bracket.row.saturating_sub(view.scroll_row),
                    display_col.saturating_sub(view.scroll_col),
                )
            };
            let x = area
                .x
                .saturating_add(gutter)
                .saturating_add(x_in_text as u16);
            let y = area.y.saturating_add(visual_row as u16);
            if x < area.right()
                && y < area.bottom()
                && let Some(symbol) = view.buffer.grapheme_at(bracket.row, bracket.col)
            {
                frame.render_widget(
                    Paragraph::new(symbol).style(
                        Style::default()
                            .fg(theme.mint)
                            .bg(theme.elevated2)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Rect::new(x, y, 1, 1),
                );
            }
        }
    }

    for secondary in view.secondary_cursors {
        let (visual_row, x_in_text) = if app.word_wrap {
            let position = layout.visual_position_for_cursor(view.buffer, *secondary);
            (
                position.row.saturating_sub(view.visual_scroll_row),
                position.column,
            )
        } else {
            let display_col =
                view.buffer
                    .display_width_before(secondary.row, secondary.col, TAB_WIDTH);
            (
                secondary.row.saturating_sub(view.scroll_row),
                display_col.saturating_sub(view.scroll_col),
            )
        };
        let x = area
            .x
            .saturating_add(gutter)
            .saturating_add(x_in_text as u16);
        let y = area.y.saturating_add(visual_row as u16);
        if x < area.right() && y < area.bottom() {
            frame.render_widget(
                Paragraph::new(if theme.unicode_symbols { "▏" } else { "|" })
                    .style(Style::default().fg(theme.mint).add_modifier(Modifier::BOLD)),
                Rect::new(x, y, 1, 1),
            );
        }
    }

    let (cursor_visual_row, cursor_x_in_text) = if app.word_wrap {
        let position = layout.visual_position_for_cursor(view.buffer, view.cursor);
        (
            position.row.saturating_sub(view.visual_scroll_row),
            position.column,
        )
    } else {
        let cursor_display_col =
            view.buffer
                .display_width_before(view.cursor.row, view.cursor.col, TAB_WIDTH);
        (
            view.cursor.row.saturating_sub(view.scroll_row),
            cursor_display_col.saturating_sub(view.scroll_col),
        )
    };

    let cursor_x = area
        .x
        .saturating_add(gutter)
        .saturating_add(cursor_x_in_text as u16);
    let cursor_y = area.y.saturating_add(cursor_visual_row as u16);
    if cursor_x < area.right() && cursor_y < area.bottom() {
        frame.set_cursor_position((cursor_x, cursor_y));
        if view.focused {
            app.last_cursor_screen.set(Some((cursor_x, cursor_y)));
            if app.ghost_is_visible()
                && let Some(ghost) = app.ghost.as_ref()
            {
                render_ghost(frame, area, app, ghost, cursor_x, cursor_y);
            }
        }
    }
}

/// Draws an AI typing suggestion after the cursor in faint italics. Only
/// the first line is drawn in place; longer suggestions say how many more
/// lines Tab will insert.
fn render_ghost(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    ghost: &crate::app::GhostSuggestion,
    x: u16,
    y: u16,
) {
    let theme = app.theme;
    let mut lines = ghost.text.lines();
    let mut first = lines.next().unwrap_or("").to_owned();
    let mut rest = lines.count();
    if first.is_empty() && rest > 0 {
        first = format!(
            "{}{}",
            if theme.unicode_symbols { "↵ " } else { "> " },
            ghost.text.lines().nth(1).unwrap_or("").trim_start()
        );
        rest -= 1;
    }
    let suffix = if rest > 0 {
        format!("  +{rest} more line{}", if rest == 1 { "" } else { "s" })
    } else {
        String::new()
    };
    let width = area.right().saturating_sub(x) as usize;
    let text = display_slice(&first, 0, width, TAB_WIDTH);
    let used = UnicodeWidthStr::width(text.as_str());
    let buffer = frame.buffer_mut();
    buffer.set_string(
        x,
        y,
        &text,
        Style::default()
            .fg(theme.faint)
            .add_modifier(Modifier::ITALIC),
    );
    if !suffix.is_empty() && used + suffix.len() < width {
        buffer.set_string(x + used as u16, y, suffix, Style::default().fg(theme.mint));
    }
}

#[derive(Debug, Clone, Copy)]
struct RenderCodeOptions {
    is_current_line: bool,
    selection: Option<(usize, usize)>,
    show_whitespace: bool,
    show_indent_guides: bool,
    source_range: Option<(usize, usize)>,
}

fn render_highlighted_slice(
    raw_line: &str,
    lang: &str,
    semantic: &[crate::syntax_tree::SyntaxHighlightSpan],
    skip_columns: usize,
    max_columns: usize,
    theme: &crate::theme::Theme,
    options: RenderCodeOptions,
) -> Vec<Span<'static>> {
    let RenderCodeOptions {
        is_current_line,
        selection,
        show_whitespace,
        show_indent_guides,
        source_range,
    } = options;

    if max_columns == 0 {
        return Vec::new();
    }

    let tokens = crate::syntax::tokenize_line(raw_line, lang);
    let mut spans = Vec::new();
    let mut absolute_col = 0usize;
    let mut emitted = 0usize;
    let mut grapheme_idx = 0usize;
    let mut leading_whitespace = true;

    let line_graphemes = raw_line.graphemes(true).count();
    'tokens: for token in tokens {
        if emitted >= max_columns {
            break;
        }

        let mut cur_chunk = String::new();
        let mut cur_in_sel = false;
        let mut cur_token_type = token.token_type;

        let flush_chunk = |spans: &mut Vec<Span<'static>>,
                           chunk: &mut String,
                           in_sel: bool,
                           token_type: crate::syntax::TokenType| {
            if !chunk.is_empty() {
                let mut style = token_type.style(theme, is_current_line);
                if in_sel {
                    style = style.bg(theme.selection);
                }
                spans.push(Span::styled(std::mem::take(chunk), style));
            }
        };

        for grapheme in token.text.graphemes(true) {
            let source_idx = grapheme_idx;
            let in_sel = selection
                .map(|(s, e)| source_idx >= s && source_idx < e)
                .unwrap_or(false);
            let effective_type = semantic
                .iter()
                .rev()
                .find(|span| source_idx >= span.start_col && source_idx < span.end_col)
                .map(|span| span.token_type)
                .unwrap_or(token.token_type);
            grapheme_idx += 1;

            let mut is_guide = false;
            let (display, width) = if grapheme == "\t" {
                let w = TAB_WIDTH - (absolute_col % TAB_WIDTH);
                let display = if show_whitespace {
                    format!(
                        "{}{}",
                        if theme.unicode_symbols { "→" } else { ">" },
                        " ".repeat(w.saturating_sub(1))
                    )
                } else if show_indent_guides && leading_whitespace {
                    is_guide = true;
                    format!("│{}", " ".repeat(w.saturating_sub(1)))
                } else {
                    " ".repeat(w)
                };
                (display, w)
            } else if grapheme == " " {
                let display = if show_whitespace {
                    if theme.unicode_symbols { "·" } else { "." }.to_owned()
                } else if show_indent_guides
                    && leading_whitespace
                    && absolute_col.is_multiple_of(TAB_WIDTH)
                {
                    is_guide = true;
                    if theme.unicode_symbols { "│" } else { "|" }.to_owned()
                } else {
                    " ".to_owned()
                };
                (display, 1)
            } else {
                leading_whitespace = false;
                (grapheme.to_owned(), UnicodeWidthStr::width(grapheme))
            };

            let next_col = absolute_col + width;

            if let Some((start_col, end_col)) = source_range {
                if source_idx < start_col {
                    absolute_col = next_col;
                    continue;
                }
                if source_idx >= end_col {
                    flush_chunk(&mut spans, &mut cur_chunk, cur_in_sel, cur_token_type);
                    break 'tokens;
                }
            }

            if next_col <= skip_columns {
                absolute_col = next_col;
                continue;
            }

            if absolute_col < skip_columns {
                let visible_tail = next_col - skip_columns;
                if emitted + visible_tail > max_columns {
                    flush_chunk(&mut spans, &mut cur_chunk, cur_in_sel, cur_token_type);
                    break 'tokens;
                }
                if cur_chunk.is_empty() {
                    cur_in_sel = in_sel;
                    cur_token_type = effective_type;
                } else if cur_in_sel != in_sel || cur_token_type != effective_type {
                    flush_chunk(&mut spans, &mut cur_chunk, cur_in_sel, cur_token_type);
                    cur_in_sel = in_sel;
                    cur_token_type = effective_type;
                }
                cur_chunk.push_str(&" ".repeat(visible_tail));
                emitted += visible_tail;
                absolute_col = next_col;
                continue;
            }

            if emitted + width > max_columns {
                flush_chunk(&mut spans, &mut cur_chunk, cur_in_sel, cur_token_type);
                break 'tokens;
            }

            if is_guide {
                // Indent guides are quiet structure, not text: draw them in
                // the faintest tone so the code stays in front.
                flush_chunk(&mut spans, &mut cur_chunk, cur_in_sel, cur_token_type);
                let mut style = crate::syntax::TokenType::Text
                    .style(theme, is_current_line)
                    .fg(theme.elevated2)
                    .remove_modifier(Modifier::BOLD);
                if in_sel {
                    style = style.bg(theme.selection);
                }
                spans.push(Span::styled(display, style));
                emitted += width;
                absolute_col = next_col;
                continue;
            }
            if cur_chunk.is_empty() {
                cur_in_sel = in_sel;
                cur_token_type = effective_type;
            } else if cur_in_sel != in_sel || cur_token_type != effective_type {
                flush_chunk(&mut spans, &mut cur_chunk, cur_in_sel, cur_token_type);
                cur_in_sel = in_sel;
                cur_token_type = effective_type;
            }
            cur_chunk.push_str(&display);
            emitted += width;
            absolute_col = next_col;
        }

        flush_chunk(&mut spans, &mut cur_chunk, cur_in_sel, cur_token_type);
    }

    if emitted < max_columns {
        let remaining = max_columns - emitted;
        let reaches_logical_eol = source_range
            .map(|(_, end_col)| end_col >= line_graphemes)
            .unwrap_or(true);
        let pad_in_sel = reaches_logical_eol
            && selection
                .map(|(s, e)| e > line_graphemes && grapheme_idx >= s)
                .unwrap_or(false);
        if pad_in_sel {
            spans.push(Span::styled(" ", Style::default().bg(theme.selection)));
            if remaining > 1 && is_current_line {
                spans.push(Span::styled(
                    " ".repeat(remaining - 1),
                    Style::default().bg(theme.cur_line),
                ));
            }
        } else if is_current_line {
            spans.push(Span::styled(
                " ".repeat(remaining),
                Style::default().bg(theme.cur_line),
            ));
        }
    }

    spans
}

/// One clickable piece of the status line.
#[derive(Debug, Clone)]
pub struct StatusItem {
    pub text: String,
    pub style: Style,
    pub command: Option<Command>,
    /// Higher stays visible longer when the line is narrow.
    priority: u8,
}

impl StatusItem {
    fn new(text: impl Into<String>, style: Style, priority: u8) -> Self {
        Self {
            text: text.into(),
            style,
            command: None,
            priority,
        }
    }

    fn on_click(mut self, command: Command) -> Self {
        self.command = Some(command);
        self
    }

    fn width(&self) -> u16 {
        UnicodeWidthStr::width(self.text.as_str()) as u16
    }
}

const STATUS_GAP: u16 = 3;

/// The left-hand message: the latest outcome, or a hint for whatever has
/// focus. Written for people, not for the code.
fn status_left(app: &App) -> Vec<StatusItem> {
    let theme = app.theme;
    let Some(message) = status_message(app) else {
        return Vec::new();
    };
    let mut items = vec![message];
    if app.status.is_none()
        && matches!(app.mode, AppMode::Editing)
        && !app.terminal_focused
        && !app.explorer_focused
        && app.selection_range().is_some()
    {
        let action = Style::default()
            .fg(theme.text)
            .add_modifier(Modifier::UNDERLINED);
        items.push(StatusItem::new("Copy", action, 0).on_click(Command::Copy));
        items.push(StatusItem::new("Cut", action, 0).on_click(Command::Cut));
        items.push(StatusItem::new("Ask AI", action, 0).on_click(Command::AiIntent));
    }
    items
}

fn status_message(app: &App) -> Option<StatusItem> {
    let theme = app.theme;
    let unicode = theme.unicode_symbols;
    if let Some(status) = app.status.as_deref() {
        let (text, style) = if status == "Saved" {
            (
                format!("{} Saved", if unicode { "✓" } else { "+" }),
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            )
        } else if let Some(rest) = status.strip_prefix("Saved as ") {
            (
                format!("{} Saved as {rest}", if unicode { "✓" } else { "+" }),
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            )
        } else if status == "Undo" || status == "Redo" {
            (status.to_owned(), Style::default().fg(theme.mint))
        } else if status.contains("failed") || status.contains("Failed") {
            (
                status.to_owned(),
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (status.to_owned(), Style::default().fg(theme.text))
        };
        return Some(StatusItem::new(text, style, 0));
    }
    if !matches!(app.mode, AppMode::Editing | AppMode::Completion) {
        return None;
    }
    if let Some(banner) = app.launch_banner() {
        return Some(StatusItem::new(
            banner,
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            0,
        ));
    }
    if app.ghost_is_visible() {
        return Some(StatusItem::new(
            format!(
                "{} AI suggestion · Tab accept · Esc dismiss",
                if unicode { "✦" } else { "*" }
            ),
            Style::default().fg(theme.mint),
            0,
        ));
    }
    let key = |command| app.shortcut_label(command);
    let hint = |text: String| Some(StatusItem::new(text, Style::default().fg(theme.muted), 0));
    if app.terminal_focused {
        return hint("Typing goes to the shell".to_owned());
    }
    if app.explorer_focused {
        return hint(
            "Files · Enter open · n new file · N new folder · F2 rename · Del delete · Esc back"
                .to_owned(),
        );
    }
    if let Some((start, end)) = app.selection_range() {
        let lines = end.row.abs_diff(start.row) + usize::from(end.col > 0 || end.row == start.row);
        let amount = if start.row == end.row {
            let start_index = app.buffer.char_index(start.row, start.col);
            let end_index = app.buffer.char_index(end.row, end.col);
            let count = end_index.abs_diff(start_index);
            format!(
                "{count} character{} selected",
                if count == 1 { "" } else { "s" }
            )
        } else {
            format!("{lines} lines selected")
        };
        return Some(StatusItem::new(amount, Style::default().fg(theme.mint), 0));
    }
    if !app.secondary_cursors.is_empty() {
        return hint(format!(
            "{} cursors · Esc back to one",
            app.secondary_cursors.len() + 1
        ));
    }
    if app.buffer.is_dirty() {
        let save = key(Command::Save)
            .map(|key| format!(" · {key} to save"))
            .unwrap_or_default();
        return Some(StatusItem::new(
            format!("{} Unsaved changes{save}", if unicode { "●" } else { "*" }),
            Style::default().fg(theme.warning),
            0,
        ));
    }
    key(Command::ShowPalette).map(|key| {
        StatusItem::new(
            format!("{key} every command"),
            Style::default().fg(theme.faint),
            0,
        )
    })
}

fn status_right_items(app: &App) -> Vec<StatusItem> {
    let theme = app.theme;
    let unicode = theme.unicode_symbols;
    let muted = Style::default().fg(theme.muted);
    let mut items = Vec::new();

    let problems = app.problem_items();
    if !problems.is_empty() {
        let errors = problems
            .iter()
            .filter(|problem| problem.severity == ProblemSeverity::Error)
            .count();
        let text = format!(
            "{} problem{}",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        );
        let style = if errors > 0 {
            Style::default().fg(theme.error)
        } else {
            Style::default().fg(theme.warning)
        };
        items.push(StatusItem::new(text, style, 80).on_click(Command::ShowProblems));
    }

    if let Some(file) = app.active_git_file() {
        let (badge, color, action) = match file.state() {
            GitFileState::New => (
                ("+", "+"),
                theme.mint,
                Some(("Stage", Command::GitStageFile)),
            ),
            GitFileState::Modified => (
                ("●", "*"),
                theme.warning,
                Some(("Stage", Command::GitStageFile)),
            ),
            GitFileState::PartlyStaged => (
                ("◐", "~"),
                theme.warning,
                Some(("Stage rest", Command::GitStageFile)),
            ),
            GitFileState::Staged => (
                ("✚", "+"),
                theme.mint,
                Some(("Unstage", Command::GitUnstageFile)),
            ),
            GitFileState::Conflict => (("!", "!"), theme.error, None),
        };
        let label = match file.state() {
            GitFileState::New => "New file",
            GitFileState::Modified => "Modified",
            GitFileState::PartlyStaged => "Partly staged",
            GitFileState::Staged => "Staged",
            GitFileState::Conflict => "Conflict",
        };
        let symbol = if unicode { badge.0 } else { badge.1 };
        let on_badge = if file.state() == GitFileState::Conflict {
            Command::GitConflicts
        } else {
            Command::ShowChanges
        };
        items.push(
            StatusItem::new(format!("{symbol} {label}"), Style::default().fg(color), 75)
                .on_click(on_badge),
        );
        if let Some((text, command)) = action {
            let link = Style::default()
                .fg(theme.text)
                .add_modifier(Modifier::UNDERLINED);
            items.push(StatusItem::new(text, link, 74).on_click(command));
        }
    }

    if app.git_repository_active() {
        let mut branch = if app.git_snapshot.branch.is_empty() {
            "git".to_owned()
        } else {
            display_slice(&app.git_snapshot.branch, 0, 18, TAB_WIDTH)
        };
        let (ahead, behind) = (app.git_snapshot.ahead, app.git_snapshot.behind);
        if ahead > 0 {
            branch.push_str(&format!(" {}{ahead}", if unicode { "↑" } else { "^" }));
        }
        if behind > 0 {
            branch.push_str(&format!(" {}{behind}", if unicode { "↓" } else { "v" }));
        }
        let changed = app.git_snapshot.files.len();
        let text = if changed > 0 {
            format!("{branch} · {changed} changed")
        } else {
            branch
        };
        items.push(StatusItem::new(text, muted, 60).on_click(Command::ShowChanges));
    }

    let cursor = app.active_cursor();
    items.push(
        StatusItem::new(
            format!("Ln {}, Col {}", cursor.row + 1, cursor.col + 1),
            muted,
            90,
        )
        .on_click(Command::GoToLine),
    );
    items.push(
        StatusItem::new(
            if app.buffer.uses_tab_indent() {
                "Tabs"
            } else {
                "Spaces: 4"
            },
            muted,
            45,
        )
        .on_click(Command::ToggleIndentStyle),
    );
    items.push(
        StatusItem::new(app.buffer.language(), muted, 50).on_click(Command::LanguageServerStatus),
    );
    if app.buffer.line_ending() != crate::buffer::LineEnding::Lf {
        items.push(StatusItem::new(
            app.buffer.line_ending().label(),
            Style::default().fg(theme.warning),
            20,
        ));
    }
    if app.buffer.has_bom() {
        items.push(StatusItem::new("BOM", muted, 15));
    }
    if let Some(label) = app.language_server_label()
        && !app.language_server_ready()
    {
        items.push(StatusItem::new(
            format!("Starting {label}…"),
            Style::default().fg(theme.faint),
            30,
        ));
    }

    let spark = if unicode { "✦ " } else { "* " };
    let ai = match app.ai_status() {
        crate::app::AiStatus::Ready => StatusItem::new(
            format!("{spark}AI ready"),
            Style::default().fg(theme.mint),
            40,
        ),
        crate::app::AiStatus::Working => StatusItem::new(
            format!("{spark}AI thinking…"),
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            85,
        ),
        crate::app::AiStatus::NotConfigured => StatusItem::new(
            format!("{spark}Set up AI"),
            Style::default().fg(theme.faint),
            25,
        ),
    };
    items.push(ai.on_click(Command::AiIntent));
    items.push(
        StatusItem::new(
            "Terminal",
            Style::default().fg(if app.terminal_visible {
                theme.mint
            } else {
                theme.faint
            }),
            35,
        )
        .on_click(Command::ToggleTerminal),
    );
    items.push(
        StatusItem::new("F1 Help", Style::default().fg(theme.muted), 100)
            .on_click(Command::ShowHelp),
    );
    items
}

/// Lays out the status line: the message on the left, then right-aligned
/// items. When space runs out the lowest-priority items disappear first and
/// the message is shortened with an ellipsis, so nothing ever overlaps.
pub fn status_layout(app: &App, width: u16) -> Vec<(u16, StatusItem)> {
    let mut right = status_right_items(app);
    let mut left = status_left(app);
    let left_width: u16 = left.iter().map(|item| item.width() + 2).sum();
    // Action hints (an AI suggestion, a selection) keep their full text and
    // push secondary items out; ordinary messages may shorten.
    let welcome = app.launch_banner();
    let showing_welcome = welcome.is_some()
        && left
            .first()
            .is_some_and(|item| Some(&item.text) == welcome.as_ref());
    let min_message = if showing_welcome {
        // The welcome (with its Ritru Labs credit) lasts a few seconds; the
        // usual items step aside for it and return on the first key.
        left_width.min(78)
    } else if app.ghost_is_visible() || app.selection_range().is_some() {
        left_width.min(48)
    } else {
        left_width.min(24)
    };
    let right_width = |items: &[StatusItem]| -> u16 {
        items
            .iter()
            .map(|item| item.width() + STATUS_GAP)
            .sum::<u16>()
    };
    while !right.is_empty() && right_width(&right) + min_message + 2 > width {
        let lowest = right
            .iter()
            .enumerate()
            .min_by_key(|(_, item)| item.priority)
            .map(|(index, _)| index)
            .unwrap_or(0);
        right.remove(lowest);
    }

    let mut placed = Vec::new();
    let mut x = width.saturating_sub(right_width(&right)) + STATUS_GAP - 1;
    let right_start = x;
    for item in right {
        let item_width = item.width();
        placed.push((x, item));
        x += item_width + STATUS_GAP;
    }
    // Left group: the message, then any actions. Actions drop before the
    // message is shortened.
    let available = right_start.saturating_sub(STATUS_GAP) as usize;
    while left.len() > 1
        && left
            .iter()
            .map(|item| item.width() as usize + 2)
            .sum::<usize>()
            > available
    {
        left.pop();
    }
    let mut x = 1u16;
    let mut left_placed = Vec::new();
    for (index, mut item) in left.into_iter().enumerate() {
        let room = available.saturating_sub(x as usize);
        if index == 0 && UnicodeWidthStr::width(item.text.as_str()) > room {
            item.text = format!(
                "{}…",
                display_slice(&item.text, 0, room.saturating_sub(1), TAB_WIDTH)
            );
        }
        let width = item.width();
        left_placed.push((x, item));
        x += width + 2;
    }
    left_placed.extend(placed);
    left_placed
}

pub fn status_command_at(app: &App, width: u16, column: u16) -> Option<Command> {
    status_layout(app, width)
        .into_iter()
        .find(|(x, item)| column >= *x && column < x + item.width())
        .and_then(|(_, item)| item.command)
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.surface)),
        area,
    );
    let buffer = frame.buffer_mut();
    for (x, item) in status_layout(app, area.width) {
        buffer.set_string(area.x + x, area.y, &item.text, item.style.bg(theme.surface));
    }
}

fn render_symbols(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 64.min(area.width.saturating_sub(4)).max(20);
    let popup_height = 15.min(area.height.saturating_sub(2)).max(8);
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                overlay_title(app, "Go to symbol"),
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );
    let inner = inset(popup, 2, 1);
    let width = inner.width as usize;
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            app.symbol_query.as_str().to_owned(),
            Style::default().fg(theme.text),
        ),
    ])];
    let symbols = app.filtered_symbols();
    let rows = (inner.height as usize).saturating_sub(3).max(1);
    if symbols.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching symbols",
            Style::default().fg(theme.muted),
        )));
    } else {
        let selected = app.symbol_selected.min(symbols.len() - 1);
        let first = if selected >= rows {
            selected + 1 - rows
        } else {
            0
        };
        for (index, symbol) in symbols.iter().enumerate().skip(first).take(rows) {
            click_target(
                app,
                Rect::new(
                    inner.x,
                    inner.y + 1 + (index - first) as u16,
                    inner.width,
                    1,
                ),
                crate::app::ClickTarget::Row(index),
            );
            let is_selected = index == selected;
            let background = if is_selected {
                theme.elevated2
            } else {
                theme.elevated
            };
            let marker = if is_selected { ">" } else { " " };
            let label = format!("{marker} {:<8} {}", symbol.kind, symbol.name);
            let label = display_slice(&label, 0, width.saturating_sub(8), TAB_WIDTH);
            let line_no = format!("line {}", symbol.start_row + 1);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{label:<w$}", w = width.saturating_sub(line_no.len() + 1)),
                    Style::default()
                        .fg(if is_selected { theme.text } else { theme.muted })
                        .bg(background),
                ),
                Span::styled(
                    format!(" {line_no}"),
                    Style::default().fg(theme.faint).bg(background),
                ),
            ]));
        }
    }
    lines.push(Line::from(Span::styled(
        "↑↓ choose · Enter jump · Esc close",
        Style::default().fg(theme.faint),
    )));
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );
}

/// Single-cell ASCII lookalikes for the interface symbols Mellow draws, used
/// when the terminal cannot show Unicode (for example `LANG=C`). One cell
/// maps to one cell, so layout never shifts.
const ASCII_LOOKALIKES: &[(&str, &str)] = &[
    ("·", "-"),
    ("…", "."),
    ("→", ">"),
    ("←", "<"),
    ("↑", "^"),
    ("↓", "v"),
    ("›", ">"),
    ("‹", "<"),
    ("▸", ">"),
    ("▶", ">"),
    ("⎿", ">"),
    ("●", "*"),
    ("•", "*"),
    ("◐", "~"),
    ("✦", "*"),
    ("✓", "+"),
    ("✔", "+"),
    ("✚", "+"),
    ("×", "x"),
    ("—", "-"),
    ("–", "-"),
    ("─", "-"),
    ("│", "|"),
    ("┌", "+"),
    ("┐", "+"),
    ("└", "+"),
    ("┘", "+"),
    ("╭", "+"),
    ("╮", "+"),
    ("╰", "+"),
    ("╯", "+"),
    ("├", "+"),
    ("┤", "+"),
    ("┬", "+"),
    ("┴", "+"),
    ("┼", "+"),
    ("“", "\""),
    ("”", "\""),
    ("‘", "'"),
    ("’", "'"),
];

/// Replaces interface symbols with ASCII lookalikes across the whole frame.
fn asciify_symbols(buffer: &mut ratatui::buffer::Buffer) {
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            if let Some((_, ascii)) = ASCII_LOOKALIKES
                .iter()
                .find(|(symbol, _)| *symbol == cell.symbol())
            {
                cell.set_symbol(ascii);
            }
        }
    }
}

const ASCII_BORDER: ratatui::symbols::border::Set<'static> = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Rounded corners, or plain ASCII where the terminal cannot show Unicode.
fn rounded_border(unicode: bool) -> ratatui::symbols::border::Set<'static> {
    if unicode {
        ratatui::symbols::border::ROUNDED
    } else {
        ASCII_BORDER
    }
}

/// Square corners, or plain ASCII where the terminal cannot show Unicode.
fn plain_border(unicode: bool) -> ratatui::symbols::border::Set<'static> {
    if unicode {
        ratatui::symbols::border::PLAIN
    } else {
        ASCII_BORDER
    }
}

/// Clears an overlay's box and records it, so the mouse handler can tell a
/// click inside the overlay from one outside it.
fn clear_overlay(frame: &mut Frame<'_>, app: &App, popup: Rect) {
    frame.render_widget(Clear, popup);
    app.overlay_area.set(Some(popup));
}

/// Draws a row of buttons left to right, three columns apart, and records
/// each one so clicking it acts like its key.
fn draw_buttons(frame: &mut Frame<'_>, app: &App, row: Rect, buttons: &[(&str, Style, KeyEvent)]) {
    let mut x = row.x;
    for (label, style, key) in buttons {
        let width = (UnicodeWidthStr::width(*label) as u16).min(row.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        frame
            .buffer_mut()
            .set_stringn(x, row.y, label, width as usize, *style);
        click_target(
            app,
            Rect::new(x, row.y, width, 1),
            crate::app::ClickTarget::Key(*key),
        );
        x = x.saturating_add(width + 3);
    }
}

/// Records a clickable row or button of the overlay being drawn.
fn click_target(app: &App, rect: Rect, target: crate::app::ClickTarget) {
    app.overlay_targets.borrow_mut().push((rect, target));
}

/// A horizontal rule character that respects the ASCII fallback.
fn rule_glyph(app: &App) -> &'static str {
    if app.theme.unicode_symbols {
        "─"
    } else {
        "-"
    }
}

/// The marker in front of the selected row of a list.
fn select_marker(app: &App) -> &'static str {
    if app.theme.unicode_symbols {
        "› "
    } else {
        "> "
    }
}

fn render_palette(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 64.min(area.width.saturating_sub(4)).max(20);
    let popup_height = 15.min(area.height.saturating_sub(2)).max(8);
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                overlay_title(app, "Commands"),
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let input_width = inner.width.saturating_sub(2) as usize;
    let (palette_view, palette_cursor_x) = app.palette_query.visible_window(input_width);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "> ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if app.palette_query.is_empty() {
                    "Type a command…"
                } else {
                    palette_view.as_str()
                },
                Style::default().fg(if app.palette_query.is_empty() {
                    theme.faint
                } else {
                    theme.text
                }),
            ),
        ]),
        Line::from(Span::styled(
            rule_glyph(app).repeat(inner.width as usize),
            Style::default().fg(theme.elevated2),
        )),
    ];

    let header_rows = lines.len();
    let footer_rows = 1;
    let available_item_rows = (inner.height as usize)
        .saturating_sub(header_rows + footer_rows)
        .max(1);

    let matches = app.filtered_palette_entries();
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching commands",
            Style::default().fg(theme.muted),
        )));
    } else {
        let total_matches = matches.len();
        let selected = app.palette_selected.min(total_matches.saturating_sub(1));
        let window_start = if selected >= available_item_rows {
            selected + 1 - available_item_rows
        } else {
            0
        };
        let window_end = (window_start + available_item_rows).min(total_matches);

        for (item_idx, &entry_index) in matches[window_start..window_end].iter().enumerate() {
            let visible_index = window_start + item_idx;
            click_target(
                app,
                Rect::new(
                    inner.x,
                    inner.y + (header_rows + item_idx) as u16,
                    inner.width,
                    1,
                ),
                crate::app::ClickTarget::Row(visible_index),
            );
            let entry = COMMAND_SPECS[entry_index];
            let is_selected = visible_index == selected;
            let background = if is_selected {
                theme.elevated2
            } else {
                theme.elevated
            };
            let marker = if is_selected {
                if theme.unicode_symbols { "▸" } else { ">" }
            } else {
                " "
            };
            let shortcut = app.shortcut_label(entry.command).unwrap_or_default();
            let label_width = inner.width.saturating_sub(18) as usize;
            let label = display_slice(entry.label, 0, label_width, TAB_WIDTH);
            let label = format!("{label:<label_width$}");
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{marker} {label}"),
                    Style::default()
                        .fg(if is_selected { theme.text } else { theme.muted })
                        .bg(background)
                        .add_modifier(if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Span::styled(
                    format!("{shortcut:>14}"),
                    Style::default()
                        .fg(if is_selected { theme.mint } else { theme.faint })
                        .bg(background),
                ),
            ]));
        }

        let rendered_items = window_end - window_start;
        for _ in rendered_items..available_item_rows {
            lines.push(Line::from(""));
        }
    }

    let description = app
        .filtered_palette_entries()
        .get(app.palette_selected)
        .map(|&index| COMMAND_SPECS[index].help)
        .unwrap_or("");
    lines.push(if description.is_empty() {
        Line::from(Span::styled(
            "↑↓ choose · Enter run · Esc close",
            Style::default().fg(theme.faint),
        ))
    } else {
        let connector = if theme.unicode_symbols { "⎿ " } else { "> " };
        Line::from(vec![
            Span::styled(connector, Style::default().fg(theme.faint)),
            Span::styled(
                display_slice(
                    description,
                    0,
                    inner.width.saturating_sub(2) as usize,
                    TAB_WIDTH,
                ),
                Style::default().fg(theme.muted),
            ),
        ])
    });

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );
    let cursor_x = inner
        .x
        .saturating_add(2)
        .saturating_add(palette_cursor_x as u16)
        .min(inner.right().saturating_sub(1));
    frame.set_cursor_position((cursor_x, inner.y));
}

fn render_quick_open(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 72.min(area.width.saturating_sub(4)).max(28);
    let popup_height = 17.min(area.height.saturating_sub(2)).max(9);
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                overlay_title(app, "Open file"),
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let input_width = inner.width.saturating_sub(2) as usize;
    let (query_view, query_cursor_x) = app.quick_open_query.visible_window(input_width);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "> ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if app.quick_open_query.is_empty() {
                    "Type a filename or relative/absolute path…"
                } else {
                    query_view.as_str()
                },
                Style::default().fg(if app.quick_open_query.is_empty() {
                    theme.faint
                } else {
                    theme.text
                }),
            ),
        ]),
        Line::from(Span::styled(
            rule_glyph(app).repeat(inner.width as usize),
            Style::default().fg(theme.elevated2),
        )),
    ];

    let matches = app.filtered_quick_open_entries();
    let available = (inner.height as usize).saturating_sub(4).max(1);
    let selected = app.quick_open_selected.min(matches.len().saturating_sub(1));
    let start = if selected >= available {
        selected + 1 - available
    } else {
        0
    };
    let end = (start + available).min(matches.len());

    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching files. Type a path like ../notes.txt or ~/file and press Enter.",
            Style::default().fg(theme.muted),
        )));
    } else {
        for (visible_index, &candidate_index) in matches[start..end].iter().enumerate() {
            let list_index = start + visible_index;
            let path = &app.quick_open_candidates[candidate_index];
            click_target(
                app,
                Rect::new(inner.x, inner.y + 2 + visible_index as u16, inner.width, 1),
                crate::app::ClickTarget::Row(list_index),
            );
            let is_selected = list_index == selected;
            let background = if is_selected {
                theme.elevated2
            } else {
                theme.elevated
            };
            let marker = if is_selected {
                if theme.unicode_symbols { "▸" } else { ">" }
            } else {
                " "
            };
            let max_path = inner.width.saturating_sub(4) as usize;
            let visible_path = display_slice(&path.to_string_lossy(), 0, max_path, TAB_WIDTH);
            lines.push(Line::from(Span::styled(
                format!("{marker} {visible_path}"),
                Style::default()
                    .fg(if is_selected { theme.text } else { theme.muted })
                    .bg(background)
                    .add_modifier(if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )));
        }
    }

    while lines.len() < inner.height.saturating_sub(1) as usize {
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ choose · Tab complete · Enter open · Esc close",
        Style::default().fg(theme.faint),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );
    let cursor_x = inner
        .x
        .saturating_add(2)
        .saturating_add(query_cursor_x as u16)
        .min(inner.right().saturating_sub(1));
    frame.set_cursor_position((cursor_x, inner.y));
}

fn active_completion_anchor(area: Rect, app: &App) -> Option<(u16, u16, Rect)> {
    let terminal_height = terminal_panel_height(area.height, app.terminal_panel());
    let workspace_height = area
        .height
        .saturating_sub(HEADER_ROWS)
        .saturating_sub(terminal_height)
        .saturating_sub(footer_rows(area.height));
    let workspace = Rect::new(
        area.x,
        area.y.saturating_add(HEADER_ROWS),
        area.width,
        workspace_height.max(1),
    );

    let explorer = explorer_width(workspace.width, app.explorer_visible, app.split_enabled());
    let mut editor_area = workspace;
    editor_area.x = editor_area.x.saturating_add(explorer);
    editor_area.width = editor_area.width.saturating_sub(explorer).max(1);

    let active_pane = if app.split_enabled() {
        app.active_pane_index()
    } else {
        0
    };
    let pane_area = if rendered_split(workspace.width, app.explorer_visible, app.split_enabled()) {
        match app.split_orientation() {
            SplitOrientation::SideBySide => {
                let (left_width, right_width) = split_widths(editor_area.width, app.split_ratio());
                if active_pane == 0 {
                    Rect::new(editor_area.x, editor_area.y, left_width, editor_area.height)
                } else {
                    Rect::new(
                        editor_area.x.saturating_add(left_width).saturating_add(1),
                        editor_area.y,
                        right_width,
                        editor_area.height,
                    )
                }
            }
            SplitOrientation::Stacked => {
                let (top_height, bottom_height) =
                    split_heights(editor_area.height, app.split_ratio());
                if active_pane == 0 {
                    Rect::new(editor_area.x, editor_area.y, editor_area.width, top_height)
                } else {
                    Rect::new(
                        editor_area.x,
                        editor_area.y.saturating_add(top_height).saturating_add(1),
                        editor_area.width,
                        bottom_height,
                    )
                }
            }
        }
    } else {
        editor_area
    };

    let view = app.pane_view(active_pane)?;
    let gutter = gutter_width(view.buffer.line_count());
    let text_width = pane_area.width.saturating_sub(gutter).max(1) as usize;
    let (visual_row, display_col) = if app.word_wrap {
        let layout = VisualLayout::new(view.buffer, text_width, TAB_WIDTH, true);
        let position = layout.visual_position_for_cursor(view.buffer, view.cursor);
        (
            position.row.saturating_sub(view.visual_scroll_row),
            position.column,
        )
    } else {
        (
            view.cursor.row.saturating_sub(view.scroll_row),
            view.buffer
                .display_width_before(view.cursor.row, view.cursor.col, TAB_WIDTH)
                .saturating_sub(view.scroll_col),
        )
    };

    let x = pane_area
        .x
        .saturating_add(gutter)
        .saturating_add(display_col as u16);
    let y = pane_area.y.saturating_add(visual_row as u16);
    (x < pane_area.right() && y < pane_area.bottom()).then_some((x, y, pane_area))
}

fn render_completion(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if app.completion_items.is_empty() {
        return;
    }

    let theme = app.theme;
    let max_width = area.width.saturating_sub(2).max(1);
    let popup_width = 54.min(max_width).max(28.min(max_width));
    let visible_items = app.completion_items.len().min(8) as u16;
    let popup_height = (visible_items + 3)
        .max(5)
        .min(area.height.saturating_sub(2).max(1));

    let (cursor_x, cursor_y, pane_area) = active_completion_anchor(area, app).unwrap_or((
        area.x.saturating_add(2),
        area.y.saturating_add(HEADER_ROWS),
        area,
    ));

    let min_x = area.x.saturating_add(1);
    let max_x = area.right().saturating_sub(popup_width).saturating_sub(1);
    let popup_x = cursor_x.clamp(min_x, max_x.max(min_x));

    let below_y = cursor_y.saturating_add(1);
    let popup_y = if below_y.saturating_add(popup_height) <= pane_area.bottom() {
        below_y
    } else {
        cursor_y.saturating_sub(popup_height).max(pane_area.y)
    };
    let popup = Rect::new(popup_x, popup_y, popup_width, popup_height);

    clear_overlay(frame, app, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(rounded_border(theme.unicode_symbols))
        .border_style(Style::default().fg(theme.mint))
        .title(Span::styled(
            " Suggestions ",
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let available = inner.height.saturating_sub(1) as usize;
    let selected = app
        .completion_selected
        .min(app.completion_items.len().saturating_sub(1));
    let start = selected.saturating_sub(available.saturating_sub(1));

    let mut lines = Vec::with_capacity(inner.height as usize);
    for (index, item) in app
        .completion_items
        .iter()
        .enumerate()
        .skip(start)
        .take(available)
    {
        let is_selected = index == selected;
        let marker = if is_selected {
            if theme.unicode_symbols { "▸" } else { ">" }
        } else {
            " "
        };
        let detail = item.detail.as_deref().unwrap_or("");
        let label_width = (inner.width as usize).saturating_mul(3) / 5;
        let detail_width = (inner.width as usize)
            .saturating_sub(label_width)
            .saturating_sub(4);
        let label = display_slice(&item.label, 0, label_width, TAB_WIDTH);
        let detail = display_slice(detail, 0, detail_width, TAB_WIDTH);
        let style = if is_selected {
            Style::default()
                .fg(theme.text)
                .bg(theme.elevated2)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted).bg(theme.elevated)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker} {label:<label_width$}  {detail}"),
            style,
        )));
    }

    while lines.len() < available {
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ choose · Tab/Enter apply · Esc",
        Style::default().fg(theme.faint),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );

    if cursor_x < area.right() && cursor_y < area.bottom() {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_explorer_path_action(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    title: &str,
    footer: &str,
) {
    let popup = centered_rect(area, 72, 7);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let value = if app.explorer_action_query.is_empty() {
        "<workspace-relative path>".to_owned()
    } else {
        app.explorer_action_query.as_str().to_owned()
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Path: ", Style::default().fg(app.theme.mint)),
                Span::styled(value, Style::default().fg(app.theme.text)),
            ]),
            Line::raw(""),
            Line::from(Span::styled(footer, Style::default().fg(app.theme.faint))),
        ]),
        inner,
    );
}

fn render_explorer_delete_confirmation(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 68, 8);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Delete? ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.warning))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let target = app
        .explorer_action_target()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_owned());
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("Permanently delete this workspace file or empty directory?"),
            Line::raw(""),
            Line::from(Span::styled(target, Style::default().fg(app.theme.text))),
            Line::raw(""),
            Line::from(Span::styled(
                "Y / Enter delete · N / Esc cancel",
                Style::default().fg(app.theme.faint),
            )),
        ]),
        inner,
    );
}

fn render_references(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 86, 18);
    clear_overlay(frame, app, popup);
    let theme = app.theme;
    let block = Block::default()
        .title(overlay_title(app, "References"))
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(theme.mint))
        .style(Style::default().bg(theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let mut lines = Vec::new();
    let visible = inner.height.saturating_sub(2) as usize;
    let start = app
        .reference_selected
        .saturating_sub(visible.saturating_sub(1));
    for (index, location) in app
        .reference_items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        click_target(
            app,
            Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1),
            crate::app::ClickTarget::Row(index),
        );
        let marker = if index == app.reference_selected {
            select_marker(app)
        } else {
            "  "
        };
        lines.push(Line::from(format!(
            "{marker}{}:{}:{}",
            location.path.display(),
            location.range.start.line + 1,
            location.range.start.character + 1
        )));
    }
    lines.push(Line::from(Span::styled(
        "↑/↓ navigate · PgUp/PgDn · Enter open exact location · Esc close",
        Style::default().fg(theme.faint),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_rename_input(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 64, 7);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Rename ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("New name: {}", app.rename_query.as_str())),
            Line::raw(""),
            Line::raw("Enter request preview · Esc cancel"),
        ]),
        inner,
    );
}

fn render_lsp_edit_confirmation(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let height = area.height.saturating_sub(4).clamp(8, 30);
    let popup = centered_rect(area, 96, height);
    clear_overlay(frame, app, popup);
    let files = app
        .pending_lsp_preview
        .iter()
        .filter(|row| matches!(row, LspPreviewRow::File(_)))
        .count();
    let block = Block::default()
        .title(format!(
            " {} · {} edit(s) in {} file(s) ",
            app.pending_lsp_label,
            app.pending_lsp_edits.len(),
            files
        ))
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(theme.mint))
        .style(Style::default().bg(theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let width = inner.width as usize;
    let body_rows = inner.height.saturating_sub(1) as usize;
    let mut lines: Vec<Line<'_>> = app
        .pending_lsp_preview
        .iter()
        .skip(app.pending_lsp_scroll)
        .take(body_rows)
        .map(|row| {
            let (text, style) = match row {
                LspPreviewRow::File(path) => (
                    path.clone(),
                    Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
                ),
                LspPreviewRow::Location(line) => {
                    (format!("  line {line}"), Style::default().fg(theme.faint))
                }
                LspPreviewRow::Removed(text) => (
                    format!("− {text}"),
                    Style::default().fg(theme.diff_text).bg(theme.diff_del_bg),
                ),
                LspPreviewRow::Added(text) => (
                    format!("+ {text}"),
                    Style::default().fg(theme.diff_text).bg(theme.diff_add_bg),
                ),
                LspPreviewRow::Note(text) => {
                    (format!("  {text}"), Style::default().fg(theme.warning))
                }
            };
            let text = display_slice(&text, 0, width, TAB_WIDTH);
            let pad = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
            Line::styled(format!("{text}{}", " ".repeat(pad)), style)
        })
        .collect();
    while lines.len() < body_rows {
        lines.push(Line::raw(""));
    }
    let more = app.pending_lsp_preview.len() > app.pending_lsp_scroll + body_rows;
    lines.push(Line::styled(
        format!(
            "Y / Enter apply all · N / Esc cancel · ↑↓ PgUp/PgDn scroll{}",
            if more { " · more below" } else { "" }
        ),
        Style::default().fg(theme.faint),
    ));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_code_actions(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 82, 16);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(overlay_title(app, "Quick fixes"))
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let mut lines = Vec::new();
    for (i, action) in app
        .code_action_items
        .iter()
        .enumerate()
        .take(inner.height.saturating_sub(2) as usize)
    {
        click_target(
            app,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            crate::app::ClickTarget::Row(i),
        );
        let marker = if i == app.code_action_selected {
            select_marker(app)
        } else {
            "  "
        };
        let suffix = if action.has_resource_operations {
            " [file operation blocked]"
        } else if action.has_command && action.edits.is_empty() {
            " [command blocked]"
        } else if action.has_command {
            " [edit + command; command ignored]"
        } else {
            ""
        };
        lines.push(Line::from(format!("{marker}{}{suffix}", action.title)));
    }
    lines.push(Line::raw(
        "↑/↓ select · Enter preview supported edits · Esc close",
    ));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_language_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::lsp::ServerState;
    let theme = app.theme;
    let health = app.language_health();
    let popup = centered_rect(area, 78, 16);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(format!(" Language server · {} ", health.language))
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(theme.mint))
        .style(Style::default().bg(theme.elevated));
    let inner = inset(block.inner(popup), 1, 0);
    frame.render_widget(block, popup);

    let (state, state_color) = match health.state {
        ServerState::NoServerForLanguage => ("No language server for this file type", theme.muted),
        ServerState::NotInstalled => ("Not installed", theme.warning),
        ServerState::NotStarted => ("Installed · not started", theme.muted),
        ServerState::Starting => ("Starting…", theme.muted),
        ServerState::Running => ("Running", theme.mint),
        ServerState::Failed => ("Stopped after an error", theme.error),
    };
    let label = |text: &str| Span::styled(format!("{text:<12}"), Style::default().fg(theme.muted));
    let value = |text: String| Span::styled(text, Style::default().fg(theme.text));
    let mut lines = vec![Line::from(vec![
        label("Status"),
        Span::styled(
            state,
            Style::default()
                .fg(state_color)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    if let Some(server) = &health.server {
        lines.push(Line::from(vec![label("Server"), value(server.clone())]));
    }
    if let Some(command) = &health.command {
        lines.push(Line::from(vec![label("Command"), value(command.clone())]));
    }
    if let Some(path) = &health.installed {
        lines.push(Line::from(vec![
            label("Found at"),
            value(path.display().to_string()),
        ]));
    }
    if !health.capabilities.is_empty() {
        lines.push(Line::from(vec![
            label("Supports"),
            value(health.capabilities.join(", ")),
        ]));
    }
    if let Some(error) = &health.last_error {
        lines.push(Line::from(vec![
            label("Last error"),
            Span::styled(error.clone(), Style::default().fg(theme.error)),
        ]));
    }
    if health.state == ServerState::NotInstalled
        && let Some(hint) = health.install_hint
    {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![label("Install"), value(hint.to_owned())]));
    }
    if let Some(variable) = health.override_variable {
        lines.push(Line::styled(
            format!("Using another server? Set {variable} to its command."),
            Style::default().fg(theme.faint),
        ));
    }
    if health.state == ServerState::NoServerForLanguage {
        lines.push(Line::styled(
            "Highlighting and word completion still work without a language server.",
            Style::default().fg(theme.faint),
        ));
    }
    let footer = inner.height.saturating_sub(1) as usize;
    while lines.len() < footer {
        lines.push(Line::raw(""));
    }
    lines.truncate(footer);
    lines.push(Line::styled(
        "R restart server · Esc close",
        Style::default().fg(theme.faint),
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_problems(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let problems = app.problem_items();
    let popup_width = 92.min(area.width.saturating_sub(4)).max(36);
    let popup_height = 19.min(area.height.saturating_sub(2)).max(9);
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(if problems.is_empty() {
                theme.muted
            } else {
                theme.warning
            }))
            .title(Span::styled(
                format!(
                    " {} Problems · {} ",
                    overlay_marker(theme.unicode_symbols),
                    problems.len()
                ),
                Style::default()
                    .fg(if problems.is_empty() {
                        theme.muted
                    } else {
                        theme.warning
                    })
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let available = inner.height.saturating_sub(1) as usize;
    let selected = app.problem_selected.min(problems.len().saturating_sub(1));
    let start = if available > 0 && selected >= available {
        selected + 1 - available
    } else {
        0
    };

    let mut lines = Vec::with_capacity(inner.height as usize);
    if problems.is_empty() {
        lines.push(Line::from(Span::styled(
            "No syntax or language-server diagnostics for the active file.",
            Style::default().fg(theme.muted),
        )));
    } else {
        for (index, problem) in problems.iter().enumerate().skip(start).take(available) {
            let is_selected = index == selected;
            let (symbol, severity_color) = match problem.severity {
                ProblemSeverity::Error => {
                    (if theme.unicode_symbols { "●" } else { "E" }, theme.error)
                }
                ProblemSeverity::Warning => {
                    (if theme.unicode_symbols { "▲" } else { "W" }, theme.warning)
                }
                ProblemSeverity::Information => {
                    (if theme.unicode_symbols { "i" } else { "I" }, theme.mint)
                }
                ProblemSeverity::Hint => {
                    (if theme.unicode_symbols { "·" } else { "H" }, theme.muted)
                }
            };
            let prefix = format!(
                "{symbol} {:>4}:{:<3} {:<14} ",
                problem.cursor.row + 1,
                problem.cursor.col + 1,
                display_slice(&problem.source, 0, 14, TAB_WIDTH)
            );
            let message_width = (inner.width as usize).saturating_sub(prefix.len());
            let message = display_slice(&problem.message, 0, message_width, TAB_WIDTH);
            let background = if is_selected {
                theme.elevated2
            } else {
                theme.elevated
            };
            lines.push(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(severity_color)
                        .bg(background)
                        .add_modifier(if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Span::styled(
                    message,
                    Style::default()
                        .fg(if is_selected { theme.text } else { theme.muted })
                        .bg(background),
                ),
            ]));
        }
    }

    while lines.len() < available {
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ choose · Enter go to problem · Esc close",
        Style::default().fg(theme.faint),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );
}

fn render_git_commit_input(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 78, 8);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Commit ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Message: {}", app.git_commit_query.as_str())),
            Line::raw(""),
            Line::from(format!(
                "{} staged hunk(s) · {} unstaged hunk(s)",
                app.git_snapshot.staged_count(),
                app.git_snapshot.unstaged_count()
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "Enter commit · Ctrl+G draft message · Esc cancel",
                Style::default().fg(app.theme.faint),
            )),
        ]),
        inner,
    );
}

fn render_git_branches(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 76, 18);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(overlay_title(app, "Branches"))
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible = inner.height.saturating_sub(2) as usize;
    let start = app
        .git_branch_selected
        .saturating_sub(visible.saturating_sub(1));
    let mut lines = Vec::new();
    for (index, branch) in app
        .git_branches
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        click_target(
            app,
            Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1),
            crate::app::ClickTarget::Row(index),
        );
        let selected = index == app.git_branch_selected;
        let cursor = if selected { select_marker(app) } else { "  " };
        let current = if branch.current { "* " } else { "  " };
        let style = if selected {
            Style::default().fg(app.theme.text).bg(app.theme.elevated2)
        } else if branch.current {
            Style::default().fg(app.theme.mint)
        } else {
            Style::default().fg(app.theme.muted)
        };
        lines.push(Line::from(Span::styled(
            format!("{cursor}{current}{}", branch.name),
            style,
        )));
    }
    lines.push(Line::from(Span::styled(
        "↑/↓ select · Enter switch · N new · R rename · D safe delete · Esc close",
        Style::default().fg(app.theme.faint),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_git_branch_create(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 70, 7);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" New branch ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Name: {}", app.git_branch_query.as_str())),
            Line::raw(""),
            Line::from(Span::styled(
                "Enter create · Esc back",
                Style::default().fg(app.theme.faint),
            )),
        ]),
        inner,
    );
}

fn render_git_branch_rename(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 72, 8);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Rename branch ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let source = app.git_branch_action_target().unwrap_or("<unknown>");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("From: {source}")),
            Line::from(format!("To:   {}", app.git_branch_query.as_str())),
            Line::raw(""),
            Line::from(Span::styled(
                "Enter rename · Esc back",
                Style::default().fg(app.theme.faint),
            )),
        ]),
        inner,
    );
}

fn render_git_branch_delete_confirmation(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 70, 9);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Delete branch? ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.warning))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let branch = app.git_branch_action_target().unwrap_or("<unknown>");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("Delete this local branch using Git's safe -d check?"),
            Line::raw(""),
            Line::from(Span::styled(branch, Style::default().fg(app.theme.text))),
            Line::raw(""),
            Line::from(Span::styled(
                "Y / Enter delete if merged · N / Esc cancel",
                Style::default().fg(app.theme.faint),
            )),
        ]),
        inner,
    );
}

fn render_git_blame(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 110, 24);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Who changed each line ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible = inner.height.saturating_sub(1) as usize;
    let start = app
        .git_blame_selected
        .saturating_sub(visible.saturating_sub(1));
    let mut lines = Vec::new();
    for (index, item) in app.git_blame.iter().enumerate().skip(start).take(visible) {
        let selected = index == app.git_blame_selected;
        let marker = if selected { select_marker(app) } else { "  " };
        let text = format!(
            "{marker}{:>5}  {:<10}  {:<18}  {}",
            item.row + 1,
            item.short_oid,
            item.author.chars().take(18).collect::<String>(),
            item.text
        );
        let style = if selected {
            Style::default().fg(app.theme.text).bg(app.theme.elevated2)
        } else {
            Style::default().fg(app.theme.muted)
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    lines.push(Line::from(Span::styled(
        "↑/↓ · PgUp/PgDn · Enter jump to line · Esc close",
        Style::default().fg(app.theme.faint),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_git_conflicts(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 88, 20);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Conflicts ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.warning))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible = inner.height.saturating_sub(3) as usize;
    let start = app
        .git_conflict_selected
        .saturating_sub(visible.saturating_sub(1));
    let mut lines = vec![Line::from(Span::styled(
        "Manual resolution workflow: open → edit markers → save → mark resolved",
        Style::default().fg(app.theme.muted),
    ))];

    for (index, path) in app
        .git_conflicts
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        click_target(
            app,
            Rect::new(
                inner.x,
                inner.y + 1 + (index - start) as u16,
                inner.width,
                1,
            ),
            crate::app::ClickTarget::Row(index),
        );
        let selected = index == app.git_conflict_selected;
        let marker = if selected { select_marker(app) } else { "  " };
        let style = if selected {
            Style::default().fg(app.theme.text).bg(app.theme.elevated2)
        } else {
            Style::default().fg(app.theme.warning)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", path.display()),
            style,
        )));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "↑/↓ select · Enter open marker · M mark active saved file resolved · R refresh · Esc close",
        Style::default().fg(app.theme.faint),
    )));
    lines.truncate(inner.height as usize);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_git_history(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 100, 22);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(overlay_title(app, "History"))
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible = inner.height.saturating_sub(1) as usize;
    let start = app
        .git_history_selected
        .saturating_sub(visible.saturating_sub(1));
    let mut lines = Vec::new();
    for (index, commit) in app.git_history.iter().enumerate().skip(start).take(visible) {
        click_target(
            app,
            Rect::new(inner.x, inner.y + (index - start) as u16, inner.width, 1),
            crate::app::ClickTarget::Row(index),
        );
        let selected = index == app.git_history_selected;
        let marker = if selected { select_marker(app) } else { "  " };
        let line = format!(
            "{marker}{}  {}  {}  {}",
            commit.short_oid, commit.date, commit.author, commit.subject
        );
        let style = if selected {
            Style::default().fg(app.theme.text).bg(app.theme.elevated2)
        } else {
            Style::default().fg(app.theme.muted)
        };
        lines.push(Line::from(Span::styled(line, style)));
    }
    lines.push(Line::from(Span::styled(
        "↑/↓ · PgUp/PgDn · Esc close",
        Style::default().fg(app.theme.faint),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Added and removed line counts for one hunk's patch text.
fn hunk_line_counts(patch: &str) -> (usize, usize) {
    let body = patch
        .split_inclusive('\n')
        .skip_while(|line| !line.starts_with("@@ "))
        .skip(1);
    let mut added = 0;
    let mut removed = 0;
    for line in body {
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

fn render_changes(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let hunks = &app.git_snapshot.hunks;
    let list_rows = hunks.len().clamp(1, 6) as u16;
    let popup_width = 104.min(area.width.saturating_sub(4)).max(42);
    let popup_height = (list_rows + 16).min(area.height.saturating_sub(2)).max(10);
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);

    let branch = if app.git_snapshot.branch.is_empty() {
        "repository"
    } else {
        app.git_snapshot.branch.as_str()
    };
    let staged = app.git_snapshot.staged_count();
    let unstaged = app.git_snapshot.unstaged_count();
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                format!(
                    " {} Changes · {branch} · {unstaged} not staged · {staged} staged ",
                    overlay_marker(theme.unicode_symbols)
                ),
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let footer_rows = 2u16;
    let selected = app.changes_selected.min(hunks.len().saturating_sub(1));
    let list_area = Rect::new(inner.x, inner.y, inner.width, list_rows);
    let diff_area = Rect::new(
        inner.x,
        inner.y + list_rows + 1,
        inner.width,
        inner.height.saturating_sub(list_rows + 1 + footer_rows),
    );

    if hunks.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "Nothing to commit. Every file matches the last commit.",
                Style::default().fg(theme.muted),
            )),
            list_area,
        );
    } else {
        let start = if selected >= list_rows as usize {
            selected + 1 - list_rows as usize
        } else {
            0
        };
        let path_width = (inner.width as usize).saturating_sub(34).min(48);
        let mut lines = Vec::new();
        for (index, hunk) in hunks
            .iter()
            .enumerate()
            .skip(start)
            .take(list_rows as usize)
        {
            let is_selected = index == selected;
            let background = if is_selected {
                theme.elevated2
            } else {
                theme.elevated
            };
            let base = Style::default().bg(background);
            let marker = if is_selected {
                if theme.unicode_symbols { "▸ " } else { "> " }
            } else {
                "  "
            };
            let path = display_slice(&hunk.path.to_string_lossy(), 0, path_width, TAB_WIDTH);
            let place = if hunk.synthetic_file {
                "new file".to_owned()
            } else {
                format!("line {}", hunk.new_start.max(1))
            };
            let (added, removed) = hunk_line_counts(&hunk.patch);
            let (stage_text, stage_color) = match hunk.stage {
                GitHunkStage::Unstaged => ("not staged", theme.warning),
                GitHunkStage::Staged => ("staged", theme.mint),
            };
            lines.push(Line::from(vec![
                Span::styled(marker, base.fg(theme.mint)),
                Span::styled(
                    format!("{path:<path_width$}  "),
                    base.fg(if is_selected { theme.text } else { theme.muted })
                        .add_modifier(if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Span::styled(format!("{place:<10}"), base.fg(theme.faint)),
                Span::styled(format!("{:>4}", format!("+{added}")), base.fg(theme.mint)),
                Span::styled(
                    format!("{:>5}  ", format!("−{removed}")),
                    base.fg(theme.error),
                ),
                Span::styled(stage_text, base.fg(stage_color)),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
            list_area,
        );

        // The selected change itself, so you can decide without leaving.
        if let Some(hunk) = hunks.get(selected) {
            let add_bg = theme.diff_add_bg;
            let del_bg = theme.diff_del_bg;
            let width = diff_area.width as usize;
            let total = app.selected_hunk_diff_lines();
            let rows = diff_area.height as usize;
            let scroll = app.changes_diff_scroll.min(total.saturating_sub(1));
            // Keep the last row for a "more" hint when the diff overflows.
            let visible = if total.saturating_sub(scroll) > rows {
                rows.saturating_sub(1)
            } else {
                rows
            };
            let mut diff_lines: Vec<Line<'_>> = hunk
                .patch
                .lines()
                .skip_while(|line| !line.starts_with("@@ "))
                .skip(1)
                .skip(scroll)
                .take(visible)
                .map(|line| {
                    let (style, text) = if let Some(rest) = line.strip_prefix('+') {
                        (
                            Style::default().fg(theme.diff_text).bg(add_bg),
                            format!("+ {rest}"),
                        )
                    } else if let Some(rest) = line.strip_prefix('-') {
                        (
                            Style::default().fg(theme.diff_text).bg(del_bg),
                            format!("− {rest}"),
                        )
                    } else {
                        (
                            Style::default().fg(theme.faint),
                            format!("  {}", line.strip_prefix(' ').unwrap_or(line)),
                        )
                    };
                    let text = display_slice(&text, 0, width, TAB_WIDTH);
                    let pad = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
                    Line::from(Span::styled(format!("{text}{}", " ".repeat(pad)), style))
                })
                .collect();
            let hidden_below = total.saturating_sub(scroll + visible);
            if hidden_below > 0 {
                let above = if scroll > 0 {
                    format!("↑ {scroll} above · ")
                } else {
                    String::new()
                };
                diff_lines.push(Line::styled(
                    format!("  {above}↓ {hidden_below} more · Shift+↑↓ scroll diff"),
                    Style::default().fg(theme.faint),
                ));
            }
            frame.render_widget(
                Paragraph::new(Line::styled(
                    rule_glyph(app).repeat(inner.width as usize),
                    Style::default().fg(theme.elevated2),
                )),
                Rect::new(inner.x, inner.y + list_rows, inner.width, 1),
            );
            frame.render_widget(
                Paragraph::new(diff_lines).style(Style::default().bg(theme.elevated)),
                diff_area,
            );
        }
    }

    let footer = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(footer_rows),
        inner.width,
        footer_rows,
    );
    let note = if app.has_unsaved_tabs() {
        "Save your files before staging or discarding."
    } else {
        "Discard always asks first."
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "↑↓ choose · Shift+↑↓ scroll · Enter open · S stage · U unstage · R discard · Esc close",
                Style::default().fg(theme.muted),
            ),
            Line::styled(note, Style::default().fg(theme.faint)),
        ]),
        footer,
    );
}
fn render_revert_confirmation(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup = centered_rect(area, 72, 9);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.error))
            .title(Span::styled(
                " Discard this change? ",
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let hunk = app.git_snapshot.hunks.get(app.changes_selected);
    let path = hunk
        .map(|hunk| hunk.path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "selected change".to_owned());
    let preview = hunk
        .map(|hunk| hunk.preview.as_str())
        .unwrap_or("selected hunk");

    let lines = vec![
        Line::from(Span::styled(
            path,
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            display_slice(preview, 0, inner.width as usize, TAB_WIDTH),
            Style::default().fg(theme.muted),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "This discards only the selected unstaged Git hunk from the working tree.",
            Style::default().fg(theme.warning),
        )),
    ];
    // A long path or the warning can wrap; the buttons keep the last row.
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .style(Style::default().fg(theme.text).bg(theme.elevated)),
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(2),
        ),
    );
    let key = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
    draw_buttons(
        frame,
        app,
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        &[
            (
                " [R / Enter] Revert hunk ",
                Style::default()
                    .fg(theme.canvas)
                    .bg(theme.error)
                    .add_modifier(Modifier::BOLD),
                key(KeyCode::Enter),
            ),
            (
                " [Esc] Cancel ",
                Style::default().fg(theme.text).bg(theme.elevated2),
                key(KeyCode::Esc),
            ),
        ],
    );
}

/// Short labels for the quick requests shown in the Ask AI box.
const AI_SUGGESTION_LABELS: [&str; 3] = ["Explain", "Fix problems", "Write tests"];

/// Quick requests offered in the Ask AI box; Tab cycles through them.
pub fn ai_suggestions(has_selection: bool) -> [&'static str; 3] {
    if has_selection {
        [
            "Explain this code",
            "Fix any problems in this code",
            "Write tests for this code",
        ]
    } else {
        [
            "Explain what this code does",
            "Find bugs near the cursor",
            "Write tests for this code",
        ]
    }
}

/// Places a small popup just below the cursor line (or above it when there
/// is no room), so the conversation stays next to the code it is about.
fn anchored_rect(area: Rect, app: &App, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    let explorer = explorer_width(area.width, app.explorer_visible, app.split_enabled());
    let (cursor_x, cursor_y) = app
        .last_cursor_screen
        .get()
        .unwrap_or((explorer + 6, area.height / 3));
    let editor_bottom = area.bottom().saturating_sub(1);
    let y = if cursor_y + 1 + height <= editor_bottom {
        cursor_y + 1
    } else {
        cursor_y.saturating_sub(height).max(HEADER_ROWS)
    };
    let min_x = (explorer + 1).min(area.right().saturating_sub(width));
    let x = cursor_x
        .saturating_sub(4)
        .clamp(min_x, area.right().saturating_sub(width + 1).max(min_x));
    Rect::new(x, y, width, height)
}

pub fn ai_prompt_rect(area: Rect, app: &App) -> Rect {
    anchored_rect(area, app, 84, 8)
}

fn ai_box(app: &App, title: String) -> Block<'static> {
    let theme = app.theme;
    Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_set(rounded_border(theme.unicode_symbols))
        .border_style(Style::default().fg(theme.mint))
        .style(Style::default().bg(theme.elevated))
}

fn render_ai_prompt(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let spark = if theme.unicode_symbols { "✦" } else { "*" };
    let popup = ai_prompt_rect(area, app);
    clear_overlay(frame, app, popup);
    let block = ai_box(app, format!(" {spark} Ask AI about {} ", app.ai_subject()));
    let inner = inset(block.inner(popup), 1, 0);
    frame.render_widget(block, popup);

    let width = inner.width as usize;
    let (query, cursor_x) = app.ai_query.visible_window(width.saturating_sub(3));
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ),
        if app.ai_query.is_empty() {
            Span::styled(
                "Ask anything, or press Tab for a suggestion",
                Style::default().fg(theme.faint),
            )
        } else {
            Span::styled(query, Style::default().fg(theme.text))
        },
    ])];
    let mut chips = vec![Span::styled("Tab: ", Style::default().fg(theme.faint))];
    for (index, suggestion) in AI_SUGGESTION_LABELS.iter().enumerate() {
        if index > 0 {
            chips.push(Span::styled(" · ", Style::default().fg(theme.faint)));
        }
        chips.push(Span::styled(
            *suggestion,
            Style::default()
                .fg(theme.text)
                .add_modifier(Modifier::UNDERLINED),
        ));
    }
    lines.push(Line::from(chips));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("Sends ", Style::default().fg(theme.faint)),
        Span::styled(
            app.ai_context_label().to_owned(),
            Style::default().fg(theme.text),
        ),
        Span::styled(
            format!(
                " with the file name and language to {}. Nothing else.",
                app.ai_provider_name()
            ),
            Style::default().fg(theme.faint),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "Enter send · Tab suggestion · Esc cancel",
        Style::default().fg(theme.faint),
    )));
    let lines: Vec<Line<'_>> = lines.into_iter().take(inner.height as usize).collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    frame.set_cursor_position((
        inner.x
            + 2
            + if app.ai_query.is_empty() {
                0
            } else {
                cursor_x as u16
            },
        inner.y,
    ));
}

/// Marker that opens every list overlay title, matching the agent-style AI panels.
fn overlay_marker(unicode: bool) -> &'static str {
    if unicode { "●" } else { "*" }
}

/// Title for a list overlay: " ● Name " (or " * Name " without Unicode).
fn overlay_title(app: &App, name: &str) -> String {
    format!(" {} {name} ", overlay_marker(app.theme.unicode_symbols))
}

/// One frame of the "working" spinner. It is derived from the clock rather
/// than stored state: the event loop redraws at least every 100 ms, so the
/// frame advances on its own while an AI request is in flight.
fn working_frame(unicode: bool) -> &'static str {
    const BRAILLE: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const ASCII: [&str; 4] = ["|", "/", "-", "\\"];
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() / 100);
    if unicode {
        BRAILLE[(tick % BRAILLE.len() as u128) as usize]
    } else {
        ASCII[(tick % ASCII.len() as u128) as usize]
    }
}

fn render_ai_waiting(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let spark = if theme.unicode_symbols { "✦" } else { "*" };
    // Four content rows (status, query, gap, note) plus the border.
    let popup = anchored_rect(area, app, 84, 6);
    clear_overlay(frame, app, popup);
    let block = ai_box(
        app,
        format!(" {spark} {} is working ", app.ai_provider_name()),
    );
    let inner = inset(block.inner(popup), 1, 0);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("{} ", working_frame(theme.unicode_symbols)),
                    Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
                ),
                Span::styled("Working on it", Style::default().fg(theme.text)),
            ]),
            Line::from(vec![
                Span::styled("> ", Style::default().fg(theme.faint)),
                Span::styled(
                    app.ai_query.as_str().to_owned(),
                    Style::default().fg(theme.muted),
                ),
            ]),
            Line::raw(""),
            Line::styled(
                "Nothing changes until you accept. Esc stops waiting.",
                Style::default().fg(theme.faint),
            ),
        ])
        .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_ai_review(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let spark = if theme.unicode_symbols { "✦" } else { "*" };
    let Some(proposal) = app.ai_proposal.as_ref() else {
        return;
    };
    let width = 100.min(area.width.saturating_sub(4));
    let text_width = width.saturating_sub(4).max(1) as usize;
    let summary_rows: usize = proposal
        .summary
        .lines()
        .map(|line| UnicodeWidthStr::width(line).div_ceil(text_width).max(1))
        .sum();
    // Blank line, change-count connector, then one row per removed and added line.
    let diff_rows = proposal.replacement.as_deref().map_or(0, |replacement| {
        2 + app.ai_before_text().lines().count() + replacement.lines().count()
    });
    let height = (summary_rows + diff_rows + 5) as u16;
    let popup = centered_rect(
        area,
        width,
        height.clamp(8, 28).min(area.height.saturating_sub(2)),
    );
    clear_overlay(frame, app, popup);
    let title = if proposal.replacement.is_some() {
        format!(" {spark} Suggested change ")
    } else {
        format!(" {spark} {}'s answer ", app.ai_provider_name())
    };
    let block = ai_box(app, title);
    let inner = inset(block.inner(popup), 1, 0);
    frame.render_widget(block, popup);

    let footer_rows = 2u16;
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_rows),
    );
    // Agent-style result: a bullet headline, then a connector that says how
    // many lines change, then the diff itself.
    let bullet = if theme.unicode_symbols { "● " } else { "* " };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            bullet,
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ),
        Span::styled(proposal.summary.clone(), Style::default().fg(theme.text)),
    ])];
    if let Some(replacement) = proposal.replacement.as_deref() {
        let removed = app.ai_before_text().lines().count();
        let added = replacement.lines().count();
        let connector = if theme.unicode_symbols { "⎿ " } else { "> " };
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(format!("  {connector} "), Style::default().fg(theme.faint)),
            Span::styled(
                format!("+{added} added  −{removed} removed"),
                Style::default().fg(theme.muted),
            ),
        ]));
        let width = inner.width as usize;
        let add_bg = theme.diff_add_bg;
        let del_bg = theme.diff_del_bg;
        let row = |sign: &str, text: &str, bg: Color| {
            let content = display_slice(&format!("{sign} {text}"), 0, width, TAB_WIDTH);
            let pad = width.saturating_sub(UnicodeWidthStr::width(content.as_str()));
            Line::styled(
                format!("{content}{}", " ".repeat(pad)),
                Style::default().fg(theme.diff_text).bg(bg),
            )
        };
        for line in app.ai_before_text().lines() {
            lines.push(row("−", line, del_bg));
        }
        for line in replacement.lines() {
            lines.push(row("+", line, add_bg));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.ai_review_scroll, 0)),
        body,
    );

    let actions = if proposal.replacement.is_some() {
        Line::from(vec![
            Span::styled(
                " Accept  Enter ",
                Style::default()
                    .fg(theme.canvas)
                    .bg(theme.mint)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                " Reject  Esc ",
                Style::default().fg(theme.text).bg(theme.elevated2),
            ),
            Span::styled(
                "   ↑↓ scroll · accepting is one undoable edit",
                Style::default().fg(theme.faint),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                " Close  Enter ",
                Style::default().fg(theme.text).bg(theme.elevated2),
            ),
            Span::styled(
                "   ↑↓ scroll · C copy answer",
                Style::default().fg(theme.faint),
            ),
        ])
    };
    frame.render_widget(
        Paragraph::new(vec![Line::raw(""), actions]),
        Rect::new(
            inner.x,
            inner.bottom().saturating_sub(footer_rows),
            inner.width,
            footer_rows,
        ),
    );
    let actions_row = inner.bottom().saturating_sub(footer_rows) + 1;
    let key = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
    if proposal.replacement.is_some() {
        click_target(
            app,
            Rect::new(inner.x, actions_row, 15, 1),
            crate::app::ClickTarget::Key(key(KeyCode::Enter)),
        );
        click_target(
            app,
            Rect::new(inner.x + 18, actions_row, 13, 1),
            crate::app::ClickTarget::Key(key(KeyCode::Esc)),
        );
    } else {
        click_target(
            app,
            Rect::new(inner.x, actions_row, 14, 1),
            crate::app::ClickTarget::Key(key(KeyCode::Enter)),
        );
    }
}

pub fn ai_setup_rect(area: Rect) -> Rect {
    centered_rect(
        area,
        76.min(area.width.saturating_sub(4)),
        20.min(area.height.saturating_sub(2)),
    )
}

fn render_ai_setup(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let spark = if theme.unicode_symbols { "✦" } else { "*" };
    let form = &app.ai_setup;
    let popup = ai_setup_rect(area);
    clear_overlay(frame, app, popup);
    let block = ai_box(app, format!(" {spark} Set up AI "));
    let inner = inset(block.inner(popup), 2, 1);
    frame.render_widget(block, popup);

    let provider = form.provider();
    let label_width = 13usize;
    let value_width = (inner.width as usize).saturating_sub(label_width + 2);
    let row = |index: usize, label: &str, value: Vec<Span<'static>>| {
        let selected = form.field == index;
        let mut spans = vec![
            Span::styled(
                if selected { select_marker(app) } else { "  " },
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{label:<label_width$}"),
                Style::default()
                    .fg(if selected { theme.text } else { theme.muted })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ];
        spans.extend(value);
        Line::from(spans)
    };
    let text_value = |input: &crate::input::TextInput, placeholder: &str, selected: bool| {
        let (view, _) = input.visible_window(value_width);
        if input.is_empty() {
            vec![Span::styled(
                placeholder.to_owned(),
                Style::default().fg(theme.faint),
            )]
        } else {
            vec![Span::styled(
                view,
                Style::default().fg(if selected { theme.text } else { theme.muted }),
            )]
        }
    };

    let key_value = if !form.api_key.is_empty() {
        let typed = form.api_key.as_str();
        let tail: String = typed
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        vec![Span::styled(
            format!(
                "{}{tail}",
                "•".repeat(typed.chars().count().saturating_sub(4).min(16))
            ),
            Style::default().fg(theme.text),
        )]
    } else if form.key_on_file {
        vec![Span::styled(
            "Saved · type to replace",
            Style::default().fg(theme.muted),
        )]
    } else if let Some(name) = form.key_from_environment() {
        vec![Span::styled(
            format!("Using {name} from your environment"),
            Style::default().fg(theme.muted),
        )]
    } else if provider.needs_api_key() {
        vec![Span::styled(
            "Paste your key here",
            Style::default().fg(theme.faint),
        )]
    } else {
        vec![Span::styled("Not needed", Style::default().fg(theme.faint))]
    };

    let mut lines = vec![
        Line::styled(
            "AI is optional. Mellow only sends code when you ask, and always",
            Style::default().fg(theme.muted),
        ),
        Line::styled(
            "shows what it will send first.",
            Style::default().fg(theme.muted),
        ),
        Line::raw(""),
        row(
            0,
            "Provider",
            vec![
                Span::styled("‹ ", Style::default().fg(theme.faint)),
                Span::styled(
                    provider.label(),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ›", Style::default().fg(theme.faint)),
            ],
        ),
        row(
            1,
            "Model",
            text_value(&form.model, "Type the model name", form.field == 1),
        ),
        row(2, "API key", key_value),
        Line::styled(
            format!(
                "{:width$}{}",
                "",
                match crate::ai::endpoint_host(form.endpoint.as_str()) {
                    Some(host) => format!("Your key and code are sent only to {host}."),
                    None => "Your key and code go only to the address below.".to_owned(),
                },
                width = label_width + 2
            ),
            Style::default().fg(theme.faint),
        ),
        row(
            3,
            "Address",
            text_value(&form.endpoint, "https://…", form.field == 3),
        ),
        row(
            4,
            "Suggestions",
            vec![Span::styled(
                if form.inline {
                    "On · grey text while you type, Tab accepts"
                } else {
                    "Off · turn on for grey text while you type"
                },
                Style::default().fg(if form.inline { theme.mint } else { theme.muted }),
            )],
        ),
    ];
    lines.push(Line::styled(
        format!(
            "{:width$}Sends about 60 lines near the cursor after a pause.",
            "",
            width = label_width + 2
        ),
        Style::default().fg(theme.faint),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            " Save  Enter ",
            if form.field == 5 {
                Style::default()
                    .fg(theme.canvas)
                    .bg(theme.mint)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.mint).bg(theme.elevated2)
            },
        ),
        Span::raw("   "),
        Span::styled(
            " Cancel  Esc ",
            Style::default().fg(theme.text).bg(theme.elevated2),
        ),
        Span::raw("   "),
        Span::styled(
            " Remove setup  Ctrl+D ",
            Style::default().fg(theme.error).bg(theme.elevated2),
        ),
    ]));
    lines.push(Line::raw(""));
    if let Some(error) = form.error.as_deref() {
        lines.push(Line::styled(
            error.to_owned(),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        lines.push(Line::styled(
            format!(
                "↑↓ move · ←→ change provider · keys are saved privately in {}",
                crate::ai::config_path()
                    .display()
                    .to_string()
                    .replace(&std::env::var("HOME").unwrap_or_default(), "~")
            ),
            Style::default().fg(theme.faint),
        ));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    let input = match form.field {
        1 => Some(&form.model),
        2 => Some(&form.api_key),
        3 => Some(&form.endpoint),
        _ => None,
    };
    if let Some(input) = input {
        let (_, cursor) = input.visible_window(value_width);
        let shown = if form.field == 2 {
            form.api_key.as_str().chars().count().min(20)
        } else {
            cursor
        };
        frame.set_cursor_position((
            inner.x + 2 + label_width as u16 + shown as u16,
            inner.y + 3 + form.field as u16,
        ));
    }
}

fn render_settings(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(area, 72, 16);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(" Settings ")
        .borders(Borders::ALL)
        .border_set(plain_border(app.theme.unicode_symbols))
        .border_style(Style::default().fg(app.theme.mint))
        .style(Style::default().bg(app.theme.elevated));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let values = [
        ("Theme", app.theme_preset.label().to_owned()),
        (
            "Word wrap",
            if app.word_wrap { "On" } else { "Off" }.to_owned(),
        ),
        (
            "Whitespace",
            if app.show_whitespace {
                "Shown"
            } else {
                "Hidden"
            }
            .to_owned(),
        ),
        (
            "Indent guides",
            if app.show_indent_guides {
                "Shown"
            } else {
                "Hidden"
            }
            .to_owned(),
        ),
        (
            "File explorer default",
            if app.explorer_visible {
                "Visible"
            } else {
                "Hidden"
            }
            .to_owned(),
        ),
        (
            "Automatic completions",
            if app.auto_completion_enabled {
                "On"
            } else {
                "Off"
            }
            .to_owned(),
        ),
        (
            "Copy on select",
            if app.copy_on_select { "On" } else { "Off" }.to_owned(),
        ),
        (
            "Format on save",
            if app.format_on_save { "On" } else { "Off" }.to_owned(),
        ),
        (
            "AI assistant",
            app.ai_config_summary()
                .unwrap_or_else(|| "Not set up · Enter to set up".to_owned()),
        ),
    ];

    let mut lines = vec![
        Line::from(Span::styled(
            "Saved for every project. Open projects keep their own layout.",
            Style::default().fg(app.theme.muted),
        )),
        Line::raw(""),
    ];
    for (index, (label, value)) in values.into_iter().enumerate() {
        let selected = index == app.settings_selected;
        let style = if selected {
            Style::default().fg(app.theme.text).bg(app.theme.elevated2)
        } else {
            Style::default().fg(app.theme.muted)
        };
        lines.push(Line::from(Span::styled(
            format!(
                "{} {:<24} {}",
                if selected {
                    if app.theme.unicode_symbols {
                        "›"
                    } else {
                        ">"
                    }
                } else {
                    " "
                },
                label,
                value
            ),
            style,
        )));
    }
    lines.extend([
        Line::raw(""),
        Line::from(Span::styled(
            "↑/↓ select · Enter/Space change · Esc close",
            Style::default().fg(app.theme.faint),
        )),
        Line::from(Span::styled(
            "Custom keys: edit keybindings.conf, then run Reload keybindings",
            Style::default().fg(app.theme.faint),
        )),
    ]);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Everyday actions taught on first launch and in Help, in reading order.
const ESSENTIALS: &[(Command, &str)] = &[
    (Command::Save, "Save"),
    (Command::OpenFile, "Open a file"),
    (Command::Find, "Find (press twice: all files)"),
    (Command::ShowPalette, "Every command"),
    (Command::ToggleExplorer, "Show or hide files"),
    (Command::ToggleTerminal, "Terminal (again: back)"),
    (Command::AiIntent, "Ask AI"),
    (Command::Undo, "Undo"),
    (Command::CloseTab, "Close file"),
    (Command::Quit, "Quit"),
];

const MORE_KEYS: &[(Command, &str)] = &[
    (Command::Redo, "Redo"),
    (Command::Replace, "Replace"),
    (Command::GoToLine, "Go to line"),
    (Command::NextTab, "Next file"),
    (Command::ToggleComment, "Comment line"),
    (Command::DuplicateLines, "Duplicate line"),
    (Command::SplitEditor, "Split editor"),
    (Command::GoToDefinition, "Go to definition"),
];

/// One row per entry whose shortcut this terminal can send; the key column
/// is sized to the longest visible label so long chords never collide.
fn key_lines(app: &App, entries: &[(Command, &str)]) -> Vec<Line<'static>> {
    let theme = app.theme;
    let labelled: Vec<(String, &str)> = entries
        .iter()
        .filter_map(|(command, text)| Some((app.shortcut_label(*command)?, *text)))
        .collect();
    let key_width = labelled
        .iter()
        .map(|(key, _)| UnicodeWidthStr::width(key.as_str()))
        .max()
        .unwrap_or(0)
        + 3;
    labelled
        .into_iter()
        .map(|(key, text)| {
            Line::from(vec![
                Span::styled(
                    format!("{key:<key_width$}"),
                    Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
                ),
                Span::styled(text.to_owned(), Style::default().fg(theme.text)),
            ])
        })
        .collect()
}

fn render_onboarding(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let essentials = key_lines(app, ESSENTIALS);
    let height = (essentials.len() as u16 + 9).min(area.height.saturating_sub(2));
    let popup = centered_rect(area, 56.min(area.width.saturating_sub(4)), height);
    clear_overlay(frame, app, popup);
    let block = Block::default()
        .title(Span::styled(
            format!(
                " {} Welcome to {} · {} ",
                if theme.unicode_symbols { "✦" } else { "*" },
                crate::brand::PRODUCT,
                crate::brand::CREDIT
            ),
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_set(rounded_border(theme.unicode_symbols))
        .border_style(Style::default().fg(theme.mint))
        .style(Style::default().bg(theme.elevated));
    let inner = inset(block.inner(popup), 2, 1);
    frame.render_widget(block, popup);

    let mut lines = vec![
        Line::from(Span::styled(
            "Just start typing. There are no modes to learn.",
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    lines.extend(essentials);
    lines.extend([
        Line::raw(""),
        Line::from(Span::styled(
            "F1 shows this again. The mouse works too.",
            Style::default().fg(theme.muted),
        )),
        Line::from(Span::styled(
            "Start typing, or press Enter",
            Style::default().fg(theme.mint),
        )),
    ]);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

pub fn help_rect(area: Rect) -> Rect {
    centered_rect(
        area,
        78.min(area.width.saturating_sub(4)),
        20.min(area.height.saturating_sub(2)).max(8),
    )
}

fn render_help(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup = help_rect(area);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                if theme.unicode_symbols {
                    " ✦ Help "
                } else {
                    " * Help "
                },
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let heading = |text: &'static str| {
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(theme.faint)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let mut left = vec![heading("EVERYDAY KEYS")];
    left.extend(key_lines(app, ESSENTIALS));

    let mut right = vec![heading("MORE KEYS")];
    right.extend(key_lines(app, MORE_KEYS));
    right.extend([
        Line::raw(""),
        heading("MOUSE"),
        Line::styled(
            "Click to move · drag to select",
            Style::default().fg(theme.text),
        ),
        Line::styled(
            "Double-click selects a word",
            Style::default().fg(theme.text),
        ),
    ]);

    let footer_rows = 2u16;
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_rows),
    );
    if inner.width >= 64 {
        let columns = Layout::horizontal([Constraint::Percentage(54), Constraint::Percentage(46)])
            .split(body);
        frame.render_widget(Paragraph::new(left), columns[0]);
        frame.render_widget(Paragraph::new(right), columns[1]);
    } else {
        left.push(Line::raw(""));
        left.extend(right);
        frame.render_widget(Paragraph::new(left), body);
    }

    let tip = if crate::terminal::keyboard_enhanced() {
        "Everything else: press Ctrl+P and type what you want to do."
    } else {
        "Everything else: Ctrl+P. Only keys your terminal can send are shown."
    };
    let footer = vec![
        Line::from(Span::styled(tip, Style::default().fg(theme.muted))),
        Line::from(Span::styled(
            "Esc close · Ctrl+P every command",
            Style::default().fg(theme.faint),
        )),
    ];
    frame.render_widget(
        Paragraph::new(footer).wrap(Wrap { trim: false }),
        Rect::new(
            inner.x,
            inner.bottom().saturating_sub(footer_rows),
            inner.width,
            footer_rows,
        ),
    );
}

fn render_close_terminal_confirmation(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup = centered_rect(area, 60, 6);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(plain_border(app.theme.unicode_symbols))
            .border_style(Style::default().fg(theme.warning))
            .title(Span::styled(
                " A job is still running ",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "Closing this terminal stops the program running in it.",
                Style::default().fg(theme.text),
            ),
            Line::raw(""),
            Line::styled(
                "Y close anyway · N / Esc keep it",
                Style::default().fg(theme.faint),
            ),
        ]),
        inset(popup, 2, 1),
    );
}

fn render_close_tab_confirmation(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup = centered_rect(area, 60, 7);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.warning))
            .title(Span::styled(
                " Save changes before closing? ",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let save_style = if app.confirmation_selected == 0 {
        Style::default()
            .fg(theme.canvas)
            .bg(theme.mint)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.mint).bg(theme.elevated2)
    };
    let discard_style = if app.confirmation_selected == 1 {
        Style::default()
            .fg(theme.canvas)
            .bg(theme.error)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.error).bg(theme.elevated2)
    };
    let cancel_style = if app.confirmation_selected == 2 {
        Style::default()
            .fg(theme.canvas)
            .bg(theme.muted)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text).bg(theme.elevated2)
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("{} has unsaved changes.", app.active_display_path()),
            Style::default().fg(theme.text),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" [S] Save & Close ", save_style),
            Span::raw("   "),
            Span::styled(" [D] Discard ", discard_style),
            Span::raw("   "),
            Span::styled(" [Esc] Cancel ", cancel_style),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.text).bg(theme.elevated)),
        inner,
    );
}

pub fn quit_dialog_rect(area: Rect, save_failed: bool) -> Rect {
    centered_rect(area, 60, if save_failed { 11 } else { 7 })
}

fn render_quit_confirmation(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let save_failed = app
        .status
        .as_deref()
        .is_some_and(|s| s.starts_with("Save failed"));
    let popup = quit_dialog_rect(area, save_failed);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.warning))
            .title(Span::styled(
                " Save changes before quitting? ",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let save_style = if app.confirmation_selected == 0 {
        Style::default()
            .fg(theme.canvas)
            .bg(theme.mint)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.mint).bg(theme.elevated2)
    };
    let discard_style = if app.confirmation_selected == 1 {
        Style::default()
            .fg(theme.canvas)
            .bg(theme.error)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.error).bg(theme.elevated2)
    };
    let cancel_style = if app.confirmation_selected == 2 {
        Style::default()
            .fg(theme.canvas)
            .bg(theme.muted)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text).bg(theme.elevated2)
    };
    let mut lines = vec![
        Line::raw(display_slice(
            &match app.unsaved_count() {
                1 => format!("{} has unsaved changes.", app.active_display_path()),
                count => format!(
                    "{count} files have unsaved changes, including {}.",
                    app.active_display_path()
                ),
            },
            0,
            inner.width as usize,
            TAB_WIDTH,
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" [S] Save & Quit ", save_style),
            Span::raw("   "),
            Span::styled(" [D] Discard ", discard_style),
            Span::raw("   "),
            Span::styled(" [Esc] Cancel ", cancel_style),
        ]),
    ];
    lines.push(Line::styled(
        "← → choose · Enter confirm · Esc go back",
        Style::default().fg(theme.faint),
    ));
    if save_failed {
        lines.push(Line::styled(
            "Save failed. Edits kept; Mellow is still open.",
            Style::default().fg(theme.error),
        ));
        let error = app.status.as_deref().unwrap_or("");
        lines.push(Line::raw(display_slice(
            error,
            0,
            inner.width as usize,
            TAB_WIDTH,
        )));
        lines.push(Line::styled(
            "[A] Save As · [S] Retry Save · Esc return to editor",
            Style::default().fg(theme.mint),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.text).bg(theme.elevated)),
        inner,
    );
}

fn render_find(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 46.min(area.width.saturating_sub(4)).max(24);
    let popup_height = 5;
    let popup_x = area.right().saturating_sub(popup_width + 2);
    let popup_y = area.top() + HEADER_ROWS + 1;
    let popup = Rect::new(popup_x, popup_y, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                " Find ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 1, 1);
    let count_text = if app.find_query.is_empty() {
        "".to_string()
    } else if app.find_matches.is_empty() {
        "0 matches".to_string()
    } else {
        format!("{}/{}", app.find_selected + 1, app.find_matches.len())
    };

    let count_len = count_text.len();
    let query_width = (inner.width as usize).saturating_sub(count_len + 3).max(1);

    let (query_view, query_cursor_x) = app.find_query.visible_window(query_width);
    let display_query = if app.find_query.is_empty() {
        "Type to search…".to_string()
    } else {
        query_view
    };

    let line1 = Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<query_width$}", display_query),
            Style::default().fg(if app.find_query.is_empty() {
                theme.faint
            } else {
                theme.text
            }),
        ),
        Span::styled(
            count_text,
            Style::default().fg(
                if app.find_matches.is_empty() && !app.find_query.is_empty() {
                    theme.error
                } else {
                    theme.mint
                },
            ),
        ),
    ]);

    let line2 = Line::from(Span::styled(
        if inner.width >= 42 {
            "↑↓ matches · Ctrl+F again: all files · Esc"
        } else {
            "↑↓ matches · Esc"
        },
        Style::default().fg(theme.faint),
    ));

    frame.render_widget(
        Paragraph::new(vec![line1, line2]).style(Style::default().bg(theme.elevated)),
        inner,
    );
    let cursor_x = inner
        .x
        .saturating_add(2)
        .saturating_add(query_cursor_x as u16)
        .min(inner.right().saturating_sub(1));
    frame.set_cursor_position((cursor_x, inner.y));
}

fn render_replace(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 88.min(area.width.saturating_sub(4)).max(36);
    let popup_height = 20.min(area.height.saturating_sub(2)).max(11);
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                " Replace ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let label_width = 10usize;
    let input_width = (inner.width as usize)
        .saturating_sub(label_width + 2)
        .max(1);
    let (find_view, find_cursor) = app.replace_query.visible_window(input_width);
    let (replace_view, replace_cursor) = app.replace_with.visible_window(input_width);
    let active_find = app.replace_active_field == 0;
    let active_replace = app.replace_active_field == 1;

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                if active_find {
                    "▸ Find    "
                } else {
                    "  Find    "
                },
                Style::default()
                    .fg(if active_find { theme.mint } else { theme.muted })
                    .add_modifier(if active_find {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                if app.replace_query.is_empty() {
                    "Type search text or regex…"
                } else {
                    find_view.as_str()
                },
                Style::default().fg(if app.replace_query.is_empty() {
                    theme.faint
                } else {
                    theme.text
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                if active_replace {
                    "▸ Replace "
                } else {
                    "  Replace "
                },
                Style::default()
                    .fg(if active_replace {
                        theme.mint
                    } else {
                        theme.muted
                    })
                    .add_modifier(if active_replace {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                if app.replace_with.is_empty() {
                    "(replace with empty text)"
                } else {
                    replace_view.as_str()
                },
                Style::default().fg(if app.replace_with.is_empty() {
                    theme.faint
                } else {
                    theme.text
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(
                    "[Alt+C] Case {}  ",
                    if app.replace_options.case_sensitive {
                        "ON"
                    } else {
                        "off"
                    }
                ),
                Style::default().fg(if app.replace_options.case_sensitive {
                    theme.mint
                } else {
                    theme.faint
                }),
            ),
            Span::styled(
                format!(
                    "[Alt+R] Regex {}  ",
                    if app.replace_options.regex {
                        "ON"
                    } else {
                        "off"
                    }
                ),
                Style::default().fg(if app.replace_options.regex {
                    theme.mint
                } else {
                    theme.faint
                }),
            ),
            Span::styled(
                format!(
                    "[Alt+W] Whole word {}",
                    if app.replace_options.whole_word {
                        "ON"
                    } else {
                        "off"
                    }
                ),
                Style::default().fg(if app.replace_options.whole_word {
                    theme.mint
                } else {
                    theme.faint
                }),
            ),
        ]),
        Line::from(Span::styled(
            rule_glyph(app).repeat(inner.width as usize),
            Style::default().fg(theme.elevated2),
        )),
    ];

    let footer_rows = 2usize;
    let available = (inner.height as usize)
        .saturating_sub(lines.len() + footer_rows)
        .max(1);
    let selected = app
        .replace_selected
        .min(app.replace_preview.len().saturating_sub(1));
    let start = if selected >= available {
        selected + 1 - available
    } else {
        0
    };

    if app.replace_query.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Enter a search pattern to preview replacements.",
            Style::default().fg(theme.muted),
        )));
    } else if app.replace_preview.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No replaceable matches.",
            Style::default().fg(theme.muted),
        )));
    } else {
        for (index, item) in app
            .replace_preview
            .iter()
            .enumerate()
            .skip(start)
            .take(available)
        {
            let is_selected = index == selected;
            let background = if is_selected {
                theme.elevated2
            } else {
                theme.elevated
            };
            let marker = if is_selected {
                if theme.unicode_symbols { "▸" } else { ">" }
            } else {
                " "
            };
            let location = format!("{}:{:<3}", item.search.row + 1, item.search.col + 1);
            let before_width = (inner.width as usize).saturating_sub(20) / 2;
            let after_width = (inner.width as usize)
                .saturating_sub(20)
                .saturating_sub(before_width);
            let before = display_slice(
                &item.matched_text.replace('\n', "↵"),
                0,
                before_width,
                TAB_WIDTH,
            );
            let after = display_slice(
                &item.replacement.replace('\n', "↵"),
                0,
                after_width,
                TAB_WIDTH,
            );
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{marker} {location:<9} "),
                    Style::default()
                        .fg(if is_selected { theme.mint } else { theme.faint })
                        .bg(background),
                ),
                Span::styled(
                    format!("{before:<before_width$}"),
                    Style::default()
                        .fg(if is_selected { theme.text } else { theme.muted })
                        .bg(background),
                ),
                Span::styled(" → ", Style::default().fg(theme.mint).bg(background)),
                Span::styled(
                    after,
                    Style::default()
                        .fg(if is_selected { theme.text } else { theme.muted })
                        .bg(background),
                ),
            ]));
        }
    }

    while lines.len() < (inner.height as usize).saturating_sub(footer_rows) {
        lines.push(Line::raw(""));
    }

    let preview_count = if app.replace_preview.len() >= crate::search::MAX_REPLACE_PREVIEW {
        format!("{}+ preview matches", app.replace_preview.len())
    } else {
        format!("{} matches", app.replace_preview.len())
    };
    lines.push(Line::from(Span::styled(
        format!(
            "{preview_count} · Tab field · ↑↓ preview · Enter replace one · Ctrl+Enter Replace All"
        ),
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(Span::styled(
        "Alt+C case · Alt+R regex · Alt+W whole word · Esc close",
        Style::default().fg(theme.faint),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );

    let (cursor_row, cursor_offset) = if active_find {
        (inner.y, find_cursor)
    } else {
        (inner.y.saturating_add(1), replace_cursor)
    };
    frame.set_cursor_position((
        inner
            .x
            .saturating_add(label_width as u16)
            .saturating_add(cursor_offset as u16)
            .min(inner.right().saturating_sub(1)),
        cursor_row,
    ));
}

fn render_project_search(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 104.min(area.width.saturating_sub(4)).max(42);
    let popup_height = 22.min(area.height.saturating_sub(2)).max(11);
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                overlay_title(app, "Search all files"),
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let input_width = inner.width.saturating_sub(2) as usize;
    let (query_view, query_cursor) = app.project_search_query.visible_window(input_width);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "> ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if app.project_search_query.is_empty() {
                    "Search across project files…"
                } else {
                    query_view.as_str()
                },
                Style::default().fg(if app.project_search_query.is_empty() {
                    theme.faint
                } else {
                    theme.text
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(
                    "[Alt+C] Case {}  ",
                    if app.project_search_options.case_sensitive {
                        "ON"
                    } else {
                        "off"
                    }
                ),
                Style::default().fg(if app.project_search_options.case_sensitive {
                    theme.mint
                } else {
                    theme.faint
                }),
            ),
            Span::styled(
                format!(
                    "[Alt+R] Regex {}  ",
                    if app.project_search_options.regex {
                        "ON"
                    } else {
                        "off"
                    }
                ),
                Style::default().fg(if app.project_search_options.regex {
                    theme.mint
                } else {
                    theme.faint
                }),
            ),
            Span::styled(
                format!(
                    "[Alt+W] Whole word {}",
                    if app.project_search_options.whole_word {
                        "ON"
                    } else {
                        "off"
                    }
                ),
                Style::default().fg(if app.project_search_options.whole_word {
                    theme.mint
                } else {
                    theme.faint
                }),
            ),
        ]),
        Line::from(Span::styled(
            rule_glyph(app).repeat(inner.width as usize),
            Style::default().fg(theme.elevated2),
        )),
    ];

    let footer_rows = 2usize;
    let available = (inner.height as usize)
        .saturating_sub(lines.len() + footer_rows)
        .max(1);
    let results = &app.project_search_report.results;
    let selected = app
        .project_search_selected
        .min(results.len().saturating_sub(1));
    let start = if selected >= available {
        selected + 1 - available
    } else {
        0
    };

    if app.project_search_query.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Type a query. Search is bounded to UTF-8 project files.",
            Style::default().fg(theme.muted),
        )));
    } else if results.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No project matches.",
            Style::default().fg(theme.muted),
        )));
    } else {
        for (index, result) in results.iter().enumerate().skip(start).take(available) {
            let is_selected = index == selected;
            let background = if is_selected {
                theme.elevated2
            } else {
                theme.elevated
            };
            let marker = if is_selected {
                if theme.unicode_symbols { "▸" } else { ">" }
            } else {
                " "
            };
            let location = format!("{}:{}", result.row + 1, result.col + 1);
            let path_width = (inner.width as usize).saturating_mul(2) / 5;
            let preview_width = (inner.width as usize)
                .saturating_sub(path_width)
                .saturating_sub(14);
            let path = display_slice(&result.path.to_string_lossy(), 0, path_width, TAB_WIDTH);
            let preview = display_slice(&result.line_preview, 0, preview_width, TAB_WIDTH);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{marker} "),
                    Style::default()
                        .fg(if is_selected { theme.mint } else { theme.faint })
                        .bg(background),
                ),
                Span::styled(
                    format!("{path:<path_width$} "),
                    Style::default()
                        .fg(if is_selected { theme.text } else { theme.muted })
                        .bg(background),
                ),
                Span::styled(
                    format!("{location:<10} "),
                    Style::default().fg(theme.mint).bg(background),
                ),
                Span::styled(
                    preview,
                    Style::default()
                        .fg(if is_selected { theme.text } else { theme.faint })
                        .bg(background),
                ),
            ]));
        }
    }

    while lines.len() < (inner.height as usize).saturating_sub(footer_rows) {
        lines.push(Line::raw(""));
    }

    let report = &app.project_search_report;
    let mut notes = Vec::new();
    if report.unsaved_files > 0 {
        notes.push(format!("{} unsaved", report.unsaved_files));
    }
    let mut skipped = Vec::new();
    if report.skipped_large > 0 {
        skipped.push(format!("{} >2 MiB", report.skipped_large));
    }
    if report.skipped_binary > 0 {
        skipped.push(format!("{} binary", report.skipped_binary));
    }
    if report.skipped_unreadable > 0 {
        skipped.push(format!("{} unreadable", report.skipped_unreadable));
    }
    if !skipped.is_empty() {
        notes.push(format!("skipped {}", skipped.join(", ")));
    }
    if app.project_search_list_truncated {
        notes.push("first 5,000 files only".to_owned());
    }
    if report.truncated {
        notes.push("limit reached".to_owned());
    }
    let incomplete = !skipped.is_empty() || app.project_search_list_truncated || report.truncated;
    lines.push(Line::from(Span::styled(
        format!(
            "{} results · {} files{}",
            results.len(),
            report.files_scanned,
            if notes.is_empty() {
                String::new()
            } else {
                format!(" · {}", notes.join(" · "))
            }
        ),
        Style::default().fg(if incomplete {
            theme.warning
        } else {
            theme.muted
        }),
    )));
    lines.push(Line::from(Span::styled(
        "↑↓ navigate · Enter open exact match · Alt+C/R/W options · Esc close",
        Style::default().fg(theme.faint),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );
    frame.set_cursor_position((
        inner
            .x
            .saturating_add(2)
            .saturating_add(query_cursor as u16)
            .min(inner.right().saturating_sub(1)),
        inner.y,
    ));
}

fn render_goto(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 38.min(area.width.saturating_sub(4)).max(20);
    let popup_height = 5;
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                " Go to line ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 1, 1);
    let max_lines = app.buffer.line_count();
    let input_width = inner.width.saturating_sub(2) as usize;
    let (goto_view, goto_cursor_x) = app.goto_query.visible_window(input_width);
    let display_query = if app.goto_query.is_empty() {
        "line[:col]".to_string()
    } else {
        goto_view
    };

    let line1 = Line::from(vec![
        Span::styled(
            ": ",
            Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            display_query,
            Style::default().fg(if app.goto_query.is_empty() {
                theme.faint
            } else {
                theme.text
            }),
        ),
    ]);

    let line2 = Line::from(Span::styled(
        format!("Enter jump  ·  Esc cancel  ·  (1 - {max_lines})"),
        Style::default().fg(theme.faint),
    ));

    frame.render_widget(
        Paragraph::new(vec![line1, line2]).style(Style::default().bg(theme.elevated)),
        inner,
    );
    let cursor_x = inner
        .x
        .saturating_add(2)
        .saturating_add(goto_cursor_x as u16)
        .min(inner.right().saturating_sub(1));
    frame.set_cursor_position((cursor_x, inner.y));
}

fn render_save_conflict(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup = centered_rect(area, 72, 10);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.warning))
            .title(Span::styled(
                if app.conflict_read_only {
                    " Read-only file "
                } else {
                    " File changed on disk "
                },
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let reason = app
        .conflict_reason
        .as_deref()
        .unwrap_or("The file changed outside Mellow.");
    let lines = vec![
        Line::from(Span::styled(
            if app.conflict_read_only {
                "This file is protected from writing."
            } else {
                "The on-disk version changed after this buffer was opened."
            },
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::styled(reason, Style::default().fg(theme.warning))),
    ];
    // The reason can wrap, so it gets its own area and the buttons stay on a
    // fixed row where clicks are tested.
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .style(Style::default().fg(theme.text).bg(theme.elevated)),
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(3),
        ),
    );
    let key = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
    draw_buttons(
        frame,
        app,
        Rect::new(inner.x, inner.bottom().saturating_sub(3), inner.width, 1),
        &[
            (
                " [S] Save As ",
                Style::default()
                    .fg(theme.canvas)
                    .bg(theme.mint)
                    .add_modifier(Modifier::BOLD),
                key(KeyCode::Char('s')),
            ),
            (
                " [O] Overwrite ",
                Style::default()
                    .fg(theme.canvas)
                    .bg(theme.error)
                    .add_modifier(Modifier::BOLD),
                key(KeyCode::Char('o')),
            ),
            (
                " [Esc] Cancel ",
                Style::default().fg(theme.text).bg(theme.elevated2),
                key(KeyCode::Esc),
            ),
        ],
    );
    frame.buffer_mut().set_stringn(
        inner.x,
        inner.bottom().saturating_sub(1),
        "No file is changed until you choose an action.",
        inner.width as usize,
        Style::default().fg(theme.faint),
    );
}

fn render_recovery(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup = centered_rect(area, 68, 9);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.warning))
            .title(Span::styled(
                " Unsaved work found ",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let source = app
        .recovery_candidate
        .as_ref()
        .and_then(|record| record.original_path.as_ref())
        .map(|path| app.short_path(path))
        .unwrap_or_else(|| "Untitled buffer".to_owned());

    // The message may wrap on narrow terminals; it gets the rows above the
    // buttons so the buttons stay where clicks are tested.
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Mellow found unsaved edits from an earlier interrupted session.",
                Style::default().fg(theme.text),
            )),
            Line::from(Span::styled(source, Style::default().fg(theme.muted))),
        ])
        .wrap(ratatui::widgets::Wrap { trim: true })
        .style(Style::default().fg(theme.text).bg(theme.elevated)),
        Rect::new(inner.x, inner.y, inner.width, 3),
    );
    let (restore, discard, discard_all) = recovery_button_rects(area);
    let show_all = app.pending_draft_count() > 1;
    let buffer = frame.buffer_mut();
    if show_all {
        buffer.set_stringn(
            discard_all.x,
            discard_all.y,
            RECOVERY_DISCARD_ALL_LABEL,
            discard_all.width as usize,
            Style::default()
                .fg(theme.text)
                .bg(theme.elevated2)
                .add_modifier(Modifier::BOLD),
        );
    }
    buffer.set_stringn(
        restore.x,
        restore.y,
        RECOVERY_RESTORE_LABEL,
        restore.width as usize,
        Style::default()
            .fg(theme.canvas)
            .bg(theme.mint)
            .add_modifier(Modifier::BOLD),
    );
    buffer.set_stringn(
        discard.x,
        discard.y,
        RECOVERY_DISCARD_LABEL,
        discard.width as usize,
        Style::default()
            .fg(theme.canvas)
            .bg(theme.error)
            .add_modifier(Modifier::BOLD),
    );
    buffer.set_stringn(
        inner.x,
        inner.y + 5,
        "Restore keeps the buffer dirty until you save it.",
        inner.width as usize,
        Style::default().fg(theme.faint),
    );
}

const RECOVERY_RESTORE_LABEL: &str = " [R] Restore ";
const RECOVERY_DISCARD_LABEL: &str = " [D] Discard journal ";
const RECOVERY_DISCARD_ALL_LABEL: &str = " [A] Discard all ";

/// A button in the "Unsaved work found" dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryButton {
    Restore,
    Discard,
    DiscardAll,
}

/// Where the recovery dialog draws its two buttons. Drawing and mouse
/// hit-testing both use this, so a click always lands on what is shown.
pub fn recovery_button_rects(area: Rect) -> (Rect, Rect, Rect) {
    let popup = centered_rect(area, 68, 9);
    let inner = inset(popup, 2, 1);
    let row = inner.y + 3;
    let restore_width = (RECOVERY_RESTORE_LABEL.len() as u16).min(inner.width);
    let discard_x = inner.x + restore_width + 3;
    let discard_width =
        (RECOVERY_DISCARD_LABEL.len() as u16).min(inner.right().saturating_sub(discard_x));
    let all_x = discard_x + discard_width + 3;
    let all_width =
        (RECOVERY_DISCARD_ALL_LABEL.len() as u16).min(inner.right().saturating_sub(all_x));
    (
        Rect::new(inner.x, row, restore_width, 1),
        Rect::new(discard_x, row, discard_width, 1),
        Rect::new(all_x, row, all_width, 1),
    )
}

pub fn recovery_button_hit(
    area: Rect,
    column: u16,
    row: u16,
    show_all: bool,
) -> Option<RecoveryButton> {
    let (restore, discard, discard_all) = recovery_button_rects(area);
    let inside = |rect: Rect| {
        rect.width > 0 && row == rect.y && column >= rect.x && column < rect.x + rect.width
    };
    if inside(restore) {
        Some(RecoveryButton::Restore)
    } else if inside(discard) {
        Some(RecoveryButton::Discard)
    } else if show_all && inside(discard_all) {
        Some(RecoveryButton::DiscardAll)
    } else {
        None
    }
}

fn render_save_as(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = app.theme;
    let popup_width = 68.min(area.width.saturating_sub(4)).max(28);
    let popup_height = 7;
    let popup = centered_rect(area, popup_width, popup_height);
    clear_overlay(frame, app, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(rounded_border(theme.unicode_symbols))
            .border_style(Style::default().fg(theme.mint))
            .title(Span::styled(
                " Save as ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.elevated)),
        popup,
    );

    let inner = inset(popup, 2, 1);
    let input_width = inner.width.saturating_sub(6) as usize;
    let (path_view, path_cursor_x) = app.save_as_query.visible_window(input_width);
    let display_path = if app.save_as_query.is_empty() {
        "path/to/file".to_owned()
    } else {
        path_view
    };

    let status = app.status.as_deref().unwrap_or("");
    let status_style = if status.starts_with("Save As failed") {
        Style::default().fg(theme.error)
    } else {
        Style::default().fg(theme.muted)
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                "Path: ",
                Style::default().fg(theme.mint).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                display_path,
                Style::default().fg(if app.save_as_query.is_empty() {
                    theme.faint
                } else {
                    theme.text
                }),
            ),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "Enter save  ·  Esc cancel  ·  existing destinations are protected",
            Style::default().fg(theme.faint),
        )),
        Line::from(Span::styled(status, status_style)),
    ];

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.elevated)),
        inner,
    );

    let cursor_x = inner
        .x
        .saturating_add(6)
        .saturating_add(path_cursor_x as u16)
        .min(inner.right().saturating_sub(1));
    frame.set_cursor_position((cursor_x, inner.y));
}

fn centered_rect(area: Rect, preferred_width: u16, preferred_height: u16) -> Rect {
    let width = preferred_width.min(area.width.saturating_sub(2)).max(1);
    let height = preferred_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect::new(
        area.x.saturating_add(horizontal),
        area.y.saturating_add(vertical),
        area.width.saturating_sub(horizontal.saturating_mul(2)),
        area.height.saturating_sub(vertical.saturating_mul(2)),
    )
}

#[allow(dead_code)]
pub fn display_slice(
    input: &str,
    skip_columns: usize,
    max_columns: usize,
    tab_width: usize,
) -> String {
    if max_columns == 0 {
        return String::new();
    }

    let mut result = String::new();
    let mut absolute_col = 0usize;
    let mut emitted = 0usize;

    for grapheme in input.graphemes(true) {
        let (display, width) = if grapheme == "\t" {
            let width = tab_width - (absolute_col % tab_width);
            (" ".repeat(width), width)
        } else {
            let width = UnicodeWidthStr::width(grapheme);
            (grapheme.to_owned(), width)
        };

        let next_col = absolute_col + width;
        if next_col <= skip_columns {
            absolute_col = next_col;
            continue;
        }

        if absolute_col < skip_columns {
            let visible_tail = next_col - skip_columns;
            if emitted + visible_tail > max_columns {
                break;
            }
            result.push_str(&" ".repeat(visible_tail));
            emitted += visible_tail;
            absolute_col = next_col;
            continue;
        }

        if emitted + width > max_columns {
            break;
        }

        result.push_str(&display);
        emitted += width;
        absolute_col = next_col;
    }

    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn design_lock_failed_quit_choices_render_at_supported_sizes() {
        use crate::{
            app::{App, AppMode},
            buffer::Buffer,
        };
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(80, 24), (120, 34), (160, 45)] {
            let mut app = App::new(Buffer::empty(None));
            app.mode = AppMode::ConfirmQuit;
            app.status = Some("Save failed: disposable fixture".to_owned());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| super::render(frame, &app)).unwrap();
            let content: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(content.contains("Save As"));
            assert!(content.contains("Retry Save"));
            assert!(content.contains("Mellow is still open"));
        }
    }

    fn render_rows(app: &crate::app::App, width: u16, height: u16) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| super::render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn ai_review_reads_as_an_agent_result_with_a_change_count() {
        use crate::{ai::AiProposal, app::App, app::AppMode, buffer::Buffer};
        let mut app = App::new(Buffer::empty(None));
        app.mode = AppMode::AiReview;
        app.ai_proposal = Some(AiProposal {
            summary: "Rename the variable".to_owned(),
            replacement: Some("let total = 1;\nlet sum = total;".to_owned()),
        });
        let rows = render_rows(&app, 100, 30);
        let text = rows.join("\n");
        assert!(text.contains("● Rename the variable"), "{text}");
        assert!(text.contains("⎿  +2 added  −0 removed"), "{text}");
        assert!(text.contains("+ let total = 1;"), "{text}");
        assert!(text.contains("Accept"), "{text}");
    }

    #[test]
    fn ascii_mode_draws_no_unicode_in_the_interface() {
        use crate::app::{App, AppMode};
        let mut app = App::new(crate::buffer::Buffer::empty(None));
        app.buffer.insert_text(
            &mut crate::cursor::Cursor::default(),
            "fn main() {\n    x\n}\n",
        );
        app.theme.unicode_symbols = false;
        app.show_indent_guides = true;
        for mode in [
            AppMode::Editing,
            AppMode::Palette,
            AppMode::QuickOpen,
            AppMode::Help,
            AppMode::Settings,
            AppMode::ConfirmQuit,
            AppMode::Find,
            AppMode::GoToLine,
            AppMode::AiPrompt,
        ] {
            app.mode = mode;
            for (row_index, row) in render_rows(&app, 80, 24).iter().enumerate() {
                let odd: String = row.chars().filter(|ch| !ch.is_ascii()).collect();
                assert!(
                    odd.is_empty(),
                    "{mode:?} row {row_index} has non-ASCII {odd:?}: {row}"
                );
            }
        }
    }

    #[test]
    fn recovery_buttons_are_drawn_where_clicks_are_tested() {
        let mut app = crate::app::App::new(crate::buffer::Buffer::empty(None));
        app.mode = crate::app::AppMode::Recovery;
        for (width, height) in [(80u16, 24u16), (120, 34)] {
            let rows = render_rows(&app, width, height);
            let (restore, discard, _) =
                super::recovery_button_rects(ratatui::layout::Rect::new(0, 0, width, height));
            let text_at = |rect: ratatui::layout::Rect| -> String {
                rows[rect.y as usize]
                    .chars()
                    .skip(rect.x as usize)
                    .take(rect.width as usize)
                    .collect()
            };
            assert_eq!(text_at(restore), " [R] Restore ", "{width}x{height}");
            assert_eq!(
                text_at(discard),
                " [D] Discard journal ",
                "{width}x{height}"
            );
        }
    }

    #[test]
    fn go_to_symbol_overlay_fits_at_80_by_24() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let mut app = crate::app::App::new(crate::buffer::Buffer::open(Some(path)).unwrap());
        app.mode = crate::app::AppMode::Symbols;
        let text = render_rows(&app, 80, 24).join("\n");
        assert!(text.contains("● Go to symbol"), "{text}");
        assert!(text.contains("fn       main"), "{text}");
        assert!(text.contains("line 1"), "{text}");
        assert!(text.contains("Enter jump"), "{text}");
    }

    #[test]
    fn list_overlay_titles_open_with_the_agent_marker() {
        use crate::{app::App, app::AppMode, buffer::Buffer};
        let mut app = App::new(Buffer::empty(None));
        app.mode = AppMode::Palette;
        let text = render_rows(&app, 80, 24).join("\n");
        assert!(text.contains("● Commands"), "{text}");
        app.mode = AppMode::QuickOpen;
        let text = render_rows(&app, 80, 24).join("\n");
        assert!(text.contains("● Open file"), "{text}");
    }

    #[test]
    fn ai_surfaces_keep_their_actions_at_80_by_24() {
        use crate::{ai::AiProposal, app::App, app::AppMode, buffer::Buffer};
        let mut app = App::new(Buffer::empty(None));

        app.mode = AppMode::AiPrompt;
        let text = render_rows(&app, 80, 24).join("\n");
        assert!(text.contains("Enter send"), "{text}");
        assert!(text.contains("Esc cancel"), "{text}");

        app.mode = AppMode::AiWaiting;
        let text = render_rows(&app, 80, 24).join("\n");
        assert!(text.contains("Working on it"), "{text}");
        assert!(text.contains("Esc stops waiting"), "{text}");

        app.mode = AppMode::AiReview;
        app.ai_proposal = Some(AiProposal {
            summary: "Tidy the loop".to_owned(),
            replacement: Some("for x in xs {}".to_owned()),
        });
        let text = render_rows(&app, 80, 24).join("\n");
        assert!(text.contains("● Tidy the loop"), "{text}");
        assert!(text.contains("Accept"), "{text}");
        assert!(text.contains("Reject"), "{text}");
    }

    #[test]
    fn working_spinner_has_an_ascii_fallback() {
        for _ in 0..20 {
            let frame = super::working_frame(false);
            assert!(["|", "/", "-", "\\"].contains(&frame), "{frame}");
        }
        assert!(super::working_frame(true).chars().count() == 1);
    }

    #[test]
    fn compact_header_keeps_actions_and_unsaved_state_visible() {
        use crate::{app::App, buffer::Buffer, cursor::Cursor};
        let mut app = App::new(Buffer::empty(Some(std::path::PathBuf::from(
            "a-very-long-file-name-that-must-not-hide-the-dirty-state.py",
        ))));
        app.buffer.insert_text(&mut Cursor::default(), "unsaved");
        let rows = render_rows(&app, 80, 24);
        assert!(
            rows[0].trim_end().ends_with("Open  Save  Quit"),
            "{}",
            rows[0]
        );
        assert!(rows[1].contains("Unsaved"), "{}", rows[1]);
        assert!(rows[23].contains("Unsaved changes"), "{}", rows[23]);
    }

    #[test]
    fn header_clicks_match_rendered_labels() {
        use crate::{app::App, buffer::Buffer, command::Command};
        let app = App::new(Buffer::empty(Some(std::path::PathBuf::from("notes.txt"))));
        for width in [64u16, 80, 120, 160] {
            let row = &render_rows(&app, width, 24)[0];
            for (label, command) in [
                ("Save", Command::Save),
                ("Quit", Command::Quit),
                ("Open", Command::OpenFile),
            ] {
                let column = row
                    .find(label)
                    .unwrap_or_else(|| panic!("{label} in {row}"));
                let column = row[..column].chars().count() as u16;
                assert_eq!(
                    super::header_hit(&app, width, column),
                    Some(super::HeaderHit::Action(command)),
                    "{label} at {width} columns"
                );
            }
            let tab = row.find("notes.txt").unwrap();
            let tab = row[..tab].chars().count() as u16;
            assert_eq!(
                super::header_hit(&app, width, tab),
                Some(super::HeaderHit::Tab(0))
            );
        }
    }

    #[test]
    fn status_line_never_overlaps_and_keeps_help_visible() {
        use crate::{app::App, buffer::Buffer};
        let mut app = App::new(Buffer::empty(Some(std::path::PathBuf::from("a.py"))));
        app.status = Some(
            "Completion unavailable: the Python language helper is not installed on this machine"
                .to_owned(),
        );
        for width in [40u16, 64, 80, 120, 160] {
            let placed = super::status_layout(&app, width);
            let mut end = 0;
            for (x, item) in &placed {
                assert!(*x >= end, "overlap at {width}: {placed:?}");
                end = x + item.width();
            }
            assert!(end <= width, "overflow at {width}: {placed:?}");
            assert!(
                placed.iter().any(|(_, item)| item.text == "F1 Help"),
                "help hidden at {width}"
            );
        }
    }

    use super::{RenderCodeOptions, display_slice};

    #[test]
    fn expands_tabs_at_terminal_tab_stops() {
        assert_eq!(display_slice("a\tb", 0, 10, 4), "a   b");
    }

    #[test]
    fn respects_wide_characters() {
        assert_eq!(display_slice("a漢b", 0, 3, 4), "a漢");
    }

    #[test]
    fn horizontal_scroll_does_not_render_half_a_wide_character() {
        assert_eq!(display_slice("a漢b", 2, 4, 4), " b");
    }

    #[test]
    fn myn_e03_clipping_wide_char_does_not_render_later_tokens() {
        let theme = crate::theme::Theme::detect();
        let spans = super::render_highlighted_slice(
            "a 漢 b",
            "Plain Text",
            &[],
            0,
            2,
            &theme,
            RenderCodeOptions {
                is_current_line: false,
                selection: None,
                show_whitespace: false,
                show_indent_guides: false,
                source_range: None,
            },
        );
        let rendered: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rendered, "a ");
    }

    #[test]
    fn goal_visual_whitespace_and_indent_guides_are_real_rendering() {
        let theme = crate::theme::Theme::detect();
        let whitespace = super::render_highlighted_slice(
            "    value\t= 1",
            "Rust",
            &[],
            0,
            40,
            &theme,
            RenderCodeOptions {
                is_current_line: false,
                selection: None,
                show_whitespace: true,
                show_indent_guides: false,
                source_range: None,
            },
        );
        let whitespace_text: String = whitespace
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(whitespace_text.contains("····value"));
        assert!(whitespace_text.contains('→'));

        let guides = super::render_highlighted_slice(
            "        value",
            "Rust",
            &[],
            0,
            40,
            &theme,
            RenderCodeOptions {
                is_current_line: false,
                selection: None,
                show_whitespace: false,
                show_indent_guides: true,
                source_range: None,
            },
        );
        let guide_text: String = guides.iter().map(|span| span.content.as_ref()).collect();
        assert!(guide_text.starts_with("│   │   value"));
        // Guides are drawn in the faint structure tone, never as text.
        for span in guides.iter().filter(|span| span.content == "│") {
            assert_eq!(span.style.fg, Some(theme.elevated2));
        }
    }

    #[test]
    fn myn_ux18_selection_does_not_paint_trailing_eol_on_single_line() {
        let theme = crate::theme::Theme::detect();
        let spans = super::render_highlighted_slice(
            "abc",
            "Plain Text",
            &[],
            0,
            10,
            &theme,
            RenderCodeOptions {
                is_current_line: false,
                selection: Some((0, 1)),
                show_whitespace: false,
                show_indent_guides: false,
                source_range: None,
            },
        );
        let eol_sel = spans
            .iter()
            .any(|s| s.style.bg == Some(theme.selection) && s.content == " ");
        assert!(!eol_sel, "Found false trailing EOL selection cell");
    }

    #[test]
    fn myn_ux08_palette_renders_without_panic_on_small_and_standard_terminal() {
        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.mode = AppMode::Palette;
        app.palette_selected = 9; // select the 10th item
        terminal.draw(|f| super::render(f, &app)).unwrap();

        // Also test on constrained terminal (e.g. 60x10)
        let backend_small = TestBackend::new(60, 10);
        let mut terminal_small = Terminal::new(backend_small).unwrap();
        terminal_small.draw(|f| super::render(f, &app)).unwrap();
    }

    #[test]
    fn goal_wrap_renders_continuation_rows_without_mutating_source() {
        use crate::app::App;
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(30, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.buffer.insert_text(
            &mut app.cursor,
            "alpha beta gamma delta epsilon zeta eta theta",
        );
        let source = app.buffer.contents();
        app.word_wrap = true;

        terminal.draw(|frame| super::render(frame, &app)).unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains('↪') || rendered.contains("> "));
        assert_eq!(app.buffer.contents(), source);
    }

    #[test]
    fn goal_git_changes_overlay_renders_stage_path_line_and_preview() {
        use std::path::PathBuf;

        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use crate::git::{GitHunk, GitHunkStage};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.git_snapshot.branch = "feature/demo".to_owned();
        app.git_snapshot.hunks.push(GitHunk {
            path: PathBuf::from("src/main.rs"),
            stage: GitHunkStage::Unstaged,
            old_start: 12,
            old_count: 1,
            new_start: 12,
            new_count: 1,
            header: "@@ -12 +12 @@".to_owned(),
            preview: "let updated = true;".to_owned(),
            patch: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -12 +12 @@\n-let updated = false;\n+let updated = true;\n"
                .to_owned(),
            synthetic_file: false,
            untracked: false,
        });
        app.mode = AppMode::Changes;

        terminal.draw(|frame| super::render(frame, &app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Changes · "));
        assert!(content.contains("feature/demo"));
        assert!(content.contains("src/main.rs"));
        assert!(content.contains("line 12"));
        assert!(content.contains("+1"));
        assert!(content.contains("not staged"));
        // The selected change's diff is shown in the dialog.
        assert!(content.contains("− let updated = false;"));
        assert!(content.contains("+ let updated = true;"));
        assert!(content.contains("S stage"));
        assert!(content.contains("R discard"));
    }

    #[test]
    fn language_status_view_explains_the_current_file() {
        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new(Buffer::empty(None));
        app.mode = AppMode::LanguageStatus;
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|frame| super::render(frame, &app)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Language server · Plain Text"));
        assert!(content.contains("No language server for this file type"));
        assert!(content.contains("R restart server"));
    }

    /// Audit: long hunks were cropped with no way to see the rest.
    #[test]
    fn long_git_diffs_scroll_and_say_how_much_is_hidden() {
        use std::path::PathBuf;

        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use crate::git::{GitHunk, GitHunkStage};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let body: String = (1..=60).map(|n| format!("+added line {n}\n")).collect();
        let mut app = App::new(Buffer::empty(None));
        app.git_snapshot.hunks.push(GitHunk {
            path: PathBuf::from("src/long.rs"),
            stage: GitHunkStage::Unstaged,
            old_start: 1,
            old_count: 0,
            new_start: 1,
            new_count: 60,
            header: "@@ -0,0 +1,60 @@".to_owned(),
            preview: "added line 1".to_owned(),
            patch: format!("--- a/src/long.rs\n+++ b/src/long.rs\n@@ -0,0 +1,60 @@\n{body}"),
            synthetic_file: false,
            untracked: false,
        });
        app.mode = AppMode::Changes;
        let screen = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
            terminal.draw(|frame| super::render(frame, app)).unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };

        let top = screen(&app);
        assert!(top.contains("+ added line 1 "));
        assert!(top.contains("more · Shift+↑↓ scroll diff"));
        assert!(!top.contains("added line 60"));

        app.changes_diff_scroll = 59;
        let bottom = screen(&app);
        assert!(bottom.contains("+ added line 60"));
        assert!(!bottom.contains("+ added line 1 "));
    }

    #[test]
    fn goal_replace_overlay_renders_preview_options_and_replace_all_action() {
        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use crate::search::{SearchOptions, replacement_matches};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.buffer
            .insert_text(&mut app.cursor, "alpha alpha\nalpha");
        app.replace_query.set("alpha");
        app.replace_with.set("beta");
        app.replace_options = SearchOptions {
            case_sensitive: true,
            regex: false,
            whole_word: true,
        };
        app.replace_preview = replacement_matches(
            &app.buffer.contents(),
            "alpha",
            "beta",
            app.replace_options,
            100,
        )
        .unwrap();
        app.mode = AppMode::Replace;

        terminal.draw(|frame| super::render(frame, &app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Replace"));
        assert!(content.contains("alpha"));
        assert!(content.contains("beta"));
        assert!(content.contains("Case ON"));
        assert!(content.contains("Whole word ON"));
        assert!(content.contains("Ctrl+Enter Replace All"));
    }

    #[test]
    fn goal_project_search_overlay_renders_exact_result_context_and_bounds() {
        use std::path::PathBuf;

        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use crate::search::{ProjectSearchReport, ProjectSearchResult};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(110, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.project_search_query.set("needle");
        app.project_search_report = ProjectSearchReport {
            results: vec![ProjectSearchResult {
                path: PathBuf::from("src/main.rs"),
                row: 14,
                col: 8,
                end_row: 14,
                end_col: 14,
                line_preview: "let needle = value;".to_owned(),
                matched_text: "needle".to_owned(),
            }],
            files_scanned: 42,
            bytes_scanned: 12 * 1024,
            truncated: true,
            unsaved_files: 1,
            skipped_large: 2,
            skipped_binary: 0,
            skipped_unreadable: 0,
        };
        app.mode = AppMode::ProjectSearch;

        terminal.draw(|frame| super::render(frame, &app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Search all files"));
        assert!(content.contains("src/main.rs"));
        assert!(content.contains("15:9"));
        assert!(content.contains("let needle = value;"));
        assert!(content.contains("42 files"));
        assert!(content.contains("1 unsaved"));
        assert!(content.contains("skipped 2 >2 MiB"));
        assert!(content.contains("limit reached"));
    }

    #[test]
    fn goal_code_intel_completion_overlay_renders_real_items() {
        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use crate::lsp::CompletionItem;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.completion_items = vec![CompletionItem {
            label: "println!".to_owned(),
            detail: Some("macro".to_owned()),
            filter_text: None,
            sort_text: None,
            insert_text: "println!".to_owned(),
            edit_range: None,
        }];
        app.mode = AppMode::Completion;

        terminal.draw(|frame| super::render(frame, &app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Suggestions"));
        assert!(content.contains("println!"));
        assert!(content.contains("macro"));
    }

    #[test]
    fn goal_code_intel_problems_overlay_renders_tree_sitter_diagnostic() {
        use std::path::PathBuf;

        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use crate::cursor::Cursor;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut buffer = Buffer::empty(Some(PathBuf::from("broken.rs")));
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "fn main( {");

        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(buffer);
        app.mode = AppMode::Problems;

        terminal.draw(|frame| super::render(frame, &app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Problems · "));
        assert!(content.contains("Tree-sitter"));
    }

    #[test]
    fn goal_terminal_pane_renders_focus_and_shell_surface() {
        use crate::app::App;
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.terminal_visible = true;
        app.terminal_focused = true;

        terminal.draw(|f| super::render(f, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("Terminal · "));
        assert!(content.contains("Ctrl+T back to editor"));
        assert!(content.contains("Starting your shell"));
    }

    #[test]
    fn goal_workspace_tabs_render_open_files_and_dirty_marker() {
        use crate::app::App;
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let one = dir.path().join("one.txt");
        let two = dir.path().join("two.txt");
        std::fs::write(&one, "one").unwrap();
        std::fs::write(&two, "two").unwrap();

        let mut app = App::new(Buffer::open(Some(one)).unwrap());
        app.open_path_in_tab(two).unwrap();
        app.buffer.insert_char(&mut app.cursor, '!');

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::render(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("one.txt"));
        assert!(content.contains("two.txt"));
    }

    #[test]
    fn goal_safety_conflict_overlay_explains_safe_choices() {
        use crate::app::{App, AppMode};
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Buffer::empty(None));
        app.mode = AppMode::SaveConflict;
        app.conflict_reason = Some("file content changed".to_owned());
        terminal.draw(|f| super::render(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("File changed on disk"));
        assert!(content.contains("Save As"));
        assert!(content.contains("Overwrite"));
    }

    #[test]
    fn goal_file_lifecycle_named_missing_path_shows_new_indicator() {
        use crate::app::App;
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use std::path::PathBuf;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(Buffer::empty(Some(PathBuf::from("new-file.txt"))));
        terminal.draw(|f| super::render(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("new"));
        assert!(!content.contains("saved"));
    }

    #[test]
    fn myn_ux15_untitled_buffer_shows_new_indicator() {
        use crate::app::App;
        use crate::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(Buffer::empty(None));
        terminal.draw(|f| super::render(f, &app)).unwrap();

        // The rendered buffer should contain "new" rather than "saved"
        let buffer = terminal.backend().buffer();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("new"));
        assert!(!content.contains("saved"));
    }
}
