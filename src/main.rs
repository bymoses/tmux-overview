mod actions;
mod app;
mod model;
mod preview;
mod render;
mod rows;
mod saved;
mod state;
mod stats;
mod terminal;
mod tmux_api;
mod util;

fn main() -> std::io::Result<()> {
    app::run()
}
