use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{ConfirmAction, InputAction, Modal, ModalKeyResult};
use crate::saved::{delete_saved_session, rename_saved_session};
use crate::tmux_api::{tmux, tmux_status_ok};

fn expand_home(value: &str) -> String {
    if value == "~" || value.starts_with("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(value.trim_start_matches("~/")).to_string_lossy().to_string();
        }
    }
    value.to_string()
}

fn path_with_trailing_slash(path: PathBuf) -> String {
    let mut s = path.to_string_lossy().to_string();
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

fn common_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = first.clone();
    for value in values.iter().skip(1) {
        while !value.starts_with(&prefix) {
            if prefix.pop().is_none() {
                return String::new();
            }
        }
    }
    prefix
}

fn complete_path_value(value: &str) -> Option<String> {
    let raw = value.trim();
    let expanded = expand_home(if raw.is_empty() { "." } else { raw });
    let path = PathBuf::from(&expanded);
    let (dir, prefix) = if expanded.ends_with('/') {
        (path, String::new())
    } else {
        let parent = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        (parent, file_name)
    };

    let mut names: Vec<String> = fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let ty = entry.file_type().ok()?;
            if !ty.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            (name.starts_with(&prefix) && !name.is_empty()).then_some(name)
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return None;
    }

    let common = common_prefix(&names);
    let selected = if common.len() > prefix.len() {
        common
    } else {
        names.into_iter().next()?
    };
    Some(path_with_trailing_slash(dir.join(selected)))
}

fn parse_add_value(value: &str) -> (&str, &str) {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    for (prefix, kind) in [
        ("session:", "session"),
        ("s:", "session"),
        ("session ", "session"),
        ("s ", "session"),
        ("window:", "window"),
        ("w:", "window"),
        ("window ", "window"),
        ("w ", "window"),
    ] {
        if lower.starts_with(prefix) {
            return (kind, trimmed[prefix.len()..].trim());
        }
    }
    ("window", trimmed)
}

pub(crate) fn perform_confirm_action(action: ConfirmAction) -> Result<String, String> {
    match action {
        ConfirmAction::KillSession { target, name } => {
            if tmux_status_ok(&["kill-session", "-t", &target]) {
                Ok(format!("killed session {}", name))
            } else {
                Err(format!("failed to kill session {}", name))
            }
        }
        ConfirmAction::KillWindow { target, name } => {
            if tmux_status_ok(&["kill-window", "-t", &target]) {
                Ok(format!("deleted window {}", name))
            } else {
                Err(format!("failed to delete window {}", name))
            }
        }
        ConfirmAction::DeleteSaved { name } => {
            delete_saved_session(&name)?;
            Ok(format!("deleted saved {}", name))
        }
    }
}

fn rename_session_tree(target: &str, old_name: &str, new_name: &str) -> Result<usize, String> {
    if old_name == new_name {
        return Ok(0);
    }

    let sessions_raw = tmux(&["list-sessions", "-F", "#{session_id}\t#{session_name}"])
        .map_err(|e| e.to_string())?;
    let descendant_prefix = format!("{}/", old_name);
    let mut renames: Vec<(String, String, String)> = sessions_raw
        .lines()
        .filter_map(|line| {
            let (id, name) = line.split_once('\t')?;
            if name == old_name || name.starts_with(&descendant_prefix) {
                let suffix = &name[old_name.len()..];
                Some((id.to_string(), name.to_string(), format!("{}{}", new_name, suffix)))
            } else {
                None
            }
        })
        .collect();

    if !renames.iter().any(|(id, name, _)| id == target && name == old_name) {
        return Err(format!("session {} no longer exists", old_name));
    }

    let existing: HashSet<String> = sessions_raw
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(_, name)| name.to_string()))
        .collect();
    let sources: HashSet<String> = renames.iter().map(|(_, source, _)| source.clone()).collect();
    let mut destinations = HashSet::new();
    for (_, _, destination) in &renames {
        if !destinations.insert(destination.clone()) {
            return Err(format!("duplicate destination session {}", destination));
        }
        if existing.contains(destination) && !sources.contains(destination) {
            return Err(format!("session {} already exists", destination));
        }
    }

    // Descendants move first so renaming a parent into its own namespace does
    // not collide with a child that is about to move deeper into that tree.
    renames.sort_by(|a, b| b.1.matches('/').count().cmp(&a.1.matches('/').count()));
    let mut completed: Vec<(String, String)> = Vec::new();
    for (id, source, destination) in &renames {
        if !tmux_status_ok(&["rename-session", "-t", id, destination]) {
            for (completed_id, original_name) in completed.iter().rev() {
                let _ = tmux_status_ok(&["rename-session", "-t", completed_id, original_name]);
            }
            return Err(format!("failed to rename session {}", source));
        }
        completed.push((id.clone(), source.clone()));
    }
    Ok(renames.len().saturating_sub(1))
}

pub(crate) fn perform_input_action(action: InputAction, value: String) -> Result<String, String> {
    let value = value.trim().to_string();
    match action {
        InputAction::RenameSession { target, old_name } => {
            if value.is_empty() || value.contains(':') || value.contains('\n') {
                return Err("invalid session name".to_string());
            }
            let child_count = rename_session_tree(&target, &old_name, &value)?;
            if child_count == 0 {
                Ok(format!("renamed {} → {}", old_name, value))
            } else {
                Ok(format!(
                    "renamed {} → {} with {} child session{}",
                    old_name,
                    value,
                    child_count,
                    if child_count == 1 { "" } else { "s" }
                ))
            }
        }
        InputAction::RenameWindow { target, old_name } => {
            if value.is_empty() || value.contains('\n') {
                return Err("invalid window name".to_string());
            }
            if tmux_status_ok(&["rename-window", "-t", &target, &value]) {
                Ok(format!("renamed window {} → {}", old_name, value))
            } else {
                Err(format!("failed to rename window {}", old_name))
            }
        }
        InputAction::RenameSavedSession { old_name } => {
            if value.is_empty() || value.contains(':') || value.contains('\n') {
                return Err("invalid session name".to_string());
            }
            rename_saved_session(&old_name, &value)?;
            Ok(format!("renamed saved {} → {}", old_name, value))
        }
        InputAction::SetDefaultPath { target, name } => {
            if value.is_empty() {
                if tmux_status_ok(&["set-option", "-u", "-t", &target, "@overview_default_path"]) {
                    Ok(format!("cleared default path for {}", name))
                } else {
                    Err(format!("failed to clear path for {}", name))
                }
            } else if tmux_status_ok(&["set-option", "-t", &target, "@overview_default_path", &value]) {
                Ok(format!("default path for {}: {}", name, value))
            } else {
                Err(format!("failed to set path for {}", name))
            }
        }
        InputAction::AddTarget {
            session_target,
            session_name,
            cwd,
        } => {
            let (kind, name) = parse_add_value(&value);
            if name.is_empty() || name.contains('\n') {
                return Err("invalid name".to_string());
            }
            if kind == "session" {
                if name.contains(':') {
                    return Err("invalid session name".to_string());
                }
                let mut args = vec!["new-session", "-d", "-s", name];
                if !cwd.is_empty() {
                    args.push("-c");
                    args.push(&cwd);
                }
                if tmux_status_ok(&args) {
                    Ok(format!("created session {}", name))
                } else {
                    Err(format!("failed to create session {}", name))
                }
            } else {
                let Some(target) = session_target else {
                    return Err("select a live session/window, or use s:name".to_string());
                };
                let display_target = session_name.unwrap_or_else(|| target.clone());
                let mut args = vec!["new-window", "-d", "-t", &target, "-n", name];
                if !cwd.is_empty() {
                    args.push("-c");
                    args.push(&cwd);
                }
                if tmux_status_ok(&args) {
                    Ok(format!("created window {} in {}", name, display_target))
                } else {
                    Err(format!("failed to create window {}", name))
                }
            }
        }
    }
}
pub(crate) fn handle_modal_key(modal: &mut Modal, key: &[u8]) -> ModalKeyResult {
    match modal.clone() {
        Modal::None => ModalKeyResult::Ignored,
        Modal::Confirm { action, .. } => match key {
            b"y" | b"Y" | b"\r" | b"\n" => {
                *modal = Modal::None;
                ModalKeyResult::Applied(match perform_confirm_action(action) {
                    Ok(msg) => msg,
                    Err(e) => e,
                })
            }
            b"n" | b"N" | b"q" | [0x1b] | [3] => {
                *modal = Modal::None;
                ModalKeyResult::Redraw
            }
            _ => ModalKeyResult::Ignored,
        },
        Modal::Info { .. } => match key {
            b"q" | b"\r" | b"\n" | b" " | [0x1b] | [3] => {
                *modal = Modal::None;
                ModalKeyResult::Redraw
            }
            _ => ModalKeyResult::Ignored,
        },
        Modal::Input { title, mut value, action } => match key {
            [9] => {
                if matches!(action, InputAction::SetDefaultPath { .. }) {
                    if let Some(completed) = complete_path_value(&value) {
                        value = completed;
                        *modal = Modal::Input { title, value, action };
                        return ModalKeyResult::Redraw;
                    }
                }
                ModalKeyResult::Ignored
            }
            b"\r" | b"\n" => {
                *modal = Modal::None;
                ModalKeyResult::Applied(match perform_input_action(action, value) {
                    Ok(msg) => msg,
                    Err(e) => e,
                })
            }
            [0x1b] | [3] => {
                *modal = Modal::None;
                ModalKeyResult::Redraw
            }
            [8] | [127] => {
                value.pop();
                *modal = Modal::Input { title, value, action };
                ModalKeyResult::Redraw
            }
            [byte] if byte.is_ascii_graphic() || *byte == b' ' => {
                value.push(*byte as char);
                *modal = Modal::Input { title, value, action };
                ModalKeyResult::Redraw
            }
            _ => ModalKeyResult::Ignored,
        },
    }
}
