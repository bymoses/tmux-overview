use std::env;
use std::fs;
use std::path::PathBuf;

use crate::model::{SavedPane, SavedSession, SavedWindow, Session};
use crate::tmux_api::{tmux, tmux_status_ignore, tmux_status_ok};
use crate::util::{format_saved_time, hex_decode, hex_encode};

pub(crate) fn saved_sessions_dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(base.join("tmux-overview").join("sessions"))
}

pub(crate) fn saved_session_path(name: &str) -> Option<PathBuf> {
    Some(saved_sessions_dir()?.join(format!("{}.tsv", hex_encode(name))))
}

pub(crate) fn load_saved_sessions() -> Vec<SavedSession> {
    let Some(dir) = saved_sessions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("tsv") {
            continue;
        }
        let saved_label = fs::metadata(&path)
            .and_then(|m| m.modified())
            .map(format_saved_time)
            .unwrap_or_default();
        let Ok(contents) = fs::read_to_string(path) else { continue };
        let mut session: Option<SavedSession> = None;
        let mut current_window: Option<SavedWindow> = None;

        for line in contents.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            match parts.as_slice() {
                ["v1"] => {}
                ["session", name] => {
                    session = Some(SavedSession {
                        name: hex_decode(name),
                        default_path: String::new(),
                        saved_label: saved_label.clone(),
                        windows: Vec::new(),
                    });
                }
                ["default_path", path] => {
                    if let Some(s) = session.as_mut() {
                        s.default_path = hex_decode(path);
                    }
                }
                ["window", index, name, active, layout] => {
                    if let Some(window) = current_window.take() {
                        if let Some(s) = session.as_mut() {
                            s.windows.push(window);
                        }
                    }
                    current_window = Some(SavedWindow {
                        index: hex_decode(index),
                        name: hex_decode(name),
                        active: *active == "1",
                        layout: hex_decode(layout),
                        panes: Vec::new(),
                    });
                }
                ["pane", index, active, cwd, command] => {
                    if let Some(window) = current_window.as_mut() {
                        window.panes.push(SavedPane {
                            index: hex_decode(index),
                            active: *active == "1",
                            cwd: hex_decode(cwd),
                            command: hex_decode(command),
                        });
                    }
                }
                _ => {}
            }
        }
        if let Some(window) = current_window.take() {
            if let Some(s) = session.as_mut() {
                s.windows.push(window);
            }
        }
        if let Some(mut s) = session {
            s.windows.sort_by_key(|w| w.index.parse::<usize>().unwrap_or(usize::MAX));
            for w in &mut s.windows {
                w.panes.sort_by_key(|p| p.index.parse::<usize>().unwrap_or(usize::MAX));
            }
            if !s.name.is_empty() && !s.windows.is_empty() {
                sessions.push(s);
            }
        }
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    sessions
}
pub(crate) fn save_session_snapshot(session: &Session) -> Result<(), String> {
    let Some(path) = saved_session_path(&session.name) else {
        return Err("no state directory".to_string());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let windows_raw = tmux(&[
        "list-windows",
        "-t",
        &session.id,
        "-F",
        "#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}\t#{window_layout}",
    ])
    .map_err(|e| e.to_string())?;

    let mut out = String::new();
    out.push_str("v1\n");
    out.push_str("session\t");
    out.push_str(&hex_encode(&session.name));
    out.push('\n');
    let default_path = tmux(&["show-option", "-qv", "-t", &session.id, "@overview_default_path"])
        .unwrap_or_default()
        .trim()
        .to_string();
    if !default_path.is_empty() {
        out.push_str("default_path\t");
        out.push_str(&hex_encode(&default_path));
        out.push('\n');
    }

    for line in windows_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 5 {
            continue;
        }
        let window_id = parts[0];
        out.push_str("window\t");
        out.push_str(&hex_encode(parts[1]));
        out.push('\t');
        out.push_str(&hex_encode(parts[2]));
        out.push('\t');
        out.push_str(if parts[3] == "1" { "1" } else { "0" });
        out.push('\t');
        out.push_str(&hex_encode(parts[4]));
        out.push('\n');

        let panes_raw = tmux(&[
            "list-panes",
            "-t",
            window_id,
            "-F",
            "#{pane_index}\t#{pane_active}\t#{pane_current_path}\t#{pane_current_command}",
        ])
        .unwrap_or_default();
        for pane_line in panes_raw.lines() {
            let pane_parts: Vec<&str> = pane_line.split('\t').collect();
            if pane_parts.len() < 4 {
                continue;
            }
            out.push_str("pane\t");
            out.push_str(&hex_encode(pane_parts[0]));
            out.push('\t');
            out.push_str(if pane_parts[1] == "1" { "1" } else { "0" });
            out.push('\t');
            out.push_str(&hex_encode(pane_parts[2]));
            out.push('\t');
            out.push_str(&hex_encode(pane_parts[3]));
            out.push('\n');
        }
    }

    fs::write(path, out).map_err(|e| e.to_string())
}

pub(crate) fn delete_saved_session(name: &str) -> Result<(), String> {
    let Some(path) = saved_session_path(name) else {
        return Err("no state directory".to_string());
    };
    fs::remove_file(path).map_err(|e| e.to_string())
}

pub(crate) fn rename_saved_session(old_name: &str, new_name: &str) -> Result<(), String> {
    if new_name.trim().is_empty() {
        return Err("empty session name".to_string());
    }
    if old_name == new_name {
        return Ok(());
    }
    let Some(old_path) = saved_session_path(old_name) else {
        return Err("no state directory".to_string());
    };
    let Some(new_path) = saved_session_path(new_name) else {
        return Err("no state directory".to_string());
    };
    let contents = fs::read_to_string(&old_path).map_err(|e| e.to_string())?;
    let mut out = String::new();
    for line in contents.lines() {
        if line.starts_with("session\t") {
            out.push_str("session\t");
            out.push_str(&hex_encode(new_name));
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    fs::write(&new_path, out).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(old_path);
    Ok(())
}

pub(crate) fn is_shell_command(command: &str) -> bool {
    matches!(
        command,
        "" | "sh" | "bash" | "zsh" | "fish" | "nu" | "dash" | "ash" | "elvish"
    )
}
pub(crate) fn restore_saved_session(saved: &SavedSession) -> Result<(), String> {
    if saved.windows.is_empty() {
        return Err("saved session has no windows".to_string());
    }
    if tmux_status_ok(&["has-session", "-t", &saved.name]) {
        tmux_status_ignore(&["switch-client", "-t", &saved.name]);
        return Ok(());
    }

    let first_cwd = saved
        .windows
        .iter()
        .flat_map(|w| w.panes.iter())
        .find(|p| !p.cwd.is_empty())
        .map(|p| p.cwd.as_str())
        .unwrap_or(".");

    if !tmux_status_ok(&[
        "new-session",
        "-d",
        "-s",
        &saved.name,
        "-n",
        "__restore_dummy__",
        "-c",
        first_cwd,
    ]) {
        return Err("failed to create session".to_string());
    }
    tmux_status_ignore(&[
        "move-window",
        "-s",
        &format!("{}:__restore_dummy__", saved.name),
        "-t",
        &format!("{}:9999", saved.name),
    ]);

    if !saved.default_path.is_empty() {
        tmux_status_ignore(&["set-option", "-t", &saved.name, "@overview_default_path", &saved.default_path]);
    }

    for window in &saved.windows {
        let panes = if window.panes.is_empty() {
            vec![SavedPane {
                index: "0".to_string(),
                active: true,
                cwd: first_cwd.to_string(),
                command: String::new(),
            }]
        } else {
            window.panes.clone()
        };
        let cwd = panes.iter().find(|p| !p.cwd.is_empty()).map(|p| p.cwd.as_str()).unwrap_or(first_cwd);
        let target_window = format!("{}:{}", saved.name, window.index);
        let created_window = tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{window_id}",
            "-t",
            &target_window,
            "-n",
            &window.name,
            "-c",
            cwd,
        ])
        .unwrap_or_default()
        .trim()
        .to_string();
        let target_window_ref = if created_window.is_empty() {
            let fallback = tmux(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                &saved.name,
                "-n",
                &window.name,
                "-c",
                cwd,
            ])
            .unwrap_or_default()
            .trim()
            .to_string();
            if fallback.is_empty() { target_window.clone() } else { fallback }
        } else {
            created_window
        };

        for pane in panes.iter().skip(1) {
            let pane_cwd = if pane.cwd.is_empty() { cwd } else { &pane.cwd };
            tmux_status_ignore(&["split-window", "-d", "-t", &target_window_ref, "-c", pane_cwd]);
        }
        if !window.layout.is_empty() {
            tmux_status_ignore(&["select-layout", "-t", &target_window_ref, &window.layout]);
        }

        for pane in &panes {
            let target_pane = format!("{}.{}", target_window_ref, pane.index);
            if !pane.command.is_empty() && !is_shell_command(&pane.command) {
                tmux_status_ignore(&["send-keys", "-t", &target_pane, "-l", &pane.command]);
            }
        }
        if let Some(active) = panes.iter().find(|p| p.active) {
            tmux_status_ignore(&["select-pane", "-t", &format!("{}.{}", target_window_ref, active.index)]);
        }
    }

    tmux_status_ignore(&["kill-window", "-t", &format!("{}:9999", saved.name)]);
    if let Some(active_window) = saved.windows.iter().find(|w| w.active).or_else(|| saved.windows.first()) {
        tmux_status_ignore(&["select-window", "-t", &format!("{}:{}", saved.name, active_window.index)]);
    }
    tmux_status_ignore(&["switch-client", "-t", &saved.name]);
    Ok(())
}
