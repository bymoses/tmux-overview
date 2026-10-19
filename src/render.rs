use std::collections::HashMap;
use std::io::{self, Write};

use crate::model::{AgentAlert, Modal, ProcStats, Row};
use crate::preview::preview_for_row;
use crate::terminal::term_size;
use crate::util::{is_bg_session, plain_truncate, session_display_name, session_is_expanded, strip_ansi, text_width, visible_truncate_ansi};

const SESSION_COLOR: &str = "\x1b[1;38;5;252m";
/// `__bg` session rows and their windows: dim, not bold.
const BG_COLOR: &str = "\x1b[22;38;5;240m";

pub(crate) fn session_expanded(row: &Row, expanded: &HashMap<String, bool>) -> bool {
    match row {
        Row::Session(s) => session_is_expanded(expanded, &s.name),
        _ => false,
    }
}

const ALERT_COLOR: &str = "\x1b[1;38;2;255;100;150m";
/// Same red as agents mode's failed agents.
const ERROR_COLOR: &str = "\x1b[1;38;5;203m";

fn format_age(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}s", seconds)
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Bright badge for an agent that stopped and has not been reviewed yet.
/// A failed run (pi-kit's "error" state) is red with \u{2717}, apart from a plain stop.
fn alert_badge(alert: &AgentAlert) -> String {
    let (glyph, colour) = match alert.state.as_str() {
        "error" => ("\u{2717}", ERROR_COLOR),
        "exited" => ("\u{25cc}", ALERT_COLOR),
        _ => ("\u{25cf}", ALERT_COLOR),
    };
    let count = if alert.panes > 1 { alert.panes.to_string() } else { String::new() };
    let age = if alert.idle_at > 0 {
        format!(" {}", format_age(now_epoch().saturating_sub(alert.idle_at)))
    } else {
        String::new()
    };
    format!("{} {}{}{}\x1b[0m", colour, glyph, count, age)
}

pub(crate) fn format_cpu(stats: &ProcStats) -> String {
    format!("{:.1}%", stats.cpu)
}

pub(crate) fn format_mem(stats: &ProcStats) -> String {
    if stats.rss_kb >= 1_048_576 {
        format!("{:.1}G", stats.rss_kb as f64 / 1_048_576.0)
    } else if stats.rss_kb >= 1024 {
        format!("{:.0}M", stats.rss_kb as f64 / 1024.0)
    } else {
        format!("{}K", stats.rss_kb)
    }
}
fn session_index_before_at_depth(rows: &[Row], index: usize, depth: usize) -> Option<usize> {
    rows[..index]
        .iter()
        .rposition(|row| matches!(row, Row::Session(session) if session.depth == depth))
}

fn session_has_later_sibling(rows: &[Row], index: usize) -> bool {
    let Row::Session(session) = &rows[index] else {
        return false;
    };
    for row in &rows[index + 1..] {
        if let Row::Session(candidate) = row {
            if candidate.depth == session.depth {
                return true;
            }
            if candidate.depth < session.depth {
                return false;
            }
        }
    }
    false
}

fn session_has_visible_child(rows: &[Row], session_index: usize) -> bool {
    let Row::Session(session) = &rows[session_index] else {
        return false;
    };
    for row in &rows[session_index + 1..] {
        if let Row::Session(candidate) = row {
            if candidate.depth <= session.depth {
                return false;
            }
            if candidate.depth == session.depth + 1 {
                return true;
            }
        }
    }
    false
}

fn window_has_following_item(rows: &[Row], index: usize, session_index: usize) -> bool {
    let Row::Window(window) = &rows[index] else {
        return false;
    };
    let owner_depth = match &rows[session_index] {
        Row::Session(session) => session.depth,
        _ => return false,
    };
    for row in &rows[index + 1..] {
        match row {
            Row::Window(candidate) if candidate.session_id == window.session_id => return true,
            Row::Session(candidate) if candidate.depth == owner_depth + 1 => return true,
            Row::Session(candidate) if candidate.depth <= owner_depth => return false,
            _ => {}
        }
    }
    false
}

fn tree_prefix(rows: &[Row], index: usize) -> String {
    const SESSION_EDGE: &str = "\x1b[38;5;252m";
    const WINDOW_EDGE: &str = "\x1b[38;5;244m";

    let mut prefix = String::new();
    let (depth, is_window) = match &rows[index] {
        Row::Session(session) => (session.depth, false),
        Row::Window(window) => (window.depth, true),
        _ => return prefix,
    };

    let guide_end = if is_window { depth + 1 } else { depth };
    for ancestor_depth in 1..guide_end {
        if let Some(ancestor_index) = session_index_before_at_depth(rows, index, ancestor_depth) {
            prefix.push_str(SESSION_EDGE);
            prefix.push_str(if session_has_later_sibling(rows, ancestor_index) { "│  " } else { "   " });
        }
    }

    if !is_window {
        if depth > 0 {
            prefix.push_str(SESSION_EDGE);
            prefix.push_str(if session_has_later_sibling(rows, index) { "├─ " } else { "└─ " });
        }
        return prefix;
    }

    let Some(session_index) = rows[..index]
        .iter()
        .rposition(|row| matches!(row, Row::Session(session) if session.depth == depth))
    else {
        return prefix;
    };
    let following = window_has_following_item(rows, index, session_index);
    prefix.push_str(if following && session_has_visible_child(rows, session_index) {
        SESSION_EDGE
    } else {
        WINDOW_EDGE
    });
    prefix.push_str(if following { "├" } else { "└" });
    prefix.push_str(WINDOW_EDGE);
    prefix.push_str(match &rows[index] {
        Row::Window(window) if window.active => "─◆ ",
        _ => "─  ",
    });
    prefix
}

pub(crate) fn row_parts(
    row: &Row,
    expanded: &HashMap<String, bool>,
    show_stats: bool,
    show_tree: bool,
) -> (String, String, String, Vec<String>, bool) {
    match row {
        Row::WatchHeader => (
            "󰈈  watchlist".to_string(),
            String::new(),
            String::new(),
            Vec::new(),
            false,
        ),
        Row::Session(s) => {
            let arrow = if session_expanded(row, expanded) { "▾" } else { "▸" };
            let base = if is_bg_session(&s.name) { BG_COLOR } else { SESSION_COLOR };
            let mut name = if s.current {
                format!("\x1b[38;2;255;100;150m{}{}", s.display_name, base)
            } else {
                s.display_name.clone()
            };
            if let Some(saved_label) = &s.saved_label {
                name.push_str(&format!("\x1b[38;5;240m 󰆓 {}{}", saved_label, base));
            }
            if s.alerts > 0 {
                name.push_str(&format!("{} ● {}{}", ALERT_COLOR, s.alerts, base));
            }
            let right = if s.default_path.is_empty() {
                Vec::new()
            } else {
                vec![format!("path {}", s.default_path)]
            };
            let indent = if show_tree { String::new() } else { "  ".repeat(s.depth) };
            (
                // Restate the row colour: tree_prefix leaves its edge colour set.
                format!("{}{}{} {}", base, indent, arrow, name),
                format_cpu(&s.stats),
                format_mem(&s.stats),
                right,
                false,
            )
        }
        Row::WatchWindow(w) => {
            let dot = if w.active { "•" } else { " " };
            let mut item = if show_stats {
                format!("   {} {}:{}  {}", dot, session_display_name(&w.session_name), w.index, w.name)
            } else {
                format!("   {} {}  {}", dot, session_display_name(&w.session_name), w.name)
            };
            if let Some(alert) = &w.alert {
                item.push_str(&alert_badge(alert));
            }
            (
                item,
                format_cpu(&w.stats),
                format_mem(&w.stats),
                w.pane_labels.clone(),
                true,
            )
        }
        Row::Window(w) => {
            let dot = if w.active { "•" } else { " " };
            let mut item = if is_bg_session(&w.session_name) { BG_COLOR.to_string() } else { String::new() };
            item.push_str(&if show_tree && show_stats {
                format!("{}: {}", w.index, w.name)
            } else if show_tree {
                w.name.clone()
            } else if show_stats {
                format!("{}   {} {}: {}", "  ".repeat(w.depth), dot, w.index, w.name)
            } else {
                format!("{}   {} {}", "  ".repeat(w.depth), dot, w.name)
            });
            if let Some(alert) = &w.alert {
                item.push_str(&alert_badge(alert));
            }
            (
                item,
                format_cpu(&w.stats),
                format_mem(&w.stats),
                w.pane_labels.clone(),
                true,
            )
        }
        Row::SavedSession(s) => (
            format!("󰆓 {}", session_display_name(&s.name)),
            String::new(),
            String::new(),
            vec![format!("{} · {} windows", s.saved_label, s.windows.len())],
            false,
        ),
    }
}
pub(crate) fn compute_item_width(
    rows: &[Row],
    expanded: &HashMap<String, bool>,
    screen_width: usize,
    show_stats: bool,
    show_tree: bool,
) -> usize {
    let max_item = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let item = row_parts(row, expanded, show_stats, show_tree).0;
            text_width(&format!("{}{}", if show_tree { tree_prefix(rows, index) } else { String::new() }, item))
        })
        .max()
        .unwrap_or(24);

    // Reserve room for CPU/RAM and the command separator in stats mode.
    // Compact mode can dedicate nearly the full picker width to names/titles.
    let reserved = if show_stats { 24 } else { 2 };
    let max_allowed = screen_width.saturating_sub(reserved).max(20);
    max_item.clamp(20, max_allowed)
}

pub(crate) fn cumulative_session_stats(rows: &[Row]) -> ProcStats {
    let mut total = ProcStats::default();
    for row in rows {
        if let Row::Session(s) = row {
            total.cpu += s.stats.cpu;
            total.rss_kb += s.stats.rss_kb;
        }
    }
    total
}

pub(crate) fn row_stats(row: &Row) -> ProcStats {
    match row {
        Row::Session(s) => s.stats.clone(),
        Row::WatchWindow(w) | Row::Window(w) => w.stats.clone(),
        Row::WatchHeader | Row::SavedSession(_) => ProcStats::default(),
    }
}

pub(crate) fn pane_columns(labels: &[String]) -> String {
    let pane_width = 18usize;
    labels
        .iter()
        .map(|label| {
            let clipped = visible_truncate_ansi(label, pane_width);
            let pad = pane_width.saturating_sub(text_width(&clipped));
            format!("{}{}", clipped, " ".repeat(pad))
        })
        .collect::<Vec<_>>()
        .join(" \x1b[38;5;238m│\x1b[38;5;244m ")
}

pub(crate) fn help_modal() -> Modal {
    Modal::Info {
        title: "tmux overview help".to_string(),
        lines: vec![
            "enter switch/restore".to_string(),
            "A agents tree".to_string(),
            "j/k or arrows move".to_string(),
            "g/G first/last".to_string(),
            "h/l or arrows fold/unfold".to_string(),
            "space toggle session fold".to_string(),
            "v preview, +/- resize".to_string(),
            "s compact/stats view".to_string(),
            "t tree view".to_string(),
            ". show/hide helpers".to_string(),
            "a add window/session".to_string(),
            "w watch/unwatch window".to_string(),
            "r rename, R reload".to_string(),
            "S save layout".to_string(),
            "K kill session".to_string(),
            "D delete row".to_string(),
            "c default path".to_string(),
            "q quit".to_string(),
        ],
    }
}

pub(crate) fn modal_lines(modal: &Modal) -> Option<Vec<String>> {
    match modal {
        Modal::None => None,
        Modal::Confirm { title, .. } => Some(vec![
            title.clone(),
            String::new(),
            "y/enter confirm    n/esc cancel".to_string(),
        ]),
        Modal::Input { title, value, action } => {
            let help = match action {
                crate::model::InputAction::SetDefaultPath { .. } => "tab complete path    enter apply    esc cancel",
                _ => "enter apply    esc cancel",
            };
            Some(vec![
                title.clone(),
                String::new(),
                format!("> {}█", value),
                String::new(),
                help.to_string(),
            ])
        }
        Modal::Info { title, lines } => {
            let mut out = vec![title.clone(), String::new()];
            out.extend(lines.iter().cloned());
            out.push(String::new());
            out.push("enter/space/q/esc close".to_string());
            Some(out)
        }
    }
}

pub(crate) fn draw_modal(out: &mut String, modal: &Modal, term_h: usize, term_w: usize) {
    let Some(lines) = modal_lines(modal) else {
        return;
    };
    let content_w = lines.iter().map(|l| text_width(l)).max().unwrap_or(20).max(32);
    let box_w = (content_w + 4).min(term_w.saturating_sub(4).max(20));
    let box_h = lines.len() + 2;
    let x = term_w.saturating_sub(box_w) / 2 + 1;
    let y = term_h.saturating_sub(box_h) / 2 + 1;

    out.push_str("\x1b[0m");
    out.push_str(&format!("\x1b[{};{}H", y, x));
    out.push_str("\x1b[49m\x1b[38;5;244m┌");
    out.push_str(&"─".repeat(box_w.saturating_sub(2)));
    out.push_str("┐\x1b[0m");
    for (i, line) in lines.iter().enumerate() {
        out.push_str(&format!("\x1b[{};{}H", y + i + 1, x));
        out.push_str("\x1b[49m\x1b[38;5;244m│ ");
        if i == 0 {
            out.push_str("\x1b[38;2;255;100;150m");
        } else {
            out.push_str("\x1b[38;5;252m");
        }
        let inner_w = box_w.saturating_sub(4);
        let clipped = plain_truncate(line, inner_w);
        let visible = text_width(&clipped).min(inner_w);
        out.push_str(&clipped);
        out.push_str(&" ".repeat(inner_w.saturating_sub(visible)));
        out.push_str("\x1b[38;5;244m │\x1b[0m");
    }
    out.push_str(&format!("\x1b[{};{}H", y + box_h - 1, x));
    out.push_str("\x1b[49m\x1b[38;5;244m└");
    out.push_str(&"─".repeat(box_w.saturating_sub(2)));
    out.push_str("┘\x1b[0m");
}

pub(crate) fn row_text(
    rows: &[Row],
    index: usize,
    expanded: &HashMap<String, bool>,
    item_width: usize,
    show_stats: bool,
    show_tree: bool,
) -> String {
    let cpu_width = 7usize;
    let mem_width = 6usize;
    let (item, cpu, mem, pane_labels, is_window) = row_parts(&rows[index], expanded, show_stats, show_tree);
    let item = format!("{}{}", if show_tree { tree_prefix(rows, index) } else { String::new() }, item);
    let item_visible_width = text_width(&item).min(item_width);
    let item = visible_truncate_ansi(&item, item_width);
    let pad = " ".repeat(item_width.saturating_sub(item_visible_width));

    let cpu_cell = format!("{:>cpu_width$}", cpu, cpu_width = cpu_width);
    let mem_cell = format!("{:>mem_width$}", mem, mem_width = mem_width);
    let stats_value = row_stats(&rows[index]);
    let cpu_high = is_window && stats_value.cpu > 100.0;
    let mem_high = is_window && stats_value.rss_kb > 500 * 1024;
    let cpu_colour = if cpu_high { "\x1b[38;2;255;100;150m" } else { "\x1b[38;5;240m" };
    let mem_colour = if mem_high { "\x1b[38;2;255;100;150m" } else { "\x1b[38;5;240m" };
    let stats = format!("{}{} {}{}", cpu_colour, cpu_cell, mem_colour, mem_cell);

    if !show_stats {
        if pane_labels.is_empty() {
            format!("{}{}", item, pad)
        } else if is_window {
            format!("{}{}\x1b[38;5;244m   {}", item, pad, pane_columns(&pane_labels))
        } else {
            format!("{}{}\x1b[38;5;240m   {}", item, pad, pane_labels.join("  "))
        }
    } else if pane_labels.is_empty() {
        format!("{}{}  {}", item, pad, stats)
    } else if is_window {
        format!(
            "{}{}  {}\x1b[38;5;244m   {}",
            item,
            pad,
            stats,
            pane_columns(&pane_labels)
        )
    } else {
        format!(
            "{}{}  {}\x1b[38;5;240m   {}",
            item,
            pad,
            stats,
            pane_labels.join("  ")
        )
    }
}

pub(crate) fn render(
    rows: &[Row],
    selected: usize,
    expanded: &HashMap<String, bool>,
    preview_percent: usize,
    show_preview: bool,
    show_stats: bool,
    show_tree: bool,
    stats_interval_secs: u64,
    message: &str,
    modal: &Modal,
) -> io::Result<()> {
    let (h, w) = term_size();
    // Every row is drawn; the last row's CRLF is dropped below so the screen never scrolls.
    let body_h = h.max(8);
    let preview_h = if show_preview {
        let mut ph = body_h * preview_percent / 100;
        ph = ph.max(3).min(body_h.saturating_sub(4).max(3));
        ph
    } else {
        0
    };
    let sep_h = if show_preview { 1 } else { 0 };
    let list_h = body_h.saturating_sub(preview_h + sep_h).max(1);

    let side_w = if w >= 80 { w / 5 } else { 0 };
    let delim_w = if side_w > 0 { 1 } else { 0 };
    let left_w = w.saturating_sub(side_w + delim_w).max(20);
    let item_width = compute_item_width(rows, expanded, left_w.saturating_sub(2), show_stats, show_tree);
    let total = cumulative_session_stats(rows);
    let help_lines = vec![
        "tmux overview".to_string(),
        if show_stats {
            format!("total {} {}", format_cpu(&total), format_mem(&total))
        } else {
            "compact picker".to_string()
        },
        if show_stats {
            format!("stats {}s", stats_interval_secs)
        } else {
            "stats hidden".to_string()
        },
        message.to_string(),
        String::new(),
        "enter switch".to_string(),
        "A agents".to_string(),
        "j/k move".to_string(),
        "h/l fold".to_string(),
        "+/- resize".to_string(),
        "v preview".to_string(),
        "s stats".to_string(),
        "t tree".to_string(),
        ". hidden".to_string(),
        "a add".to_string(),
        "w watch".to_string(),
        "r rename".to_string(),
        "R reload".to_string(),
        "S save layout".to_string(),
        "K kill session".to_string(),
        "D delete row".to_string(),
        "c path".to_string(),
        "? help".to_string(),
        "q quit".to_string(),
    ];
    let mut out = String::new();
    // Ask terminals that support synchronized output (Ghostty, iTerm2,
    // kitty, recent VTE, etc.) to present this full frame atomically.
    // Do not clear the whole screen on every frame: modal typing/renaming can
    // make tmux popups briefly show the cleared screen. Each rendered row uses
    // EL (\x1b[K), so repainting from home is enough to erase the previous
    // frame without a blank intermediate frame.
    out.push_str("\x1b[?2026h\x1b[H");

    // Keep the selected row away from the picker edges when enough rows are
    // available, similar to a four-line scrolloff in an editor.
    let scrolloff = 4.min(list_h.saturating_sub(1) / 2);
    let offset = selected.saturating_sub(list_h.saturating_sub(scrolloff + 1));
    for line_idx in 0..list_h {
        let idx = offset + line_idx;
        if rows.is_empty() && line_idx == 0 {
            out.push_str(" no sessions");
            out.push_str(&" ".repeat(left_w.saturating_sub(12)));
        } else if idx < rows.len() {
            let text = row_text(rows, idx, expanded, item_width, show_stats, show_tree);
            let content_w = left_w.saturating_sub(1);
            if idx == selected {
                let plain = plain_truncate(&strip_ansi(&text), content_w);
                let plain_w = text_width(&plain);
                out.push_str("\x1b[48;2;255;100;150m\x1b[38;2;0;0;0m ");
                out.push_str(&plain);
                out.push_str(&" ".repeat(content_w.saturating_sub(plain_w)));
                out.push_str("\x1b[0m");
            } else {
                match rows[idx] {
                    Row::WatchHeader => out.push_str("\x1b[1;38;5;240m "),
                    Row::Session(ref s) if is_bg_session(&s.name) => out.push_str(&format!("{} ", BG_COLOR)),
                    Row::Session(_) => out.push_str(&format!("{} ", SESSION_COLOR)),
                    Row::Window(ref w) if is_bg_session(&w.session_name) => out.push_str(&format!("{} ", BG_COLOR)),
                    Row::WatchWindow(_) | Row::Window(_) => out.push_str("\x1b[38;5;244m "),
                    Row::SavedSession(_) => out.push_str("\x1b[38;5;240m "),
                }
                out.push_str(&visible_truncate_ansi(&text, content_w));
                let visible_w = text_width(&text).min(content_w);
                out.push_str(&" ".repeat(content_w.saturating_sub(visible_w)));
                out.push_str("\x1b[0m");
            }
        } else {
            out.push_str(&" ".repeat(left_w));
        }

        if side_w > 0 {
            out.push_str("\x1b[38;5;238m│\x1b[0m");
            let help = help_lines.get(line_idx).map(String::as_str).unwrap_or("");
            if line_idx == 0 {
                out.push_str("\x1b[38;2;255;100;150m ");
            } else {
                out.push_str("\x1b[38;5;244m ");
            }
            out.push_str(&plain_truncate(help, side_w.saturating_sub(1)));
            out.push_str("\x1b[0m");
        }
        out.push_str("\x1b[K\r\n");
    }

    if show_preview {
        let label = if side_w == 0 {
            format!(" preview {}% · ? help ", preview_percent)
        } else {
            format!(" preview {}% ", preview_percent)
        };
        let label_w = text_width(&label).min(w);
        let left = w.saturating_sub(label_w) / 2;
        let right = w.saturating_sub(left + label_w);
        out.push_str("\x1b[38;5;238m");
        out.push_str(&"─".repeat(left));
        out.push_str("\x1b[38;5;244m");
        out.push_str(&plain_truncate(&label, w.saturating_sub(left)));
        out.push_str("\x1b[38;5;238m");
        out.push_str(&"─".repeat(right));
        out.push_str("\x1b[0m\r\n");
        let preview = rows
            .get(selected)
            .map(|r| preview_for_row(r, w, preview_h))
            .unwrap_or_default();
        let top_pad = preview_h.saturating_sub(preview.len());
        for _ in 0..top_pad {
            out.push_str("\x1b[K\r\n");
        }
        for line in preview.iter().take(preview_h) {
            out.push_str(&visible_truncate_ansi(line, w));
            out.push_str("\x1b[K\r\n");
        }
    }

    if out.ends_with("\r\n") {
        out.truncate(out.len() - 2);
    }
    draw_modal(&mut out, modal, h, w);
    out.push_str("\x1b[?2026l");
    print!("{}", out);
    io::stdout().flush()
}
