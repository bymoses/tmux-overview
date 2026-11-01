use std::io;
use std::process::Command;
use std::sync::OnceLock;

static SERVER: OnceLock<String> = OnceLock::new();

pub(crate) fn set_server(server: String) { let _ = SERVER.set(server); }

fn command() -> Command {
    let mut cmd = Command::new("tmux");
    if let Some(server) = SERVER.get() { cmd.args([if server.starts_with('/') { "-S" } else { "-L" }, server]); }
    cmd
}

pub(crate) fn tmux(args: &[&str]) -> io::Result<String> {
    let output = command().args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Identifies the running tmux server; window ids (`@N`) are only unique within it.
pub(crate) fn server_start_time() -> String {
    tmux(&["display-message", "-p", "#{start_time}"])
        .map(|out| out.trim().to_string())
        .unwrap_or_default()
}

pub(crate) fn tmux_status_ok(args: &[&str]) -> bool {
    command()
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(crate) fn tmux_status_ignore(args: &[&str]) {
    let _ = command().args(args).status();
}
