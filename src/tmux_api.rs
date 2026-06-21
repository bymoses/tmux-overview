use std::io;
use std::process::Command;

pub(crate) fn tmux(args: &[&str]) -> io::Result<String> {
    let output = Command::new("tmux").args(args).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(crate) fn tmux_status_ok(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(crate) fn tmux_status_ignore(args: &[&str]) {
    let _ = Command::new("tmux").args(args).status();
}
