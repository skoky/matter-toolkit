mod app;
mod matter;
mod network;
mod setup_code;
mod state;
mod types;
mod ui;
mod utils;

use anyhow::Result;
use app::App;
use env_logger;
use app::STATE_PATH;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::{
    io::{self, Stdout},
    time::Duration,
};
use types::{Modal, PendingTask};
use ui::draw;

#[tokio::main]
async fn main() -> Result<()> {
    let log_file = std::fs::File::create("client-debug.log")?;
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("matc=debug"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();

    let state = state::AppState::load(STATE_PATH)?;
    matter::ensure_credentials(&state)?;

    let mut app = App::new(state);
    let mut terminal = setup_terminal()?;
    app.queue_task(PendingTask::RefreshScan, "Scanning LAN for Matter devices...");

    let result = run_app(&mut terminal, &mut app).await;
    restore_terminal(&mut terminal)?;
    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    while !app.quit {
        terminal.draw(|frame| draw(frame, app))?;

        if app.pending_task.is_some() {
            if let Err(err) = app.run_pending_task().await {
                app.modal = Some(Modal::Message(format!("{err:#}")));
                app.status = format!("{err:#}");
                app.status_kind = types::StatusKind::Error;
            }
            continue;
        }

        app.check_auto_refresh();

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if let Err(err) = app.on_key(key).await {
                    app.modal = Some(Modal::Message(format!("{err:#}")));
                    app.status = format!("{err:#}");
                    app.status_kind = types::StatusKind::Error;
                }
            }
        }
    }
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
