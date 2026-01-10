mod analyzer;
mod app;
mod collector;
mod gpu;
mod logger;
mod models;
mod ui;

use app::App;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::fs::OpenOptions;
use std::{io, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Logging must not write to stdout/stderr while TUI is active (it corrupts the screen).
    // Write logs to a file next to the executable.
    let log_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("process_monitor.log")))
        .unwrap_or_else(|| "process_monitor.log".into());

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to open log file {:?}: {}", log_path, e))?;

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .format_timestamp_secs()
        .init();

    // Setup Terminal
    enable_raw_mode().map_err(|e| format!("Failed to enable raw mode: {}", e))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| format!("Failed to setup terminal: {}", e))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| format!("Failed to create terminal: {}", e))?;

    // Create App State
    let mut app = App::new();

    // Main Loop
    let res = run_app(&mut terminal, &mut app);

    // Restore Terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Application error: {:?}", err);
        eprintln!("Check process_monitor.log for more details.");
        return Err(err.into());
    }

    Ok(())
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> io::Result<()>
where
    std::io::Error: From<B::Error>, // Ensure backend errors can convert to io::Error
{
    let tick_rate = Duration::from_millis(1000); // 1s update balances smoothness and CPU usage
    let mut last_tick = std::time::Instant::now();

    loop {
        terminal.draw(|f| ui::ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    // On Windows terminals we can receive key release events; ignore those.
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                        app.on_key(key);
                    }
                }
                Event::Resize(_, _) => {
                    // Re-draw happens each loop anyway.
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = std::time::Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
