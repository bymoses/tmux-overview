import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";

// End-to-end tests for src/main.rs.
//
// Run with:
//   bun test tests/e2e.test.ts
//   bun test tests/e2e.test.ts -t startup
//
// The tests drive the overview as a black box inside isolated tmux servers.
// They pass -S with a per-test socket, set temporary HOME/XDG dirs, and clear
// TMUX when talking to tmux from the test runner, so the user's live tmux server
// is never mutated by this suite.

const REPO_DIR = resolve(import.meta.dir, "..");
const SRC = join(REPO_DIR, "src/main.rs");
const TMP_ROOT = mkdtempSync(join(tmpdir(), "tmux-overview-e2e."));
const BUILD_DIR = join(TMP_ROOT, "build");
const BIN = join(BUILD_DIR, "tmux-overview");
const RUST_IMAGE = process.env.TMUX_OVERVIEW_RUST_IMAGE ?? "rust:1-alpine";
const OVERVIEW_W = process.env.TMUX_OVERVIEW_E2E_WIDTH ?? "120";
const OVERVIEW_H = process.env.TMUX_OVERVIEW_E2E_HEIGHT ?? "34";
const TEST_TIMEOUT_MS = 30_000;

type CmdResult = { exitCode: number; stdout: string; stderr: string };

function run(cmd: string[], env: Record<string, string | undefined> = {}): CmdResult {
  const result = Bun.spawnSync({
    cmd,
    env: { ...process.env, ...env },
    stdout: "pipe",
    stderr: "pipe",
  });
  return {
    exitCode: result.exitCode ?? 1,
    stdout: new TextDecoder().decode(result.stdout),
    stderr: new TextDecoder().decode(result.stderr),
  };
}

function commandExists(command: string): boolean {
  return run(["sh", "-c", `command -v ${command} >/dev/null 2>&1`]).exitCode === 0;
}

const missingReasons = [
  commandExists("tmux") ? "" : "tmux is unavailable",
  commandExists("rustc") || commandExists("docker")
    ? ""
    : "rustc is unavailable and docker is unavailable; cannot build tmux-overview",
].filter(Boolean);

const runnable = missingReasons.length === 0;
const e2e = runnable ? test : test.skip;

function shQuote(value: string): string {
  return `'${value.replaceAll("'", `'\\''`)}'`;
}

function hexEncode(value: string): string {
  return Buffer.from(value, "utf8").toString("hex");
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

function buildOverview(): void {
  mkdirSync(BUILD_DIR, { recursive: true });

  if (commandExists("rustc")) {
    const built = run(["rustc", "-C", "debuginfo=0", SRC, "-o", BIN]);
    expect(built.exitCode, built.stderr).toBe(0);
    return;
  }

  const built = run([
    "docker",
    "run",
    "--rm",
    "-v",
    `${REPO_DIR}:/src:ro`,
    "-v",
    `${BUILD_DIR}:/out`,
    "-w",
    "/src",
    RUST_IMAGE,
    "rustc",
    "-C",
    "debuginfo=0",
    "/src/src/main.rs",
    "-o",
    "/out/tmux-overview",
  ]);
  expect(built.exitCode, built.stderr).toBe(0);
}

beforeAll(() => {
  if (!runnable) {
    console.warn(`Skipping tmux overview e2e tests: ${missingReasons.join("; ")}`);
    return;
  }
  buildOverview();
  // The Docker fallback build alone takes ~5s, bun's default hook timeout.
}, 180_000);

afterAll(() => {
  rmSync(TMP_ROOT, { recursive: true, force: true });
});

class Fixture {
  readonly slug: string;
  readonly testTmp: string;
  readonly home: string;
  readonly cache: string;
  readonly state: string;
  readonly socket: string;
  readonly runnerSession: string;
  runnerPane = "";

  constructor(slug: string) {
    this.slug = `${slug}_${process.pid}_${Date.now()}`.replace(/[^A-Za-z0-9_]/g, "_");
    this.testTmp = join(TMP_ROOT, this.slug);
    this.home = join(this.testTmp, "home");
    this.cache = join(this.testTmp, "xdg-cache");
    this.state = join(this.testTmp, "xdg-state");
    this.socket = join(this.testTmp, "tmux.sock");
    this.runnerSession = `runner_${this.slug}`;
  }

  tmux(args: string[]): CmdResult {
    return run(["tmux", "-S", this.socket, ...args], { TMUX: "" });
  }

  setup(): void {
    mkdirSync(this.home, { recursive: true });
    mkdirSync(this.cache, { recursive: true });
    mkdirSync(this.state, { recursive: true });

    const started = this.tmux([
      "-f",
      "/dev/null",
      "new-session",
      "-d",
      "-x",
      OVERVIEW_W,
      "-y",
      OVERVIEW_H,
      "-s",
      this.runnerSession,
      "-n",
      "overview",
      `env HOME=${shQuote(this.home)} XDG_CACHE_HOME=${shQuote(this.cache)} XDG_STATE_HOME=${shQuote(this.state)} /bin/sh`,
    ]);
    expect(started.exitCode, started.stderr).toBe(0);

    const pane = this.tmux(["display-message", "-p", "-t", `${this.runnerSession}:overview`, "#{pane_id}"]);
    expect(pane.exitCode, pane.stderr).toBe(0);
    this.runnerPane = pane.stdout.trim();
    expect(this.runnerPane).toStartWith("%");
  }

  cleanup(): void {
    if (existsSync(this.socket)) {
      this.tmux(["kill-server"]);
    }
  }

  fail(message: string): never {
    let capture = "";
    try {
      capture = this.runnerPane ? this.captureRunner() : "";
    } catch {
      capture = "";
    }
    throw new Error(`${message}${capture ? `\n--- runner pane capture ---\n${capture}\n--- end capture ---` : ""}`);
  }

  async waitUntil(description: string, predicate: () => boolean | Promise<boolean>, timeoutMs = 5_000): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (await predicate()) return;
      await sleep(50);
    }
    if (await predicate()) return;
    this.fail(`timeout waiting for ${description}`);
  }

  capturePane(target: string): string {
    const captured = this.tmux(["capture-pane", "-p", "-J", "-t", target, "-S", "0", "-E", "-"]);
    expect(captured.exitCode, captured.stderr).toBe(0);
    return captured.stdout;
  }

  captureRunner(): string {
    return this.capturePane(this.runnerPane);
  }

  captureRunnerAnsi(): string {
    const captured = this.tmux(["capture-pane", "-p", "-e", "-t", this.runnerPane]);
    expect(captured.exitCode, captured.stderr).toBe(0);
    return captured.stdout;
  }

  runnerContains(needle: string): boolean {
    return this.captureRunner().includes(needle);
  }

  runnerLacks(needle: string): boolean {
    return !this.captureRunner().includes(needle);
  }

  paneContains(target: string, needle: string): boolean {
    return this.capturePane(target).includes(needle);
  }

  sendRunnerKey(...keys: string[]): void {
    const sent = this.tmux(["send-keys", "-t", this.runnerPane, ...keys]);
    expect(sent.exitCode, sent.stderr).toBe(0);
  }

  sendRunnerLiteral(text: string): void {
    const sent = this.tmux(["send-keys", "-l", "-t", this.runnerPane, text]);
    expect(sent.exitCode, sent.stderr).toBe(0);
  }

  async selectPreviewText(text: string, maxSteps = 40): Promise<void> {
    for (let i = 0; i <= maxSteps; i++) {
      if (this.runnerContains(text)) return;
      this.sendRunnerKey("j");
      await sleep(80);
    }
    this.fail(`could not select preview text ${text}`);
  }

  /** Start the overview; `ready` is the text that marks its first render. */
  async launchOverview(ready = "tmux overview"): Promise<void> {
    const cmd = [
      "env",
      `HOME=${shQuote(this.home)}`,
      `XDG_CACHE_HOME=${shQuote(this.cache)}`,
      `XDG_STATE_HOME=${shQuote(this.state)}`,
      `TMUX_OVERVIEW_WIDTH=${shQuote(OVERVIEW_W)}`,
      `TMUX_OVERVIEW_HEIGHT=${shQuote(OVERVIEW_H)}`,
      "TMUX_OVERVIEW_STATS_INTERVAL=60",
      "TMUX_OVERVIEW_TITLE_INTERVAL_MS=5000",
      shQuote(BIN),
    ].join(" ");
    this.sendRunnerKey(cmd, "C-m");
    await this.waitUntil("overview initial render", () => this.runnerContains(ready));
  }

  async quitOverview(): Promise<void> {
    if (!this.runnerPane || !existsSync(this.socket)) return;
    this.sendRunnerKey("q");
    try {
      await this.waitUntil("overview quit", () => this.runnerLacks("tmux overview"), 1_500);
    } catch {
      // A failed quit is best-effort cleanup only; kill-server in cleanup() is authoritative.
    }
  }

  async createSessionWithOutput(session: string, window: string, text: string): Promise<void> {
    const created = this.tmux(["new-session", "-d", "-s", session, "-n", window, "-c", this.testTmp, "/bin/sh"]);
    expect(created.exitCode, created.stderr).toBe(0);
    this.tmux(["send-keys", "-l", "-t", `${session}:${window}.0`, `printf '%s\\n' ${shQuote(text)}`]);
    this.tmux(["send-keys", "-t", `${session}:${window}.0`, "C-m"]);
    await this.waitUntil(`pane output ${text}`, () => this.paneContains(`${session}:${window}.0`, text), 3_000);
  }

  savedSessionFile(session: string): string {
    return join(this.state, "tmux-overview", "sessions", `${hexEncode(session)}.tsv`);
  }

  createSavedSessionFile(session: string, window: string, cwd = this.testTmp): string {
    const path = this.savedSessionFile(session);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(
      path,
      [
        "v1",
        `session\t${hexEncode(session)}`,
        `default_path\t${hexEncode(cwd)}`,
        `window\t${hexEncode("1")}\t${hexEncode(window)}\t1\t${hexEncode("")}`,
        `pane\t${hexEncode("0")}\t1\t${hexEncode(cwd)}\t${hexEncode("bash")}`,
        "",
      ].join("\n"),
    );
    return path;
  }

  windowNames(session: string): string {
    const listed = this.tmux(["list-windows", "-t", session, "-F", "#{window_name}"]);
    expect(listed.exitCode, listed.stderr).toBe(0);
    return listed.stdout;
  }

  defaultServerLacksSession(session: string): boolean {
    // Safe read-only probe. TMUX= avoids accidentally targeting this fixture's
    // isolated socket through the environment; no live/default mutation occurs.
    return run(["tmux", "has-session", "-t", session], { TMUX: "" }).exitCode !== 0;
  }
}

async function withFixture(slug: string, body: (f: Fixture) => Promise<void> | void): Promise<void> {
  const f = new Fixture(slug);
  f.setup();
  try {
    await body(f);
  } finally {
    await f.quitOverview();
    f.cleanup();
  }
}

e2e("no live-server damage: all mutations stay on the isolated socket", async () => {
  await withFixture("live_probe", async (f) => {
    const session = `e2e_live_probe_${process.pid}`;
    await f.createSessionWithOutput(session, "main", "LIVE_PROBE_OUTPUT");
    expect(f.defaultServerLacksSession(session)).toBe(true);

    await f.launchOverview();
    f.sendRunnerKey("g", "D", "n", "q");

    expect(f.tmux(["has-session", "-t", session]).exitCode).toBe(0);
    expect(f.defaultServerLacksSession(session)).toBe(true);
  });
}, TEST_TIMEOUT_MS);

e2e("startup rendering hides _popup_* sessions", async () => {
  await withFixture("startup", async (f) => {
    const session = `alpha_startup_${process.pid}`;
    const popup = `_popup_hidden_${process.pid}`;
    await f.createSessionWithOutput(session, "main", "STARTUP_RENDER_OK");
    await f.createSessionWithOutput(popup, "scratch", "POPUP_SHOULD_BE_HIDDEN");

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("startup preview content", () => f.runnerContains("STARTUP_RENDER_OK"));
    const screen = f.captureRunner();
    expect(screen).toContain("tmux overview");
    expect(screen).toContain(session);
    expect(screen).toContain("main");
    expect(screen).toContain("STARTUP_RENDER_OK");
    expect(screen).toContain("◆");
    expect(screen).not.toContain("○");
    expect(screen).not.toContain(popup);
    expect(screen).not.toContain("POPUP_SHOULD_BE_HIDDEN");
  });
}, TEST_TIMEOUT_MS);

// capture-pane -e writes attributes as changes carried across lines and trims blank
// cells; pi's message boxes are a background spanning lines and the full width.
e2e("preview keeps backgrounds that span lines and the full width", async () => {
  await withFixture("bgspan", async (f) => {
    const session = `alpha_bg_${process.pid}`;
    await f.createSessionWithOutput(session, "main", "BG_READY");
    const pane = `${session}:main.0`;
    f.tmux(["send-keys", "-l", "-t", pane, "clear; printf '\\033[48;2;10;20;30mBGTOP\\033[K\\nBGNEXT\\033[K\\033[0m\\nPLAIN\\n'"]);
    f.tmux(["send-keys", "-t", pane, "C-m"]);
    await f.waitUntil("pane output", () => f.paneContains(pane, "PLAIN"), 3_000);

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("preview content", () => f.runnerContains("BGNEXT"));
    const lines = f.captureRunnerAnsi().split("\n");
    const next = lines.find((l) => l.includes("BGNEXT"))!;
    expect(next).toMatch(/48;2;10;20;30m[^\x1b]*BGNEXT/); // restated on its own line
    expect(next).toMatch(/BGNEXT {4}/); // the blank cells after the text keep the background
    expect(lines.find((l) => l.includes("PLAIN"))!).not.toMatch(/48;2;10;20;30m[^\x1b]*PLAIN/);
  });
}, TEST_TIMEOUT_MS);

e2e("navigation, preview resize, and preview toggle", async () => {
  await withFixture("navigation", async (f) => {
    const session = `alpha_nav_${process.pid}`;
    await f.createSessionWithOutput(session, "main", "NAV_PREVIEW_CONTENT");

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("selected session preview", () => f.runnerContains("NAV_PREVIEW_CONTENT"));

    const previewStateFile = join(f.cache, "tmux-overview", "preview-percent");
    f.sendRunnerKey("+");
    await f.waitUntil("preview grows", () => f.runnerContains("preview 70%"));
    expect(readFileSync(previewStateFile, "utf8").trim()).toBe("70");

    await f.quitOverview();
    await f.launchOverview();
    await f.waitUntil("preview size restored", () => f.runnerContains("preview 70%"));

    f.sendRunnerKey("-");
    await f.waitUntil("preview shrinks", () => f.runnerContains("preview 65%"));
    expect(readFileSync(previewStateFile, "utf8").trim()).toBe("65");
    f.sendRunnerKey("?");
    await f.waitUntil("help modal", () => f.runnerContains("tmux overview help"));
    expect(f.captureRunner()).toContain("S save layout");
    f.sendRunnerKey("C-m");
    await f.waitUntil("help modal closed", () => f.runnerLacks("tmux overview help"));

    f.sendRunnerKey("v");
    await f.waitUntil("preview hidden", () => f.runnerLacks("preview 65%"));
    expect(f.captureRunner()).not.toContain("NAV_PREVIEW_CONTENT");

    f.sendRunnerKey("M-s");
    await f.waitUntil("overview closed by Alt+s", () => f.runnerLacks("tmux overview"));
  });
}, TEST_TIMEOUT_MS);

e2e("picker keeps a four-row margin around deep selections", async () => {
  await withFixture("scrolloff", async (f) => {
    const session = `alpha_scrolloff_${process.pid}`;
    await f.createSessionWithOutput(session, "window_00", "SCROLLOFF_PREVIEW");
    for (let i = 1; i < 12; i++) {
      const name = `window_${String(i).padStart(2, "0")}`;
      expect(f.tmux(["new-window", "-d", "-t", session, "-n", name, "-c", f.testTmp, "/bin/sh"]).exitCode).toBe(0);
    }
    const targetName = "window_08";
    const targetId = f.tmux(["display-message", "-p", "-t", `${session}:${targetName}`, "#{window_id}"]).stdout.trim();
    const cursorFile = join(f.cache, "tmux-overview", "cursor");

    await f.launchOverview();
    f.sendRunnerKey("g");
    for (let i = 0; i < 9; i++) {
      f.sendRunnerKey("j");
      await sleep(30);
    }
    await f.waitUntil("deep picker row selected", () =>
      existsSync(cursorFile) && readFileSync(cursorFile, "utf8").includes(targetId),
    );

    const selectedLine = f.captureRunner().split("\n").findIndex((line) => line.includes(targetName));
    expect(selectedLine).toBe(6);
  });
}, TEST_TIMEOUT_MS);

e2e("add modal creates windows and sessions", async () => {
  await withFixture("add", async (f) => {
    const session = `alpha_add_${process.pid}`;
    const windowName = `new_window_${process.pid}`;
    const sessionName = `new_session_${process.pid}`;
    const defaultPath = join(f.testTmp, "add-default-path");
    mkdirSync(defaultPath, { recursive: true });
    await f.createSessionWithOutput(session, "main", "ADD_BASE_PREVIEW");
    expect(f.tmux(["set-option", "-t", session, "@overview_default_path", defaultPath]).exitCode).toBe(0);

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("add base session visible", () => f.runnerContains(session));

    f.sendRunnerKey("a");
    await f.waitUntil("add modal", () => f.runnerContains("window name, or s:name"));
    f.sendRunnerLiteral(windowName);
    f.sendRunnerKey("C-m");
    await f.waitUntil("new window listed", () => f.windowNames(session).includes(windowName));
    await f.waitUntil("new window visible", () => f.runnerContains(windowName) && f.runnerLacks("enter apply"));
    const newWindowPath = f.tmux(["display-message", "-p", "-t", `${session}:${windowName}`, "#{pane_current_path}"]).stdout.trim();
    expect(newWindowPath).toBe(defaultPath);
    expect(f.captureRunner()).toContain("path ");

    f.sendRunnerKey("a");
    await f.waitUntil("second add modal", () => f.runnerContains("add"));
    f.sendRunnerLiteral(`s:${sessionName}`);
    f.sendRunnerKey("C-m");
    await f.waitUntil("new session exists", () => f.tmux(["has-session", "-t", sessionName]).exitCode === 0);
    await f.waitUntil("new session visible", () => f.runnerContains(sessionName));
  });
}, TEST_TIMEOUT_MS);

e2e("watchlist and cursor position persist across overview invocations", async () => {
  await withFixture("watch_cursor", async (f) => {
    const session = `alpha_watch_${process.pid}`;
    const first = `main_watch_${process.pid}`;
    const second = `second_watch_${process.pid}`;
    await f.createSessionWithOutput(session, first, "WATCH_FIRST_PREVIEW");
    expect(f.tmux(["new-window", "-d", "-t", session, "-n", second, "-c", f.testTmp, "/bin/sh"]).exitCode).toBe(0);
    f.tmux(["send-keys", "-l", "-t", `${session}:${second}.0`, `printf '%s\\n' 'WATCH_SECOND_PREVIEW'`]);
    f.tmux(["send-keys", "-t", `${session}:${second}.0`, "C-m"]);
    await f.waitUntil("second pane output", () => f.paneContains(`${session}:${second}.0`, "WATCH_SECOND_PREVIEW"), 3_000);
    const secondId = f.tmux(["display-message", "-p", "-t", `${session}:${second}`, "#{window_id}"]).stdout.trim();
    const cursorFile = join(f.cache, "tmux-overview", "cursor");

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("cursor moved to first session", () => existsSync(cursorFile) && readFileSync(cursorFile, "utf8").startsWith("session\t"));
    f.sendRunnerKey("j");
    await f.waitUntil("cursor moved to first window", () => existsSync(cursorFile) && readFileSync(cursorFile, "utf8").startsWith("window\t"));
    f.sendRunnerKey("j");
    await f.waitUntil("second window row selected", () => existsSync(cursorFile) && readFileSync(cursorFile, "utf8").includes(secondId));
    f.sendRunnerKey("w");
    await f.waitUntil("watchlist visible", () => f.runnerContains("watchlist") && f.runnerContains(second));
    expect(readFileSync(cursorFile, "utf8")).toContain(secondId);
    expect(readFileSync(join(f.cache, "tmux-overview", "watchlist.tsv"), "utf8").trim()).not.toBe("");
    f.sendRunnerKey("q");
    await f.waitUntil("overview quit after watch", () => f.runnerLacks("tmux overview"));

    await f.launchOverview();
    await f.waitUntil("watch cursor restored", () => f.runnerContains("watchlist") && f.runnerContains(second));
  });
}, TEST_TIMEOUT_MS);

e2e("slash-named sessions render as independently foldable trees", async () => {
  await withFixture("nested_sessions", async (f) => {
    const parent = `alpha_tree_${process.pid}`;
    const child = `${parent}/child`;
    const grandchild = `${child}/grandchild`;
    const parentWindow = `parent_window_${process.pid}`;
    const childWindow = `child_window_${process.pid}`;
    const grandchildWindow = `grandchild_window_${process.pid}`;
    await f.createSessionWithOutput(parent, parentWindow, "PARENT_TREE_PREVIEW");
    await f.createSessionWithOutput(child, childWindow, "CHILD_TREE_PREVIEW");
    await f.createSessionWithOutput(grandchild, grandchildWindow, "GRANDCHILD_TREE_PREVIEW");

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("nested sessions visible", () => {
      const screen = f.captureRunner();
      return screen.includes(parent) && screen.includes("child") && screen.includes("grandchild");
    });
    const expanded = f.captureRunner();
    expect(expanded.indexOf(parent)).toBeLessThan(expanded.indexOf("child"));
    expect(expanded.indexOf("child")).toBeLessThan(expanded.indexOf("grandchild"));
    expect(expanded).toContain("├─◆");
    expect(expanded).toContain("└─ ▾ child");
    expect(expanded).toContain("└─ ▾ grandchild");

    f.sendRunnerKey("h");
    await f.waitUntil("parent tree collapsed", () => {
      const screen = f.captureRunner();
      return !screen.includes(childWindow) && !screen.includes(grandchildWindow);
    });

    f.sendRunnerKey("l");
    await f.waitUntil("parent tree expanded again", () => f.runnerContains(childWindow));
    f.sendRunnerKey("j");
    await sleep(100);
    f.sendRunnerKey("j");
    await f.waitUntil("child session selected", () => f.runnerContains("CHILD_TREE_PREVIEW"));
    f.sendRunnerKey("h");
    await f.waitUntil("child tree collapsed independently", () => {
      const screen = f.captureRunner();
      return screen.includes(childWindow) === false && screen.includes("grandchild") === false;
    });
    expect(f.captureRunner()).toContain("▸ child");

    const treeStateFile = join(f.cache, "tmux-overview", "show-tree");
    f.sendRunnerKey("t");
    await f.waitUntil("tree connectors hidden", () =>
      f.runnerContains("tree view off") && f.runnerContains("•") && f.runnerLacks("└─ ▸ child"),
    );
    expect(readFileSync(treeStateFile, "utf8").trim()).toBe("0");

    await f.quitOverview();
    await f.launchOverview();
    expect(f.captureRunner()).toContain("•");
    expect(f.captureRunner()).not.toContain("└─ ▸ child");

    f.sendRunnerKey("t");
    await f.waitUntil("tree connectors restored", () =>
      f.runnerContains("tree view on") && f.runnerContains("◆") && f.runnerContains("└─ ▸ child"),
    );
    expect(readFileSync(treeStateFile, "utf8").trim()).toBe("1");
  });
}, TEST_TIMEOUT_MS);

e2e("helper-symbol visibility persists across overview invocations", async () => {
  await withFixture("hidden_sessions", async (f) => {
    const parent = `alpha_visible_${process.pid}`;
    const floatSession = `${parent}/__float`;
    const agentSession = `${parent}/__agents`;
    const hiddenRoot = `__background_${process.pid}`;
    await f.createSessionWithOutput(parent, "visible", "VISIBLE_SESSION_PREVIEW");
    await f.createSessionWithOutput(floatSession, "float", "HIDDEN_FLOAT_PREVIEW");
    await f.createSessionWithOutput(agentSession, "agents", "HIDDEN_AGENT_PREVIEW");
    await f.createSessionWithOutput(hiddenRoot, "worker", "HIDDEN_ROOT_PREVIEW");

    await f.launchOverview();
    let screen = f.captureRunner();
    expect(screen).toContain(parent);
    expect(screen).not.toContain("◫ float");
    expect(screen).not.toContain("󰚩 agents");
    expect(screen).not.toContain(hiddenRoot);
    expect(screen).not.toContain("HIDDEN_FLOAT_PREVIEW");
    expect(screen).not.toContain("HIDDEN_AGENT_PREVIEW");

    const stateFile = join(f.cache, "tmux-overview", "show-hidden");
    f.sendRunnerKey(".");
    await f.waitUntil("hidden helper sessions visible", () => f.runnerContains("◫ float") && f.runnerContains("󰚩 agents"));
    expect(f.captureRunner()).not.toContain("__float");
    expect(f.captureRunner()).not.toContain("__agents");
    f.sendRunnerKey("g");
    await f.waitUntil("hidden root session visible", () => f.runnerContains(hiddenRoot));
    expect(readFileSync(stateFile, "utf8").trim()).toBe("1");

    await f.quitOverview();
    await f.launchOverview();
    await f.waitUntil("hidden visibility restored", () =>
      f.runnerContains("◫ float") && f.runnerContains("󰚩 agents") && f.runnerContains(hiddenRoot),
    );

    f.sendRunnerKey(".");
    await f.waitUntil("hidden sessions hidden again", () => {
      const hidden = f.captureRunner();
      return !hidden.includes("◫ float") && !hidden.includes("󰚩 agents") && !hidden.includes(hiddenRoot);
    });
    expect(readFileSync(stateFile, "utf8").trim()).toBe("0");
  });
}, TEST_TIMEOUT_MS);

e2e("agents helper session starts collapsed and unfolds with l", async () => {
  await withFixture("agents_collapsed", async (f) => {
    const parent = `alpha_agents_${process.pid}`;
    const agentWindow = `worker_${process.pid}`;
    await f.createSessionWithOutput(parent, "visible", "AGENTS_PARENT_PREVIEW");
    await f.createSessionWithOutput(`${parent}/__agents`, agentWindow, "AGENTS_WORKER_PREVIEW");

    await f.launchOverview();
    f.sendRunnerKey(".");
    const title = "\u{F06A9} agents";
    await f.waitUntil("agents helper session visible", () => f.runnerContains(title));
    expect(f.captureRunner()).toContain(`\u25b8 ${title}`);
    expect(f.captureRunner()).not.toContain(agentWindow);

    // The selected row is the one painted with the highlight background.
    const selected = () => f.captureRunnerAnsi().split("\n").find((l) => l.includes("48;2;255;100;150m")) ?? "";
    for (const key of ["k", "j"]) {
      for (let i = 0; i < 20 && !selected().includes(title); i++) {
        f.sendRunnerKey(key);
        await sleep(80);
      }
    }
    expect(selected()).toContain(title);
    f.sendRunnerKey("l");
    await f.waitUntil("agents session unfolded", () => f.runnerContains(agentWindow));
  });
}, TEST_TIMEOUT_MS);

e2e("enter on an __agents or __float session asks before switching", async () => {
  await withFixture("confirm_helper", async (f) => {
    const parent = `alpha_confirm_${process.pid}`;
    await f.createSessionWithOutput(parent, "main", "CONFIRM_PARENT");
    await f.createSessionWithOutput(`${parent}/__float`, "floaty", "CONFIRM_FLOAT");
    await f.launchOverview();
    f.sendRunnerKey(".");
    await f.waitUntil("float session listed", () => f.runnerContains("\u25eb float"));
    const selected = () => f.captureRunnerAnsi().split("\n").find((l) => l.includes("48;2;255;100;150m")) ?? "";
    for (const key of ["k", "j"]) {
      for (let i = 0; i < 20 && !selected().includes("floaty"); i++) {
        f.sendRunnerKey(key);
        await sleep(80);
      }
    }
    expect(selected()).toContain("floaty");

    f.sendRunnerKey("Enter");
    await f.waitUntil("confirmation shown", () => f.runnerContains("go to "));
    f.sendRunnerKey("n");
    await f.waitUntil("confirmation closed", () => f.runnerLacks("go to "));
    expect(f.captureRunner()).toContain("tmux overview");

    f.sendRunnerKey("Enter");
    await f.waitUntil("confirmation again", () => f.runnerContains("go to "));
    f.sendRunnerKey("y");
    await f.waitUntil("overview left after yes", () => f.runnerLacks("tmux overview"));
  });
}, TEST_TIMEOUT_MS);

e2e("bg helper session starts collapsed even when helpers are shown", async () => {
  await withFixture("bg_collapsed", async (f) => {
    const parent = `alpha_bg_${process.pid}`;
    const bgSession = `${parent}/__bg`;
    const bgWindow = `bgjob_${process.pid}`;
    // "zz" sorts after "__bg" by name; bg must still be listed last.
    const lastByName = `${parent}/zz_child`;
    await f.createSessionWithOutput(parent, "visible", "BG_PARENT_PREVIEW");
    await f.createSessionWithOutput(bgSession, bgWindow, "BG_JOB_PREVIEW");
    await f.createSessionWithOutput(`${parent}/__float`, "float", "BG_FLOAT_PREVIEW");
    await f.createSessionWithOutput(lastByName, "child", "BG_CHILD_PREVIEW");

    await f.launchOverview();
    f.sendRunnerKey(".");
    const bgTitle = "\u{F018D} bg";
    await f.waitUntil("bg helper session visible", () => f.runnerContains(bgTitle));
    const screen = f.captureRunner();
    expect(screen).toContain(`\u25b8 ${bgTitle}`);
    expect(screen).not.toContain("__bg");
    expect(screen).not.toContain(bgWindow);
    expect(screen.indexOf("\u25eb float")).toBeLessThan(screen.indexOf(bgTitle));
    expect(screen.indexOf("zz_child")).toBeLessThan(screen.indexOf(bgTitle));

    // The title is dim even after the tree connector, which sets its own colour.
    const bgLine = f.captureRunnerAnsi().split("\n").find((line) => line.includes(bgTitle)) ?? "";
    const beforeTitle = bgLine.slice(bgLine.lastIndexOf("\u2500"), bgLine.indexOf("\u25b8"));
    expect(beforeTitle).toContain("38;5;240");
  });
}, TEST_TIMEOUT_MS);

e2e("watchlist ignores window ids saved by another tmux server", async () => {
  await withFixture("watch_stale", async (f) => {
    const session = `alpha_stale_${process.pid}`;
    await f.createSessionWithOutput(session, "main", "STALE_WATCH_PREVIEW");
    const ids = f.tmux(["list-windows", "-a", "-F", "#{window_id}"]).stdout.trim().split("\n");

    // Old bare-id format and entries tagged with a different server start time.
    const stateDir = join(f.cache, "tmux-overview");
    mkdirSync(stateDir, { recursive: true });
    const stale = [...ids, ...ids.map((id) => `1\t${id}`)].join("\n");
    writeFileSync(join(stateDir, "watchlist.tsv"), `${stale}\n`);

    await f.launchOverview();
    expect(f.captureRunner()).toContain(session);
    expect(f.captureRunner()).not.toContain("watchlist");
  });
}, TEST_TIMEOUT_MS);

e2e("stats picker mode persists across overview invocations", async () => {
  await withFixture("stats_toggle", async (f) => {
    const session = `alpha_stats_${process.pid}`;
    await f.createSessionWithOutput(session, "main", "STATS_TOGGLE_PREVIEW");

    const stateFile = join(f.cache, "tmux-overview", "show-stats");
    const currentPicker = () => f.captureRunner().split("\n").slice(0, 11).join("\n");
    await f.launchOverview();
    expect(f.captureRunner()).toContain("stats 60s");
    expect(currentPicker()).toContain("0: main");
    f.sendRunnerKey("s");
    await f.waitUntil("compact picker", () => f.runnerContains("compact picker") && f.runnerContains("stats hidden"));
    expect(currentPicker()).not.toContain("0: main");
    expect(readFileSync(stateFile, "utf8").trim()).toBe("0");

    await f.quitOverview();
    await f.launchOverview();
    await f.waitUntil("compact picker restored", () => f.runnerContains("compact picker") && f.runnerContains("stats hidden"));
    expect(currentPicker()).not.toContain("0: main");

    f.sendRunnerKey("s");
    await f.waitUntil("stats picker restored", () => f.runnerContains("stats 60s") && f.runnerContains("stats view on"));
    expect(currentPicker()).toContain("0: main");
    expect(readFileSync(stateFile, "utf8").trim()).toBe("1");
  });
}, TEST_TIMEOUT_MS);

e2e("expand/collapse state persists across overview invocations", async () => {
  await withFixture("fold", async (f) => {
    const session = `alpha_fold_${process.pid}`;
    const first = `main_fold_${process.pid}`;
    const second = `second_fold_${process.pid}`;
    await f.createSessionWithOutput(session, first, "FOLD_MAIN");
    expect(f.tmux(["new-window", "-d", "-t", session, "-n", second, "-c", f.testTmp, "/bin/sh"]).exitCode).toBe(0);

    await f.launchOverview();
    const stateFile = join(f.cache, "tmux-overview", "expanded.tsv");
    f.sendRunnerKey("g");
    await f.waitUntil("fold target visible", () => f.runnerContains(session));
    f.sendRunnerKey("h");
    await f.waitUntil("collapsed state persisted", () => existsSync(stateFile) && readFileSync(stateFile, "utf8").includes(`${session}\t0`));
    // Wait for the exit: keys typed into a still-running overview are commands.
    await f.quitOverview();

    expect(existsSync(stateFile)).toBe(true);
    expect(readFileSync(stateFile, "utf8")).toContain(`${session}\t0`);

    await f.launchOverview();
    const collapsed = f.captureRunner();
    expect(collapsed).toContain(session);
    expect(collapsed).not.toContain(first);
    expect(collapsed).not.toContain(second);

    f.sendRunnerKey("g", "l");
    await f.waitUntil("expanded window visible", () => f.runnerContains(first));
    expect(readFileSync(stateFile, "utf8")).toContain(`${session}\t1`);
  });
}, TEST_TIMEOUT_MS);

e2e("saved session snapshots can be restored", async () => {
  await withFixture("save_restore", async (f) => {
    const session = `alpha_save_${process.pid}`;
    const first = `main_save_${process.pid}`;
    const second = `second_save_${process.pid}`;
    const defaultPath = join(f.testTmp, "default-path");
    mkdirSync(defaultPath, { recursive: true });

    await f.createSessionWithOutput(session, first, "SAVE_RESTORE_MAIN");
    expect(f.tmux(["new-window", "-d", "-t", session, "-n", second, "-c", f.testTmp, "/bin/sh"]).exitCode).toBe(0);
    expect(f.tmux(["set-option", "-t", session, "@overview_default_path", defaultPath]).exitCode).toBe(0);

    await f.launchOverview();
    f.sendRunnerKey("g", "S");

    const snapshot = f.savedSessionFile(session);
    await f.waitUntil("snapshot file created", () => existsSync(snapshot));
    expect(existsSync(snapshot)).toBe(true);
    expect(readFileSync(snapshot, "utf8")).toContain(hexEncode(second));

    await f.quitOverview();
    expect(f.tmux(["kill-session", "-t", session]).exitCode).toBe(0);

    await f.launchOverview();
    f.sendRunnerKey("G", "C-m");
    await f.waitUntil("restored session exists", () => f.tmux(["has-session", "-t", session]).exitCode === 0, 5_000);
    // The session appears first; windows are added after it, then the dummy goes.
    await f.waitUntil("restored windows created", () => {
      const names = f.windowNames(session);
      return names.includes(second) && !names.includes("__restore_dummy__");
    }, 5_000);
    expect(f.windowNames(session)).toContain(first);
    expect(f.windowNames(session)).toContain(second);
    expect(f.tmux(["show-option", "-qv", "-t", session, "@overview_default_path"]).stdout.trim()).toBe(defaultPath);
  });
}, TEST_TIMEOUT_MS);

e2e("default path modal supports tab completion", async () => {
  await withFixture("path_tab", async (f) => {
    const session = `alpha_path_${process.pid}`;
    const suggested = join(f.testTmp, "suggested-dir");
    mkdirSync(suggested, { recursive: true });
    await f.createSessionWithOutput(session, "main", "PATH_TAB_CONTENT");
    expect(f.tmux(["set-option", "-t", session, "@overview_default_path", join(f.testTmp, "sug")]).exitCode).toBe(0);

    await f.launchOverview();
    f.sendRunnerKey("g", "c");
    await f.waitUntil("path modal", () => f.runnerContains("tab complete path"));
    f.sendRunnerKey("Tab");
    await f.waitUntil("path completed", () => f.runnerContains("suggested-dir/"));
    f.sendRunnerKey("C-m");
    await f.waitUntil("path persisted", () => f.tmux(["show-option", "-qv", "-t", session, "@overview_default_path"]).stdout.trim() === `${suggested}/`);
  });
}, TEST_TIMEOUT_MS);

e2e("renaming a session cascades to all slash-named descendants", async () => {
  await withFixture("rename_tree", async (f) => {
    const parent = `alpha_rename_tree_${process.pid}`;
    const child = `${parent}/child`;
    const grandchild = `${child}/grandchild`;
    const renamedParent = `${parent}_renamed`;
    const renamedChild = `${renamedParent}/child`;
    const renamedGrandchild = `${renamedChild}/grandchild`;
    await f.createSessionWithOutput(parent, "parent", "RENAME_TREE_PARENT");
    await f.createSessionWithOutput(child, "child", "RENAME_TREE_CHILD");
    await f.createSessionWithOutput(grandchild, "grandchild", "RENAME_TREE_GRANDCHILD");

    await f.launchOverview();
    f.sendRunnerKey("g", "r");
    await f.waitUntil("tree rename modal", () => f.runnerContains(`rename session ${parent}`));
    f.sendRunnerLiteral("_renamed");
    f.sendRunnerKey("C-m");

    await f.waitUntil("renamed session tree exists", () =>
      f.tmux(["has-session", "-t", `=${renamedParent}`]).exitCode === 0 &&
      f.tmux(["has-session", "-t", `=${renamedChild}`]).exitCode === 0 &&
      f.tmux(["has-session", "-t", `=${renamedGrandchild}`]).exitCode === 0,
    );
    expect(f.tmux(["has-session", "-t", `=${parent}`]).exitCode).not.toBe(0);
    expect(f.tmux(["has-session", "-t", `=${child}`]).exitCode).not.toBe(0);
    expect(f.tmux(["has-session", "-t", `=${grandchild}`]).exitCode).not.toBe(0);
    await f.waitUntil("renamed tree redrawn", () => f.runnerLacks("enter apply") && f.runnerContains(renamedParent));
  });
}, TEST_TIMEOUT_MS);

e2e("rename live session modal path", async () => {
  await withFixture("rename_session", async (f) => {
    const session = `alpha_rename_${process.pid}`;
    const renamed = `${session}_renamed`;
    await f.createSessionWithOutput(session, "main", "RENAME_SESSION_CONTENT");

    await f.launchOverview();
    f.sendRunnerKey("g", "r");
    await f.waitUntil("rename session modal", () => f.runnerContains(`rename session ${session}`));
    f.sendRunnerLiteral("_renamed");
    f.sendRunnerKey("C-m");

    await f.waitUntil("renamed session exists", () => f.tmux(["has-session", "-t", `=${renamed}`]).exitCode === 0);
    expect(f.tmux(["has-session", "-t", `=${session}`]).exitCode).not.toBe(0);
  });
}, TEST_TIMEOUT_MS);

e2e("rename live window modal path", async () => {
  await withFixture("rename_window", async (f) => {
    const session = `alpha_win_rename_${process.pid}`;
    const window = "mainwin";
    const renamed = `${window}_renamed`;
    await f.createSessionWithOutput(session, window, "RENAME_WINDOW_CONTENT");

    await f.launchOverview();
    f.sendRunnerKey("g", "j", "r");
    await f.waitUntil("rename window modal", () => f.runnerContains("rename window"));
    f.sendRunnerLiteral("_renamed");
    f.sendRunnerKey("C-m");

    await f.waitUntil("renamed window visible to tmux", () => f.windowNames(session).includes(renamed));
    expect(f.windowNames(session)).not.toContain(`${window}\n`);
  });
}, TEST_TIMEOUT_MS);

e2e("rename/delete saved session modal paths", async () => {
  await withFixture("saved_modals", async (f) => {
    const saved = `saved_modal_${process.pid}`;
    const renamed = `${saved}_renamed`;
    const oldPath = f.createSavedSessionFile(saved, "saved_window");

    await f.launchOverview();
    f.sendRunnerKey("G", "r");
    await f.waitUntil("rename saved modal", () => f.runnerContains(`rename saved session ${saved}`));
    f.sendRunnerLiteral("_renamed");
    f.sendRunnerKey("C-m");

    const newPath = f.savedSessionFile(renamed);
    await f.waitUntil("renamed saved file", () => existsSync(newPath) && !existsSync(oldPath));
    expect(existsSync(oldPath)).toBe(false);

    f.sendRunnerKey("G", "D");
    await f.waitUntil("delete saved modal", () => f.runnerContains(`delete saved session ${renamed}`));
    f.sendRunnerKey("n");
    expect(existsSync(newPath)).toBe(true);

    f.sendRunnerKey("D");
    await f.waitUntil("delete saved modal again", () => f.runnerContains(`delete saved session ${renamed}`));
    f.sendRunnerKey("y");
    await f.waitUntil("saved file deleted", () => !existsSync(newPath));
  });
}, TEST_TIMEOUT_MS);

e2e("delete window and kill session confirm paths", async () => {
  await withFixture("delete_confirm", async (f) => {
    const session = `alpha_delete_${process.pid}`;
    const keep = `keep_delete_${process.pid}`;
    const doomed = `doomed_delete_${process.pid}`;
    await f.createSessionWithOutput(session, keep, "DELETE_KEEP");
    expect(f.tmux(["new-window", "-d", "-t", session, "-n", doomed, "-c", f.testTmp, "/bin/sh"]).exitCode).toBe(0);

    await f.launchOverview();
    f.sendRunnerKey("g", "j", "j", "D");
    await f.waitUntil("delete window modal", () => f.runnerContains("delete window"));
    f.sendRunnerKey("n");
    expect(f.windowNames(session)).toContain(doomed);

    f.sendRunnerKey("D");
    await f.waitUntil("delete window modal again", () => f.runnerContains("delete window"));
    f.sendRunnerKey("y");
    await f.waitUntil("window deleted", () => !f.windowNames(session).includes(doomed));

    await f.quitOverview();
    await f.launchOverview();
    f.sendRunnerKey("g", "K");
    await f.waitUntil("kill session modal", () => f.runnerContains(`kill session ${session}`));
    f.sendRunnerKey("n");
    expect(f.tmux(["has-session", "-t", session]).exitCode).toBe(0);

    f.sendRunnerKey("K");
    await f.waitUntil("kill session modal again", () => f.runnerContains(`kill session ${session}`));
    f.sendRunnerKey("y");
    await f.waitUntil("session killed", () => f.tmux(["has-session", "-t", session]).exitCode !== 0);
  });
}, TEST_TIMEOUT_MS);

e2e("preview sanitization survives cursor-control output", async () => {
  await withFixture("ansi_preview", async (f) => {
    const session = `alpha_ansi_${process.pid}`;
    expect(f.tmux(["new-session", "-d", "-s", session, "-n", "ansi", "-c", f.testTmp, "/bin/sh"]).exitCode).toBe(0);
    f.tmux([
      "send-keys",
      "-t",
      `${session}:ansi.0`,
      "printf 'SANITIZE_TOP\\n\\033[31mCOLOR_OK\\033[0m\\n\\033[2J\\033[999;1HCURSOR_CONTROL_TEXT\\nSANITIZE_BOTTOM\\n'",
      "C-m",
    ]);

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("ansi preview selected", () => f.runnerContains(session));

    const screen = f.captureRunner();
    expect(screen).toContain("tmux overview");
    expect(screen).toContain(session);
    // tmux may apply destructive controls to the source pane before capture;
    // the invariant here is that rendering that pane does not corrupt the
    // overview's header/list frame.
  });
}, TEST_TIMEOUT_MS);

e2e("unreviewed agent panes badge their window and roll up to the session", async () => {
  await withFixture("agent_alert", async (f) => {
    const session = `alpha_agent_${process.pid}`;
    const window = `worker_${process.pid}`;
    await f.createSessionWithOutput(session, window, "AGENT_PANE_OUTPUT");

    // A producer publishes its stop on the pane, then rings the bell. tmux keeps
    // window_bell_flag set until the window is focused, which is the "not
    // reviewed yet" half of the signal.
    const idleAt = Math.floor(Date.now() / 1000) - 120;
    // tmux-agent-alert.sh marks the pane that rang with @agent_unseen: the bell is per window.
    for (const [name, value] of [
      ["@agent_kind", "pi"],
      ["@agent_state", "idle"],
      ["@agent_idle_at", String(idleAt)],
      ["@agent_unseen", "1"],
    ]) {
      expect(f.tmux(["set-option", "-p", "-t", `${session}:${window}.0`, name, value]).exitCode).toBe(0);
    }
    // An idle pi beside it in the same window did not ring: it must not be counted.
    const quiet = f.tmux(["split-window", "-d", "-P", "-F", "#{pane_id}", "-t", `${session}:${window}.0`, "/bin/sh"]).stdout.trim();
    for (const [name, value] of [
      ["@agent_kind", "pi"],
      ["@agent_state", "idle"],
      ["@agent_idle_at", String(idleAt)],
    ]) {
      expect(f.tmux(["set-option", "-p", "-t", quiet, name, value]).exitCode).toBe(0);
    }
    f.tmux(["send-keys", "-l", "-t", `${session}:${window}.0`, "printf '\\a'"]);
    f.tmux(["send-keys", "-t", `${session}:${window}.0`, "C-m"]);
    await f.waitUntil(
      "bell alert recorded",
      () => f.tmux(["display-message", "-p", "-t", `${session}:${window}`, "#{window_bell_flag}"]).stdout.trim() === "1",
    );

    await f.launchOverview();
    await f.waitUntil("alert badge rendered", () => f.runnerContains("● 2m"));
    const screen = f.captureRunner();
    // Window badge carries the age; the session row aggregates its subtree. One pane, not two:
    // a count of 2 would read "\u25cf2 2m".
    expect(screen).not.toContain("2 2m");
    expect(screen).toContain("● 2m");
    expect(screen).toContain("● 1");
  });
}, TEST_TIMEOUT_MS);

e2e("agents mode: spawn tree, folding, compact counts, pins and project order", async () => {
  await withFixture("agents_mode", async (f) => {
    const first = `ag_${process.pid}`;
    const second = `zz_${process.pid}`;
    await f.createSessionWithOutput(first, "orch", "ORCH_OUTPUT");
    await f.createSessionWithOutput(`${first}/__agents`, "worker", "WORKER_OUTPUT");
    await f.createSessionWithOutput(second, "solo", "SOLO_OUTPUT");
    const pane = (target: string) => f.tmux(["display-message", "-p", "-t", target, "#{pane_id}"]).stdout.trim();
    const orch = pane(`${first}:orch`);
    const set = (target: string, opts: Record<string, string>) => {
      for (const [k, v] of Object.entries(opts)) expect(f.tmux(["set-option", "-p", "-t", target, k, v]).exitCode).toBe(0);
    };
    set(orch, { "@agent_kind": "pi", "@agent_state": "idle", "@pi_session": "/x/2026_conv-orch.jsonl" });
    const w1 = pane(`${first}/__agents:worker`);
    set(w1, { "@agent_kind": "pi", "@agent_state": "working", "@agent_parent": orch, "@agent_name": "w1", "@agent_bg": "2", "@agent_ctx": "874312 1000000", "@agent_compaction": "off" });
    // Resting on its own bg job: amber, counted apart from idle.
    set(pane(`${second}:solo`), { "@agent_kind": "pi", "@agent_state": "waiting" });
    // A run that stopped on a failed request: red \u2717, counted on its project.
    const failed = f.tmux(["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", `${second}:`, "-n", "failed", "/bin/sh"]).stdout.trim();
    set(failed, { "@agent_kind": "pi", "@agent_state": "error", "@agent_error": "Connection error." });
    // A finished worker (idle, bell not yet seen) must not flag its project or parent.
    const done = f.tmux(["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", `${first}/__agents:`, "-n", "done", "/bin/sh"]).stdout.trim();
    expect(done).toStartWith("%");
    set(done, { "@agent_kind": "pi", "@agent_state": "idle", "@agent_parent": orch, "@agent_name": "w2" });
    f.tmux(["send-keys", "-l", "-t", done, "printf '\\a'"]);
    f.tmux(["send-keys", "-t", done, "C-m"]);
    await f.waitUntil("worker bell", () => f.tmux(["display-message", "-p", "-t", done, "#{window_bell_flag}"]).stdout.trim() === "1");

    await f.launchOverview();
    f.sendRunnerKey("A");
    await f.waitUntil("agents mode", () => f.runnerContains("A overview"));
    const selected = () => f.captureRunnerAnsi().split("\n").find((l) => l.includes("48;2;255;100;150m")) ?? "";
    const select = async (text: string) => {
      for (const key of ["k", "j"]) {
        for (let i = 0; i < 15 && !selected().includes(text); i++) {
          f.sendRunnerKey(key);
          await sleep(80);
        }
      }
      expect(selected()).toContain(text);
    };

    // Neither project holds the runner session, so both start collapsed; a project row shows only
    // its agent count, plus \u25b2N for agents past 350k context (w1 holds 874k); a failure inside
    // turns the collapsed row red.
    await f.waitUntil("collapsed projects", () => f.runnerContains(`\u25b8 ${first}  3`));
    expect(f.captureRunner()).toContain(`\u25b8 ${first}  3 \u25b21`);
    expect(f.captureRunner()).not.toMatch(new RegExp(`\u25b8 ${second}  2 \\S`));
    expect(f.captureRunnerAnsi().split("\n").find((l) => l.includes("\u25b8") && l.includes(` ${second}`))).toContain("38;5;203m");

    // Unfolding the project shows the orchestrator with its worker folded under it.
    await select(first);
    f.sendRunnerKey("l");
    await f.waitUntil("folded orchestrator", () => f.runnerContains("\u2442 2 \u25cf1 "));
    expect(f.captureRunner()).not.toContain("\u2442 2 \u25cf1 \u25cf");
    // The folded worker past 350k marks its parent; the open project drops its own mark.
    expect(f.captureRunner()).toContain("\u2442 2 \u25cf1 \u25b21");
    expect(f.captureRunner()).not.toContain(`\u25be ${first}  3 \u25b2`);
    // (The project preview on the right still lists w1; the tree row does not.)
    expect(f.captureRunner()).not.toContain("\u2500 \u25cf w1");
    await select("orch");
    f.sendRunnerKey("l");
    await f.waitUntil("worker under orchestrator", () => f.runnerContains("\u2500 \u25cf w1"));
    // Pane id after the name; the bg jobs it waits for after its state.
    expect(f.captureRunner()).toContain(`w1 ${w1}`);
    expect(f.captureRunner()).toMatch(/working \S* ?\u26992/);
    // Context column off: w1's context marks follow its state (\u25b2 full, \u2298 auto-compaction off).
    expect(f.captureRunner()).toMatch(/working \S* ?\u26992 \u25b2 \u2298/);

    // `c` adds the context column, red from 350k tokens; saved.
    expect(f.captureRunner()).not.toContain("874k/1M");
    f.sendRunnerKey("c");
    await f.waitUntil("context column", () => f.runnerContains("874k/1M"));
    // \u2298 right of the tokens: pi-kit's /compaction off.
    expect(f.captureRunner()).toContain("874k/1M \u2298");
    // tmux writes bold + colour as one SGR or two (`1;38;5;203m` or `1m\e[38;5;203m`), by what came before.
    expect(f.captureRunnerAnsi()).toMatch(/1(;|m\x1b\[)38;5;203m874k\/1M/);
    expect(readFileSync(join(f.cache, "tmux-overview", "agents-context"), "utf8").trim()).toBe("1");

    // A pin keeps the agent listed under its collapsed project, keyed by its conversation.
    f.sendRunnerKey("p");
    await f.waitUntil("pin mark", () => f.runnerContains("orch \u26b2"));
    await select(first);
    f.sendRunnerKey("h");
    await f.waitUntil("project collapsed", () => f.runnerContains(`\u25b8 ${first}`));
    expect(f.captureRunner()).toContain("orch \u26b2");
    expect(readFileSync(join(f.cache, "tmux-overview", "agent-pins"), "utf8").trim()).toBe("2026_conv-orch");

    // K moves a project up among its siblings; the order is saved.
    await select(second);
    f.sendRunnerKey("K");
    await f.waitUntil("order changed", () => {
      const s = f.captureRunner();
      return s.indexOf(`\u25b8 ${second}`) < s.indexOf(`\u25b8 ${first}`);
    });
    expect(readFileSync(join(f.cache, "tmux-overview", "agents-order"), "utf8")).toBe(`${second}\n${first}\n`);

    // The selected row survives closing and reopening: an agent by its conversation.
    await select(first);
    f.sendRunnerKey("l");
    await select("orch");
    expect(readFileSync(join(f.cache, "tmux-overview", "agents-cursor"), "utf8").trim()).toBe("a:2026_conv-orch");
    f.sendRunnerKey("q");
    await f.waitUntil("agents mode closed", () => f.runnerLacks("A overview"));
    await f.launchOverview("A overview");
    await f.waitUntil("selection restored", () => selected().includes("orch"));
  });
}, TEST_TIMEOUT_MS);

e2e("unicode-width pane titles and multi-pane preview layout", async () => {
  await withFixture("unicode_multi", async (f) => {
    const session = `alpha_unicode_${process.pid}`;
    const window = `multi_unicode_${process.pid}`;
    await f.createSessionWithOutput(session, window, "LEFT_PANE_UNICODE");
    const p0 = f.tmux(["display-message", "-p", "-t", `${session}:${window}.0`, "#{pane_id}"]).stdout.trim();
    const split = f.tmux(["split-window", "-h", "-P", "-F", "#{pane_id}", "-t", `${session}:${window}`, "-c", f.testTmp, "/bin/sh"]);
    expect(split.exitCode, split.stderr).toBe(0);
    const p1 = split.stdout.trim();
    expect(f.tmux(["select-pane", "-t", p0, "-T", "幅pane"]).exitCode).toBe(0);
    expect(f.tmux(["select-pane", "-t", p1, "-T", "emoji🙂pane"]).exitCode).toBe(0);
    f.tmux(["send-keys", "-l", "-t", p1, "printf '%s\\n' 'RIGHT_PANE_UNICODE'"]);
    f.tmux(["send-keys", "-t", p1, "C-m"]);
    await f.waitUntil("right pane output", () => f.paneContains(p1, "RIGHT_PANE_UNICODE"));

    await f.launchOverview();
    const initial = f.captureRunner();
    expect(initial).toContain("幅pane");
    expect(initial).toContain("emoji🙂");

    f.sendRunnerKey("g", "j");
    await f.waitUntil("left preview content", () => f.runnerContains("LEFT_PANE_UNICODE"));
    await f.waitUntil("right preview content", () => f.runnerContains("RIGHT_PANE_UNICODE"));
    expect(f.captureRunner()).toContain("│");
  });
}, TEST_TIMEOUT_MS);
