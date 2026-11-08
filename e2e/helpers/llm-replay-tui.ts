import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import headless from "@xterm/headless";
import type { Terminal } from "@xterm/headless";
import { startStdioAcpFixture } from "./stdio-acp-fixture.js";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
export const REPLAY_BINARY = process.env.PERI_E2E_BINARY ?? path.join(projectRoot, "target/debug/peri");
export const FIXTURE_MODEL_ORIGIN = "http://fixture-model.test";

export interface ReplayWorld {
  root: string;
  home: string;
  work: string;
  evidence: string;
}

export interface ReplayTuiOptions {
  label: string;
  sessionId: string;
  proxy: string;
  run: string;
  cols?: number;
  rows?: number;
  permissionMode?: string;
  modelLabel?: string;
  ready?: (screen: string) => boolean;
  env?: Record<string, string>;
  args?: string[];
}

/** 一个 case 的客户端按声明共享 world；只有不同 case 才创建新 world。 */
export async function createReplayWorld(): Promise<ReplayWorld> {
  const root = await mkdtemp(path.join(os.tmpdir(), "peri-wire-e2e-"));
  const world = { root, home: path.join(root, "home"), work: path.join(root, "work"), evidence: path.join(root, "evidence") };
  await Promise.all([mkdir(path.join(world.home, ".peri"), { recursive: true }), mkdir(world.work), mkdir(world.evidence)]);
  await writeFile(path.join(world.home, ".peri/settings.json"), JSON.stringify({
    config: {
      language: "en", active_alias: "sonnet",
      providers: [{ id: "fixture", type: "anthropic", apiKey: "synthetic-fixture-key", baseUrl: FIXTURE_MODEL_ORIGIN,
        models: { sonnet: "fixture-model", haiku: "fixture-model", opus: "fixture-model", fable: "fixture-model" } }],
      profiles: Object.fromEntries(["sonnet", "haiku", "opus", "fable"].map((alias) =>
        [alias, { provider: "fixture", model: "fixture-model", effort: "medium" }])),
    },
  }), { mode: 0o600 });
  return world;
}

export function replayEnvironment(world: ReplayWorld, route?: { proxy: string; run: string }): Record<string, string> {
  return {
    HOME: world.home,
    XDG_CONFIG_HOME: path.join(world.home, "config"),
    XDG_CACHE_HOME: path.join(world.home, "cache"),
    XDG_DATA_HOME: path.join(world.home, "data"),
    PATH: process.env.PATH ?? "/usr/local/bin:/usr/bin:/bin",
    TERM: "xterm-256color", LANG: "C.UTF-8", LC_ALL: "C.UTF-8", TZ: "UTC",
    HTTP_PROXY: "http://127.0.0.1:9", HTTPS_PROXY: "http://127.0.0.1:9", ALL_PROXY: "http://127.0.0.1:9",
    NO_PROXY: "localhost,127.0.0.1", no_proxy: "localhost,127.0.0.1",
    ...(route ? { PERI_MODEL_TRANSPORT_PROXY: route.proxy, PERI_REPLAY_RUN: route.run } : {}),
  };
}

export async function prepareReplaySessions(world: ReplayWorld, count = 1): Promise<string[]> {
  const peer = await startStdioAcpFixture({
    binary: REPLAY_BINARY, cwd: world.work, home: world.home, env: replayEnvironment(world),
    onNotification() {}, onServerRequest() {},
    handleServerRequest(request) { return { error: { code: -32601, message: `unexpected preparation request: ${request.method}` } }; },
  });
  const ids: string[] = [];
  try {
    const initialized = await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: { "peri.userInputQueue": true } } });
    if (initialized.error || initialized.result?.protocolVersion !== 1) throw new Error(`initialize: ${JSON.stringify(initialized)}`);
    for (let index = 0; index < count; index++) {
      const created = await peer.call("session/new", { cwd: world.work });
      if (created.error || typeof created.result?.sessionId !== "string") throw new Error(`session/new: ${JSON.stringify(created)}`);
      ids.push(created.result.sessionId);
    }
    for (const sessionId of ids) {
      const closed = await peer.call("session/close", { sessionId });
      if (closed.error) throw new Error(`session/close: ${JSON.stringify(closed)}`);
    }
    const code = await peer.close();
    if (code !== 0) throw new Error(`preparation exit ${code}: ${await peer.diagnostics()}`);
    return ids;
  } finally { await peer.close(); }
}

/** 必须在所有 world 进程退出后使用；复制现有存储，不改其 schema。 */
export async function checkpointReplayWorld(world: ReplayWorld): Promise<void> {
  await cp(world.home, path.join(world.root, "initial-home"), { recursive: true, errorOnExist: true, force: false });
  await cp(world.work, path.join(world.root, "initial-work"), { recursive: true, errorOnExist: true, force: false });
}

export async function restoreReplayWorld(world: ReplayWorld): Promise<void> {
  await Promise.all([rm(world.home, { recursive: true, force: true }), rm(world.work, { recursive: true, force: true })]);
  await cp(path.join(world.root, "initial-home"), world.home, { recursive: true });
  await cp(path.join(world.root, "initial-work"), world.work, { recursive: true });
}

/** 真实 PTY 负责进程/输入，xterm 负责 ANSI、宽字符及当前可见屏幕的解析。 */
export class ReplayTui {
  private running = false;
  private disposed = false;
  private lastScreen = "";
  private readonly terminal: Terminal;
  private readonly child: ChildProcessWithoutNullStreams;
  private pendingOutput: Promise<void> = Promise.resolve();
  private exitCode: number | undefined;
  private bridgeFailure: Error | undefined;
  private stderr = "";
  private rawOutput: Buffer[] = [];
  private readonly closed: Promise<void>;
  private receivedOutput = 0;
  private parsedOutput = 0;
  private controlSequence = 0;
  private readonly resizeReplies = new Map<number, { resolve(sequence: number): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> }>();

  private constructor(readonly world: ReplayWorld, readonly label: string, options: ReplayTuiOptions) {
    const cols = options.cols ?? 120, rows = options.rows ?? 40;
    this.terminal = new headless.Terminal({ cols, rows, scrollback: 5000, allowProposedApi: true });
    this.child = spawn(process.env.PERI_E2E_PYTHON ?? "python3", [path.join(projectRoot, "e2e/helpers/llm-replay-pty.py")], {
      cwd: world.work, stdio: ["pipe", "pipe", "pipe"],
      env: { PATH: process.env.PATH ?? "/usr/bin:/bin", LANG: "C.UTF-8" },
    });
    this.running = true;
    this.closed = new Promise((resolve) => this.child.once("close", () => {
      if (this.exitCode === undefined) this.bridgeFailure ??= new Error(`PTY bridge closed: ${this.stderr}`);
      resolve();
    }));
    this.child.on("error", (error) => { this.bridgeFailure = error; });
    this.child.stdin.on("error", (error) => { if (this.running) this.bridgeFailure = error; });
    this.child.stderr.on("data", (chunk) => { this.stderr = (this.stderr + String(chunk)).slice(-16_384); });
    const lines = createInterface({ input: this.child.stdout });
    lines.on("line", (line) => {
      try {
        const message = JSON.parse(line);
        if (message.type === "data") {
          const bytes = Buffer.from(message.data, "base64");
          this.rawOutput.push(bytes);
          const sequence = ++this.receivedOutput;
          this.pendingOutput = this.pendingOutput.then(() => new Promise<void>((resolve) => this.terminal.write(bytes, () => {
            this.parsedOutput = sequence;
            resolve();
          })));
        } else if (message.type === "resized") {
          const pending = this.resizeReplies.get(message.id);
          if (!pending) throw new Error("unexpected PTY resize acknowledgement");
          clearTimeout(pending.timer);
          this.resizeReplies.delete(message.id);
          pending.resolve(this.receivedOutput);
        } else if (message.type === "exit") {
          this.exitCode = message.code;
        }
      } catch (error) { this.bridgeFailure = error instanceof Error ? error : new Error(String(error)); }
    });
    this.terminal.onData((data) => this.input(data));
    this.control({
      command: [REPLAY_BINARY, "--resume", options.sessionId, "--permission-mode", options.permissionMode ?? "bypass", "--config-file", path.join(world.home, ".peri/settings.json"), ...(options.args ?? [])],
      cwd: world.work, env: { ...replayEnvironment(world, options), ...options.env }, cols, rows,
    });
  }

  static async start(world: ReplayWorld, options: ReplayTuiOptions): Promise<ReplayTui> {
    const tui = new ReplayTui(world, options.label, options);
    try {
      await tui.waitFor(options.ready ?? ((screen) => screen.includes(options.modelLabel ?? "fixture-model") && screen.includes("/: commands")), "TUI ready", 60_000);
      return tui;
    } catch (error) { await tui.save("startup-failed"); await tui.stop(); throw error; }
  }

  private control(message: unknown): void {
    if (!this.child.stdin.writable) throw new Error("PTY control is closed");
    this.child.stdin.write(`${JSON.stringify(message)}\n`);
  }

  private input(value: string): void {
    if (this.running && this.exitCode === undefined) this.control({ type: "input", data: Buffer.from(value).toString("base64") });
  }

  async screen(): Promise<string> {
    if (this.disposed) return this.lastScreen;
    await this.pendingOutput;
    if (this.bridgeFailure) throw this.bridgeFailure;
    const buffer = this.terminal.buffer.active;
    this.lastScreen = Array.from({ length: this.terminal.rows }, (_, row) =>
      buffer.getLine(buffer.viewportY + row)?.translateToString(true, 0, this.terminal.cols) ?? "").join("\n");
    return this.lastScreen;
  }

  async key(key: string): Promise<void> {
    const keys: Record<string, string> = {
      "C-u": "\x15", "C-c": "\x03", "C-d": "\x04", "C-l": "\x0c", "C-a": "\x01", "C-e": "\x05",
      Enter: "\r", Escape: "\x1b", Up: "\x1b[A", Down: "\x1b[B", Left: "\x1b[D", Right: "\x1b[C",
      Tab: "\t", "S-Tab": "\x1b[Z", "Shift-Tab": "\x1b[Z", Space: " ", Backspace: "\x7f",
      PageUp: "\x1b[5~", PageDown: "\x1b[6~", Home: "\x1b[H", End: "\x1b[F", Delete: "\x1b[3~",
    };
    if (!(key in keys)) throw new Error(`unsupported test key: ${key}`);
    this.input(keys[key]);
  }

  async type(text: string): Promise<void> { this.input(text); }

  async typeText(text: string): Promise<void> { this.input(text); }

  async paste(text: string): Promise<void> { this.input(`\x1b[200~${text}\x1b[201~`); }

  async resize(cols: number, rows: number): Promise<void> {
    if (!Number.isInteger(cols) || !Number.isInteger(rows) || cols < 10 || rows < 5) throw new Error("invalid terminal dimensions");
    await this.pendingOutput;
    this.terminal.resize(cols, rows);
    const id = ++this.controlSequence;
    const acknowledged = new Promise<number>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.resizeReplies.delete(id);
        reject(new Error("PTY resize acknowledgement timed out"));
      }, 10_000);
      this.resizeReplies.set(id, { resolve, reject, timer });
    });
    this.control({ type: "resize", id, cols, rows });
    const beforeRedraw = await acknowledged;
    await this.waitFor(() => this.parsedOutput > beforeRedraw, "application redraw after PTY resize", 10_000);
  }

  async prompt(text: string): Promise<void> {
    if (text.includes("\n")) throw new Error("prompt() accepts one line; multiline paste needs its own input test");
    await this.key("C-u");
    this.input(text);
    await this.waitFor((screen) => screen.includes(text), "typed prompt is visible", 10_000);
    await this.key("Enter");
  }

  async command(name: string): Promise<void> {
    const command = name.replace(/^\//, "");
    if (!/^[a-z][a-z0-9:-]*$/.test(command)) throw new Error("command() accepts one slash command without arguments");
    await this.key("C-u");
    this.input(`/${command}`);
    const popupVisible = (screen: string) => screen.split("\n").some((line) => line.includes(`/${command}`) && /[─━]{3}/u.test(line));
    await this.waitFor(popupVisible, "slash completion menu is visible");
    const beforeSelection = this.receivedOutput;
    await this.key("Enter");
    // UI commands open their panel on selection; agent commands first complete the editor text.
    const screen = await this.waitFor((value) => this.parsedOutput > beforeSelection && !popupVisible(value), "slash selection is rendered and completion closes");
    const inEditor = (value: string) => value.split("\n").some((line) => new RegExp(`^\\s*❯\\s+/${command}\\s*$`, "u").test(line));
    if (inEditor(screen)) {
      const beforeSubmit = this.receivedOutput;
      await this.key("Enter");
      await this.waitFor((value) => this.parsedOutput > beforeSubmit && !inEditor(value), "slash command leaves the editor");
    }
  }

  async waitFor(predicate: (screen: string) => boolean, description: string, timeoutMs = 30_000): Promise<string> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const screen = await this.screen();
      if (predicate(screen)) return screen;
      if (this.exitCode !== undefined) throw new Error(`${description}: Peri exited (${this.exitCode})\n${screen}`);
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    throw new Error(`${description}: timed out after ${timeoutMs}ms\n${this.lastScreen}`);
  }

  async save(name: string): Promise<void> {
    const screen = await this.screen().catch(() => this.lastScreen);
    await writeFile(path.join(this.world.evidence, `${this.label}-${name}.txt`), screen);
    await writeFile(path.join(this.world.evidence, `${this.label}-${name}.pty`), Buffer.concat(this.rawOutput));
  }

  async stop(): Promise<void> {
    if (!this.running) return;
    if (this.exitCode === undefined && this.child.stdin.writable) this.control({ type: "stop" });
    this.running = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([this.closed, new Promise<void>((resolve) => {
      timer = setTimeout(() => { this.child.kill("SIGKILL"); resolve(); }, 5000);
    })]);
    if (timer) clearTimeout(timer);
    await this.pendingOutput;
    await this.screen().catch(() => {});
    this.terminal.dispose();
    this.disposed = true;
  }
}

export async function readReplayFile(world: ReplayWorld, name: string): Promise<string> {
  return readFile(path.join(world.work, name), "utf8");
}

/** These cases use only synthetic inputs and credentials; keep exact requests for failure diagnosis. */
export function syntheticRequestEvidence(world: ReplayWorld, mode: string) {
  return async ({ context, request }: { context: { owner: string; execution: string; call: string; attempt: number; purpose: string }; request: { body: Buffer } }) => {
    const identity = [context.owner, context.execution, context.call, context.attempt, context.purpose]
      .map((value) => String(value).replace(/[^a-zA-Z0-9_-]/g, "_")).join("-");
    await writeFile(path.join(world.evidence, `${mode}-request-${identity}.json`), request.body);
  };
}
