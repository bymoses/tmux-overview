use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn session_display_name(name: &str) -> String {
    name.split('/')
        .map(|component| match component {
            "__float" => "◫ float",
            "__agents" => "󰚩 agents",
            _ => component,
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn hex_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn hex_decode(s: &str) -> String {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if let (Some(hi), Some(lo)) = (val(bytes[i]), val(bytes[i + 1])) {
            out.push((hi << 4) | lo);
        }
        i += 2;
    }
    String::from_utf8_lossy(&out).to_string()
}

pub(crate) fn format_saved_time(time: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(time)
        .unwrap_or_else(|_| Duration::from_secs(0));
    let fmt = if elapsed < Duration::from_secs(24 * 60 * 60) {
        "+%H:%M"
    } else {
        "+%b %d"
    };
    let epoch = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
        .to_string();
    let at = format!("@{}", epoch);
    let out = Command::new("date")
        .args(["-d", &at, fmt])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if out.is_empty() {
        if elapsed < Duration::from_secs(24 * 60 * 60) {
            let hours = elapsed.as_secs() / 3600;
            let mins = (elapsed.as_secs() % 3600) / 60;
            format!("{:02}:{:02} ago", hours, mins)
        } else {
            format!("{}d ago", elapsed.as_secs() / 86_400)
        }
    } else {
        out
    }
}
pub(crate) fn char_width(ch: char) -> usize {
    let c = ch as u32;
    if ch.is_control()
        || (0x0300..=0x036f).contains(&c)
        || (0x1ab0..=0x1aff).contains(&c)
        || (0x1dc0..=0x1dff).contains(&c)
        || (0x20d0..=0x20ff).contains(&c)
        || (0xfe00..=0xfe0f).contains(&c)
        || c == 0x200d
    {
        0
    } else if (0x1100..=0x115f).contains(&c)
        || (0x2329..=0x232a).contains(&c)
        || (0x2e80..=0xa4cf).contains(&c)
        || (0xac00..=0xd7a3).contains(&c)
        || (0xf900..=0xfaff).contains(&c)
        || (0xfe10..=0xfe19).contains(&c)
        || (0xfe30..=0xfe6f).contains(&c)
        || (0xff00..=0xff60).contains(&c)
        || (0xffe0..=0xffe6).contains(&c)
        || (0x1f000..=0x1faff).contains(&c)
    {
        2
    } else {
        1
    }
}

pub(crate) fn visible_truncate_ansi(s: &str, max: usize) -> String {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut visible = 0usize;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }

            // Keep only SGR colour/style sequences (CSI ... m). Drop cursor
            // movement, clear-screen, OSC/title, etc. Captured panes can
            // contain those and they would otherwise corrupt the overview UI.
            if bytes[i] == b'[' {
                let start = i - 1;
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7e).contains(&b) {
                        if b == b'm' {
                            out.push_str(&String::from_utf8_lossy(&bytes[start..i]));
                        }
                        break;
                    }
                }
            } else if matches!(bytes[i], b']' | b'P' | b'_' | b'^' | b'X') {
                // String controls: OSC (]), DCS (P), APC (_), PM (^), SOS (X).
                // Skip the whole payload. DCS/tmux passthrough payloads can
                // contain nested escapes; leaking those is enough to corrupt
                // the overview cursor position.
                let is_osc = bytes[i] == b']';
                i += 1;
                while i < bytes.len() {
                    if is_osc && bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else if matches!(bytes[i], b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/') {
                // Character set selection is ESC plus an intermediate plus one
                // final byte; drop the whole sequence.
                i = (i + 2).min(bytes.len());
            } else {
                // Other one/two byte escape sequences.
                i += 1;
            }
            continue;
        }

        if visible >= max {
            break;
        }

        let Some(ch) = s[i..].chars().next() else { break };
        i += ch.len_utf8();
        let display_ch = if ch == '\t' { ' ' } else { ch };
        let cw = char_width(display_ch);
        if cw == 0 && display_ch.is_control() {
            continue;
        }
        if visible + cw > max {
            break;
        }
        out.push(display_ch);
        visible += cw;
    }
    out.push_str("\x1b[0m");
    out
}
pub(crate) fn plain_truncate(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let display_ch = if ch == '\t' { ' ' } else { ch };
        let cw = char_width(display_ch);
        if cw == 0 && display_ch.is_control() {
            continue;
        }
        if width + cw > max {
            break;
        }
        out.push(display_ch);
        width += cw;
    }
    out
}
pub(crate) fn text_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            while let Some(c) = chars.next() {
                if c.is_ascii_alphabetic() || c == 'm' {
                    break;
                }
            }
        } else if ch == '\t' {
            width += 1;
        } else {
            width += char_width(ch);
        }
    }
    width
}

pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            while let Some(c) = chars.next() {
                if c.is_ascii_alphabetic() || c == 'm' {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}
