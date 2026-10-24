//! Terminal front end for polu: pick input files, inspect the resolved costs, solve and save.
//!
//! `polu-tui [scenario.polu.json | folder]` opens a scenario (and loads it) or starts the file
//! search in a folder; with no argument it starts in the current folder.

#[cfg(not(any(feature = "csv", feature = "json", feature = "xlsx")))]
compile_error!("polu-tui needs at least one of the `csv`, `json` or `xlsx` features");

mod app;
mod io;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyEventKind};

use app::App;

fn main() -> std::io::Result<()> {
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    let mut app = App::new(std::env::current_dir()?);
    match arg {
        Some(path) if path.is_dir() => app.root = path,
        Some(path) => {
            app.open_scenario(&path);
            if app.scenario_path.is_some() {
                app.load();
            }
        }
        None => {}
    }

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    while !app.quit {
        app.poll();
        terminal.draw(|frame| ui::draw(frame, app))?;
        // wake up regularly so a running solve's clock and result show without a key press
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle(key);
        }
    }
    Ok(())
}
