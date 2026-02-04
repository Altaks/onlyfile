use ratatui::{DefaultTerminal, Frame, crossterm};
use tracing::info;

use crate::app::App;

mod app;
mod logging;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = std::env::current_dir()?;

    ratatui::run(|terminal| App::default().run(terminal))?;

    Ok(())
}
