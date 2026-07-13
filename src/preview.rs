use crate::model::{Row, SavedSession};
use crate::tmux_api::tmux;
use crate::util::{plain_truncate, session_display_name, text_width, visible_truncate_ansi};

pub(crate) fn capture_preview(target: &str, lines: usize) -> Vec<String> {
    let start = format!("-{}", lines.saturating_mul(2).max(lines));
    let out = tmux(&["capture-pane", "-ep", "-S", &start, "-t", target]).unwrap_or_default();
    tail_nonempty_lines(&out, lines)
}

pub(crate) fn tail_nonempty_lines(text: &str, lines: usize) -> Vec<String> {
    let mut v: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    while v.last().is_some_and(|s| s.trim().is_empty()) {
        v.pop();
    }
    if v.len() > lines {
        v = v[v.len() - lines..].to_vec();
    }
    v
}

#[derive(Clone)]
struct PaneGeom {
    id: String,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

fn window_panes(window_id: &str) -> Vec<PaneGeom> {
    let panes_raw = tmux(&[
        "list-panes",
        "-t",
        window_id,
        "-F",
        "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}",
    ])
    .unwrap_or_default();

    let mut panes = Vec::new();
    for line in panes_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 5 {
            continue;
        }
        let Ok(left) = parts[1].parse::<usize>() else { continue };
        let Ok(top) = parts[2].parse::<usize>() else { continue };
        let Ok(width) = parts[3].parse::<usize>() else { continue };
        let Ok(height) = parts[4].parse::<usize>() else { continue };
        panes.push(PaneGeom {
            id: parts[0].to_string(),
            left,
            top,
            width,
            height,
        });
    }
    panes
}

pub(crate) fn ansi_fit(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let clipped = visible_truncate_ansi(s, width);
    let visible = text_width(&clipped).min(width);
    format!("{}{}", clipped, " ".repeat(width.saturating_sub(visible)))
}

struct PanePreview {
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    top_pad: usize,
    content: Vec<String>,
}

pub(crate) fn window_layout_preview(window_id: &str, width: usize, height: usize) -> Option<Vec<String>> {
    let panes = window_panes(window_id);
    if panes.len() <= 1 || width < 12 || height < 3 {
        return None;
    }

    let source_w = panes.iter().map(|p| p.left + p.width).max().unwrap_or(1).max(1);
    let source_h = panes.iter().map(|p| p.top + p.height).max().unwrap_or(1).max(1);
    let mut previews = Vec::new();

    for pane in panes {
        let x1 = pane.left * width / source_w;
        let y1 = pane.top * height / source_h;
        let mut x2 = (pane.left + pane.width) * width / source_w;
        let mut y2 = (pane.top + pane.height) * height / source_h;
        x2 = x2.max(x1 + 1).min(width);
        y2 = y2.max(y1 + 1).min(height);
        if x1 >= width || y1 >= height || x2 <= x1 || y2 <= y1 {
            continue;
        }

        let pane_h = y2 - y1;
        let content = capture_preview(&pane.id, pane_h);
        let top_pad = pane_h.saturating_sub(content.len());
        previews.push(PanePreview {
            x1,
            y1,
            x2,
            y2,
            top_pad,
            content,
        });
    }

    previews.sort_by_key(|p| (p.y1, p.x1));
    let mut lines = Vec::new();
    for y in 0..height {
        let mut line = String::new();
        let mut x = 0usize;
        let mut active: Vec<&PanePreview> = previews
            .iter()
            .filter(|p| p.y1 <= y && y < p.y2)
            .collect();
        active.sort_by_key(|p| p.x1);

        let mut drew_segment = false;
        for pane in active {
            if pane.x1 > x {
                line.push_str(&" ".repeat(pane.x1 - x));
                x = pane.x1;
            }
            if drew_segment && x < width {
                line.push_str("\x1b[38;5;238m│\x1b[0m");
                x += 1;
            }
            if pane.x2 <= x {
                continue;
            }
            let pane_w = pane.x2 - x;
            let local_y = y - pane.y1;
            let text = if local_y >= pane.top_pad {
                pane.content.get(local_y - pane.top_pad).map(String::as_str).unwrap_or("")
            } else {
                ""
            };
            line.push_str(&ansi_fit(text, pane_w));
            x = pane.x2;
            drew_segment = true;
        }
        if x < width {
            line.push_str(&" ".repeat(width - x));
        }
        lines.push(line);
    }

    Some(lines)
}

pub(crate) fn active_window_for_session(session_id: &str) -> Option<String> {
    let windows_raw = tmux(&[
        "list-windows",
        "-t",
        session_id,
        "-F",
        "#{window_id}\t#{window_active}",
    ])
    .unwrap_or_default();
    for line in windows_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 2 && parts[1] == "1" {
            return Some(parts[0].to_string());
        }
    }
    None
}

pub(crate) fn preview_for_row(row: &Row, width: usize, height: usize) -> Vec<String> {
    match row {
        Row::WatchHeader => Vec::new(),
        Row::WatchWindow(w) | Row::Window(w) => {
            if let Some(lines) = window_layout_preview(&w.id, width, height) {
                return lines;
            }
            capture_preview(&w.id, height)
        }
        Row::Session(s) => {
            if let Some(window_id) = active_window_for_session(&s.id) {
                if let Some(lines) = window_layout_preview(&window_id, width, height) {
                    return lines;
                }
                return capture_preview(&window_id, height);
            }
            capture_preview(&s.id, height)
        }
        Row::SavedSession(s) => saved_session_preview(s, width, height),
    }
}
pub(crate) fn saved_session_preview(session: &SavedSession, width: usize, height: usize) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("󰆓 saved layout: {}", session_display_name(&session.name)));
    lines.push(format!("last saved: {}", session.saved_label));
    lines.push(format!("{} windows", session.windows.len()));
    if !session.default_path.is_empty() {
        lines.push(format!("default path: {}", session.default_path));
    }
    lines.push(String::new());
    for window in &session.windows {
        lines.push(format!("{}: {}", window.index, window.name));
        for pane in &window.panes {
            let cmd = if pane.command.is_empty() { "shell" } else { &pane.command };
            lines.push(format!("  pane {}  {}  {}", pane.index, cmd, pane.cwd));
        }
    }
    lines
        .into_iter()
        .take(height)
        .map(|line| format!("\x1b[38;5;244m{}", plain_truncate(&line, width)))
        .collect()
}
