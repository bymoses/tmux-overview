use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

pub(crate) fn state_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("tmux-overview").join("expanded.tsv"))
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
