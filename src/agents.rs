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
use crate::util::{plain_truncate, strip_ansi, text_width, visible_truncate_ansi};

const SELECTED: &str = "\x1b[48;2;255;100;150m\x1b[38;2;0;0;0m";
/// Session (project) rows: bold full white, the brightest text in the list.
const PROJECT_COLOR: &str = "\x1b[1;38;5;255m";
/// Workers (spawned by another agent): dimmer than top-level agents, which are the ones to talk to.
const WORKER_NAME: &str = "\x1b[38;5;240m";
const WORKING: &str = "\x1b[38;5;114m";
/// Resting on its own bg job or scheduled check: it will wake by itself.
const WAITING: &str = "\x1b[38;5;179m";
/// The run stopped on a failed request (pi-kit's "error" state): look at it.
const ERROR: &str = "\x1b[1;38;5;203m";
/// Same colour as the overview's "stopped, not reviewed yet" badge.
const FINISHED: &str = "\x1b[1;38;2;255;100;150m";
const DIM: &str = "\x1b[38;5;244m";
const RULE: &str = "\x1b[38;5;238m";
/// Agent names: soft grey under the bold white session rows; state colours stay on dots and state text.
const NAME: &str = "\x1b[38;5;248m";
/// Fold arrows, tree lines and plain counts: below DIM so names and coloured counts stand out.
const FAINT: &str = "\x1b[38;5;240m";
const REFRESH_MS: u128 = 500;
const PIN: &str = "\u{26b2}";
/// After the name of a session with `/compaction off` (pi-kit's status line shows `\u{2298}cmp`).
const NO_COMPACT: &str = "\u{2298}";

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
    /// Window bell flag plus this pane's @agent_unseen (set by tmux-agent-alert.sh): the bell
    /// is per window, so in a shared window only the mark says which pane stopped.
    pub bell: bool,
    /// Open conversation log (pi-kit's @pi_session); empty before the first message.
    pub session_file: String,
    /// pi-kit's @agent_bg: detached bash jobs and scheduled checks it is waiting for.
    pub bg: usize,
    /// pi-kit's @agent_error: why the last run failed, while the state is "error".
    pub error: String,
    /// pi-kit's @agent_ctx: context tokens in use (None = unknown, e.g. just compacted) and
    /// the model's window (0 = not published: pi-kit before 0.3.21).
    pub ctx_tokens: Option<u64>,
    pub ctx_window: u64,
    /// pi-kit's @agent_compaction = "off": `/compaction off`, so it grows past the auto-compaction threshold.
    pub compaction_off: bool,
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

    /// Pin identity: the conversation log name, which a reboot restore keeps; else the pane.
    pub fn pin_key(&self) -> String {
        std::path::Path::new(&self.session_file)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("pane:{}", self.pane))
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

    /// pi-kit's resting state while a detached bash job or scheduled check is pending.
    fn waiting(&self) -> bool {
        self.state == "waiting"
    }

    /// The last run ended on a failed request (connection, 5xx, auth).
    fn errored(&self) -> bool {
        self.state == "error"
    }

    /// Context at or past CTX_ALERT: time to compact or hand off.
    fn ctx_full(&self) -> bool {
        self.ctx_tokens.is_some_and(|t| t >= CTX_ALERT)
    }

    /// Stopped and not looked at yet, as in the overview's alert badge.
    fn finished(&self) -> bool {
        self.bell && !self.working() && !self.waiting() && !self.errored()
    }

    fn dot(&self) -> &'static str {
        if self.errored() {
            "\u{2717}"
        } else if self.waiting() {
            "\u{25d0}"
        } else if self.working() || self.finished() {
            "●"
        } else {
            "○"
        }
    }

    fn colour(&self) -> &'static str {
        if self.errored() {
            ERROR
        } else if self.finished() {
            FINISHED
        } else if self.working() {
            WORKING
        } else if self.waiting() {
            WAITING
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
        "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{@agent_kind}\t#{@agent_state}\t#{@agent_state_at}\t#{@agent_parent}\t#{pane_dead}\t#{window_bell_flag}\t#{@agent_name}\t#{@pi_session}\t#{@agent_unseen}\t#{@agent_bg}\t#{@agent_error}\t#{@agent_ctx}\t#{@agent_compaction}",
    ])
    .unwrap_or_default();
    raw.lines()
        .filter_map(|line| {
            let p: Vec<&str> = line.split('\t').collect();
            if p.len() < 16 || p[4] != "pi" || p[5] == "exited" || p[8] == "1" || p[1].starts_with("_popup_") {
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
                bell: p[9] == "1" && p[12] == "1",
                name: p[10].to_string(),
                session_file: p[11].to_string(),
                bg: p[13].parse().unwrap_or(0),
                error: p[14].to_string(),
                ctx_tokens: p[15].split(' ').next().and_then(|t| t.parse().ok()),
                ctx_window: p[15].split(' ').nth(1).and_then(|t| t.parse().ok()).unwrap_or(0),
                compaction_off: p.get(16) == Some(&"off"),
            })
        })
        .collect()
}

/// `Project` is a session-tree node (`dgt`, `dgt/eclion`); counts cover its whole subtree.
#[derive(Clone)]
pub(crate) enum ARow {
    Project { name: String, depth: usize, agents: usize, counts: Counts, expanded: bool },
    /// `working` counts hidden descendants while folded (`expanded` false). A finished worker
    /// is not flagged on its parent: only top-level agents waiting on the user matter.
    Agent { idx: usize, prefix: String, children: usize, expanded: bool, counts: Counts, pinned: bool, worker: bool },
}

/// Agents per state, drawn as coloured counts; zeros left out.
#[derive(Clone, Copy, Default)]
pub(crate) struct Counts {
    working: usize,
    waiting: usize,
    finished: usize,
    errors: usize,
    /// Context at or past CTX_ALERT: the session wants a compaction.
    full: usize,
}

/// Context sizes around pi's auto-compaction at 250k: amber once past it (an opted-out or
/// mid-run session), red and \u{25b2} at 350k, where it is clearly overdue.
const CTX_ALERT: u64 = 350_000;
const CTX_WARN: u64 = 250_000;
/// Marks an agent, or hidden agents, past CTX_ALERT.
const FULL_MARK: &str = "\u{25b2}";

impl Counts {
    /// `finished` only for agents `notify` accepts: a worker that finished reports to its parent.
    fn of<'a>(list: impl Iterator<Item = &'a Agent>, notify: impl Fn(&Agent) -> bool) -> Counts {
        let mut c = Counts::default();
        for a in list {
            c.working += a.working() as usize;
            c.waiting += a.waiting() as usize;
            c.finished += (a.finished() && notify(a)) as usize;
            c.errors += a.errored() as usize;
            c.full += a.ctx_full() as usize;
        }
        c
    }
}

/// What a row layout depends on besides the agents.
pub(crate) struct Layout<'a> {
    pub expanded: &'a HashMap<String, bool>,
    pub pins: &'a HashSet<String>,
    pub order: &'a [String],
    pub current: &'a str,
}

struct Tree<'a> {
    agents: &'a [Agent],
    children: HashMap<&'a str, Vec<usize>>,
    layout: &'a Layout<'a>,
}

impl ARow {
    fn key(&self, agents: &[Agent]) -> String {
        match self {
            ARow::Project { name, .. } => format!("p:{}", name),
            ARow::Agent { idx, .. } => agents[*idx].pane.clone(),
        }
    }

    /// Like `key`, but an agent is named by its conversation (pin key), which outlives pane
    /// ids across a reboot restore; saved so a reopened agents mode lands on the same row.
    fn saved_key(&self, agents: &[Agent]) -> String {
        match self {
            ARow::Project { name, .. } => format!("p:{}", name),
            ARow::Agent { idx, .. } => format!("a:{}", agents[*idx].pin_key()),
        }
    }
}

fn expanded_key(project: &str) -> String {
    format!("agents:{}", project)
}

/// An agent's workers start folded; the row still shows their counts.
fn agent_key(pane: &str) -> String {
    format!("agents@{}", pane)
}

fn agent_expanded(expanded: &HashMap<String, bool>, pane: &str) -> bool {
    expanded.get(&agent_key(pane)).copied().unwrap_or(false)
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

/// Sibling order: the saved order first, then the current branch, then by name.
fn node_sort_key(n: &str, order: &[String], current: &str) -> (usize, bool, String) {
    (order.iter().position(|o| o == n).unwrap_or(usize::MAX), !in_subtree(current, n), n.to_string())
}

/// Rows: the session tree (`dgt` \u{2192} `dgt/eclion`), each node's own agents as a spawn
/// tree before its child nodes. A parent that is not a live agent (recruited pane, gone,
/// or after a reboot) makes the agent a root of its node. Pinned agents stay listed
/// under a collapsed project or a folded parent.
pub(crate) fn build_rows(agents: &[Agent], layout: &Layout) -> Vec<ARow> {
    let (expanded, current) = (layout.expanded, layout.current);
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
    for list in kids.values_mut() {
        list.sort_by_key(|n| node_sort_key(n, layout.order, current));
    }
    let tree = Tree { agents, children, layout };

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
            // Finished: only top-level agents; a worker finishing reports to its parent, not the user.
            counts: Counts::of(members.iter().map(|&i| &agents[i]), |a| !tree.is_worker_pane(&a.pane)),
            expanded: open,
        });
        // One step past the project's arrow, plus room for an agent's own fold arrow.
        let lead = "  ".repeat(depth + 2);
        if !open {
            let mut pinned: Vec<usize> = members.into_iter().filter(|&i| tree.pinned(i)).collect();
            pinned.sort_by_key(|&i| window_order(&agents[i]));
            for i in pinned {
                push_tree(&mut rows, &tree, i, &lead, None, &mut seen);
            }
            continue;
        }
        for &root in roots.get(node.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
            push_tree(&mut rows, &tree, root, &lead, None, &mut seen);
        }
        for child in kids.get(&node).cloned().unwrap_or_default().into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    rows
}

impl Tree<'_> {
    fn kids(&self, idx: usize) -> Vec<usize> {
        self.children.get(self.agents[idx].pane.as_str()).cloned().unwrap_or_default()
    }

    fn is_worker(&self, idx: usize) -> bool {
        self.children.values().any(|kids| kids.contains(&idx))
    }

    fn is_worker_pane(&self, pane: &str) -> bool {
        self.agents.iter().position(|a| a.pane == pane).is_some_and(|i| self.is_worker(i))
    }

    fn pinned(&self, idx: usize) -> bool {
        self.layout.pins.contains(&self.agents[idx].pin_key())
    }

    /// Every agent below `idx` in the spawn tree, depth first.
    fn descendants(&self, idx: usize, seen: &mut HashSet<usize>, out: &mut Vec<usize>) {
        for kid in self.kids(idx) {
            if seen.insert(kid) {
                out.push(kid);
                self.descendants(kid, seen, out);
            }
        }
    }
}

/// `last`: None for a root, else whether this is its parent's last child. An agent with
/// workers shows ▸/▾ in place of its branch dash (or of the root's indent).
fn push_tree(rows: &mut Vec<ARow>, tree: &Tree, idx: usize, lead: &str, last: Option<bool>, seen: &mut HashSet<usize>) {
    if !seen.insert(idx) {
        return;
    }
    let agents = tree.agents;
    let kids = tree.kids(idx);
    let open = agent_expanded(tree.layout.expanded, &agents[idx].pane);
    let fold = match (kids.is_empty(), open) {
        (true, _) => None,
        (false, true) => Some("▾"),
        (false, false) => Some("▸"),
    };
    let prefix = match (last, fold) {
        (None, None) => lead.to_string(),
        // The root indent ends in two spaces; the arrow takes them so dots stay aligned.
        (None, Some(f)) => format!("{}{} ", &lead[..lead.len().saturating_sub(2)], f),
        (Some(l), f) => format!("{}{}{} ", lead, if l { "└" } else { "├" }, f.unwrap_or("─")),
    };
    let mut counts = Counts::default();
    let mut hidden = Vec::new();
    if !open {
        tree.descendants(idx, &mut seen.clone(), &mut hidden);
        // Hidden workers: busy and failed ones show; a finished worker reports to this parent.
        counts = Counts::of(hidden.iter().map(|&i| &agents[i]), |_| false);
    }
    rows.push(ARow::Agent { idx, prefix, children: kids.len(), expanded: open, counts, pinned: tree.pinned(idx), worker: tree.is_worker(idx) });
    // Children hang under the parent's name, not its dot: a connector in the dot's column
    // reads as a sibling.
    let below = format!(
        "{}{}",
        lead,
        match last {
            Some(false) => "│    ",
            Some(true) => "     ",
            None => "  ",
        }
    );
    // Folded: only pinned workers stay listed.
    let shown: Vec<usize> = if open { kids } else { hidden.into_iter().filter(|&i| tree.pinned(i)).collect() };
    for (n, kid) in shown.iter().enumerate() {
        push_tree(rows, tree, *kid, &below, Some(n + 1 == shown.len()), seen);
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

/// `waiting 3m \u{2699}2`: state, its age, and the bg jobs it waits for when there are any.
fn state_text(a: &Agent) -> String {
    let bg = if a.bg > 0 { format!("\u{2699}{}", a.bg) } else { String::new() };
    vec![a.state.clone(), age(a.state_at), bg].into_iter().filter(|s| !s.is_empty()).collect::<Vec<String>>().join(" ")
}

/// `174k`, `1M`: floored like pi's own status line, so both read the same.
fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// `174k/1M` and its colour by size: amber from CTX_WARN, red from CTX_ALERT.
/// `?` = unknown until the next reply after a compaction; `-` = pi-kit too old to publish it.
fn ctx_text(a: &Agent) -> (String, &'static str) {
    if a.ctx_window == 0 {
        return ("-".to_string(), FAINT);
    }
    let Some(t) = a.ctx_tokens else {
        return (format!("?/{}", tokens(a.ctx_window)), FAINT);
    };
    let colour = if t >= CTX_ALERT { ERROR } else if t >= CTX_WARN { WAITING } else { FAINT };
    (format!("{}/{}", tokens(t), tokens(a.ctx_window)), colour)
}

/// ` ▲1`: hidden agents whose context wants a compaction, in red; nothing when none.
fn full_mark(n: usize, base: &str) -> String {
    if n == 0 {
        return String::new();
    }
    format!(" \x1b[0m{}{}{}\x1b[0m{}", ERROR, FULL_MARK, n, base)
}

/// Working green, waiting amber, failed red, finished purple, context full red ▲; zeros left
/// out; `base` restored after each.
fn counts(c: &Counts, base: &str) -> String {
    let (working, waiting, finished) = (c.working, c.waiting, c.finished);
    let mut s = String::new();
    if working > 0 {
        s.push_str(&format!(" \x1b[0m{}●{}\x1b[0m{}", WORKING, working, base));
    }
    if waiting > 0 {
        s.push_str(&format!(" \x1b[0m{}\u{25d0}{}\x1b[0m{}", WAITING, waiting, base));
    }
    if c.errors > 0 {
        s.push_str(&format!(" \x1b[0m{}\u{2717}{}\x1b[0m{}", ERROR, c.errors, base));
    }
    if finished > 0 {
        s.push_str(&format!(" \x1b[0m{}●{}\x1b[0m{}", FINISHED, finished, base));
    }
    s.push_str(&full_mark(c.full, base));
    s
}

/// `s` in the faint tree colour, then back to `base`.
fn faint(s: &str, base: &str) -> String {
    format!("\x1b[0m{}{}\x1b[0m{}", FAINT, s, base)
}

/// Name, pin mark, and the pane id in the faint colour (ends in `base`).
fn label_with_pin(a: &Agent, pinned: bool, base: &str) -> String {
    let name = if pinned { format!("{} {}", a.label(), PIN) } else { a.label() };
    format!("{} {}", name, faint(&a.pane, base))
}

/// One list row (may hold colour codes) plus its base colour. Columns: tree, ●/○, label, state + age, ⑂ children.
fn row_text(row: &ARow, agents: &[Agent], name_w: usize, show_ctx: bool) -> (String, &'static str) {
    match row {
        ARow::Project { name, depth, agents: n, counts: c, expanded } => {
            let arrow = if *expanded { "▾" } else { "▸" };
            let leaf = name.rsplit('/').next().unwrap_or(name);
            // A collapsed branch hiding a finished top-level agent takes its colour.
            // A failure hidden in a collapsed branch is the louder one.
            let base = match (*expanded, c.errors > 0, c.finished > 0) {
                (false, true, _) => ERROR,
                (false, false, true) => FINISHED,
                _ => PROJECT_COLOR,
            };
            // Just the agent count: per-state counts on every project made a noisy column; a
            // collapsed project's colour still flags a failure or a finished agent inside.
            // ...except "context full": a collapsed project marks agents that want a compaction.
            let full = if *expanded { String::new() } else { full_mark(c.full, base) };
            (format!("{}{} {}  {}{}", "  ".repeat(*depth), faint(arrow, base), leaf, faint(&n.to_string(), base), full), base)
        }
        ARow::Agent { idx, prefix, children, counts: c, pinned, worker, .. } => {
            let a = &agents[*idx];
            // Names stay in the plain colour (workers dimmer); only the dot and the state text carry the state.
            let base = if *worker { WORKER_NAME } else { NAME };
            let tint = |s: &str| format!("\x1b[0m{}{}\x1b[0m{}", a.colour(), s, base);
            // Tree lines and fold arrows recede.
            let head = format!("{}{} {}", faint(prefix, base), tint(a.dot()), label_with_pin(a, *pinned, base));
            let pad = name_w.saturating_sub(text_width(&head));
            let state = state_text(a);
            // Context column off: the context marks follow the state instead (\u{25b2} full, \u{2298} auto-compaction off).
            let mut mark = String::new();
            if !show_ctx && a.ctx_full() {
                mark.push_str(&format!(" \x1b[0m{}{}\x1b[0m{}", ERROR, FULL_MARK, base));
            }
            if !show_ctx && a.compaction_off {
                mark.push_str(&format!(" {}", faint(NO_COMPACT, base)));
            }
            let pad_w = 15usize.saturating_sub(text_width(&state) + text_width(&mark));
            let state = format!("{}{}{}", tint(&state), mark, " ".repeat(pad_w));
            // Optional context column (`c`), fixed width so the child counts stay aligned.
            let state = if show_ctx {
                let (t, colour) = ctx_text(a);
                // `255k/1M \u{2298}`: auto-compaction is off for this session.
                let off = if a.compaction_off { format!(" {}", faint(NO_COMPACT, base)) } else { String::new() };
                let w = text_width(&t) + if off.is_empty() { 0 } else { 2 };
                format!("{}\x1b[0m{}{}\x1b[0m{}{}{}", state, colour, t, base, off, " ".repeat(11usize.saturating_sub(w)))
            } else {
                state
            };
            // The child count is faint too; its coloured counts keep their colours.
            let kids = if *children > 0 { format!("   \x1b[0m{}⑂ {}\x1b[0m{}{}", FAINT, children, base, counts(c, base)) } else { String::new() };
            (format!("{}{}  {}{}", head, " ".repeat(pad), state, kids), base)
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
                fit(&format!(" {} · {}:{} {} · {} · ctx {}{}", a.pane, a.session, a.window_index, a.window_name, state_text(a), ctx_text(a).0, kids), w)
            ));
            // Why the run failed, under the header, so the reason shows without opening the pane.
            if a.errored() && !a.error.is_empty() {
                out.push(format!("{}{}\x1b[0m", ERROR, fit(&format!(" \u{2717} {}", a.error), w)));
            }
            let rest = h.saturating_sub(out.len());
            let body = capture_preview(&a.pane, rest);
            for _ in 0..rest.saturating_sub(body.len()) {
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
                let (c, dot, state) = (a.colour(), a.dot(), state_text(a));
                let line = format!(" {c}{dot}\x1b[0m {}  {c}{state}\x1b[0m  {DIM}{}:{}\x1b[0m", a.label(), a.session, a.window_index);
                out.push(ansi_fit(&line, w));
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

/// `confirm`: a pending "go to <session>?" question, drawn in place of the key hints.
fn render(rows: &[ARow], agents: &[Agent], selected: usize, show_preview: bool, show_ctx: bool, list_percent: usize, confirm: Option<&str>) -> io::Result<()> {
    let (h, w) = term_size();
    let body_h = h.max(4);
    let list_h = body_h.saturating_sub(1);
    let list_w = if show_preview { (w * list_percent / 100).clamp(30, w.saturating_sub(21).max(30)) } else { w };
    let preview_w = w.saturating_sub(list_w + 1);

    let name_w = rows
        .iter()
        .filter_map(|r| match r {
            ARow::Agent { idx, prefix, pinned, .. } => Some(text_width(prefix) + 2 + text_width(&label_with_pin(&agents[*idx], *pinned, ""))),
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
            let hint = " A overview · enter go · h/l fold · p pin · J/K order · c context · v preview · +/- width · q quit";
            match confirm {
                Some(session) => {
                    let ask = format!(" go to {}?  y/enter yes \u{b7} any other key no", crate::util::session_display_name(session));
                    out.push_str(&format!("{}{}\x1b[0m", FINISHED, fit(&ask, list_w)));
                }
                None => out.push_str(&format!("{}{}\x1b[0m", RULE, fit(hint, list_w))),
            }
        } else if let Some(row) = rows.get(offset + y) {
            let (text, colour) = row_text(row, agents, name_w, show_ctx);
            if offset + y == selected {
                out.push_str(&format!("{} {}\x1b[0m", SELECTED, fit(&strip_ansi(&text), content_w)));
            } else {
                out.push_str(&format!("{} {}\x1b[0m", colour, ansi_fit(&text, content_w)));
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

fn go_to_pane(pane: &str) {
    tmux_status_ignore(&["switch-client", "-t", pane]);
    tmux_status_ignore(&["select-window", "-t", pane]);
    tmux_status_ignore(&["select-pane", "-t", pane]);
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
    let mut pins = crate::state::load_agent_pins();
    let mut show_ctx = crate::state::load_agents_context();
    let mut order = crate::state::load_agents_order();
    let rows_for = |agents: &[Agent], expanded: &HashMap<String, bool>, pins: &HashSet<String>, order: &[String]| {
        build_rows(agents, &Layout { expanded, pins, order, current: &current })
    };
    let mut rows = rows_for(&agents, expanded, &pins, &order);
    // Start on the row selected when agents mode last closed, else the first agent, else row 0.
    let saved = crate::state::load_agents_cursor();
    let mut selected = saved
        .as_deref()
        .and_then(|k| rows.iter().position(|r| r.saved_key(&agents) == k))
        .or_else(|| rows.iter().position(|r| matches!(r, ARow::Agent { .. })))
        .unwrap_or(0);
    let mut saved = saved.unwrap_or_default();
    let mut last_refresh = Instant::now();
    let mut dirty = true;
    // (pane, session) waiting for a yes before switching into a helper session.
    let mut confirm: Option<(String, String)> = None;

    loop {
        while drain_a.try_recv().is_ok() {}
        while drain_b.try_recv().is_ok() {}
        if last_refresh.elapsed().as_millis() >= REFRESH_MS {
            let key = rows.get(selected).map(|r| r.key(&agents));
            agents = collect_agents();
            rows = rows_for(&agents, expanded, &pins, &order);
            if let Some(i) = key.and_then(|k| select_key(&rows, &agents, &k)) {
                selected = i;
            }
            last_refresh = Instant::now();
            dirty = true; // the preview is live
        }
        selected = selected.min(rows.len().saturating_sub(1));
        // Remember the selection for the next opening (written only when it changes).
        if let Some(k) = rows.get(selected).map(|r| r.saved_key(&agents)) {
            if k != saved {
                crate::state::save_agents_cursor(&k);
                saved = k;
            }
        }
        if dirty {
            render(&rows, &agents, selected, *show_preview, show_ctx, list_percent, confirm.as_ref().map(|(_, s): &(String, String)| s.as_str()))?;
            dirty = false;
        }

        let Some(key) = read_key_timeout()? else { continue };
        dirty = true;
        // Answer to "go to <helper session>?": yes jumps, anything else just closes the question.
        if let Some((pane, _)) = confirm.take() {
            if matches!(key.as_slice(), b"y" | b"Y" | b"\r" | b"\n") {
                go_to_pane(&pane);
                return Ok(Exit::Quit);
            }
            continue;
        }
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
            b"c" => {
                show_ctx = !show_ctx;
                crate::state::save_agents_context(show_ctx);
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
                    // Fold or unfold an agent's workers; h on a folded one goes to its parent.
                    Some(ARow::Agent { idx, children, expanded: now, .. }) if children > 0 && (open != Some(false) || now) => {
                        expanded.insert(agent_key(&agents[idx].pane), open.unwrap_or(!now));
                        crate::state::save_expanded_state(expanded);
                    }
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
                rows = rows_for(&agents, expanded, &pins, &order);
                if let Some(i) = key.and_then(|k| select_key(&rows, &agents, &k)) {
                    selected = i;
                }
            }
            b"p" => {
                let Some(ARow::Agent { idx, .. }) = rows.get(selected).cloned() else { continue };
                let key = agents[idx].pin_key();
                if !pins.remove(&key) {
                    pins.insert(key);
                }
                crate::state::save_agent_pins(&pins);
                rows = rows_for(&agents, expanded, &pins, &order);
            }
            // Move the selected project among its siblings; the whole sibling order is saved.
            b"J" | b"K" => {
                let Some(ARow::Project { name, .. }) = rows.get(selected).cloned() else { continue };
                let parent = name.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
                let mut siblings: Vec<String> = rows
                    .iter()
                    .filter_map(|r| match r {
                        ARow::Project { name: n, .. } if n.rsplit_once('/').map(|(p, _)| p).unwrap_or("") == parent => Some(n.clone()),
                        _ => None,
                    })
                    .collect();
                let Some(at) = siblings.iter().position(|n| *n == name) else { continue };
                let to = if key.as_slice() == b"J" { at + 1 } else { at.wrapping_sub(1) };
                if to >= siblings.len() {
                    continue;
                }
                siblings.swap(at, to);
                order.retain(|n| !siblings.contains(n));
                order.extend(siblings);
                crate::state::save_agents_order(&order);
                rows = rows_for(&agents, expanded, &pins, &order);
                selected = select_key(&rows, &agents, &format!("p:{}", name)).unwrap_or(selected);
            }
            b"\r" | b"\n" => match rows.get(selected).cloned() {
                Some(ARow::Agent { idx, .. }) => {
                    let a = &agents[idx];
                    if crate::util::is_confirm_session(&a.session) {
                        confirm = Some((a.pane.clone(), a.session.clone()));
                        continue;
                    }
                    go_to_pane(&a.pane);
                    return Ok(Exit::Quit);
                }
                Some(ARow::Project { name, .. }) => {
                    toggle(expanded, &name, None);
                    rows = rows_for(&agents, expanded, &pins, &order);
                }
                None => {}
            },
            _ => dirty = false,
        }
    }
}
