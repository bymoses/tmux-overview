use std::collections::{HashMap, HashSet};
use std::process::Command;

use crate::model::ProcStats;
use crate::tmux_api::tmux;

pub(crate) fn collect_window_labels() -> HashMap<String, Vec<String>> {
    let panes_raw = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{window_id}\t#{pane_index}\t#{pane_current_command}\t#{pane_title}\t#{host_short}",
    ])
    .unwrap_or_default();

    let mut panes_by_window: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for line in panes_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 5 {
            continue;
        }
        let title = parts[3].trim();
        let host = parts[4].trim();
        let label = if !title.is_empty() && title != host {
            title.to_string()
        } else {
            parts[2].to_string()
        };
        let pane_index = parts[1].parse::<usize>().unwrap_or(0);
        panes_by_window
            .entry(parts[0].to_string())
            .or_default()
            .push((pane_index, label));
    }

    let mut labels = HashMap::new();
    for (window_id, mut panes) in panes_by_window {
        panes.sort_by_key(|(pane_index, _)| *pane_index);
        labels.insert(window_id, panes.into_iter().map(|(_, label)| label).collect());
    }
    labels
}
pub(crate) fn collect_proc_stats() -> (HashMap<String, ProcStats>, HashMap<String, ProcStats>) {
    let panes_raw = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_id}\t#{window_id}\t#{pane_pid}",
    ])
    .unwrap_or_default();

    let mut session_roots: HashMap<String, Vec<i32>> = HashMap::new();
    let mut window_roots: HashMap<String, Vec<i32>> = HashMap::new();
    for line in panes_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        if let Ok(pid) = parts[2].parse::<i32>() {
            session_roots.entry(parts[0].to_string()).or_default().push(pid);
            window_roots.entry(parts[1].to_string()).or_default().push(pid);
        }
    }

    let ps_raw = Command::new("ps")
        .args(["-eo", "pid=,ppid=,pcpu=,rss="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let mut proc_cpu: HashMap<i32, f64> = HashMap::new();
    let mut proc_rss: HashMap<i32, u64> = HashMap::new();
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for line in ps_raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let Ok(pid) = parts[0].parse::<i32>() else { continue };
        let Ok(ppid) = parts[1].parse::<i32>() else { continue };
        let cpu = parts[2].parse::<f64>().unwrap_or(0.0);
        let rss = parts[3].parse::<u64>().unwrap_or(0);
        proc_cpu.insert(pid, cpu);
        proc_rss.insert(pid, rss);
        children.entry(ppid).or_default().push(pid);
    }

    fn sum_roots(
        roots: &[i32],
        proc_cpu: &HashMap<i32, f64>,
        proc_rss: &HashMap<i32, u64>,
        children: &HashMap<i32, Vec<i32>>,
    ) -> ProcStats {
        let mut stats = ProcStats::default();
        let mut seen: HashSet<i32> = HashSet::new();
        let mut stack = roots.to_vec();
        while let Some(pid) = stack.pop() {
            if !seen.insert(pid) {
                continue;
            }
            stats.cpu += proc_cpu.get(&pid).copied().unwrap_or(0.0);
            stats.rss_kb += proc_rss.get(&pid).copied().unwrap_or(0);
            if let Some(kids) = children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }
        stats
    }

    let session_stats = session_roots
        .iter()
        .map(|(id, roots)| (id.clone(), sum_roots(roots, &proc_cpu, &proc_rss, &children)))
        .collect();
    let window_stats = window_roots
        .iter()
        .map(|(id, roots)| (id.clone(), sum_roots(roots, &proc_cpu, &proc_rss, &children)))
        .collect();

    (session_stats, window_stats)
}
