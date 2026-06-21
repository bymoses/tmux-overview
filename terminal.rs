use std::env;
use std::io::{self, Read, Write};
use std::process::Command;

use crate::tmux_api::tmux;

pub(crate) struct TerminalGuard {
    stty: String,
}

impl TerminalGuard {
    pub(crate) fn enter() -> io::Result<Self> {
        let stty = String::from_utf8_lossy(&Command::new("stty").arg("-g").output()?.stdout)
            .trim()
            .to_string();
        // Nonblocking-ish raw input: reads time out after 0.1s so stats can refresh
        // while the overview is visible even when no key is pressed.
        let _ = Command::new("stty")
            .args(["raw", "-echo", "min", "0", "time", "1"])
            .status();
        print!("\x1b[?1049h\x1b[?25l\x1b[?7l");
        io::stdout().flush()?;
        Ok(Self { stty })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        print!("\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();
        if !self.stty.is_empty() {
            let _ = Command::new("stty").arg(&self.stty).status();
        }
    }
}
pub(crate) fn parse_size_pair(s: &str) -> Option<(usize, usize)> {
    let mut it = s.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

pub(crate) fn clamp_term_size((rows, cols): (usize, usize)) -> (usize, usize) {
    (rows.max(10), cols.max(40))
}

pub(crate) fn term_size() -> (usize, usize) {
    if let (Ok(h), Ok(w)) = (env::var("TMUX_OVERVIEW_HEIGHT"), env::var("TMUX_OVERVIEW_WIDTH")) {
        if let (Ok(r), Ok(c)) = (h.parse::<usize>(), w.parse::<usize>()) {
            return clamp_term_size((r, c));
        }
    }

    if let Ok(s) = tmux(&["display-message", "-p", "#{popup_height} #{popup_width}"]) {
        if let Some(size) = parse_size_pair(&s) {
            return clamp_term_size(size);
        }
    }

    if let Ok(out) = Command::new("stty").arg("size").output() {
        if let Some(size) = parse_size_pair(&String::from_utf8_lossy(&out.stdout)) {
            return clamp_term_size(size);
        }
    }

    if let Ok(s) = tmux(&["display-message", "-p", "#{client_height} #{client_width}"]) {
        if let Some(size) = parse_size_pair(&s) {
            return clamp_term_size(size);
        }
    }

    (30, 100)
}
pub(crate) fn read_key_timeout() -> io::Result<Option<Vec<u8>>> {
    let mut stdin = io::stdin();
    let mut b = [0u8; 1];
    if stdin.read(&mut b)? == 0 {
        return Ok(None);
    }

    let mut key = vec![b[0]];
    if b[0] == 0x1b {
        // Pull the rest of common escape sequences if already available. With
        // stty time=1 this waits briefly, then returns to the refresh loop.
        let mut next = [0u8; 1];
        while key.len() < 8 {
            if stdin.read(&mut next)? == 0 {
                break;
            }
            key.push(next[0]);
        }
    }
    Ok(Some(key))
}
