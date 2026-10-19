use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

pub(crate) fn cache_dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("tmux-overview"))
}

pub(crate) fn state_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("expanded.tsv"))
}

pub(crate) fn watchlist_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("watchlist.tsv"))
}

pub(crate) fn cursor_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("cursor"))
}

fn scalar_state_path(name: &str) -> Option<PathBuf> {
    Some(cache_dir()?.join(name))
}

fn load_boolean_state(name: &str, default: bool) -> bool {
    let Some(path) = scalar_state_path(name) else {
        return default;
    };
    match fs::read_to_string(path).ok().as_deref().map(str::trim) {
        Some("1" | "true" | "on") => true,
        Some("0" | "false" | "off") => false,
        _ => default,
    }
}

fn save_boolean_state(name: &str, value: bool) {
    let Some(path) = scalar_state_path(name) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, if value { "1\n" } else { "0\n" });
}

/// Last screen used: the agents tree (`A`) or the session overview.
pub(crate) fn load_agents_mode_state() -> bool {
    load_boolean_state("agents-mode", false)
}

pub(crate) fn save_agents_mode_state(value: bool) {
    save_boolean_state("agents-mode", value);
}

pub(crate) fn load_show_stats_state() -> bool {
    load_boolean_state("show-stats", true)
}

pub(crate) fn save_show_stats_state(value: bool) {
    save_boolean_state("show-stats", value);
}

pub(crate) fn load_show_hidden_state() -> bool {
    load_boolean_state("show-hidden", false)
}

pub(crate) fn save_show_hidden_state(value: bool) {
    save_boolean_state("show-hidden", value);
}

pub(crate) fn load_show_tree_state() -> bool {
    load_boolean_state("show-tree", true)
}

pub(crate) fn save_show_tree_state(value: bool) {
    save_boolean_state("show-tree", value);
}

pub(crate) fn load_show_preview_state() -> bool {
    load_boolean_state("show-preview", true)
}

pub(crate) fn save_show_preview_state(value: bool) {
    save_boolean_state("show-preview", value);
}

pub(crate) fn load_preview_percent_state() -> usize {
    let Some(path) = scalar_state_path("preview-percent") else {
        return 65;
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(65)
        .clamp(20, 85)
}

pub(crate) fn save_preview_percent_state(value: usize) {
    let Some(path) = scalar_state_path("preview-percent") else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, format!("{}\n", value.clamp(20, 85)));
}

pub(crate) fn load_expanded_state() -> HashMap<String, bool> {
    let Some(path) = state_path() else {
        return HashMap::new();
    };
    let Ok(contents) = fs::read_to_string(path) else {
        return HashMap::new();
    };

    let mut expanded = HashMap::new();
    for line in contents.lines() {
        let mut parts = line.splitn(2, '\t');
        let Some(name) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        if !name.is_empty() {
            expanded.insert(name.to_string(), value == "1" || value == "true");
        }
    }
    expanded
}

pub(crate) fn save_expanded_state(expanded: &HashMap<String, bool>) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut entries: Vec<_> = expanded.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut contents = String::new();
    for (name, is_expanded) in entries {
        contents.push_str(name);
        contents.push('\t');
        contents.push_str(if *is_expanded { "1" } else { "0" });
        contents.push('\n');
    }
    let _ = fs::write(path, contents);
}

/// Watched window ids for the running server. Lines are `<server start_time>\t<@id>`;
/// tmux reuses `@N` ids after a restart, so entries from other servers (and bare
/// ids from the old format) are dropped, as are ids whose window is gone.
pub(crate) fn load_watchlist_state() -> std::collections::HashSet<String> {
    let Some(path) = watchlist_path() else {
        return std::collections::HashSet::new();
    };
    let Ok(contents) = fs::read_to_string(path) else {
        return std::collections::HashSet::new();
    };
    let server = crate::tmux_api::server_start_time();
    if server.is_empty() {
        return std::collections::HashSet::new();
    }
    let live: std::collections::HashSet<String> = crate::tmux_api::tmux(&["list-windows", "-a", "-F", "#{window_id}"])
        .unwrap_or_default()
        .lines()
        .map(ToString::to_string)
        .collect();
    contents
        .lines()
        .filter_map(|line| line.trim().split_once('\t'))
        .filter(|(start_time, id)| *start_time == server && live.contains(*id))
        .map(|(_, id)| id.to_string())
        .collect()
}

pub(crate) fn save_watchlist_state(watchlist: &std::collections::HashSet<String>) {
    let Some(path) = watchlist_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let server = crate::tmux_api::server_start_time();
    let mut entries: Vec<_> = watchlist.iter().collect();
    entries.sort();
    let mut contents = String::new();
    for id in entries {
        contents.push_str(&format!("{}\t{}\n", server, id));
    }
    let _ = fs::write(path, contents);
}

pub(crate) fn load_cursor_key() -> Option<String> {
    let contents = fs::read_to_string(cursor_path()?).ok()?;
    let key = contents.trim();
    if key.is_empty() { None } else { Some(key.to_string()) }
}

pub(crate) fn save_cursor_key(key: &str) {
    let Some(path) = cursor_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, format!("{}\n", key));
}
