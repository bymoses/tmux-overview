use std::collections::{HashMap, HashSet};

use crate::model::{ConfirmAction, InputAction, Modal, ProcStats, Row, SavedSession, Session, Window};
use crate::saved::load_saved_sessions;
use crate::tmux_api::tmux;

pub(crate) fn load_rows(
    expanded: &HashMap<String, bool>,
    watchlist: &HashSet<String>,
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
        if parts.len() < 6 || parts[1].starts_with("_popup_") {
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

    let mut watched_windows: Vec<Window> = windows_by_session
        .values()
        .flat_map(|windows| windows.iter())
        .filter(|window| watchlist.contains(&window.id))
        .cloned()
        .collect();
    watched_windows.sort_by(|a, b| a.session_name.cmp(&b.session_name).then_with(|| a.index.cmp(&b.index)));

    let mut rows = Vec::new();
    if !watched_windows.is_empty() {
        rows.push(Row::WatchHeader);
        for window in watched_windows {
            rows.push(Row::WatchWindow(window));
        }
    }

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
        let default_path = tmux(&["show-option", "-qv", "-t", parts[0], "@overview_default_path"])
            .unwrap_or_default()
            .trim()
            .to_string();
        let s = Session {
            id: parts[0].to_string(),
            name: parts[1].to_string(),
            current: parts[0] == current_session,
            saved_label,
            default_path,
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

pub(crate) fn is_selectable(row: &Row) -> bool {
    !matches!(row, Row::WatchHeader)
}

pub(crate) fn clamp_selected(selected: &mut usize, rows: &[Row]) {
    if rows.is_empty() {
        *selected = 0;
        return;
    }
    *selected = (*selected).min(rows.len().saturating_sub(1));
    if is_selectable(&rows[*selected]) {
        return;
    }
    if let Some(idx) = rows.iter().enumerate().skip(*selected + 1).find_map(|(idx, row)| is_selectable(row).then_some(idx)) {
        *selected = idx;
    } else if let Some(idx) = rows.iter().enumerate().take(*selected).rev().find_map(|(idx, row)| is_selectable(row).then_some(idx)) {
        *selected = idx;
    }
}

pub(crate) fn selectable_after(rows: &[Row], selected: usize) -> usize {
    rows.iter()
        .enumerate()
        .skip(selected.saturating_add(1))
        .find_map(|(idx, row)| is_selectable(row).then_some(idx))
        .unwrap_or(selected)
}

pub(crate) fn selectable_before(rows: &[Row], selected: usize) -> usize {
    rows.iter()
        .enumerate()
        .take(selected)
        .rev()
        .find_map(|(idx, row)| is_selectable(row).then_some(idx))
        .unwrap_or(selected)
}

pub(crate) fn first_selectable(rows: &[Row]) -> usize {
    rows.iter()
        .enumerate()
        .find_map(|(idx, row)| is_selectable(row).then_some(idx))
        .unwrap_or(0)
}

pub(crate) fn last_selectable(rows: &[Row]) -> usize {
    rows.iter()
        .enumerate()
        .rev()
        .find_map(|(idx, row)| is_selectable(row).then_some(idx))
        .unwrap_or(0)
}

pub(crate) fn reload_rows_into(
    rows: &mut Vec<Row>,
    expanded: &HashMap<String, bool>,
    watchlist: &HashSet<String>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
    saved_sessions: &[SavedSession],
) {
    *rows = load_rows(expanded, watchlist, session_stats, window_stats, window_labels, saved_sessions);
}

pub(crate) fn reload_rows_clamped(
    rows: &mut Vec<Row>,
    selected: &mut usize,
    expanded: &HashMap<String, bool>,
    watchlist: &HashSet<String>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
    saved_sessions: &[SavedSession],
) {
    let key = rows.get(*selected).map(row_key);
    reload_rows_into(rows, expanded, watchlist, session_stats, window_stats, window_labels, saved_sessions);
    if let Some(key) = key {
        if let Some(idx) = selected_index_for_key(rows, &key) {
            *selected = idx;
        }
    }
    clamp_selected(selected, rows);
}

pub(crate) fn reload_saved_rows_clamped(
    saved_sessions: &mut Vec<SavedSession>,
    rows: &mut Vec<Row>,
    selected: &mut usize,
    expanded: &HashMap<String, bool>,
    watchlist: &HashSet<String>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
) {
    *saved_sessions = load_saved_sessions();
    reload_rows_clamped(rows, selected, expanded, watchlist, session_stats, window_stats, window_labels, saved_sessions);
}

pub(crate) fn row_key(row: &Row) -> String {
    match row {
        Row::WatchHeader => "watch-header".to_string(),
        Row::WatchWindow(w) | Row::Window(w) => format!("window\t{}", w.id),
        Row::Session(s) => format!("session\t{}", s.id),
        Row::SavedSession(s) => format!("saved\t{}", s.name),
    }
}

pub(crate) fn selected_index_for_key(rows: &[Row], key: &str) -> Option<usize> {
    rows.iter()
        .enumerate()
        .find_map(|(idx, row)| (is_selectable(row) && row_key(row) == key).then_some(idx))
}

pub(crate) fn row_target(row: &Row) -> &str {
    match row {
        Row::WatchHeader => "",
        Row::Session(s) => &s.id,
        Row::WatchWindow(w) | Row::Window(w) => &w.id,
        Row::SavedSession(s) => &s.name,
    }
}

pub(crate) fn session_from_row(row: &Row) -> Option<(String, String)> {
    match row {
        Row::Session(s) => Some((s.id.clone(), s.name.clone())),
        Row::WatchWindow(w) | Row::Window(w) => Some((w.session_id.clone(), w.session_name.clone())),
        Row::WatchHeader | Row::SavedSession(_) => None,
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
        Row::WatchHeader => Modal::None,
        Row::Session(s) => Modal::Input {
            title: format!("rename session {}", s.name),
            value: s.name.clone(),
            action: InputAction::RenameSession {
                target: s.id.clone(),
                old_name: s.name.clone(),
            },
        },
        Row::WatchWindow(w) | Row::Window(w) => Modal::Input {
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

pub(crate) fn add_modal_for(row: &Row) -> Modal {
    let session = session_from_row(row);
    let cwd = match row {
        Row::SavedSession(s) => s.default_path.clone(),
        Row::WatchHeader => tmux(&["display-message", "-p", "#{pane_current_path}"])
            .unwrap_or_default()
            .trim()
            .to_string(),
        _ => default_path_for_row(row),
    };
    Modal::Input {
        title: match &session {
            Some((_, name)) => format!("add in {}: window name, or s:name", name),
            None => "add: s:name for session".to_string(),
        },
        value: String::new(),
        action: InputAction::AddTarget {
            session_target: session.as_ref().map(|(target, _)| target.clone()),
            session_name: session.map(|(_, name)| name),
            cwd,
        },
    }
}

pub(crate) fn delete_modal_for(row: &Row) -> Modal {
    match row {
        Row::WatchHeader => Modal::None,
        Row::SavedSession(s) => Modal::Confirm {
            title: format!("delete saved session {}?", s.name),
            action: ConfirmAction::DeleteSaved { name: s.name.clone() },
        },
        Row::WatchWindow(w) | Row::Window(w) => Modal::Confirm {
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
        return first_selectable(rows);
    }
    rows.iter()
        .position(|row| matches!(row, Row::Window(w) | Row::WatchWindow(w) if w.id == current_window))
        .unwrap_or_else(|| first_selectable(rows))
}
