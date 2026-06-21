use std::collections::HashMap;
use std::io::{self, Write};

use crate::model::{Modal, ProcStats, Row};
use crate::preview::preview_for_row;
use crate::terminal::term_size;
use crate::util::{plain_truncate, strip_ansi, text_width, visible_truncate_ansi};

pub(crate) fn session_expanded(row: &Row, expanded: &HashMap<String, bool>) -> bool {
    match row {
        Row::Session(s) => *expanded.get(&s.name).unwrap_or(&true),
        _ => false,
    }
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
pub(crate) fn row_parts(row: &Row, expanded: &HashMap<String, bool>) -> (String, String, String, Vec<String>, bool) {
    match row {
        Row::Session(s) => {
            let arrow = if session_expanded(row, expanded) { "▾" } else { "▸" };
            let mut name = if s.current {
                format!("\x1b[38;2;255;100;150m{}\x1b[1;38;5;252m", s.name)
            } else {
                s.name.clone()
            };
            if let Some(saved_label) = &s.saved_label {
                name.push_str(&format!("\x1b[38;5;240m 󰆓 {}\x1b[1;38;5;252m", saved_label));
            }
            (
                format!("{}  {}", arrow, name),
                format_cpu(&s.stats),
                format_mem(&s.stats),
                Vec::new(),
                false,
            )
        }
        Row::Window(w) => {
            let dot = if w.active { "●" } else { "○" };
            (
                format!("   {}  {}: {}", dot, w.index, w.name),
                format_cpu(&w.stats),
                format_mem(&w.stats),
                w.pane_labels.clone(),
                true,
            )
        }
        Row::SavedSession(s) => (
            format!("󰆓  {}", s.name),
            String::new(),
            String::new(),
            vec![format!("{} · {} windows", s.saved_label, s.windows.len())],
            false,
        ),
    }
}
pub(crate) fn compute_item_width(rows: &[Row], expanded: &HashMap<String, bool>, screen_width: usize) -> usize {
    let max_item = rows
        .iter()
        .map(|row| text_width(&row_parts(row, expanded).0))
        .max()
        .unwrap_or(24);

    // Enough room for: two spaces + CPU(7) + space + RAM(6) + optional
    // command separator. Clamp so one very long tab name does not push stats
    // across the screen.
    let max_allowed = screen_width.saturating_sub(24).max(20);
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
        Row::Window(w) => w.stats.clone(),
        Row::SavedSession(_) => ProcStats::default(),
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
            "j/k or arrows move".to_string(),
            "g/G first/last".to_string(),
            "h/l or arrows fold/unfold".to_string(),
            "space toggle session fold".to_string(),
            "v preview, +/- resize".to_string(),
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
        Modal::Input { title, value, .. } => Some(vec![
            title.clone(),
            String::new(),
            format!("> {}█", value),
            String::new(),
            "enter apply    esc cancel".to_string(),
        ]),
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

pub(crate) fn row_text(row: &Row, expanded: &HashMap<String, bool>, item_width: usize) -> String {
    let cpu_width = 7usize;
    let mem_width = 6usize;
    let (item, cpu, mem, pane_labels, is_window) = row_parts(row, expanded);
    let item_visible_width = text_width(&item).min(item_width);
    let item = visible_truncate_ansi(&item, item_width);
    let pad = " ".repeat(item_width.saturating_sub(item_visible_width));

    let cpu_cell = format!("{:>cpu_width$}", cpu, cpu_width = cpu_width);
    let mem_cell = format!("{:>mem_width$}", mem, mem_width = mem_width);
    let stats_value = row_stats(row);
    let cpu_high = is_window && stats_value.cpu > 100.0;
    let mem_high = is_window && stats_value.rss_kb > 500 * 1024;
    let cpu_colour = if cpu_high { "\x1b[38;2;255;100;150m" } else { "\x1b[38;5;240m" };
    let mem_colour = if mem_high { "\x1b[38;2;255;100;150m" } else { "\x1b[38;5;240m" };
    let stats = format!("{}{} {}{}", cpu_colour, cpu_cell, mem_colour, mem_cell);

    if pane_labels.is_empty() {
        format!("{}{}  {}", item, pad, stats)
    } else {
        format!(
            "{}{}  {}\x1b[38;5;244m   {}",
            item,
            pad,
            stats,
            pane_columns(&pane_labels)
        )
    }
}

pub(crate) fn render(
    rows: &[Row],
    selected: usize,
    expanded: &HashMap<String, bool>,
    preview_percent: usize,
    show_preview: bool,
    stats_interval_secs: u64,
    message: &str,
    modal: &Modal,
) -> io::Result<()> {
    let (h, w) = term_size();
    // Keep one spare terminal row to avoid scrolling on the final CRLF.
    let body_h = h.saturating_sub(1).max(8);
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
    let item_width = compute_item_width(rows, expanded, left_w.saturating_sub(2));
    let total = cumulative_session_stats(rows);
    let help_lines = vec![
        "tmux overview".to_string(),
        format!("total {} {}", format_cpu(&total), format_mem(&total)),
        format!("stats {}s", stats_interval_secs),
        message.to_string(),
        String::new(),
        "enter switch".to_string(),
        "j/k move".to_string(),
        "h/l fold".to_string(),
        "+/- resize".to_string(),
        "v preview".to_string(),
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
    out.push_str("\x1b[H\x1b[2J");

    let mut offset = 0usize;
    if selected >= list_h {
        offset = selected + 1 - list_h;
    }
    for line_idx in 0..list_h {
        let idx = offset + line_idx;
        if rows.is_empty() && line_idx == 0 {
            out.push_str(" no sessions");
            out.push_str(&" ".repeat(left_w.saturating_sub(12)));
        } else if idx < rows.len() {
            let text = row_text(&rows[idx], expanded, item_width);
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
                    Row::Session(_) => out.push_str("\x1b[1;38;5;252m "),
                    Row::Window(_) => out.push_str("\x1b[38;5;244m "),
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

    draw_modal(&mut out, modal, h, w);
    print!("{}", out);
    io::stdout().flush()
}
