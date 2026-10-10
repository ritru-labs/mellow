mod ai;
mod app;
mod brand;
mod buffer;
mod claude;
mod command;
mod conflict;
mod cursor;
mod format;
mod git;
mod input;
mod keymap;
mod lsp;
mod pty;
mod recovery;
mod search;
mod session;
mod settings;
mod syntax;
mod syntax_tree;
mod terminal;
mod theme;
mod ui;
mod visual;
mod workspace;

use std::path::PathBuf;

use anyhow::Result;
use app::App;
use clap::Parser;
use terminal::TerminalSession;

#[derive(Debug, Parser)]
#[command(
    name = "mellow",
    version,
    about = brand::ABOUT,
    after_help = brand::AFTER_HELP
)]
struct Cli {
    /// File or project directory to open. A missing file path creates a new buffer.
    path: Option<PathBuf>,

    /// Disable terminal mouse capture (useful when you prefer terminal-native selection).
    #[arg(long)]
    no_mouse: bool,
}

fn main() -> Result<()> {
    terminal::install_panic_hook();
    terminal::install_signal_handlers()?;
    let cli = Cli::parse();
    let mut app = App::startup(cli.path)?;
    let mut terminal = TerminalSession::new(!cli.no_mouse)?;
    app.run(terminal.terminal_mut())
}
