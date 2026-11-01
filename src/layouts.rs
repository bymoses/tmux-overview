//! Shared, pi-aware on-disk layouts. All strings are hex-encoded, including names/cwds.
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::{Mutex, OnceLock};

use crate::model::{SavedPane, SavedSession, SavedWindow};
use crate::tmux_api::{tmux, tmux_status_ok};
use crate::util::{format_saved_time, hex_decode, hex_encode};

#[derive(Clone)]
pub(crate) struct Layout {
    pub(crate) path: String,
    pub(crate) label: String,
    pub(crate) panes: usize,
    pub(crate) sessions: Vec<SavedSession>,
}

pub(crate) fn state_dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_STATE_HOME").map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(base.join("tmux-overview"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub(crate) fn pane_count(sessions: &[SavedSession]) -> usize {
    sessions.iter().flat_map(|s| &s.windows).map(|w| w.panes.len()).sum()
}

pub(crate) fn encode(sessions: &[SavedSession]) -> String {
    let mut out = format!("v2\ncreated\t{}\npanes\t{}\n", now(), pane_count(sessions));
    for s in sessions {
        out.push_str(&format!("session\t{}\ndefault_path\t{}\n", hex_encode(&s.name), hex_encode(&s.default_path)));
        for w in &s.windows {
            out.push_str(&format!("window\t{}\t{}\t{}\t{}\t{}\t{}\n", hex_encode(&w.index), hex_encode(&w.name),
                if w.active { "1" } else { "0" }, hex_encode(&w.layout), hex_encode(&w.width), hex_encode(&w.height)));
            for p in &w.panes {
                out.push_str(&format!("pane\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", hex_encode(&p.index),
                    if p.active { "1" } else { "0" }, hex_encode(&p.cwd), hex_encode(&p.command), hex_encode(&p.id),
                    hex_encode(&p.pi_session), hex_encode(&p.parent), hex_encode(&p.worker)));
            }
        }
    }
    out
}

pub(crate) fn decode(contents: &str, label: &str) -> Result<Vec<SavedSession>, String> {
    if !matches!(contents.lines().next(), Some("v1" | "v2")) {
        return decode_legacy(contents, label);
    }
    let mut sessions: Vec<SavedSession> = Vec::new();
    for line in contents.lines().skip(1) {
        let p: Vec<&str> = line.split('\t').collect();
        match p.first().copied() {
            Some("session") if p.len() == 2 => sessions.push(SavedSession {
                name: hex_decode(p[1]), saved_label: label.into(), ..SavedSession::default()
            }),
            Some("default_path") if p.len() == 2 => {
                if let Some(s) = sessions.last_mut() { s.default_path = hex_decode(p[1]); }
            }
            Some("window") if p.len() == 5 || p.len() == 7 => {
                if let Some(s) = sessions.last_mut() {
                    s.windows.push(SavedWindow { index: hex_decode(p[1]), name: hex_decode(p[2]), active: p[3] == "1",
                        layout: hex_decode(p[4]), width: p.get(5).map(|s| hex_decode(s)).unwrap_or_else(|| "120".into()),
                        height: p.get(6).map(|s| hex_decode(s)).unwrap_or_else(|| "40".into()), panes: Vec::new() });
                }
            }
            Some("pane") if p.len() == 5 || p.len() == 9 => {
                if let Some(w) = sessions.last_mut().and_then(|s| s.windows.last_mut()) {
                    w.panes.push(SavedPane { index: hex_decode(p[1]), active: p[2] == "1", cwd: hex_decode(p[3]), command: hex_decode(p[4]),
                        id: p.get(5).map(|s| hex_decode(s)).unwrap_or_default(), pi_session: p.get(6).map(|s| hex_decode(s)).unwrap_or_default(),
                        parent: p.get(7).map(|s| hex_decode(s)).unwrap_or_default(), worker: p.get(8).map(|s| hex_decode(s)).unwrap_or_default() });
                }
            }
            _ => {}
        }
    }
    validate(sessions)
}

fn validate(sessions: Vec<SavedSession>) -> Result<Vec<SavedSession>, String> {
    let mut names = HashSet::new();
    for s in &sessions {
        if s.name.is_empty() || s.name.contains([':', '\n']) || !names.insert(s.name.clone()) || s.windows.is_empty() {
            return Err("invalid/duplicate session in layout".into());
        }
        let mut indices = HashSet::new();
        for w in &s.windows {
            if w.index.parse::<u32>().is_err() || !indices.insert(&w.index) || w.panes.is_empty() {
                return Err(format!("invalid window in {}", s.name));
            }
        }
    }
    if sessions.is_empty() { return Err("layout has no sessions".into()); }
    Ok(sessions)
}

fn decode_legacy(contents: &str, label: &str) -> Result<Vec<SavedSession>, String> {
    let mut sessions: Vec<SavedSession> = Vec::new();
    for line in contents.lines().filter(|s| !s.is_empty()) {
        let p: Vec<&str> = line.split('\t').collect();
        if p.len() < 9 { return Err("unrecognized layout format".into()); }
        let si = sessions.iter().position(|s| s.name == p[0]).unwrap_or_else(|| {
            sessions.push(SavedSession { name: p[0].into(), saved_label: label.into(), ..SavedSession::default() }); sessions.len()-1
        });
        let s = &mut sessions[si];
        let wi = s.windows.iter().position(|w| w.index == p[1]).unwrap_or_else(|| {
            s.windows.push(SavedWindow { index: p[1].into(), name: p[2].into(), layout: p[3].into(), width: p[4].into(), height: p[5].into(),
                active: s.windows.is_empty(), panes: Vec::new() }); s.windows.len()-1
        });
        let clean = |i: usize| p.get(i).filter(|s| **s != "-").copied().unwrap_or("").to_string();
        let w = &mut s.windows[wi];
        let active = if w.layout.starts_with('{') {
            w.layout.split_once(&format!("\"I\":\"{}\"", clean(9))).map(|(prefix,_)|
                prefix.rsplit('{').next().unwrap_or("").contains("\"a\":true")).unwrap_or(false)
        } else { w.panes.is_empty() };
        w.panes.push(SavedPane { index: w.panes.len().to_string(), active, cwd: p[6].into(), command: p[7].into(),
            pi_session: clean(8), id: clean(9), parent: clean(10), worker: clean(11) });
    }
    validate(sessions)
}

pub(crate) fn read_layout(path: &Path) -> Result<Layout, String> {
    let label = fs::metadata(path).and_then(|m| m.modified()).map(format_saved_time).unwrap_or_default();
    let contents = fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let sessions = decode(&contents, &label)?;
    Ok(Layout { path: path.to_string_lossy().into(), label, panes: pane_count(&sessions), sessions })
}

fn legacy_path() -> Option<PathBuf> {
    env::var_os("TMUX_SNAPSHOT").map(PathBuf::from).or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/tmux-snapshot.tsv")))
}

pub(crate) fn load_layouts() -> Vec<Layout> {
    // Rows refresh with pane titles. Immutable snapshots should not be reparsed (or
    // spawn `date` once each) every time a title changes.
    static CACHE: OnceLock<Mutex<Option<(String, Vec<Layout>)>>> = OnceLock::new();
    let dir = state_dir().map(|p| p.join("layouts"));
    let legacy = legacy_path();
    let modified = |p: &Option<PathBuf>| p.as_ref().and_then(|p| fs::metadata(p).ok()).and_then(|m| m.modified().ok());
    let key = format!("{:?}:{:?}:{:?}:{:?}:{}", dir, modified(&dir), legacy, modified(&legacy), now()/3600);
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some((old, layouts)) = cache.as_ref() { if old == &key { return layouts.clone(); } }
    let mut paths: Vec<PathBuf> = dir.and_then(|p| fs::read_dir(p).ok()).into_iter().flatten()
        .flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|s| s.to_str()) == Some("tsv")).collect();
    paths.sort(); paths.reverse();
    let mut layouts: Vec<Layout> = paths.iter().filter_map(|p| read_layout(p).ok()).collect();
    if let Some(path) = legacy.filter(|p| p.is_file()) {
        if let Ok(mut l) = read_layout(&path) { l.label = format!("{} (import)", l.label); layouts.push(l); }
    }
    *cache = Some((key, layouts.clone()));
    layouts
}

pub(crate) fn resolve_layout(value: Option<&str>) -> Result<Layout, String> {
    if let Some(value) = value {
        let path = PathBuf::from(value);
        if path.is_file() { return read_layout(&path); }
        if let Some(dir) = state_dir() {
            let path = dir.join("layouts").join(if value.ends_with(".tsv") { value.into() } else { format!("{}.tsv", value) });
            if path.is_file() { return read_layout(&path); }
        }
        return Err(format!("layout not found: {}", value));
    }
    load_layouts().into_iter().next().ok_or_else(|| "no saved layouts".into())
}

pub(crate) fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".snapshot-{}-{}", std::process::id(), now()));
    fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| { let _ = fs::remove_file(&tmp); e.to_string() })
}

fn prune(dir: &Path) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("tsv")).collect();
    entries.sort(); entries.reverse();
    let day = now()/86400;
    let mut days = HashSet::new();
    for (i, path) in entries.iter().enumerate() {
        let timestamp = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.split('-').next()).and_then(|s| s.parse::<u64>().ok());
        // UTC-day retention: newest 20, plus the newest snapshot on each of the last 7 days.
        let daily = timestamp.map(|t| { let d = t/86400; d <= day && day-d < 7 && days.insert(d) }).unwrap_or(false);
        if i >= 20 && !daily { fs::remove_file(path).map_err(|e| e.to_string())?; }
    }
    Ok(())
}

pub(crate) fn save_all() -> Result<Layout, String> {
    let sessions = capture(None)?;
    let dir = state_dir().ok_or("no state directory")?.join("layouts");
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    let path = dir.join(format!("{}-{:09}.tsv", now(), nanos));
    atomic_write(&path, &encode(&sessions))?;
    prune(&dir)?;
    read_layout(&path)
}

fn session_logs() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { walk(&path, out); }
            else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") { out.push(path); }
        }
    }
    let root = env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".pi/agent")));
    let mut out = Vec::new();
    if let Some(root) = root { walk(&root.join("sessions"), &mut out); }
    out
}

fn sid_from_screen(screen: &str) -> Option<&str> {
    screen.rsplit("sid: ").next().filter(|_| screen.contains("sid: "))?.split_whitespace().next()
}

fn pi_file(pane: &str, cwd: &str, exact: &str, logs: &mut Option<Vec<PathBuf>>) -> String {
    if !exact.is_empty() && Path::new(exact).is_file() { return exact.into(); }
    let screen = tmux(&["capture-pane", "-p", "-J", "-S", "-1000", "-t", pane]).unwrap_or_default();
    let files = logs.get_or_insert_with(session_logs);
    if let Some(sid) = sid_from_screen(&screen) {
        let suffix = format!("_{}.jsonl", sid);
        if let Some(p) = files.iter().find(|p| p.to_string_lossy().ends_with(&suffix)) { return p.to_string_lossy().into(); }
    }
    // Last resort, as in tmux-snapshot.sh: vote for visible plaintext unique to one recent log.
    let mut patterns: Vec<String> = screen.split(|c: char| !(c.is_ascii_alphanumeric() || " ,.;:()/+=-".contains(c)))
        .map(str::trim).filter(|s| s.len() >= 28).map(String::from).collect();
    patterns.sort(); patterns.dedup();
    if patterns.len() > 150 { patterns = patterns.split_off(patterns.len()-150); }
    let cutoff = SystemTime::now().checked_sub(Duration::from_secs(14*86400)).unwrap_or(UNIX_EPOCH);
    let dirname = format!("--{}--", cwd.trim_start_matches('/').replace('/', "-"));
    let in_dir = files.iter().any(|p| p.parent().and_then(|p| p.file_name()).is_some_and(|n| n == dirname.as_str()));
    let candidates: Vec<(String, String)> = files.iter().filter(|p| !in_dir || p.parent().and_then(|p| p.file_name()).is_some_and(|n| n == dirname.as_str()))
        .filter(|p| fs::metadata(p).and_then(|m| m.modified()).map(|m| m >= cutoff).unwrap_or(false))
        .filter_map(|p| fs::read_to_string(p).ok().map(|text| (p.to_string_lossy().into(), text))).collect();
    let mut votes: HashMap<String, usize> = HashMap::new();
    for pattern in patterns {
        let hits: Vec<_> = candidates.iter().filter(|(_, text)| text.contains(&pattern)).collect();
        if hits.len() == 1 { *votes.entry(hits[0].0.clone()).or_default() += 1; }
    }
    let mut ranked: Vec<_> = votes.into_iter().collect(); ranked.sort_by(|a,b| b.1.cmp(&a.1));
    if let Some((file, score)) = ranked.first() {
        if *score >= 2 && ranked.get(1).map(|v| v.1 < *score).unwrap_or(true) { return file.clone(); }
    }
    String::new()
}

pub(crate) fn capture(only: Option<&str>) -> Result<Vec<SavedSession>, String> {
    let raw = tmux(&["list-sessions", "-F", "#{session_id}\t#{session_name}\t#{@overview_default_path}"]).map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    let mut logs = None;
    for line in raw.lines() {
        let p: Vec<_> = line.split('\t').collect();
        if p.len() < 2 || only.is_some_and(|id| id != p[0]) || p[1].ends_with("__bg") { continue; }
        let mut s = SavedSession { name: p[1].into(), default_path: p.get(2).copied().unwrap_or("").into(), ..SavedSession::default() };
        let raw = tmux(&["list-windows", "-t", p[0], "-F", "#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}\t#{window_layout}\t#{window_width}\t#{window_height}"]).map_err(|e| e.to_string())?;
        for line in raw.lines() {
            let p: Vec<_> = line.split('\t').collect(); if p.len() != 7 { continue; }
            let mut w = SavedWindow { index: p[1].into(), name: p[2].into(), active: p[3] == "1", layout: p[4].into(), width: p[5].into(), height: p[6].into(), panes: Vec::new() };
            let raw = tmux(&["list-panes", "-t", p[0], "-F", "#{pane_id}\t#{pane_index}\t#{pane_active}\t#{pane_current_path}\t#{pane_current_command}\t#{@agent_kind}\t#{@agent_state}\t#{@agent_parent}\t#{@agent_name}\t#{@pi_session}"]).map_err(|e| e.to_string())?;
            for line in raw.lines() {
                let p: Vec<_> = line.split('\t').collect(); if p.len() != 10 { continue; }
                let pi = p[4] == "pi" || (p[5] == "pi" && p[6] != "exited");
                w.panes.push(SavedPane { id: p[0].into(), index: p[1].into(), active: p[2] == "1", cwd: p[3].into(),
                    command: if pi { "pi".into() } else { p[4].into() }, pi_session: if pi { pi_file(p[0], p[3], p[9], &mut logs) } else { String::new() },
                    parent: if pi { p[7].into() } else { String::new() }, worker: if pi { p[8].into() } else { String::new() } });
            }
            if !w.panes.is_empty() { s.windows.push(w); }
        }
        if !s.windows.is_empty() { sessions.push(s); }
    }
    validate(sessions)
}

/// Leaf order, not pane-index order. tmux-git's JSON layouts can interleave floating panes.
pub(crate) fn layout_order(layout: &str) -> Vec<String> {
    if layout.trim_start().starts_with('{') {
        return layout.split("\"I\":\"").skip(1).filter_map(|s| s.split('"').next()).map(String::from).collect();
    }
    // Traditional tmux: leaf cells are WIDTHxHEIGHT,X,Y,PANE_ID.
    let b = layout.as_bytes(); let mut ids = Vec::new(); let mut i = 0;
    while i < b.len() {
        if b[i] == b'x' && i > 0 && b[i-1].is_ascii_digit() {
            let mut j = i+1;
            while j < b.len() && b[j].is_ascii_digit() { j += 1; }
            let mut fields = Vec::new();
            while j < b.len() && b[j] == b',' {
                j += 1; let start = j;
                while j < b.len() && b[j].is_ascii_digit() { j += 1; }
                if start == j { break; } fields.push(&layout[start..j]);
            }
            if fields.len() == 3 { ids.push(format!("%{}", fields[2])); }
            i = j;
        } else { i += 1; }
    }
    ids
}

fn quote(value: &str) -> String { format!("'{}'", value.replace('\'', "'\\''")) }

fn command(args: &[&str]) -> Result<String, String> {
    tmux(args).map(|s| s.trim().into()).map_err(|e| format!("tmux {}: {}", args.join(" "), e))
}

#[derive(Default)]
pub(crate) struct RestoreReport {
    pub(crate) sessions: usize,
    pub(crate) panes: usize,
    pub(crate) dozed: usize,
    pub(crate) workers: usize,
    pub(crate) skipped: usize,
}

impl RestoreReport {
    pub(crate) fn message(&self) -> String {
        format!("restored {} sessions / {} panes / {} pi dozing / {} workers relinked; {} sessions skipped", self.sessions, self.panes, self.dozed, self.workers, self.skipped)
    }
}

fn valid_cwd(cwd: &str) -> String {
    let clean = cwd.strip_suffix(" (deleted)").unwrap_or(cwd);
    if !clean.is_empty() {
        for path in Path::new(clean).ancestors() {
            if path.is_absolute() && path.is_dir() { return path.to_string_lossy().into(); }
        }
    }
    env::var("HOME").unwrap_or_else(|_| "/".into())
}

fn doze_command(p: &SavedPane, parent: Option<&str>, fork: bool, unset: &[String]) -> String {
    let helper = env::var("PI_DOZE_START").unwrap_or_else(|_| "/data-shared/workspace/bms/devices/shared/agents/pi-kit/bin/pi-doze-start.sh".into());
    let mut cmd = String::from("exec env");
    for key in unset { cmd.push_str(&format!(" -u {}", quote(key))); }
    // A running server's environment is not necessarily the caller's environment.
    for key in ["XDG_RUNTIME_DIR", "PI_BIN"] {
        if let Ok(value) = env::var(key) { cmd.push_str(&format!(" {}", quote(&format!("{}={}", key, value)))); }
    }
    cmd.push_str(&format!(" {} --session {}", quote(&helper), quote(&p.pi_session)));
    if fork { cmd.push_str(" --fork"); }
    if let Some(parent) = parent { cmd.push_str(&format!(" --parent {} --worker {}", quote(parent), quote(&p.worker))); }
    cmd
}

fn live_files() -> HashSet<String> {
    let raw = tmux(&["list-panes", "-a", "-F", "#{pane_id}\t#{@pi_session}\t#{@agent_kind}\t#{@agent_state}"]).unwrap_or_default();
    let mut files = HashSet::new();
    let logs = session_logs();
    for line in raw.lines() {
        let p: Vec<_> = line.split('\t').collect(); if p.len() != 4 { continue; }
        if p[2] == "pi" && p[3] != "exited" && !p[1].is_empty() { files.insert(p[1].into()); }
        let screen = tmux(&["capture-pane", "-p", "-t", p[0]]).unwrap_or_default();
        if let Some(sid) = sid_from_screen(&screen) {
            let suffix = format!("_{}.jsonl", sid);
            for file in logs.iter().filter(|f| f.to_string_lossy().ends_with(&suffix)) { files.insert(file.to_string_lossy().into()); }
        }
    }
    // Catch pi on other sockets/outside tmux too. Pi closes JSONL between appends,
    // so inspect both command lines and open file descriptors.
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()) { continue; }
            let cmd = fs::read(entry.path().join("cmdline")).unwrap_or_default();
            let args: Vec<_> = cmd.split(|b| *b == 0).map(|b| String::from_utf8_lossy(b).into_owned()).collect();
            let pi = args.iter().take(3).any(|a| Path::new(a).file_name().and_then(|n| n.to_str()).is_some_and(|n| n == "pi" || n == "pi-doze.sh"));
            if !pi { continue; }
            for pair in args.windows(2) { if matches!(pair[0].as_str(), "--session" | "--fork") { files.insert(pair[1].clone()); } }
            if let Ok(fds) = fs::read_dir(entry.path().join("fd")) {
                for fd in fds.flatten() {
                    if let Ok(path) = fs::read_link(fd.path()) { if path.extension().and_then(|s| s.to_str()) == Some("jsonl") { files.insert(path.to_string_lossy().into()); } }
                }
            }
        }
    }
    files
}

pub(crate) fn restore(sessions: &[SavedSession], fork: bool) -> Result<RestoreReport, String> {
    let existing: HashSet<String> = tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default().lines().map(String::from).collect();
    let mut occupied = if fork { HashSet::new() } else { live_files() };
    let mut report = RestoreReport::default();
    let mut mapping: HashMap<String, String> = HashMap::new();
    let mut starts = Vec::new();
    // Include PI_* from the target server, not just the restore process.
    let server_env = tmux(&["show-environment", "-g"]).unwrap_or_default();
    let mut unset: HashSet<String> = env::vars().map(|(k,_)| k).filter(|k| k.starts_with("PI_")).collect();
    for line in server_env.lines() { let key = line.split('=').next().unwrap_or("").trim_start_matches('-'); if key.starts_with("PI_") { unset.insert(key.into()); } }
    unset.insert("PI_PARENT".into()); unset.insert("PI_WORKER".into());
    let mut unset: Vec<_> = unset.into_iter().collect(); unset.sort();
    for s in sessions {
        if existing.contains(&s.name) { report.skipped += 1; continue; }
        let mut first = true;
        for w in &s.windows {
            let order = layout_order(&w.layout);
            let mut panes: Vec<_> = w.panes.iter().collect();
            panes.sort_by_key(|p| order.iter().position(|id| *id == p.id).unwrap_or(usize::MAX));
            let mut target = String::new();
            for (i,p) in panes.iter().enumerate() {
                let cwd = valid_cwd(&p.cwd);
                if cwd != p.cwd { eprintln!(":: {}:{} cwd fallback: {} → {}", s.name, w.index, p.cwd, cwd); }
                // Explicit shell holds the pane until all ids exist; no pi can race parent mapping.
                let shell = "exec /bin/sh";
                let created = if i == 0 {
                    if first {
                        let id = command(&["new-session", "-d", "-P", "-F", "#{pane_id}\t#{window_id}", "-s", &s.name, "-n", &w.name,
                            "-x", if w.width.is_empty() { "120" } else { &w.width }, "-y", if w.height.is_empty() { "40" } else { &w.height }, "-c", &cwd, shell])?;
                        if id.is_empty() { return Err(format!("failed creating {}", s.name)); }
                        let wid = id.split('\t').nth(1).ok_or("missing window id")?;
                        let index = command(&["display-message", "-p", "-t", wid, "#{window_index}"])?;
                        if index != w.index && !tmux_status_ok(&["move-window", "-s", wid, "-t", &format!("={}:{}", s.name, w.index)]) {
                            return Err(format!("failed setting window index {}:{}", s.name, w.index));
                        }
                        if !s.default_path.is_empty() {
                            let sid = command(&["display-message", "-p", "-t", wid, "#{session_id}"])?;
                            if sid.is_empty() { return Err("empty restored session id".into()); }
                            command(&["set-option", "-t", &sid, "@overview_default_path", &s.default_path])?;
                        }
                        report.sessions += 1; first = false; id
                    } else {
                        command(&["new-window", "-d", "-P", "-F", "#{pane_id}\t#{window_id}", "-t", &format!("={}:{}", s.name, w.index), "-n", &w.name, "-c", &cwd, shell])?
                    }
                } else {
                    command(&["split-window", "-d", "-P", "-F", "#{pane_id}\t#{window_id}", "-t", &target, "-c", &cwd, shell])?
                };
                let (pid, wid) = created.split_once('\t').ok_or_else(|| format!("failed creating pane in {}:{}", s.name, w.index))?;
                if pid.is_empty() || wid.is_empty() { return Err("empty restored pane/window id".into()); }
                target = wid.into();
                if !p.id.is_empty() { mapping.insert(p.id.clone(), pid.into()); }
                starts.push((pid.to_string(), (*p).clone())); report.panes += 1;
                if i > 0 { command(&["select-layout", "-t", &target, "tiled"])?; }
            }
            if !w.width.is_empty() && !w.height.is_empty() { command(&["resize-window", "-t", &target, "-x", &w.width, "-y", &w.height])?; }
            if !w.layout.is_empty() && !tmux_status_ok(&["select-layout", "-t", &target, &w.layout]) {
                return Err(format!("failed applying layout {}:{}", s.name, w.index));
            }
            // Names must survive even if server-wide automatic-rename is enabled.
            command(&["set-window-option", "-t", &target, "automatic-rename", "off"])?;
            if let Some(p) = panes.iter().find(|p| p.active) {
                if let Some(pid) = mapping.get(&p.id) { command(&["select-pane", "-t", pid])?; }
            }
        }
        if let Some(w) = s.windows.iter().find(|w| w.active).or_else(|| s.windows.first()) {
            command(&["select-window", "-t", &format!("={}:{}", s.name, w.index)])?;
        }
    }
    for (pid,p) in starts {
        if p.command == "pi" {
            if p.pi_session.is_empty() || !Path::new(&p.pi_session).is_file() || (!fork && !occupied.insert(p.pi_session.clone())) {
                eprintln!(":: {} pi session missing/already open; leaving shell", pid); continue;
            }
            let parent = mapping.get(&p.parent).map(String::as_str);
            let cmd = doze_command(&p, parent, fork, &unset);
            if !tmux_status_ok(&["respawn-pane", "-k", "-t", &pid, "-c", &valid_cwd(&p.cwd), &cmd]) { return Err(format!("failed starting doze in {}", pid)); }
            report.dozed += 1; if parent.is_some() { report.workers += 1; }
        } else if matches!(p.command.as_str(), "nvim" | "yazi") {
            let cmd = format!("{}; exec /bin/sh", p.command);
            if !tmux_status_ok(&["respawn-pane", "-k", "-t", &pid, "-c", &valid_cwd(&p.cwd), &cmd]) { return Err(format!("failed starting {}", p.command)); }
        }
    }
    Ok(report)
}
