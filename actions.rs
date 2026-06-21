use crate::model::{ConfirmAction, InputAction, Modal, ModalKeyResult};
use crate::saved::{delete_saved_session, rename_saved_session};
use crate::tmux_api::tmux_status_ok;

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

pub(crate) fn perform_input_action(action: InputAction, value: String) -> Result<String, String> {
    let value = value.trim().to_string();
    match action {
        InputAction::RenameSession { target, old_name } => {
            if value.is_empty() || value.contains(':') || value.contains('\n') {
                return Err("invalid session name".to_string());
            }
            if tmux_status_ok(&["rename-session", "-t", &target, &value]) {
                Ok(format!("renamed {} → {}", old_name, value))
            } else {
                Err(format!("failed to rename {}", old_name))
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
