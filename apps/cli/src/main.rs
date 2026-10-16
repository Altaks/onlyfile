use crate::app::App;

mod app;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ratatui::run(|terminal| App::default().run(terminal))?;

    Ok(())
}
