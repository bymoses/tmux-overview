use std::collections::HashMap;
use std::env;
use std::io::{self, IsTerminal};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::actions::handle_modal_key;
use crate::model::{Modal, ModalKeyResult, ProcStats, Row, Session};
use crate::render::{help_modal, render};
use crate::rows::{default_path_modal_for, delete_modal_for, kill_session_modal_for, load_rows, reload_rows_into, reload_saved_rows_clamped, rename_modal_for, row_target, selected_index_for_current_window};
use crate::saved::{load_saved_sessions, restore_saved_session, save_session_snapshot};
use crate::state::{load_expanded_state, save_expanded_state};
use crate::stats::{collect_proc_stats, collect_window_labels};
use crate::terminal::{read_key_timeout, TerminalGuard};
use crate::tmux_api::tmux_status_ignore;

pub(crate) fn run() -> io::Result<()> {
    if env::var("TMUX").is_err() {
        eprintln!("tmux-overview must be run inside tmux");
        return Ok(());
    }

    if !io::stdin().is_terminal() {
        eprintln!("tmux-overview needs an interactive terminal/popup");
        return Ok(());
    }

    let _guard = TerminalGuard::enter()?;
    let mut expanded = load_expanded_state();
    let mut preview_percent = env::var("TMUX_OVERVIEW_PREVIEW_PERCENT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(65)
        .clamp(20, 85);
    let stats_interval_secs = env::var("TMUX_OVERVIEW_STATS_INTERVAL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, 60);
    let title_interval_ms = env::var("TMUX_OVERVIEW_TITLE_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(100)
        .clamp(50, 5000);
    let mut show_preview = true;
    let mut message = String::from("stats are collected in the background");
    let mut modal = Modal::None;

    let (stats_tx, stats_rx) = mpsc::channel();
    thread::spawn(move || loop {
        if stats_tx.send(collect_proc_stats()).is_err() {
            break;
        }
        thread::sleep(Duration::from_secs(stats_interval_secs));
    });

    let (labels_tx, labels_rx) = mpsc::channel();
    thread::spawn(move || loop {
        if labels_tx.send(collect_window_labels()).is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(title_interval_ms));
    });

    let mut session_stats: HashMap<String, ProcStats> = HashMap::new();
    let mut window_stats: HashMap<String, ProcStats> = HashMap::new();
    let mut window_labels = collect_window_labels();
    let mut saved_sessions = load_saved_sessions();
    let mut rows = load_rows(&expanded, &session_stats, &window_stats, &window_labels, &saved_sessions);
    let mut selected = selected_index_for_current_window(&rows);
    let mut dirty = true;

    loop {
        while let Ok((new_session_stats, new_window_stats)) = stats_rx.try_recv() {
            session_stats = new_session_stats;
            window_stats = new_window_stats;
            reload_rows_into(&mut rows, &expanded, &session_stats, &window_stats, &window_labels, &saved_sessions);
            dirty = true;
        }
        while let Ok(new_window_labels) = labels_rx.try_recv() {
            if new_window_labels != window_labels {
                window_labels = new_window_labels;
                reload_rows_into(&mut rows, &expanded, &session_stats, &window_stats, &window_labels, &saved_sessions);
                dirty = true;
            }
        }

        if selected >= rows.len() {
            selected = rows.len().saturating_sub(1);
            dirty = true;
        }

        if dirty {
            render(
                &rows,
                selected,
                &expanded,
                preview_percent,
                show_preview,
                stats_interval_secs,
                &message,
                &modal,
            )?;
            dirty = false;
        }

        let Some(key) = read_key_timeout()? else {
            continue;
        };

        if !matches!(modal, Modal::None) {
            match handle_modal_key(&mut modal, key.as_slice()) {
                ModalKeyResult::Applied(msg) => {
                    message = msg;
                    reload_saved_rows_clamped(
                        &mut saved_sessions,
                        &mut rows,
                        &mut selected,
                        &expanded,
                        &session_stats,
                        &window_stats,
                        &window_labels,
                    );
                    dirty = true;
                }
                ModalKeyResult::Redraw => dirty = true,
                ModalKeyResult::Ignored => {}
            }
            continue;
        }

        match key.as_slice() {
            b"q" | [3] | [0x1b, b'f'] => break,
            b"j" | [0x1b, b'[', b'B'] => {
                if selected + 1 < rows.len() {
                    selected += 1;
                    dirty = true;
                }
            }
            b"k" | [0x1b, b'[', b'A'] => {
                selected = selected.saturating_sub(1);
                dirty = true;
            }
            b"g" => {
                selected = 0;
                dirty = true;
            }
            b"G" => {
                selected = rows.len().saturating_sub(1);
                dirty = true;
            }
            b" " => {
                if let Some(Row::Session(s)) = rows.get(selected) {
                    let now = *expanded.get(&s.name).unwrap_or(&true);
                    expanded.insert(s.name.clone(), !now);
                    save_expanded_state(&expanded);
                    reload_rows_into(&mut rows, &expanded, &session_stats, &window_stats, &window_labels, &saved_sessions);
                    dirty = true;
                }
            }
            b"l" | [0x1b, b'[', b'C'] => {
                if let Some(Row::Session(s)) = rows.get(selected) {
                    expanded.insert(s.name.clone(), true);
                    save_expanded_state(&expanded);
                    reload_rows_into(&mut rows, &expanded, &session_stats, &window_stats, &window_labels, &saved_sessions);
                    dirty = true;
                }
            }
            b"h" | [0x1b, b'[', b'D'] => {
                let collapse_session = rows.get(selected).and_then(|row| match row {
                    Row::Session(s) => Some(s.name.clone()),
                    Row::Window(w) => Some(w.session_name.clone()),
                    Row::SavedSession(_) => None,
                });
                if let Some(session_name) = collapse_session {
                    expanded.insert(session_name.clone(), false);
                    save_expanded_state(&expanded);
                    reload_rows_into(&mut rows, &expanded, &session_stats, &window_stats, &window_labels, &saved_sessions);
                    selected = rows
                        .iter()
                        .position(|row| matches!(row, Row::Session(s) if s.name == session_name))
                        .unwrap_or(selected.min(rows.len().saturating_sub(1)));
                    dirty = true;
                }
            }
            b"v" => {
                show_preview = !show_preview;
                dirty = true;
            }
            b"+" | b"=" => {
                preview_percent = (preview_percent + 5).min(85);
                dirty = true;
            }
            b"-" | b"_" => {
                preview_percent = preview_percent.saturating_sub(5).max(20);
                dirty = true;
            }
            b"?" => {
                modal = help_modal();
                dirty = true;
            }
            b"S" => {
                let session_to_save = rows.get(selected).and_then(|row| match row {
                    Row::Session(s) => Some(s.clone()),
                    Row::Window(w) => Some(Session {
                        id: w.session_id.clone(),
                        name: w.session_name.clone(),
                        current: false,
                        saved_label: None,
                        stats: ProcStats::default(),
                    }),
                    Row::SavedSession(_) => None,
                });
                if let Some(session) = session_to_save {
                    match save_session_snapshot(&session) {
                        Ok(()) => {
                            reload_saved_rows_clamped(
                                &mut saved_sessions,
                                &mut rows,
                                &mut selected,
                                &expanded,
                                &session_stats,
                                &window_stats,
                                &window_labels,
                            );
                            message = format!("saved layout {}", session.name);
                        }
                        Err(e) => message = format!("save failed: {}", e),
                    }
                    dirty = true;
                }
            }
            b"r" => {
                if let Some(row) = rows.get(selected) {
                    modal = rename_modal_for(row);
                    dirty = true;
                }
            }
            b"R" => {
                let (new_session_stats, new_window_stats) = collect_proc_stats();
                session_stats = new_session_stats;
                window_stats = new_window_stats;
                reload_saved_rows_clamped(
                    &mut saved_sessions,
                    &mut rows,
                    &mut selected,
                    &expanded,
                    &session_stats,
                    &window_stats,
                    &window_labels,
                );
                message = String::from("refreshed");
                dirty = true;
            }
            b"K" => {
                if let Some(row) = rows.get(selected) {
                    if let Some(new_modal) = kill_session_modal_for(row) {
                        modal = new_modal;
                        dirty = true;
                    }
                }
            }
            b"c" => {
                if let Some(row) = rows.get(selected) {
                    match default_path_modal_for(row) {
                        Ok(new_modal) => modal = new_modal,
                        Err(msg) => message = msg,
                    }
                    dirty = true;
                }
            }
            b"D" => {
                if let Some(row) = rows.get(selected) {
                    modal = delete_modal_for(row);
                    dirty = true;
                }
            }
            b"\r" | b"\n" => {
                if let Some(row) = rows.get(selected) {
                    match row {
                        Row::SavedSession(saved) => match restore_saved_session(&saved) {
                            Ok(()) => break,
                            Err(e) => {
                                message = format!("restore failed: {}", e);
                                dirty = true;
                            }
                        },
                        _ => {
                            let target = row_target(&row).to_string();
                            tmux_status_ignore(&["switch-client", "-t", &target]);
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
