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

/// One entry per line; blank lines dropped.
fn load_lines(name: &str) -> Vec<String> {
    scalar_state_path(name)
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(ToString::to_string).collect())
        .unwrap_or_default()
}

fn save_lines(name: &str, lines: &[String]) {
    let Some(path) = scalar_state_path(name) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, lines.iter().map(|l| format!("{}\n", l)).collect::<String>());
}

/// Agents mode pins: conversation ids (pane ids for agents without a log), so a pin
/// survives a reboot restore that gives the agent a new pane.
/// Agents mode: show each agent's context window use (`c`).
pub(crate) fn load_agents_context() -> bool {
    load_boolean_state("agents-context", false)
}

pub(crate) fn save_agents_context(on: bool) {
    save_boolean_state("agents-context", on);
}

pub(crate) fn load_agent_pins() -> std::collections::HashSet<String> {
    load_lines("agent-pins").into_iter().collect()
}

pub(crate) fn save_agent_pins(pins: &std::collections::HashSet<String>) {
    let mut lines: Vec<String> = pins.iter().cloned().collect();
    lines.sort();
    save_lines("agent-pins", &lines);
}

/// Agents mode project order: node names (`dgt`, `dgt/eclion`); earlier sorts first among siblings.
pub(crate) fn load_agents_order() -> Vec<String> {
    load_lines("agents-order")
}

pub(crate) fn save_agents_order(order: &[String]) {
    save_lines("agents-order", order);
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

/// Agents mode's selected row (`a:<conversation>` or `p:<project>`), kept across openings.
pub(crate) fn load_agents_cursor() -> Option<String> {
    let contents = fs::read_to_string(cache_dir()?.join("agents-cursor")).ok()?;
    let key = contents.trim();
    if key.is_empty() { None } else { Some(key.to_string()) }
}

pub(crate) fn save_agents_cursor(key: &str) {
    let Some(dir) = cache_dir() else {
        return;
    };
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("agents-cursor"), format!("{}\n", key));
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
