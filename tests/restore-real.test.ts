// Opt-in integration test: reads the operator's real snapshot, never restores to a live server.
import { expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";

const source = process.env.TMUX_OVERVIEW_REAL_SNAPSHOT;
const real = source ? test : test.skip;
real("real snapshot: every pane/layout/cwd, dozing pi, workers relinked, real fork wake", async () => {
  const root = mkdtempSync(join(tmpdir(), "overview-real."));
  const runtime = join(root, "runtime"); mkdirSync(runtime, { mode: 0o700 });
  const socket = join(root, "tmux.sock");
  const env = Object.fromEntries(Object.entries(process.env).filter(([k,v]) => !k.startsWith("PI_") && v !== undefined));
  Object.assign(env, { TMUX: "", XDG_RUNTIME_DIR: runtime });
  const helper = process.env.PI_DOZE_START || "/data-shared/workspace/bms/devices/shared/agents/pi-kit/bin/pi-doze-start.sh";
  Object.assign(env, { PI_DOZE_START: helper });
  const run = (args: string[]) => {
    const p = Bun.spawnSync(args, { env, stdout: "pipe", stderr: "pipe" });
    return { code: p.exitCode, out: p.stdout.toString().trim(), err: p.stderr.toString().trim() };
  };
  const tmux = (...args: string[]) => run(["tmux", "-S", socket, ...args]);
  const option = (pid: string, name: string) => tmux("show-option", "-pqv", "-t", pid, name).out;
  const until = async (description: string, predicate: () => boolean, ms = 20_000) => {
    const deadline = Date.now()+ms;
    while (Date.now() < deadline) { if (predicate()) return; await Bun.sleep(50); }
    throw new Error(`timeout: ${description}`);
  };
  const nearest = (cwd: string) => {
    let path = cwd.replace(/ \(deleted\)$/, "");
    while (!existsSync(path) || !statSync(path).isDirectory()) { const parent = dirname(path); if (path === parent) return process.env.HOME!; path = parent; }
    return path;
  };
  const leafIds = (layout: string) => layout.startsWith("{")
    ? [...layout.matchAll(/"I":"(%\d+)"/g)].map(m=>m[1])
    : [...layout.matchAll(/\d+x\d+,\d+,\d+,(\d+)(?=[,}\]]|$)/g)].map(m=>`%${m[1]}`);
  const normalized = (layout: string): unknown => {
    if (!layout.startsWith("{")) return layout.slice(layout.indexOf(",")+1).replace(/(\d+x\d+,\d+,\d+),\d+/g, "$1,ID");
    return JSON.parse(layout, (key,value) => key === "I" ? undefined : value);
  };
  const rows = readFileSync(source!, "utf8").trim().split("\n").map(line=>line.split("\t"));
  const groups = new Map<string, string[][]>();
  for (const row of rows) { const key = `${row[0]}:${row[1]}`; const group = groups.get(key) || []; group.push(row); groups.set(key,group); }
  const mapping = new Map<string,string>();
  const fallback: string[] = [];
  let dozed = 0, workers = 0;
  let forkedSession = "";
  try {
    const built = run(["make", "-C", resolve(import.meta.dir, ".."), "build"]);
    expect(built.code, built.err).toBe(0);
    const wrapper = resolve(import.meta.dir, "..", "tmux-overview.sh");
    const restored = run([wrapper, "--server", socket, "restore", source!, "--fork"]);
    expect(restored.code, restored.err).toBe(0);
    expect(tmux("list-sessions", "-F", "#{session_name}").out.split("\n").sort()).toEqual([...new Set(rows.map(r=>r[0]))].sort());
    expect(tmux("list-panes", "-a", "-F", "#{pane_id}").out.split("\n").length).toBe(rows.length);
    for (const [key,group] of groups) {
      const saved = group[0];
      const current = tmux("display-message", "-p", "-t", `=${key}`, "#{window_layout}\t#{window_name}\t#{window_width}\t#{window_height}");
      expect(current.code,current.err).toBe(0);
      const [layout,name,width,height] = current.out.split("\t");
      expect(name).toBe(saved[2]); expect(width).toBe(saved[4]); expect(height).toBe(saved[5]);
      expect(normalized(layout), key).toEqual(normalized(saved[3]));
      const oldIds = leafIds(saved[3]), newIds = leafIds(layout);
      expect(newIds.length).toBe(group.length);
      oldIds.forEach((old,i)=>mapping.set(old,newIds[i]));
      for (const row of group) {
        const pid = mapping.get(row[9])!; expect(pid).toStartWith("%");
        const cwd = tmux("display-message", "-p", "-t", pid, "#{pane_current_path}").out;
        const expected = nearest(row[6]); expect(cwd, `${key} ${pid}`).toBe(expected);
        if (expected !== row[6]) fallback.push(`${key}: ${row[6]} → ${expected}`);
      }
    }
    const pi = rows.filter(r=>r[7] === "pi");
    await until("all restored pi panes dozing", () => pi.every(r=>option(mapping.get(r[9])!, "@agent_state") === "dozing"));
    for (const row of pi) {
      const pid = mapping.get(row[9])!;
      expect(option(pid, "@pi_session")).toBe(row[8]);
      expect(option(pid, "@agent_kind")).toBe("pi");
      expect(tmux("display-message", "-p", "-t", pid, "#{pane_current_command}").out).not.toBe("pi");
      const processes = tmux("display-message", "-p", "-t", pid, "#{pane_pid}").out;
      expect(processes).toMatch(/^\d+$/);
      dozed++;
      if (row[10] !== "-") {
        expect(option(pid,"@agent_parent")).toBe(mapping.get(row[10]));
        expect(option(pid,"@agent_name")).toBe(row[11]); workers++;
      }
    }
    const wake = pi.find(r=>r[10] === "-")!;
    const pid = mapping.get(wake[9])!;
    const size = statSync(wake[8]).size;
    // The dozer publishes its public wait channel only once its input loop is armed.
    expect(tmux("wait-for", `pi-doze-${pid}`).code).toBe(0);
    expect(tmux("send-keys", "-t", pid, "Enter").code).toBe(0);
    await until("real pi opens a forked session", () => { const file = option(pid,"@pi_session"); return !!file && file !== wake[8] && existsSync(file); }, 60_000);
    forkedSession = option(pid,"@pi_session");
    expect(statSync(wake[8]).size).toBe(size);
    expect(tmux("capture-pane", "-p", "-t", pid).out).not.toContain("pi-doze-start:");
    const report = { source, panes: rows.length, windows: groups.size, sessions: new Set(rows.map(r=>r[0])).size,
      dozed, workers, cwdFallbacks: fallback, forkedSession, originalBytes: size, restoreOutput: restored.out, restoreWarnings: restored.err };
    const reportPath = process.env.TMUX_OVERVIEW_REAL_REPORT || "/tmp/pi-orchestrator/overview-layouts/restore-report.json";
    writeFileSync(reportPath, JSON.stringify(report,null,2)+"\n");
    console.log(JSON.stringify(report));
  } finally {
    tmux("kill-server");
    rmSync(root,{ recursive: true, force: true });
    if (forkedSession) rmSync(forkedSession,{force:true});
  }
}, 180_000);
