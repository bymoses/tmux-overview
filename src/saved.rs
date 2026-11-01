use std::fs;
use std::path::PathBuf;

use crate::layouts;
use crate::model::{SavedSession, Session};
use crate::tmux_api::tmux_status_ignore;
use crate::util::{hex_encode, hex_decode};

pub(crate) fn saved_sessions_dir() -> Option<PathBuf> {
    Some(layouts::state_dir()?.join("sessions"))
}

pub(crate) fn saved_session_path(name: &str) -> Option<PathBuf> {
    Some(saved_sessions_dir()?.join(format!("{}.tsv", hex_encode(name))))
}

pub(crate) fn load_saved_sessions() -> Vec<SavedSession> {
    let Some(dir) = saved_sessions_dir() else { return Vec::new() };
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("tsv") { continue; }
        if let Ok(layout) = layouts::read_layout(&path) { sessions.extend(layout.sessions); }
    }
    sessions.sort_by(|a,b| a.name.cmp(&b.name));
    sessions
}

pub(crate) fn save_session_snapshot(session: &Session) -> Result<(), String> {
    let path = saved_session_path(&session.name).ok_or("no state directory")?;
    let sessions = layouts::capture(Some(&session.id))?;
    layouts::atomic_write(&path, &layouts::encode(&sessions))
}

pub(crate) fn delete_saved_session(name: &str) -> Result<(), String> {
    let path = saved_session_path(name).ok_or("no state directory")?;
    fs::remove_file(path).map_err(|e| e.to_string())
}

pub(crate) fn rename_saved_session(old_name: &str, new_name: &str) -> Result<(), String> {
    if new_name.trim().is_empty() { return Err("empty session name".into()); }
    if old_name == new_name { return Ok(()); }
    let old_path = saved_session_path(old_name).ok_or("no state directory")?;
    let new_path = saved_session_path(new_name).ok_or("no state directory")?;
    let contents = fs::read_to_string(&old_path).map_err(|e| e.to_string())?;
    let out: String = contents.lines().map(|line| {
        if let Some(name) = line.strip_prefix("session\t") {
            if hex_decode(name) == old_name { return format!("session\t{}\n", hex_encode(new_name)); }
        }
        format!("{}\n", line)
    }).collect();
    layouts::atomic_write(&new_path, &out)?;
    fs::remove_file(old_path).map_err(|e| e.to_string())
}

pub(crate) fn restore_saved_session(saved: &SavedSession) -> Result<(), String> {
    layouts::restore(std::slice::from_ref(saved), false)?;
    tmux_status_ignore(&["switch-client", "-t", &format!("={}", saved.name)]);
    Ok(())
}
