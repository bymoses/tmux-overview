//! Agents mode: pi agents grouped by project (top session segment), as a spawn tree
//! (pi-kit publishes @agent_parent/@agent_name on workers), with a live preview of the
//! selected agent beside the list. The project the overview was opened from comes first.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::preview::{ansi_fit, capture_preview};
use crate::terminal::{read_key_timeout, term_size};
use crate::tmux_api::{tmux, tmux_status_ignore};
use crate::util::{plain_truncate, text_width, visible_truncate_ansi};

const SELECTED: &str = "\x1b[48;2;255;100;150m\x1b[38;2;0;0;0m";
const PROJECT_COLOR: &str = "\x1b[1;38;5;252m";
const WORKING: &str = "\x1b[38;5;114m";
/// Same colour as the overview's "stopped, not reviewed yet" badge.
const FINISHED: &str = "\x1b[1;38;2;255;100;150m";
const DIM: &str = "\x1b[38;5;244m";
const RULE: &str = "\x1b[38;5;238m";
const REFRESH_MS: u128 = 500;

pub(crate) enum Exit {
    Quit,
    Overview,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Agent {
    pub pane: String,
    pub session: String,
    pub window_index: String,
    pub window_name: String,
    pub state: String,
    pub state_at: u64,
    pub parent: String,
    pub name: String,
    /// Window bell flag: pi rang it when the agent stopped; tmux clears it once the window is seen.
    pub bell: bool,
}

/// Tree node of a session: its path without helper segments (`__float`, `__agents`, 
/// `__bg`): `dgt/eclion/__float` \u{2192} `dgt/eclion`.
fn node_of(session: &str) -> String {
    session.split('/').filter(|s| !s.starts_with("__")).collect::<Vec<_>>().join("/")
}

impl Agent {
    pub fn node(&self) -> String {
        node_of(&self.session)
    }

    /// Worker name, else `<helper> <window>`: `ws`, `float pi`, `agents tasks-web`.
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let helpers: Vec<&str> = self
            .session
            .split('/')
            .filter_map(|s| s.strip_prefix("__"))
            .collect();
        if helpers.is_empty() {
            self.window_name.clone()
        } else {
            format!("{} {}", helpers.join("/"), self.window_name)
        }
    }

    fn working(&self) -> bool {
        self.state == "working"
    }

    /// Stopped and not looked at yet, as in the overview's alert badge.
    fn finished(&self) -> bool {
        self.bell && !self.working()
    }

    fn dot(&self) -> &'static str {
        if self.working() || self.finished() {
            "●"
        } else {
            "○"
        }
    }

    fn colour(&self) -> &'static str {
        if self.finished() {
            FINISHED
        } else if self.working() {
            WORKING
        } else {
            DIM
        }
    }
}

/// Live pi panes (pi-kit sets @agent_kind=pi); exited and dead panes are left out.
pub(crate) fn collect_agents() -> Vec<Agent> {
    let raw = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{@agent_kind}\t#{@agent_state}\t#{@agent_state_at}\t#{@agent_parent}\t#{pane_dead}\t#{window_bell_flag}\t#{@agent_name}",
    ])
    .unwrap_or_default();
    raw.lines()
        .filter_map(|line| {
            let p: Vec<&str> = line.split('\t').collect();
            if p.len() < 11 || p[4] != "pi" || p[5] == "exited" || p[8] == "1" || p[1].starts_with("_popup_") {
                return None;
            }
            Some(Agent {
                pane: p[0].to_string(),
                session: p[1].to_string(),
                window_index: p[2].to_string(),
                window_name: p[3].to_string(),
                state: if p[5].is_empty() { "starting".to_string() } else { p[5].to_string() },
                state_at: p[6].parse().unwrap_or(0),
                parent: p[7].to_string(),
                bell: p[9] == "1",
                name: p[10].to_string(),
            })
        })
        .collect()
}

/// `Project` is a session-tree node (`dgt`, `dgt/eclion`); counts cover its whole subtree.
#[derive(Clone)]
pub(crate) enum ARow {
    Project { name: String, depth: usize, agents: usize, working: usize, finished: usize, expanded: bool },
    Agent { idx: usize, prefix: String, children: usize },
}

impl ARow {
    fn key(&self, agents: &[Agent]) -> String {
        match self {
            ARow::Project { name, .. } => format!("p:{}", name),
            ARow::Agent { idx, .. } => agents[*idx].pane.clone(),
        }
    }
}

fn expanded_key(project: &str) -> String {
    format!("agents:{}", project)
}

/// Default: open along the current session's branch, closed elsewhere.
fn project_expanded(expanded: &HashMap<String, bool>, node: &str, current: &str) -> bool {
    expanded.get(&expanded_key(node)).copied().unwrap_or_else(|| in_subtree(current, node))
}

fn window_order(a: &Agent) -> (String, usize, String) {
    (a.session.clone(), a.window_index.parse().unwrap_or(0), a.pane.clone())
}

fn in_subtree(node: &str, root: &str) -> bool {
    node == root || node.starts_with(&format!("{}/", root))
}

/// Rows: the session tree (`dgt` \u{2192} `dgt/eclion`), current branch first, each node's own
/// agents as a spawn tree before its child nodes. A parent that is not a live agent
/// (recruited pane, gone, or after a reboot) makes the agent a root of its node.
pub(crate) fn build_rows(agents: &[Agent], expanded: &HashMap<String, bool>, current: &str) -> Vec<ARow> {
    let live: HashSet<&str> = agents.iter().map(|a| a.pane.as_str()).collect();
    let nodes_of: Vec<String> = agents.iter().map(Agent::node).collect();
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut roots: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, a) in agents.iter().enumerate() {
        if !a.parent.is_empty() && a.parent != a.pane && live.contains(a.parent.as_str()) {
            children.entry(a.parent.as_str()).or_default().push(i);
        } else {
            roots.entry(nodes_of[i].as_str()).or_default().push(i);
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|&i| (agents[i].label(), agents[i].pane.clone()));
    }
    for list in roots.values_mut() {
        list.sort_by_key(|&i| window_order(&agents[i]));
    }

    // Every node with agents, plus its ancestors.
    let mut all: HashSet<String> = HashSet::new();
    for n in &nodes_of {
        let parts: Vec<&str> = n.split('/').collect();
        for k in 1..=parts.len() {
            all.insert(parts[..k].join("/"));
        }
    }
    let mut kids: HashMap<String, Vec<String>> = HashMap::new();
    for n in &all {
        let parent = n.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
        kids.entry(parent).or_default().push(n.clone());
    }
    // Branch holding the current session first, then by name.
    for list in kids.values_mut() {
        list.sort_by_key(|n| (!in_subtree(current, n), n.clone()));
    }

    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(String, usize)> = kids.get("").cloned().unwrap_or_default().into_iter().rev().map(|n| (n, 0)).collect();
    while let Some((node, depth)) = stack.pop() {
        let members: Vec<usize> = (0..agents.len()).filter(|&i| in_subtree(&nodes_of[i], &node)).collect();
        let open = project_expanded(expanded, &node, current);
        rows.push(ARow::Project {
            name: node.clone(),
            depth,
            agents: members.len(),
            working: members.iter().filter(|&&i| agents[i].working()).count(),
            finished: members.iter().filter(|&&i| agents[i].finished()).count(),
            expanded: open,
        });
        if !open {
            continue;
        }
        let lead = "  ".repeat(depth + 1);
        for &root in roots.get(node.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
            push_tree(&mut rows, agents, &children, root, lead.clone(), String::new(), &mut seen);
        }
        for child in kids.get(&node).cloned().unwrap_or_default().into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    rows
}

fn push_tree(
    rows: &mut Vec<ARow>,
    agents: &[Agent],
    children: &HashMap<&str, Vec<usize>>,
    idx: usize,
    lead: String,
    branch: String,
    seen: &mut HashSet<usize>,
) {
    if !seen.insert(idx) {
        return;
    }
    let kids = children.get(agents[idx].pane.as_str()).cloned().unwrap_or_default();
    rows.push(ARow::Agent { idx, prefix: format!("{}{}", lead, branch), children: kids.len() });
    // Children hang under the parent's name, not its dot: a connector in the dot's column
    // reads as a sibling.
    let below = format!(
        "{}{}",
        lead,
        match branch.as_str() {
            "├─ " => "│    ",
            "└─ " => "     ",
            _ => "  ",
        }
    );
    for (n, kid) in kids.iter().enumerate() {
        let b = if n + 1 == kids.len() { "└─ " } else { "├─ " };
        push_tree(rows, agents, children, *kid, below.clone(), b.to_string(), seen);
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn age(at: u64) -> String {
    if at == 0 {
        return String::new();
    }
    let s = now_epoch().saturating_sub(at);
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{}s", s)
    }
}

fn state_text(a: &Agent) -> String {
    format!("{} {}", a.state, age(a.state_at)).trim_end().to_string()
}

/// One list row, plain text plus its colour. Columns: tree, ●/○, label, state + age, ⑂ children.
fn row_text(row: &ARow, agents: &[Agent], name_w: usize) -> (String, &'static str) {
    match row {
        ARow::Project { name, depth, agents: n, working, finished, expanded } => {
            let arrow = if *expanded { "▾" } else { "▸" };
            let leaf = name.rsplit('/').next().unwrap_or(name);
            let busy = if *working > 0 { format!(" · {} working", working) } else { String::new() };
            let done = if *finished > 0 { format!(" · {} finished", finished) } else { String::new() };
            (
                format!("{}{} {}  {} agent{}{}{}", "  ".repeat(*depth), arrow, leaf, n, if *n == 1 { "" } else { "s" }, busy, done),
                // A collapsed branch hiding finished agents takes their colour.
                if *finished > 0 && !*expanded { FINISHED } else { PROJECT_COLOR },
            )
        }
        ARow::Agent { idx, prefix, children } => {
            let a = &agents[*idx];
            let head = format!("{}{} {}", prefix, a.dot(), a.label());
            let pad = name_w.saturating_sub(text_width(&head));
            let kids = if *children > 0 { format!("   ⑂ {}", children) } else { String::new() };
            let text = format!("{}{}  {:<12}{}", head, " ".repeat(pad), state_text(a), kids);
            (text, a.colour())
        }
    }
}

fn fit(s: &str, w: usize) -> String {
    let t = plain_truncate(s, w);
    let pad = w.saturating_sub(text_width(&t));
    format!("{}{}", t, " ".repeat(pad))
}

fn preview_lines(row: Option<&ARow>, agents: &[Agent], w: usize, h: usize) -> Vec<String> {
    let mut out = Vec::new();
    match row {
        Some(ARow::Agent { idx, children, .. }) => {
            let a = &agents[*idx];
            let kids = if *children > 0 { format!(" · ⑂ {}", children) } else { String::new() };
            out.push(format!(
                "{}{}\x1b[0m",
                DIM,
                fit(&format!(" {} · {}:{} {} · {}{}", a.pane, a.session, a.window_index, a.window_name, state_text(a), kids), w)
            ));
            let body = capture_preview(&a.pane, h.saturating_sub(1));
            for _ in 0..h.saturating_sub(1).saturating_sub(body.len()) {
                out.push(" ".repeat(w));
            }
            for line in body {
                out.push(format!("{}\x1b[0m", ansi_fit(&line, w)));
            }
        }
        Some(ARow::Project { name, .. }) => {
            out.push(format!("{}{}\x1b[0m", DIM, fit(&format!(" {}", name), w)));
            let mut list: Vec<&Agent> = agents.iter().filter(|a| in_subtree(&a.node(), name)).collect();
            list.sort_by_key(|a| (!a.working(), window_order(a)));
            for a in list {
                let line = fit(&format!(" {} {}  {}  {}:{}", a.dot(), a.label(), state_text(a), a.session, a.window_index), w);
                out.push(format!("{}{}\x1b[0m", a.colour(), line));
            }
        }
        None => out.push(fit(" no pi agents running", w)),
    }
    out.truncate(h);
    while out.len() < h {
        out.push(" ".repeat(w));
    }
    out
}

fn render(rows: &[ARow], agents: &[Agent], selected: usize, show_preview: bool, list_percent: usize) -> io::Result<()> {
    let (h, w) = term_size();
    let body_h = h.max(4);
    let list_h = body_h.saturating_sub(1);
    let list_w = if show_preview { (w * list_percent / 100).clamp(30, w.saturating_sub(21).max(30)) } else { w };
    let preview_w = w.saturating_sub(list_w + 1);

    let name_w = rows
        .iter()
        .filter_map(|r| match r {
            ARow::Agent { idx, prefix, .. } => Some(text_width(prefix) + 2 + text_width(&agents[*idx].label())),
            _ => None,
        })
        .max()
        .unwrap_or(10)
        .min(list_w.saturating_sub(16).max(10));

    let preview = if show_preview && preview_w > 0 {
        preview_lines(rows.get(selected), agents, preview_w, body_h)
    } else {
        Vec::new()
    };

    let scrolloff = 4.min(list_h.saturating_sub(1) / 2);
    let offset = selected.saturating_sub(list_h.saturating_sub(scrolloff + 1));
    let mut out = String::from("\x1b[?2026h\x1b[H");
    for y in 0..body_h {
        let content_w = list_w.saturating_sub(1);
        if y + 1 == body_h {
            let hint = " A overview · enter go · h/l fold · v preview · +/- width · q quit";
            out.push_str(&format!("{}{}\x1b[0m", RULE, fit(hint, list_w)));
        } else if let Some(row) = rows.get(offset + y) {
            let (text, colour) = row_text(row, agents, name_w);
            if offset + y == selected {
                out.push_str(&format!("{} {}\x1b[0m", SELECTED, fit(&text, content_w)));
            } else {
                out.push_str(&format!("{} {}\x1b[0m", colour, fit(&text, content_w)));
            }
        } else if rows.is_empty() && y == 0 {
            out.push_str(&fit(" no pi agents running", list_w));
        } else {
            out.push_str(&" ".repeat(list_w));
        }
        if !preview.is_empty() {
            out.push_str(&format!("{}│\x1b[0m", RULE));
            out.push_str(&visible_truncate_ansi(&preview[y], preview_w));
        }
        // No CRLF after the last row: it would scroll the screen up one line.
        out.push_str(if y + 1 == body_h { "\x1b[K" } else { "\x1b[K\r\n" });
    }
    out.push_str("\x1b[?2026l");
    print!("{}", out);
    io::stdout().flush()
}

/// Tree node of the session the overview was opened from.
fn current_project() -> String {
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let session = tmux(&["display-message", "-p", "-t", &pane, "#{session_name}"]).unwrap_or_default();
    node_of(session.trim())
}

fn select_key(rows: &[ARow], agents: &[Agent], key: &str) -> Option<usize> {
    rows.iter().position(|r| r.key(agents) == key)
}

/// The agents screen. Shares the overview's terminal, expanded map and preview toggle;
/// drains the overview's background channels so they do not pile up while it runs.
pub(crate) fn run<A, B>(
    expanded: &mut HashMap<String, bool>,
    show_preview: &mut bool,
    drain_a: &Receiver<A>,
    drain_b: &Receiver<B>,
) -> io::Result<Exit> {
    print!("\x1b[2J");
    let current = current_project();
    let mut list_percent = 40usize;
    let mut agents = collect_agents();
    let mut rows = build_rows(&agents, expanded, &current);
    // Start on the first agent of the current project, else the first row.
    let mut selected = rows.iter().position(|r| matches!(r, ARow::Agent { .. })).unwrap_or(0);
    let mut last_refresh = Instant::now();
    let mut dirty = true;

    loop {
        while drain_a.try_recv().is_ok() {}
        while drain_b.try_recv().is_ok() {}
        if last_refresh.elapsed().as_millis() >= REFRESH_MS {
            let key = rows.get(selected).map(|r| r.key(&agents));
            agents = collect_agents();
            rows = build_rows(&agents, expanded, &current);
            if let Some(i) = key.and_then(|k| select_key(&rows, &agents, &k)) {
                selected = i;
            }
            last_refresh = Instant::now();
            dirty = true; // the preview is live
        }
        selected = selected.min(rows.len().saturating_sub(1));
        if dirty {
            render(&rows, &agents, selected, *show_preview, list_percent)?;
            dirty = false;
        }

        let Some(key) = read_key_timeout()? else { continue };
        dirty = true;
        let toggle = |expanded: &mut HashMap<String, bool>, name: &str, open: Option<bool>| {
            let now = project_expanded(expanded, name, &current);
            expanded.insert(expanded_key(name), open.unwrap_or(!now));
            crate::state::save_expanded_state(expanded);
        };
        match key.as_slice() {
            b"q" | [3] | [0x1b, b'f'] | [0x1b, b's'] => return Ok(Exit::Quit),
            b"A" => {
                print!("\x1b[2J");
                return Ok(Exit::Overview);
            }
            b"j" | [0x1b, b'[', b'B'] => selected = (selected + 1).min(rows.len().saturating_sub(1)),
            b"k" | [0x1b, b'[', b'A'] => selected = selected.saturating_sub(1),
            b"g" => selected = 0,
            b"G" => selected = rows.len().saturating_sub(1),
            b"v" => {
                *show_preview = !*show_preview;
                crate::state::save_show_preview_state(*show_preview);
            }
            b"+" | b"=" => list_percent = (list_percent + 5).min(70),
            b"-" | b"_" => list_percent = list_percent.saturating_sub(5).max(20),
            b" " | b"l" | [0x1b, b'[', b'C'] | b"h" | [0x1b, b'[', b'D'] => {
                let open = match key.as_slice() {
                    b" " => None,
                    b"l" | [0x1b, b'[', b'C'] => Some(true),
                    _ => Some(false),
                };
                match rows.get(selected).cloned() {
                    Some(ARow::Project { name, .. }) => toggle(expanded, &name, open),
                    // h on an agent: go to its parent row (the project row for a root).
                    Some(ARow::Agent { idx, .. }) if open == Some(false) => {
                        let parent = agents[idx].parent.clone();
                        let project = format!("p:{}", agents[idx].node());
                        selected = select_key(&rows, &agents, &parent)
                            .or_else(|| select_key(&rows, &agents, &project))
                            .unwrap_or(selected);
                        continue;
                    }
                    _ => continue,
                }
                let key = rows.get(selected).map(|r| r.key(&agents));
                rows = build_rows(&agents, expanded, &current);
                if let Some(i) = key.and_then(|k| select_key(&rows, &agents, &k)) {
                    selected = i;
                }
            }
            b"\r" | b"\n" => match rows.get(selected).cloned() {
                Some(ARow::Agent { idx, .. }) => {
                    let pane = agents[idx].pane.clone();
                    tmux_status_ignore(&["switch-client", "-t", &pane]);
                    tmux_status_ignore(&["select-window", "-t", &pane]);
                    tmux_status_ignore(&["select-pane", "-t", &pane]);
                    return Ok(Exit::Quit);
                }
                Some(ARow::Project { name, .. }) => {
                    toggle(expanded, &name, None);
                    rows = build_rows(&agents, expanded, &current);
                }
                None => {}
            },
            _ => dirty = false,
        }
    }
}
