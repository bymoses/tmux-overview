mod actions;
mod agents;
mod app;
mod model;
mod layouts;
mod json;
mod subscriptions;
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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--server") {
        if i + 1 >= args.len() { return usage(); }
        tmux_api::set_server(args.remove(i + 1));
        args.remove(i);
    }
    let result = match args.first().map(String::as_str) {
        None => return app::run(),
        Some("save-all") if args.len() == 1 => layouts::save_all().map(|l| format!("saved {} panes → {}", l.panes, l.path)),
        Some("restore") => {
            let fork = args.iter().any(|a| a == "--fork");
            args.retain(|a| a != "--fork");
            if args.len() > 2 { return usage(); }
            layouts::resolve_layout(args.get(1).map(String::as_str)).and_then(|l| layouts::restore(&l.sessions, fork)).map(|r| r.message())
        }
        _ => return usage(),
    };
    match result {
        Ok(message) => { println!("{}", message); Ok(()) }
        Err(message) => { eprintln!("{}", message); std::process::exit(1); }
    }
}

fn usage() -> std::io::Result<()> {
    eprintln!("usage: tmux-overview [--server <socket>] [save-all | restore [snapshot] [--fork]]");
    std::process::exit(2);
}
