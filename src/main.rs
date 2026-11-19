//! LFM Assetto Corsa mod sync — Ratatui front-end.
//!
//! Reimplements the AC mod-sync subsystem of the LFM App as an interactive terminal UI.

mod ac;
mod api;
mod app;
mod auth;
mod config;
mod model;
mod session;
mod ui;
mod worker;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as TerminalEvent, KeyEventKind,
};
use ratatui::crossterm::execute;
use tokio::sync::mpsc;

use crate::app::App;
use crate::worker::{Command, Event};

#[derive(Parser, Debug)]
#[command(
    name = "lfm-ac",
    version,
    about = "LFM Assetto Corsa mod sync (TUI)",
    long_about = "Interactive sync of the Assetto Corsa mods required by the current LFM \
                  season. Reuses the LFM App's login and install path when available."
)]
struct Args {
    /// API token. Defaults to $LFM_API_KEY, then this tool's saved login, then the LFM App's
    /// config.json.
    #[arg(long, env = "LFM_API_KEY")]
    api_key: Option<String>,

    /// Assetto Corsa install folder. Defaults to the LFM App setting, then Steam detection.
    #[arg(long, env = "LFM_AC_PATH")]
    ac_path: Option<PathBuf>,

    /// Install these content ids, print the log and exit (no TUI).
    /// Example: --install 56,272
    #[arg(long = "install", value_delimiter = ',')]
    install: Vec<u32>,

    /// Log in through the browser (Steam), save the API key and exit.
    #[arg(long)]
    login: bool,

    /// Internal: receives the `liverytool://` login redirect.
    #[arg(long = "auth-callback", hide = true)]
    auth_callback: Option<String>,

    /// Internal: where `--auth-callback` writes the token.
    #[arg(long = "token-file", hide = true)]
    token_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Invoked by the browser through the temporary `liverytool://` handler.
    if let Some(url) = &args.auth_callback {
        let Some(token) = auth::token_from_url(url) else {
            eprintln!("the login redirect carried no token");
            std::process::exit(1);
        };
        if let Some(path) = &args.token_file {
            std::fs::write(path, token)
                .map_err(|error| anyhow::anyhow!("writing {}: {error}", path.display()))?;
        }
        return Ok(());
    }

    // An interrupted login (Ctrl+C) leaves its temporary handler behind.
    auth::cleanup_stale_handler();

    let cfg = config::load_app_config();

    let token = args
        .api_key
        .clone()
        .or_else(auth::load_token)
        .or_else(|| cfg.api_key.clone())
        .unwrap_or_default();

    let ac_path = args
        .ac_path
        .clone()
        .or_else(|| cfg.ac_path.clone())
        .filter(|path| config::is_likely_ac_path(path))
        .or_else(config::detect_ac_path);

    let api = api::ApiClient::new(token.clone())?;

    if args.login {
        return run_headless(api, ac_path, Command::Login);
    }

    if !args.install.is_empty() {
        return run_headless(api, ac_path, Command::Sync(args.install));
    }

    let (command_tx, command_rx) = mpsc::unbounded_channel::<Command>();
    let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();

    let worker = worker::Worker::new(api, ac_path.clone(), event_tx);
    std::thread::Builder::new()
        .name("lfm-worker".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("failed to start the async runtime: {error}");
                    return;
                }
            };
            runtime.block_on(worker.run(command_rx));
        })?;

    let mut app = App::new(
        command_tx,
        ac_path,
        !token.is_empty(),
        cfg.selected_content_ids.iter().copied().collect(),
    );

    if let Some(source) = &cfg.source {
        app.log(format!("Settings: {}", source.display()));
    }

    if !token.is_empty() {
        let _ = app.tx.send(Command::Refresh);
    } else {
        app.log("No API token found.");
        app.log("Press L to log in through your browser — no LFM App install needed.");
        app.log("Otherwise pass --api-key or set LFM_API_KEY.");
    }

    if app.ac_path.is_none() {
        app.log("Assetto Corsa not detected — pass --ac-path");
    }

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = run(&mut terminal, &mut app, event_rx);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Run one command without the TUI (used by `--install` and `--login`).
fn run_headless(
    api: api::ApiClient,
    ac_path: Option<PathBuf>,
    command: Command,
) -> Result<()> {
    let (command_tx, command_rx) = mpsc::unbounded_channel::<Command>();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let worker = worker::Worker::new(api, ac_path, event_tx);

    let handle = std::thread::Builder::new()
        .name("lfm-worker".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(worker.run(command_rx));
            Ok::<(), anyhow::Error>(())
        })?;

    command_tx.send(command)?;

    // Work runs in a spawned task, and closing the command channel would drop the task
    // mid-download, so wait for this command to report `Busy(false)` before shutting down.
    let mut started = false;
    while let Some(event) = event_rx.blocking_recv() {
        match event {
            Event::Busy(true) => started = true,
            Event::Busy(false) if started => break,
            Event::Log(line) => println!("{line}"),
            Event::Status(id, status) if status == crate::model::Status::Missing => {
                println!("· {id}: missing")
            }
            Event::Checked {
                id,
                status,
                present,
                total: expected,
            } => println!("· {id}: {status:?} {present}/{expected}"),
            _ => {}
        }
    }
    drop(command_tx);

    handle.join().map_err(|_| anyhow::anyhow!("worker panicked"))??;
    println!("done");
    Ok(())
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut events: mpsc::UnboundedReceiver<Event>,
) -> Result<()> {
    loop {
        while let Ok(event) = events.try_recv() {
            app.on_event(event);
        }

        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                TerminalEvent::Mouse(mouse) => app.on_mouse(mouse),
                _ => {}
            }
        }

        app.tick();

        if app.should_quit {
            return Ok(());
        }
    }
}
