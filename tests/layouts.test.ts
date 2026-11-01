import { afterAll, beforeAll, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";

const repo = resolve(import.meta.dir, "..");
const root = mkdtempSync(join(tmpdir(), "overview-layouts."));
const cache = join(root, "cache");
const bin = join(cache, "tmux-overview", "tmux-overview");
const quote = (s: string) => `'${s.replaceAll("'", `'\\''`)}'`;
const hex = (s: string) => Buffer.from(s).toString("hex");
function run(cmd: string[], env: Record<string, string | undefined> = {}) {
  const p = Bun.spawnSync(cmd, { env: { ...process.env, ...env }, stdout: "pipe", stderr: "pipe" });
  return { code: p.exitCode, out: p.stdout.toString().trim(), err: p.stderr.toString().trim() };
}
beforeAll(() => {
  const r = run(["make", "-C", repo, "build"], { XDG_CACHE_HOME: cache });
  expect(r.code, r.err).toBe(0);
}, 180_000);
afterAll(() => rmSync(root, { recursive: true, force: true }));

class LayoutFixture {
  dir = mkdtempSync(join(root, "test."));
  socket = join(this.dir, "tmux.sock");
  home = join(this.dir, "home");
  state = join(this.dir, "state");
  runtime = join(this.dir, "runtime");
  helper = join(this.dir, "doze-start");
  env: Record<string, string | undefined>;
  constructor() {
    for (const dir of [this.home, this.state, this.runtime]) mkdirSync(dir, { recursive: true });
    // Explicit helper path, never a fake tmux/pi on PATH. Publishes the public contract only.
    writeFileSync(this.helper, `#!/bin/sh
session= parent= worker= fork=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --session) session=$2; shift 2 ;;
    --parent) parent=$2; shift 2 ;;
    --worker) worker=$2; shift 2 ;;
    --fork) fork=1; shift ;;
    *) exit 2 ;;
  esac
done
tmux set-option -p -t "$TMUX_PANE" @agent_kind pi
tmux set-option -p -t "$TMUX_PANE" @agent_parent "$parent"
tmux set-option -p -t "$TMUX_PANE" @agent_name "$worker"
tmux set-option -p -t "$TMUX_PANE" @pi_session "$session"
tmux set-option -p -t "$TMUX_PANE" @test_fork "$fork"
tmux set-option -p -t "$TMUX_PANE" @test_runtime "$XDG_RUNTIME_DIR"
tmux set-option -p -t "$TMUX_PANE" @test_stale "\${PI_STALE-unset}"
tmux set-option -p -t "$TMUX_PANE" @agent_state dozing
exec /bin/sh
`);
    chmodSync(this.helper, 0o755);
    this.env = { HOME: this.home, XDG_STATE_HOME: this.state, XDG_RUNTIME_DIR: this.runtime,
      PI_DOZE_START: this.helper, PI_STALE: "should-be-unset", TMUX: "" };
  }
  tmux(...args: string[]) { return run(["tmux", "-S", this.socket, ...args], this.env); }
  overview(...args: string[]) { return run([bin, "--server", this.socket, ...args], this.env); }
  start() { expect(this.tmux("-f", "/dev/null", "new-session", "-d", "-s", "keep", "-c", this.dir, "/bin/sh").code).toBe(0); }
  cleanup() { this.tmux("kill-server"); }
  option(pane: string, option: string) { return this.tmux("show-option", "-pqv", "-t", pane, option).out; }
  async ready(panes: string[]) {
    const deadline = Date.now()+5000;
    while (Date.now() < deadline) {
      if (panes.every(p => this.option(p, "@agent_state") === "dozing")) return;
      await Bun.sleep(20);
    }
    throw new Error("helper did not publish dozing state");
  }
  legacy(rows: string[][]) {
    const file = join(this.dir, "legacy.tsv");
    writeFileSync(file, rows.map(row => row.join("\t")).join("\n")+"\n");
    return file;
  }
  session(name: string) { const file = join(this.dir, `${name}.jsonl`); writeFileSync(file, '{"type":"session","id":"test"}\n'); return file; }
}

test("save-all writes pi-aware v2, and restore skips existing sessions", async () => {
  const f = new LayoutFixture();
  try {
    f.start();
    const session = f.session("published");
    const pid = f.tmux("display-message", "-p", "-t", "=keep:", "#{pane_id}").out;
    for (const [key,value] of [["@agent_kind", "pi"], ["@agent_state", "dozing"], ["@pi_session", session]]) {
      expect(f.tmux("set-option", "-p", "-t", pid, key, value).code).toBe(0);
    }
    const saved = f.overview("save-all");
    expect(saved.code, saved.err).toBe(0);
    const dir = join(f.state, "tmux-overview", "layouts");
    const file = join(dir, readdirSync(dir)[0]);
    const text = readFileSync(file, "utf8");
    expect(text).toStartWith("v2\n");
    expect(text).toContain(hex(session));
    expect(text).toContain(hex(pid));
    const restored = f.overview("restore", file);
    expect(restored.code, restored.err).toBe(0);
    expect(restored.out).toContain("1 sessions skipped");
    expect(f.tmux("list-panes", "-a", "-F", "#{pane_id}").out).toBe(pid);
  } finally { f.cleanup(); }
});

test("legacy import creates layout-order panes, dozes pi, remaps workers, scrubs PI env", async () => {
  const f = new LayoutFixture();
  try {
    f.start();
    const parent = f.session("parent"); const worker = f.session("worker");
    // Get a native layout from the installed tmux (traditional and tmux-git JSON both work).
    expect(f.tmux("new-session", "-d", "-s", "shape", "-x", "120", "-y", "40", "/bin/sh").code).toBe(0);
    expect(f.tmux("split-window", "-d", "-t", "=shape:", "/bin/sh").code).toBe(0);
    const ids = f.tmux("list-panes", "-t", "=shape:", "-F", "#{pane_id}").out.split("\n");
    const layout = f.tmux("display-message", "-p", "-t", "=shape:", "#{window_layout}").out;
    f.tmux("kill-session", "-t", "=shape");
    // Reverse the file rows: layout leaf ids, not row order, determine placement.
    const snapshot = f.legacy([
      ["project", "3", "pair", layout, "120", "40", f.dir, "pi", worker, ids[1], ids[0], "worker's name"],
      ["project", "3", "pair", layout, "120", "40", f.home, "pi", parent, ids[0], "-", "-"],
    ]);
    const restored = f.overview("restore", snapshot, "--fork");
    expect(restored.code, restored.err).toBe(0);
    expect(restored.out).toContain("2 pi dozing / 1 workers relinked");
    const panes = f.tmux("list-panes", "-t", "=project:3", "-F", "#{pane_id}\t#{pane_current_path}").out.split("\n");
    const pids = panes.map(p => p.split("\t")[0]);
    await f.ready(pids);
    expect(panes[0]).toEndWith(f.home);
    expect(panes[1]).toEndWith(f.dir);
    expect(f.option(pids[0], "@pi_session")).toBe(parent);
    expect(f.option(pids[1], "@pi_session")).toBe(worker);
    expect(f.option(pids[1], "@agent_parent")).toBe(pids[0]);
    expect(f.option(pids[1], "@agent_name")).toBe("worker's name");
    expect(f.option(pids[0], "@test_fork")).toBe("1");
    expect(f.option(pids[0], "@test_stale")).toBe("unset");
    expect(f.option(pids[0], "@test_runtime")).toBe(f.runtime);
    const rerun = f.overview("restore", snapshot, "--fork");
    expect(rerun.code, rerun.err).toBe(0);
    expect(rerun.out).toContain("1 sessions skipped");
  } finally { f.cleanup(); }
});

test("restore leaves a shell when another pane already owns the pi session", () => {
  const f = new LayoutFixture();
  try {
    f.start();
    const file = f.session("owned");
    const pid = f.tmux("display-message", "-p", "-t", "=keep:", "#{pane_id}").out;
    for (const [key,value] of [["@agent_kind", "pi"], ["@agent_state", "idle"], ["@pi_session", file]]) f.tmux("set-option", "-p", "-t", pid, key, value);
    const layout = f.tmux("display-message", "-p", "-t", "=keep:", "#{window_layout}").out;
    const source = f.legacy([["missing", "1", "pi", layout, "120", "40", f.dir, "pi", file, "%old", "-", "-"]]);
    const restored = f.overview("restore", source);
    expect(restored.code, restored.err).toBe(0);
    expect(restored.out).toContain("0 pi dozing");
    expect(restored.err).toContain("already open");
  } finally { f.cleanup(); }
});

test("old per-session v1 remains readable without running plain pi", () => {
  const f = new LayoutFixture();
  try {
    f.start();
    const source = join(f.dir, "v1.tsv");
    writeFileSync(source, `v1\nsession\t${hex("old")}\ndefault_path\t${hex(f.dir)}\nwindow\t${hex("2")}\t${hex("old pi")}\t1\t\npane\t${hex("0")}\t1\t${hex(f.dir)}\t${hex("pi")}\n`);
    const restored = f.overview("restore", source);
    expect(restored.code, restored.err).toBe(0);
    expect(restored.out).toContain("0 pi dozing");
    expect(f.tmux("display-message", "-p", "-t", "=old:2", "#{pane_start_command}").out).toContain("exec /bin/sh");
  } finally { f.cleanup(); }
});

test("retention keeps newest 20 plus newest daily snapshots for seven UTC days", () => {
  const f = new LayoutFixture();
  try {
    f.start();
    expect(f.overview("save-all").code).toBe(0);
    const dir = join(f.state, "tmux-overview", "layouts");
    const initial = readdirSync(dir)[0]; const contents = readFileSync(join(dir, initial), "utf8");
    rmSync(join(dir, initial));
    const now = Math.floor(Date.now()/1000);
    for (let i=1;i<=30;i++) writeFileSync(join(dir, `${now-i}-000000000.tsv`), contents);
    const daily: string[] = [];
    for (let d=1;d<=8;d++) {
      const stamp = (Math.floor(now/86400)-d)*86400+3600;
      const name = `${stamp}-000000000.tsv`;
      writeFileSync(join(dir, name), contents);
      if (d <= 6) daily.push(name);
    }
    expect(f.overview("save-all").code).toBe(0);
    const retained = readdirSync(dir);
    expect(retained.length).toBe(26);
    for (const file of daily) expect(retained).toContain(file);
    expect(existsSync(join(dir, `${now-30}-000000000.tsv`))).toBe(false);
  } finally { f.cleanup(); }
});
