//! Account usage caches from pi-kit. Reading is local; endpoint work is a detached child.
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use crate::json::{self, Value};
use crate::util::{plain_truncate, text_width, visible_truncate_ansi};

const DIM: &str = "\x1b[38;5;244m";
const AMBER: &str = "\x1b[38;5;179m";
const RED: &str = "\x1b[38;5;203m";
const RESET: &str = "\x1b[0m";
static LAST_POLL: Mutex<Option<Instant>> = Mutex::new(None);

pub(crate) fn poll_if_due() {
    if env::var("TMUX_OVERVIEW_USAGE_POLL").as_deref() == Ok("off") { return; }
    let Ok(mut last) = LAST_POLL.lock() else { return };
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(300)) { return; }
    *last = Some(Instant::now());
    // The waiter owns/reaps the child; neither spawn nor wait is on the UI thread.
    std::thread::spawn(|| {
        if let Ok(mut child) = Command::new("pi-kit").args(["usage", "--poll"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() { let _ = child.wait(); }
    });
}
fn cache_dir() -> PathBuf {
    env::var_os("PI_USAGE_CACHE_DIR").map(PathBuf::from).unwrap_or_else(||
        env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(env::temp_dir).join("pi-usage"))
}
fn num(v: &Value, key: &str) -> Option<f64> { v.get(key)?.number() }
fn string<'a>(v: &'a Value, key: &str) -> Option<&'a str> { v.get(key)?.string() }
fn tint(color: &str, text: &str) -> String { format!("{color}{text}{RESET}") }

// Keep these formulas/colour precedence identical to pi-kit's status-line.ts renderLimit:
// elapsed/window rounded and clamped to 0..100; above-linear is warning, else >=90 red / >=70 amber.
fn linear(period: Option<f64>, reset: Option<f64>, now: f64) -> Option<f64> {
    let period = period.filter(|p| *p > 0.0)?;
    let remaining = (reset? - now.floor()).max(0.0);
    Some(((period - remaining).max(0.0) / period * 100.0).round().clamp(0.0, 100.0))
}
fn reset_text(seconds: f64, minutes: bool) -> Option<String> {
    if seconds <= 0.0 { return None; }
    let mins = (seconds / 60.0).ceil() as u64;
    if mins < 60 { return Some(format!("{mins}m")); }
    let hours = mins / 60; let rest = mins % 60;
    if mins < 1440 {
        return Some(if minutes && rest > 0 { format!("{hours}h{rest}m") }
            else if minutes || rest == 0 { format!("{hours}h") }
            else if hours < 2 { format!("{hours}h{rest}m") } else { format!("{}h", (mins as f64 / 60.0).round() as u64) });
    }
    let days = mins / 1440; let hours = ((mins % 1440) as f64 / 60.0).round() as u64;
    Some(if hours > 0 { format!("{days}d{hours}h") } else { format!("{days}d") })
}
fn window_label(w: &Value) -> Option<String> {
    if let Some(label) = string(w, "label").filter(|s| !s.is_empty()) { return Some(plain_truncate(label, 24)); }
    let period = num(w, "limit_window_seconds").filter(|n| *n > 0.0)?;
    for (unit, size) in [("d", 86400.0), ("h", 3600.0), ("m", 60.0), ("s", 1.0)] {
        if period % size == 0.0 { return Some(format!("{}{unit}", period / size)); }
    }
    Some(format!("{period}s"))
}
fn window(w: &Value, stale: bool, fetched: f64, now: f64, resets: bool) -> Option<String> {
    let used = num(w, "used_percent")?.round();
    if !(0.0..=100.0).contains(&used) { return None; }
    let label = window_label(w)?;
    let reset = num(w, "reset_at").or_else(|| num(w, "reset_after_seconds").map(|s| fetched / 1000.0 + s));
    // A reset invalidates a cached observation, including a formerly hit limit.
    if reset.is_some_and(|at| at <= now) { return None; }
    let period = num(w, "limit_window_seconds");
    let allowed = linear(period, reset, now);
    let usage = allowed.map(|a| format!("{used}/{a}")).unwrap_or_else(|| format!("{used}%"));
    let suffix = if resets { reset.and_then(|at| reset_text((at - now.floor()).max(0.0), period.unwrap_or(f64::INFINITY) < 86400.0)).map(|s| format!(" ↻{s}")).unwrap_or_default() } else { String::new() };
    let color = if stale { DIM } else if allowed.is_some_and(|a| used > a) { AMBER } else if used >= 90.0 { RED } else if used >= 70.0 { AMBER } else { DIM };
    Some(tint(color, &format!("{label} {usage}{suffix}")))
}
struct Subscription { value: Value, provider: String, account: String }
impl Subscription {
    fn render(&self, now: f64, resets: bool) -> String {
        let fetched = num(&self.value, "fetchedAt").unwrap_or(0.0);
        let stale = fetched <= 0.0 || fetched > now * 1000.0 || now * 1000.0 - fetched > 1800_000.0 || string(&self.value, "error") == Some("auth stale");
        let name = if self.provider == "anthropic" { format!("claude {}", self.account) } else if self.account == "main" { "codex".into() } else { format!("codex {}", self.account) };
        let windows: Vec<String> = self.value.get("windows").and_then(Value::array).unwrap_or(&[]).iter()
            .filter_map(|w| window(w, stale, fetched, now, resets)).collect();
        let mut out = tint(DIM, &format!("{name}{}", if stale || windows.is_empty() { " ?" } else { "" }));
        if !windows.is_empty() { out.push(' '); out.push_str(&windows.join(&format!("{DIM} · {RESET}"))); }
        if self.value.get("limitReached").is_some_and(Value::boolean) && !windows.is_empty() { out.push(' '); out.push_str(&tint(if stale { DIM } else { RED }, "limit")); }
        out
    }
}
pub(crate) fn line(width: usize) -> Option<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
    let mut files: Vec<_> = fs::read_dir(cache_dir()).ok()?.flatten().map(|e| e.path()).filter(|p| {
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        name.ends_with(".json") && ["anthropic", "openai-codex"].iter().any(|prefix| name == format!("{prefix}.json") || name.starts_with(&format!("{prefix}-")))
    }).collect();
    files.sort();
    let mut subscriptions = Vec::new();
    for path in files.into_iter().take(32) {
        if fs::metadata(&path).ok().is_none_or(|m| m.len() > 128 * 1024) { continue; }
        let Some(value) = fs::read_to_string(&path).ok().and_then(|s| json::parse(&s)) else { continue };
        if num(&value, "fetchedAt").is_none() || value.get("windows").and_then(Value::array).is_none() { continue; }
        let filename = path.file_name().unwrap().to_string_lossy();
        let provider = if filename.starts_with("anthropic") { "anthropic" } else { "openai-codex" }.to_string();
        let fallback = if filename == format!("{provider}.json") { "main" } else { "2" };
        let account = plain_truncate(string(&value, "account").unwrap_or(fallback), 14);
        subscriptions.push(Subscription { value, provider, account });
    }
    subscriptions.sort_by_key(|s| (s.provider != "anthropic", s.account != "main", s.account.clone()));
    if subscriptions.is_empty() { return None; }
    let render = |resets| subscriptions.iter().map(|s| s.render(now, resets)).collect::<Vec<_>>();
    let mut parts = render(true);
    if parts.iter().map(|s| text_width(s)).sum::<usize>() + 3 * (parts.len() - 1) > width { parts = render(false); }
    let combined = parts.join("   ");
    if text_width(&combined) <= width { return Some(combined); }
    // All accounts retain a place even in a narrow terminal; never wrap into key hints.
    let each = width.saturating_sub(3 * (parts.len() - 1)) / parts.len();
    Some(parts.iter().map(|s| visible_truncate_ansi(s, each)).collect::<Vec<_>>().join("   "))
}
