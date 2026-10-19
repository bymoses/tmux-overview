use crate::model::{Row, SavedSession};
use crate::tmux_api::tmux;
use crate::util::{plain_truncate, session_display_name, text_width, visible_truncate_ansi};

/// `-N` keeps trailing blanks: a full-width background (pi's message boxes) is made of
/// blank cells, and without it the box shrank to the text. Lines are made self-contained
/// because `-e` writes attributes as changes carried over from the previous line.
pub(crate) fn capture_preview(target: &str, lines: usize) -> Vec<String> {
    let start = format!("-{}", lines.saturating_mul(2).max(lines));
    let out = tmux(&["capture-pane", "-epN", "-S", &start, "-t", target]).unwrap_or_default();
    tail_nonempty_lines(&self_contained_lines(&out).join("\n"), lines)
}

/// SGR state at a point of the stream, to restate at the start of each line.
#[derive(Default, Clone)]
struct Sgr {
    attrs: [bool; 10],
    fg: Option<String>,
    bg: Option<String>,
    ul: Option<String>,
}

impl Sgr {
    fn apply(&mut self, params: &str) {
        let p: Vec<&str> = if params.is_empty() { vec!["0"] } else { params.split(';').collect() };
        let mut i = 0;
        while i < p.len() {
            let tok = p[i];
            // 38;5;n / 38;2;r;g;b span several params; the colon forms are one token.
            let ext = |i: usize| -> (String, usize) {
                match p.get(i + 1).copied() {
                    Some("5") if i + 2 < p.len() => (p[i..=i + 2].join(";"), 3),
                    Some("2") if i + 4 < p.len() => (p[i..=i + 4].join(";"), 5),
                    _ => (tok.to_string(), 1),
                }
            };
            let head = tok.split(':').next().unwrap_or("");
            let n: u32 = head.parse().unwrap_or(0);
            let mut step = 1;
            match n {
                0 => *self = Sgr::default(),
                1..=9 => self.attrs[n as usize] = true,
                21 => self.attrs[4] = true,
                22 => (self.attrs[1], self.attrs[2]) = (false, false),
                23 => self.attrs[3] = false,
                24 => self.attrs[4] = false,
                25 => (self.attrs[5], self.attrs[6]) = (false, false),
                27 => self.attrs[7] = false,
                28 => self.attrs[8] = false,
                29 => self.attrs[9] = false,
                30..=37 | 90..=97 => self.fg = Some(tok.to_string()),
                39 => self.fg = None,
                40..=47 | 100..=107 => self.bg = Some(tok.to_string()),
                49 => self.bg = None,
                38 | 48 | 58 => {
                    let (code, used) = if tok.contains(':') { (tok.to_string(), 1) } else { ext(i) };
                    step = used;
                    match n {
                        38 => self.fg = Some(code),
                        48 => self.bg = Some(code),
                        _ => self.ul = Some(code),
                    }
                }
                59 => self.ul = None,
                _ => {}
            }
            if tok.contains(':') && n == 4 {
                self.attrs[4] = !tok.ends_with(":0"); // 4:0 = no underline, 4:3 curly
            }
            i += step;
        }
    }

    fn restate(&self) -> String {
        let mut parts: Vec<String> = (1..10).filter(|&a| self.attrs[a]).map(|a| a.to_string()).collect();
        for c in vec![&self.fg, &self.bg, &self.ul] {
            if let Some(c) = c {
                parts.push(c.clone());
            }
        }
        if parts.is_empty() { String::new() } else { format!("\x1b[{}m", parts.join(";")) }
    }
}

/// Each line starts with the SGR state the previous lines left, so it renders alone.
fn self_contained_lines(text: &str) -> Vec<String> {
    let mut state = Sgr::default();
    let mut out = Vec::new();
    for line in text.lines() {
        out.push(format!("{}{}", state.restate(), line));
        let b = line.as_bytes();
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == 0x1b && b[i + 1] == b'[' {
                let mut j = i + 2;
                while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                    j += 1;
                }
                if j < b.len() && b[j] == b'm' {
                    state.apply(&line[i + 2..j]);
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }
    }
    out
}

pub(crate) fn tail_nonempty_lines(text: &str, lines: usize) -> Vec<String> {
    let mut v: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    // -N pads every line and lines start with SGR codes, so judge by visible text.
    while v.last().is_some_and(|s| crate::util::strip_ansi(s).trim().is_empty()) {
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

/// Tiled panes of a window. Floating panes are skipped: they overlap the layout, and since
/// tmux-git 34cd5da4e display-popup opens one (a modal pane) in the current window, so the
/// overview itself sits in the window it was opened from and would preview its own frames.
fn window_panes(window_id: &str) -> Vec<PaneGeom> {
    let panes_raw = tmux(&[
        "list-panes",
        "-t",
        window_id,
        "-F",
        "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{pane_floating_flag}#{pane_modal_flag}",
    ])
    .unwrap_or_default();
    let own = std::env::var("TMUX_PANE").unwrap_or_default();

    let mut panes = Vec::new();
    for line in panes_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 5 || parts[0] == own || parts.get(5).is_some_and(|f| f.contains('1')) {
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

/// Preview of a window: its tiled layout when it has several panes, else its one tiled pane.
/// Captures pane ids, never the window: a window target is its active pane, which is the
/// overview's own modal pane in the window it was opened from.
fn window_preview(window_id: &str, width: usize, height: usize) -> Vec<String> {
    let panes = window_panes(window_id);
    if let Some(lines) = window_layout_preview(&panes, width, height) {
        return lines;
    }
    match panes.first() {
        Some(p) => capture_preview(&p.id, height),
        None => capture_preview(window_id, height),
    }
}

fn window_layout_preview(panes: &[PaneGeom], width: usize, height: usize) -> Option<Vec<String>> {
    if panes.len() <= 1 || width < 12 || height < 3 {
        return None;
    }
    let panes = panes.to_vec();

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
        Row::WatchWindow(w) | Row::Window(w) => window_preview(&w.id, width, height),
        Row::Session(s) => match active_window_for_session(&s.id) {
            Some(window_id) => window_preview(&window_id, width, height),
            None => capture_preview(&s.id, height),
        },
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
