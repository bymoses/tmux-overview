#[derive(Clone, Default)]
pub(crate) struct ProcStats {
    pub(crate) cpu: f64,
    pub(crate) rss_kb: u64,
}

#[derive(Clone)]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) current: bool,
    pub(crate) saved_label: Option<String>,
    pub(crate) default_path: String,
    pub(crate) stats: ProcStats,
}

#[derive(Clone)]
pub(crate) struct Window {
    pub(crate) session_id: String,
    pub(crate) session_name: String,
    pub(crate) id: String,
    pub(crate) index: String,
    pub(crate) name: String,
    pub(crate) active: bool,
    pub(crate) pane_labels: Vec<String>,
    pub(crate) stats: ProcStats,
}

#[derive(Clone)]
pub(crate) struct SavedPane {
    pub(crate) index: String,
    pub(crate) active: bool,
    pub(crate) cwd: String,
    pub(crate) command: String,
}

#[derive(Clone)]
pub(crate) struct SavedWindow {
    pub(crate) index: String,
    pub(crate) name: String,
    pub(crate) active: bool,
    pub(crate) layout: String,
    pub(crate) panes: Vec<SavedPane>,
}

#[derive(Clone)]
pub(crate) struct SavedSession {
    pub(crate) name: String,
    pub(crate) default_path: String,
    pub(crate) saved_label: String,
    pub(crate) windows: Vec<SavedWindow>,
}

#[derive(Clone)]
pub(crate) enum Row {
    WatchHeader,
    WatchWindow(Window),
    Session(Session),
    Window(Window),
    SavedSession(SavedSession),
}

#[derive(Clone)]
pub(crate) enum ConfirmAction {
    KillSession { target: String, name: String },
    KillWindow { target: String, name: String },
    DeleteSaved { name: String },
}

#[derive(Clone)]
pub(crate) enum InputAction {
    RenameSession { target: String, old_name: String },
    RenameWindow { target: String, old_name: String },
    RenameSavedSession { old_name: String },
    SetDefaultPath { target: String, name: String },
    AddTarget {
        session_target: Option<String>,
        session_name: Option<String>,
        cwd: String,
    },
}

#[derive(Clone)]
pub(crate) enum Modal {
    None,
    Confirm { title: String, action: ConfirmAction },
    Input { title: String, value: String, action: InputAction },
    Info { title: String, lines: Vec<String> },
}

pub(crate) enum ModalKeyResult {
    Ignored,
    Redraw,
    Applied(String),
}
