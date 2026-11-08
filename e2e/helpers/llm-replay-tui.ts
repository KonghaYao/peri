import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import type { Terminal } from "@xterm/headless";
import { createReplayTerminal } from "./llm-replay-terminal.js";
import { startStdioAcpFixture } from "./stdio-acp-fixture.js";
import { assertReplayIsolation, prepareReplayIsolation, replayIsolationEnvironment, replayStorePath } from "./replay-isolation.js";

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

export interface ReplayProcessOutcome {
  label: string;
  pid?: number;
  exitCode?: number;
  bridgeExitCode?: number | null;
  shutdown: "natural" | "graceful" | "crash" | "cleanup";
  forcedKill: boolean;
}

/** 一个 case 的客户端按声明共享 world；只有不同 case 才创建新 world。 */
export async function createReplayWorld(): Promise<ReplayWorld> {
  const root = await mkdtemp(path.join(os.tmpdir(), "peri-wire-e2e-"));
  const world = { root, home: path.join(root, "home"), work: path.join(root, "work"), evidence: path.join(root, "evidence") };
  await Promise.all([mkdir(path.join(world.home, ".peri"), { recursive: true }), mkdir(world.work), mkdir(world.evidence)]);
  await prepareReplayIsolation(world);
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
    ...replayIsolationEnvironment(world),
    PATH: process.env.PATH ?? "/usr/local/bin:/usr/bin:/bin",
    TERM: "xterm-256color", LANG: "C.UTF-8", LC_ALL: "C.UTF-8", TZ: "UTC",
    HTTP_PROXY: "http://127.0.0.1:9", HTTPS_PROXY: "http://127.0.0.1:9", ALL_PROXY: "http://127.0.0.1:9",
    NO_PROXY: "localhost,127.0.0.1", no_proxy: "localhost,127.0.0.1",
    ...(route ? { PERI_MODEL_TRANSPORT_PROXY: route.proxy, PERI_REPLAY_RUN: route.run } : {}),
  };
}

export async function prepareReplaySessions(world: ReplayWorld, count = 1): Promise<string[]> {
  await prepareReplayIsolation(world);
  assertReplayIsolation(world, replayEnvironment(world));
  const peer = await startStdioAcpFixture({
    binary: REPLAY_BINARY, cwd: world.work, home: world.home, dbPath: replayStorePath(world), env: replayEnvironment(world),
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
  private pid: number | undefined;
  private bridgeExitCode: number | null | undefined;
  private shutdown: ReplayProcessOutcome["shutdown"] = "natural";
  private forcedKill = false;
  private stopPromise: Promise<void> | undefined;
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
    this.terminal = createReplayTerminal(cols, rows);
    this.child = spawn(process.env.PERI_E2E_PYTHON ?? "python3", [path.join(projectRoot, "e2e/helpers/llm-replay-pty.py")], {
      cwd: world.work, stdio: ["pipe", "pipe", "pipe"],
      env: { PATH: process.env.PATH ?? "/usr/bin:/bin", LANG: "C.UTF-8" },
    });
    this.running = true;
    this.closed = new Promise((resolve) => this.child.once("close", (code) => {
      this.bridgeExitCode = code;
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
        if (message.type === "ready") {
          this.pid = message.pid;
        } else if (message.type === "data") {
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
          this.forcedKill = message.forcedKill === true;
        }
      } catch (error) { this.bridgeFailure = error instanceof Error ? error : new Error(String(error)); }
    });
    this.terminal.onData((data) => this.input(data));
    this.control({
      command: [REPLAY_BINARY, "--db-path", replayStorePath(world), "--resume", options.sessionId, "--permission-mode", options.permissionMode ?? "bypass", "--config-file", path.join(world.home, ".peri/settings.json"), ...(options.args ?? [])],
      cwd: world.work, env: { ...replayEnvironment(world, options), ...options.env }, cols, rows,
    });
  }

  static async start(world: ReplayWorld, options: ReplayTuiOptions): Promise<ReplayTui> {
    await prepareReplayIsolation(world);
    assertReplayIsolation(world, { ...replayEnvironment(world, options), ...options.env }, options.args);
    const tui = new ReplayTui(world, options.label, options);
    try {
      await tui.waitFor(options.ready ?? ((screen) => screen.includes(options.modelLabel ?? "fixture-model") && screen.includes("/: commands")), "TUI ready", 60_000);
      return tui;
    } catch (error) { await tui.save("startup-failed"); await tui.forceCleanup(); throw error; }
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

  /** A parsed application frame, useful as an input fence; business completion still needs its own oracle. */
  outputRevision(): number { return this.parsedOutput; }

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
      if (this.exitCode !== undefined) throw new Error(`${description}: Peri exited (${this.exitCode})\n${screen}`);
      if (predicate(screen)) return screen;
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    throw new Error(`${description}: timed out after ${timeoutMs}ms\n${this.lastScreen}`);
  }

  async save(name: string): Promise<void> {
    const screen = await this.screen().catch(() => this.lastScreen);
    await writeFile(path.join(this.world.evidence, `${this.label}-${name}.txt`), screen);
    await writeFile(path.join(this.world.evidence, `${this.label}-${name}.pty`), Buffer.concat(this.rawOutput));
  }

  outcome(): ReplayProcessOutcome {
    return { label: this.label, pid: this.pid, exitCode: this.exitCode, bridgeExitCode: this.bridgeExitCode,
      shutdown: this.shutdown, forcedKill: this.forcedKill };
  }

  private async waitClosed(timeoutMs: number): Promise<boolean> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([this.closed.then(() => true), new Promise<boolean>((resolve) => {
        timer = setTimeout(() => resolve(false), timeoutMs);
      })]);
    } finally { if (timer) clearTimeout(timer); }
  }

  private async dispose(): Promise<void> {
    this.running = false;
    await this.pendingOutput;
    await this.screen().catch(() => {});
    if (!this.disposed) this.terminal.dispose();
    this.disposed = true;
    await writeFile(path.join(this.world.evidence, `${this.label}-process.json`), JSON.stringify(this.outcome(), null, 2));
  }

  private async forceCleanup(): Promise<void> {
    if (this.disposed) return;
    this.shutdown = "cleanup";
    if (this.exitCode === undefined && this.child.stdin.writable) this.control({ type: "stop" });
    if (!await this.waitClosed(5_000)) {
      this.forcedKill = true;
      this.child.kill("SIGKILL");
      await this.waitClosed(2_000);
    }
    await this.dispose();
  }

  /** 正常重启先走产品 /exit；退出失败必须让场景失败，不能以清理强杀冒充通过。 */
  async stop(): Promise<void> {
    if (this.shutdown === "crash" && this.disposed) return;
    return this.stopPromise ??= (async () => {
      if (this.disposed) return;
      try {
        if (this.exitCode === undefined) {
          this.shutdown = "graceful";
          // Empty-editor double Escape opens Rewind. Close only an observed overlay,
          // fencing each key on a rendered application frame before inspecting again.
          // Notifications and narrow layouts may hide the normal footer: absence
          // of that footer is never evidence of an overlay.
          const hasOverlay = (screen: string) => /\bEsc(?:\s*:|\s+to)?\s+(?:close|back)\b/iu.test(screen);
          for (let count = 0; count < 4 && hasOverlay(await this.screen()); count++) {
            const before = this.parsedOutput;
            await this.key("Escape");
            await this.waitFor(() => this.parsedOutput > before, "overlay closes before exit", 3_000);
          }
          try { await this.command("exit"); }
          catch (error) { if (this.exitCode !== 0) throw error; }
        }
        if (!await this.waitClosed(15_000)) throw new Error(`${this.label}: graceful Peri exit timed out`);
        if (this.exitCode !== 0 || this.bridgeExitCode !== 0 || this.forcedKill || this.bridgeFailure) {
          throw new Error(`${this.label}: unexpected process termination ${JSON.stringify(this.outcome())}: ${this.bridgeFailure?.message ?? ""}`);
        }
        await this.dispose();
      } catch (error) {
        await this.forceCleanup();
        throw error;
      }
    })();
  }

  /** 仅供显式崩溃恢复场景；调用方仍须验证新进程恢复与旧工具未重放。 */
  async crash(): Promise<ReplayProcessOutcome> {
    if (!this.running || this.exitCode !== undefined || this.stopPromise) throw new Error("cannot crash an inactive TUI");
    this.shutdown = "crash";
    this.control({ type: "crash" });
    if (!await this.waitClosed(10_000)) { await this.forceCleanup(); throw new Error("explicit crash did not terminate Peri"); }
    await this.dispose();
    if (this.exitCode !== -9 || this.bridgeExitCode !== 0 || !this.forcedKill) throw new Error(`unexpected crash outcome: ${JSON.stringify(this.outcome())}`);
    return this.outcome();
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
