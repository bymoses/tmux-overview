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

fn boolean_state_path(name: &str) -> Option<PathBuf> {
    Some(cache_dir()?.join(name))
}

fn load_boolean_state(name: &str, default: bool) -> bool {
    let Some(path) = boolean_state_path(name) else {
        return default;
    };
    match fs::read_to_string(path).ok().as_deref().map(str::trim) {
        Some("1" | "true" | "on") => true,
        Some("0" | "false" | "off") => false,
        _ => default,
    }
}

fn save_boolean_state(name: &str, value: bool) {
    let Some(path) = boolean_state_path(name) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, if value { "1\n" } else { "0\n" });
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

pub(crate) fn load_watchlist_state() -> std::collections::HashSet<String> {
    let Some(path) = watchlist_path() else {
        return std::collections::HashSet::new();
    };
    let Ok(contents) = fs::read_to_string(path) else {
        return std::collections::HashSet::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub(crate) fn save_watchlist_state(watchlist: &std::collections::HashSet<String>) {
    let Some(path) = watchlist_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut entries: Vec<_> = watchlist.iter().collect();
    entries.sort();
    let mut contents = String::new();
    for id in entries {
        contents.push_str(id);
        contents.push('\n');
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
