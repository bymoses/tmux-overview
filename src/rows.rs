use std::collections::{HashMap, HashSet};

use crate::model::{ConfirmAction, InputAction, Modal, ProcStats, Row, SavedSession, Session, Window};
use crate::saved::load_saved_sessions;
use crate::tmux_api::tmux;

pub(crate) fn load_rows(
    expanded: &HashMap<String, bool>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
    saved_sessions: &[SavedSession],
) -> Vec<Row> {
    let current_session = tmux(&["display-message", "-p", "#{session_id}"])
        .unwrap_or_default()
        .trim()
        .to_string();

    let sessions_raw = tmux(&[
        "list-sessions",
        "-F",
        "#{session_id}\t#{session_name}",
    ])
    .unwrap_or_default();

    let windows_raw = tmux(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_id}\t#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}",
    ])
    .unwrap_or_default();

    let mut windows_by_session: HashMap<String, Vec<Window>> = HashMap::new();
    for line in windows_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 6 {
            continue;
        }
        let pane_labels = window_labels.get(parts[2]).cloned().unwrap_or_default();
        let w = Window {
            session_id: parts[0].to_string(),
            session_name: parts[1].to_string(),
            id: parts[2].to_string(),
            index: parts[3].to_string(),
            name: parts[4].to_string(),
            active: parts[5] == "1",
            pane_labels,
            stats: window_stats.get(parts[2]).cloned().unwrap_or_default(),
        };
        windows_by_session.entry(w.session_id.clone()).or_default().push(w);
    }

    let mut rows = Vec::new();
    let mut live_names: HashSet<String> = HashSet::new();
    for line in sessions_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        if parts[1].starts_with("_popup_") {
            continue;
        }
        live_names.insert(parts[1].to_string());
        let saved_label = saved_sessions
            .iter()
            .find(|saved| saved.name == parts[1])
            .map(|saved| saved.saved_label.clone());
        let s = Session {
            id: parts[0].to_string(),
            name: parts[1].to_string(),
            current: parts[0] == current_session,
            saved_label,
            stats: session_stats.get(parts[0]).cloned().unwrap_or_default(),
        };
        rows.push(Row::Session(s.clone()));
        if *expanded.get(&s.name).unwrap_or(&true) {
            if let Some(ws) = windows_by_session.get(&s.id) {
                for w in ws {
                    rows.push(Row::Window(w.clone()));
                }
            }
        }
    }

    for saved in saved_sessions {
        if !live_names.contains(&saved.name) {
            rows.push(Row::SavedSession(saved.clone()));
        }
    }
    rows
}

pub(crate) fn clamp_selected(selected: &mut usize, rows_len: usize) {
    *selected = (*selected).min(rows_len.saturating_sub(1));
}

pub(crate) fn reload_rows_into(
    rows: &mut Vec<Row>,
    expanded: &HashMap<String, bool>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
    saved_sessions: &[SavedSession],
) {
    *rows = load_rows(expanded, session_stats, window_stats, window_labels, saved_sessions);
}

pub(crate) fn reload_rows_clamped(
    rows: &mut Vec<Row>,
    selected: &mut usize,
    expanded: &HashMap<String, bool>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
    saved_sessions: &[SavedSession],
) {
    reload_rows_into(rows, expanded, session_stats, window_stats, window_labels, saved_sessions);
    clamp_selected(selected, rows.len());
}

pub(crate) fn reload_saved_rows_clamped(
    saved_sessions: &mut Vec<SavedSession>,
    rows: &mut Vec<Row>,
    selected: &mut usize,
    expanded: &HashMap<String, bool>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
) {
    *saved_sessions = load_saved_sessions();
    reload_rows_clamped(rows, selected, expanded, session_stats, window_stats, window_labels, saved_sessions);
}

pub(crate) fn row_target(row: &Row) -> &str {
    match row {
        Row::Session(s) => &s.id,
        Row::Window(w) => &w.id,
        Row::SavedSession(s) => &s.name,
    }
}

pub(crate) fn session_from_row(row: &Row) -> Option<(String, String)> {
    match row {
        Row::Session(s) => Some((s.id.clone(), s.name.clone())),
        Row::Window(w) => Some((w.session_id.clone(), w.session_name.clone())),
        Row::SavedSession(_) => None,
    }
}

pub(crate) fn default_path_for_row(row: &Row) -> String {
    let Some((target, _)) = session_from_row(row) else {
        return String::new();
    };
    let saved = tmux(&["show-option", "-qv", "-t", &target, "@overview_default_path"])
        .unwrap_or_default()
        .trim()
        .to_string();
    if !saved.is_empty() {
        return saved;
    }
    tmux(&["display-message", "-p", "-t", row_target(row), "#{pane_current_path}"])
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub(crate) fn rename_modal_for(row: &Row) -> Modal {
    match row {
        Row::Session(s) => Modal::Input {
            title: format!("rename session {}", s.name),
            value: s.name.clone(),
            action: InputAction::RenameSession {
                target: s.id.clone(),
                old_name: s.name.clone(),
            },
        },
        Row::Window(w) => Modal::Input {
            title: format!("rename window {}:{}", w.index, w.name),
            value: w.name.clone(),
            action: InputAction::RenameWindow {
                target: w.id.clone(),
                old_name: w.name.clone(),
            },
        },
        Row::SavedSession(s) => Modal::Input {
            title: format!("rename saved session {}", s.name),
            value: s.name.clone(),
            action: InputAction::RenameSavedSession { old_name: s.name.clone() },
        },
    }
}

pub(crate) fn kill_session_modal_for(row: &Row) -> Option<Modal> {
    let (target, name) = session_from_row(row)?;
    Some(Modal::Confirm {
        title: format!("kill session {}?", name),
        action: ConfirmAction::KillSession { target, name },
    })
}

pub(crate) fn default_path_modal_for(row: &Row) -> Result<Modal, String> {
    let Some((target, name)) = session_from_row(row) else {
        return Err("restore saved session before changing path".to_string());
    };
    Ok(Modal::Input {
        title: format!("default path for {}", name),
        value: default_path_for_row(row),
        action: InputAction::SetDefaultPath { target, name },
    })
}

pub(crate) fn delete_modal_for(row: &Row) -> Modal {
    match row {
        Row::SavedSession(s) => Modal::Confirm {
            title: format!("delete saved session {}?", s.name),
            action: ConfirmAction::DeleteSaved { name: s.name.clone() },
        },
        Row::Window(w) => Modal::Confirm {
            title: format!("delete window {}:{}?", w.index, w.name),
            action: ConfirmAction::KillWindow {
                target: w.id.clone(),
                name: format!("{}:{}", w.index, w.name),
            },
        },
        Row::Session(s) => Modal::Confirm {
            title: format!("delete/kill session {}?", s.name),
            action: ConfirmAction::KillSession {
                target: s.id.clone(),
                name: s.name.clone(),
            },
        },
    }
}

pub(crate) fn selected_index_for_current_window(rows: &[Row]) -> usize {
    let current_window = tmux(&["display-message", "-p", "#{window_id}"])
        .unwrap_or_default()
        .trim()
        .to_string();
    if current_window.is_empty() {
        return 0;
    }
    rows.iter()
        .position(|row| matches!(row, Row::Window(w) if w.id == current_window))
        .unwrap_or(0)
}
