use std::collections::{HashMap, HashSet};

use crate::model::{ConfirmAction, InputAction, Modal, ProcStats, Row, SavedSession, Session, Window};
use crate::saved::load_saved_sessions;
use crate::tmux_api::tmux;
use crate::util::session_display_name;

pub(crate) fn session_is_hidden(name: &str) -> bool {
    name.starts_with("_popup_")
        || name
            .split('/')
            .any(|part| {
                part.starts_with("__")
                    || part.starts_with('▣')
                    || part.starts_with('◫')
                    || part.starts_with('󰚩')
                    || part.starts_with('🤖')
            })
}

fn nearest_parent_name(name: &str, live_names: &HashSet<String>) -> Option<String> {
    let mut candidate = name.rsplit_once('/')?.0;
    loop {
        if live_names.contains(candidate) {
            return Some(candidate.to_string());
        }
        let Some((parent, _)) = candidate.rsplit_once('/') else {
            return None;
        };
        candidate = parent;
    }
}

fn append_session_tree(
    rows: &mut Vec<Row>,
    session: &Session,
    parent_name: Option<&str>,
    depth: usize,
    children: &HashMap<String, Vec<Session>>,
    windows_by_session: &HashMap<String, Vec<Window>>,
    expanded: &HashMap<String, bool>,
) {
    let mut displayed = session.clone();
    displayed.depth = depth;
    let raw_display_name = parent_name
        .and_then(|parent| session.name.strip_prefix(&format!("{}/", parent)))
        .unwrap_or(&session.name);
    displayed.display_name = session_display_name(raw_display_name);
    rows.push(Row::Session(displayed));

    if !*expanded.get(&session.name).unwrap_or(&true) {
        return;
    }
    if let Some(windows) = windows_by_session.get(&session.id) {
        for window in windows {
            let mut displayed_window = window.clone();
            displayed_window.depth = depth;
            rows.push(Row::Window(displayed_window));
        }
    }
    if let Some(child_sessions) = children.get(&session.name) {
        for child in child_sessions {
            append_session_tree(
                rows,
                child,
                Some(&session.name),
                depth + 1,
                children,
                windows_by_session,
                expanded,
            );
        }
    }
}

pub(crate) fn load_rows(
    expanded: &HashMap<String, bool>,
    watchlist: &HashSet<String>,
    session_stats: &HashMap<String, ProcStats>,
    window_stats: &HashMap<String, ProcStats>,
    window_labels: &HashMap<String, Vec<String>>,
    saved_sessions: &[SavedSession],
    show_hidden: bool,
) -> Vec<Row> {
    let current_session = tmux(&["display-message", "-p", "#{session_id}"])
        .unwrap_or_default()
        .trim()
        .to_string();

    let sessions_raw = tmux(&[
        "list-sessions",
        "-F",
        "#{session_id}\t#{session_name}\t#{@overview_default_path}",
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
        if parts.len() < 6 || (!show_hidden && session_is_hidden(parts[1])) {
            continue;
        }
        let pane_labels = window_labels.get(parts[2]).cloned().unwrap_or_default();
        let w = Window {
            session_id: parts[0].to_string(),
            session_name: parts[1].to_string(),
            depth: 0,
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

    let mut sessions = Vec::new();
    let mut all_live_names = HashSet::new();
    for line in sessions_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        all_live_names.insert(parts[1].to_string());
        if !show_hidden && session_is_hidden(parts[1]) {
            continue;
        }
        let saved_label = saved_sessions
            .iter()
            .find(|saved| saved.name == parts[1])
            .map(|saved| saved.saved_label.clone());
        let default_path = parts.get(2).copied().unwrap_or_default().trim().to_string();
        sessions.push(Session {
            id: parts[0].to_string(),
            name: parts[1].to_string(),
            display_name: parts[1].to_string(),
            depth: 0,
            current: parts[0] == current_session,
            saved_label,
            default_path,
            stats: session_stats.get(parts[0]).cloned().unwrap_or_default(),
        });
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));

    let visible_names: HashSet<String> = sessions.iter().map(|session| session.name.clone()).collect();
    let mut roots = Vec::new();
    let mut children: HashMap<String, Vec<Session>> = HashMap::new();
    for session in sessions {
        if let Some(parent) = nearest_parent_name(&session.name, &visible_names) {
            children.entry(parent).or_default().push(session);
        } else {
            roots.push(session);
        }
    }
    for child_sessions in children.values_mut() {
        child_sessions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for session in &roots {
        append_session_tree(
            &mut rows,
            session,
            None,
            0,
            &children,
            &windows_by_session,
            expanded,
        );
    }

    for saved in saved_sessions {
        if !all_live_names.contains(&saved.name) && (show_hidden || !session_is_hidden(&saved.name)) {
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
    show_hidden: bool,
) {
    *rows = load_rows(expanded, watchlist, session_stats, window_stats, window_labels, saved_sessions, show_hidden);
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
    show_hidden: bool,
) {
    let key = rows.get(*selected).map(row_key);
    reload_rows_into(rows, expanded, watchlist, session_stats, window_stats, window_labels, saved_sessions, show_hidden);
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
    show_hidden: bool,
) {
    *saved_sessions = load_saved_sessions();
    reload_rows_clamped(rows, selected, expanded, watchlist, session_stats, window_stats, window_labels, saved_sessions, show_hidden);
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
