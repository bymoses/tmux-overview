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
});

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

  async launchOverview(): Promise<void> {
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
    await this.waitUntil("overview initial render", () => this.runnerContains("tmux overview"));
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
    expect(screen).not.toContain(popup);
    expect(screen).not.toContain("POPUP_SHOULD_BE_HIDDEN");
  });
}, TEST_TIMEOUT_MS);

e2e("navigation, preview resize, and preview toggle", async () => {
  await withFixture("navigation", async (f) => {
    const session = `alpha_nav_${process.pid}`;
    await f.createSessionWithOutput(session, "main", "NAV_PREVIEW_CONTENT");

    await f.launchOverview();
    f.sendRunnerKey("g");
    await f.waitUntil("selected session preview", () => f.runnerContains("NAV_PREVIEW_CONTENT"));

    f.sendRunnerKey("+");
    await f.waitUntil("preview grows", () => f.runnerContains("preview 70%"));
    f.sendRunnerKey("-");
    await f.waitUntil("preview shrinks", () => f.runnerContains("preview 65%"));
    f.sendRunnerKey("?");
    await f.waitUntil("help modal", () => f.runnerContains("tmux overview help"));
    expect(f.captureRunner()).toContain("S save layout");
    f.sendRunnerKey("C-m");
    await f.waitUntil("help modal closed", () => f.runnerLacks("tmux overview help"));

    f.sendRunnerKey("v");
    await f.waitUntil("preview hidden", () => f.runnerLacks("preview 65%"));
    expect(f.captureRunner()).not.toContain("NAV_PREVIEW_CONTENT");
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
    f.sendRunnerKey("q");

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
    await f.waitUntil("renamed saved file", () => existsSync(newPath));
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
