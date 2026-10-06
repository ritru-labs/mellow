use std::{
    io::{self, Stdout},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::Show,
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;

static TERMINATION_REQUESTED: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

/// Whether the terminal accepted the kitty keyboard protocol. When it did,
/// chords such as Ctrl+Shift+F, Ctrl+Tab and Ctrl+` arrive as themselves;
/// otherwise they collapse into plainer keys and the UI advertises fallbacks.
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

pub fn keyboard_enhanced() -> bool {
    KEYBOARD_ENHANCED.load(Ordering::Relaxed)
}

fn keyboard_protocol_allowed() -> bool {
    !matches!(
        crate::brand::env_var("KEYBOARD_PROTOCOL").as_deref(),
        Ok("off" | "0" | "legacy")
    )
}

pub type MellowTerminal = Terminal<CrosstermBackend<Stdout>>;

pub struct TerminalSession {
    terminal: MellowTerminal,
    mouse_enabled: bool,
}

impl TerminalSession {
    pub fn new(mouse_enabled: bool) -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;

        let result = (|| -> Result<Self> {
            let mut stdout = io::stdout();
            execute!(
                stdout,
                EnterAlternateScreen,
                EnableBracketedPaste,
                EnableFocusChange
            )
            .context("failed to initialize terminal modes")?;
            if mouse_enabled {
                execute!(stdout, EnableMouseCapture).context("failed to enable mouse capture")?;
            }
            if keyboard_protocol_allowed() && matches!(supports_keyboard_enhancement(), Ok(true)) {
                execute!(
                    stdout,
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )
                .context("failed to enable enhanced keyboard reporting")?;
                KEYBOARD_ENHANCED.store(true, Ordering::Relaxed);
            }

            let backend = CrosstermBackend::new(stdout);
            let mut terminal =
                Terminal::new(backend).context("failed to initialize terminal renderer")?;
            terminal.clear()?;

            Ok(Self {
                terminal,
                mouse_enabled,
            })
        })();

        if result.is_err() {
            best_effort_restore();
        }
        result
    }

    pub fn terminal_mut(&mut self) -> &mut MellowTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        if KEYBOARD_ENHANCED.swap(false, Ordering::Relaxed) {
            let _ = execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags);
        }
        if self.mouse_enabled {
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        }
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            DisableFocusChange,
            LeaveAlternateScreen,
            Show
        );
        let _ = disable_raw_mode();
    }
}

pub fn install_signal_handlers() -> Result<()> {
    let flag_state = TERMINATION_REQUESTED
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone();

    for signal in [SIGTERM, SIGHUP, SIGINT] {
        flag::register(signal, flag_state.clone())
            .with_context(|| format!("failed to install signal handler for {signal}"))?;
    }
    Ok(())
}

pub fn termination_requested() -> bool {
    TERMINATION_REQUESTED
        .get()
        .map(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(false)
}

pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        best_effort_restore();
        previous(panic_info);
    }));
}

fn best_effort_restore() {
    let mut stdout = io::stdout();
    if KEYBOARD_ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        stdout,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        LeaveAlternateScreen,
        Show
    );
    let _ = disable_raw_mode();
}
