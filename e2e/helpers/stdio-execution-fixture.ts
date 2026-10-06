import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { access, mkdir, open, readdir } from "node:fs/promises";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

type RequestId = string | number;
type WireReply = { result: any } | { error: { code: number; message: string; data?: any } };
type WireMessage = {
  id?: RequestId;
  method?: string;
  params?: any;
  result?: any;
  error?: { code: number; message: string; data?: any };
};
type WireRequest = WireMessage & { id: RequestId; method: string };

interface FixtureOptions {
  binary: string;
  cwd: string;
  home: string;
  env: Record<string, string>;
  onServerRequest(request: WireRequest): void;
  handleServerRequest(request: WireRequest): WireReply | Promise<WireReply>;
  onNotification(notification: WireMessage): void;
}

const ADMISSION_METHODS = new Set([
  "peri/execution/admit",
  "peri/execution/entered",
  "peri/execution/settle",
]);
const REVERSE_METHODS = new Set([
  "session/work/query",
  "session/work/resolve",
  "session/execute",
  "session/execute/resolve",
]);
const MAX_FRAME_BYTES = 64 * 1024 * 1024;

class JsonlPeer {
  private readonly pending = new Map<RequestId, {
    resolve(message: WireMessage): void;
    reject(error: Error): void;
    timer: ReturnType<typeof setTimeout>;
  }>();
  private readonly tasks = new Set<Promise<void>>();
  private readonly lines;
  private failure?: Error;
  private stderr = "";
  private readonly wire: Array<Record<string, unknown>> = [];
  private sequence = 0;
  private ending = false;
  readonly closed: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;

  constructor(
    readonly child: ChildProcessWithoutNullStreams,
    private readonly name: string,
    private readonly handleRequest: (request: WireRequest) => Promise<WireReply>,
    private readonly handleNotification: (message: WireMessage) => void | Promise<void>,
  ) {
    this.lines = createInterface({ input: child.stdout });
    child.stderr.on("data", (chunk) => {
      this.stderr = (this.stderr + String(chunk)).slice(-16_384);
    });
    child.on("error", (error) => this.fail(error));
    child.stdin.on("error", (error) => this.fail(error));
    this.closed = new Promise((resolve) => {
      child.once("close", (code, signal) => {
        this.fail(new Error(`${name} closed (${code ?? signal}): ${this.stderr}`));
        resolve({ code, signal });
      });
    });
    this.lines.on("line", (line) => {
      const task = this.receive(line).catch((error) => this.fail(error));
      this.tasks.add(task);
      void task.then(() => this.tasks.delete(task));
    });
  }

  private fail(error: unknown): void {
    this.failure ??= error instanceof Error ? error : new Error(String(error));
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(this.failure);
    }
    this.pending.clear();
  }

  private send(message: WireMessage): void {
    if (this.failure) throw this.failure;
    const line = JSON.stringify({ jsonrpc: "2.0", ...message });
    if (Buffer.byteLength(line) > MAX_FRAME_BYTES) throw new Error(`${this.name} frame exceeds limit`);
    if (!this.child.stdin.writable) throw new Error(`${this.name} stdin is closed`);
    this.record("send", message);
    this.child.stdin.write(`${line}\n`);
  }

  private record(direction: "send" | "receive", message: WireMessage): void {
    const params = message.params;
    const result = message.result;
    this.wire.push({
      direction,
      id: message.id,
      method: message.method,
      sessionId: params?.sessionId ?? params?.snapshot?.sessionId,
      requestId: params?.requestId,
      existingAdmissionId: params?.existingAdmission?.admissionId,
      admissionId: params?.admission?.admissionId ?? result?.admission?.admissionId,
      status: result?.status,
      reason: result?.reason,
      error: message.error,
    });
    if (this.wire.length > 24) this.wire.shift();
  }

  diagnostics(): Record<string, unknown> {
    return {
      peer: this.name,
      failure: this.failure?.message,
      wire: this.wire,
      stderr: this.stderr,
    };
  }

  async call(method: string, params: any, timeoutMs = 120_000, originalId?: RequestId): Promise<WireMessage> {
    if (this.failure) throw this.failure;
    const id = originalId ?? `fixture-${this.name}-${++this.sequence}`;
    if (this.pending.has(id)) throw new Error(`${this.name} duplicate outstanding request ID: ${id}`);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.fail(new Error(`${this.name} timeout: ${method}; execution ownership remains unknown`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try {
        this.send({ id, method, params });
      } catch (error) {
        this.fail(error);
      }
    });
  }

  private async receive(line: string): Promise<void> {
    if (Buffer.byteLength(line) > MAX_FRAME_BYTES) throw new Error(`${this.name} frame exceeds limit`);
    const message = JSON.parse(line) as WireMessage;
    this.record("receive", message);
    if (typeof message.method === "string") {
      if (message.id === undefined) {
        await this.handleNotification(message);
        return;
      }
      let reply: WireReply;
      try {
        reply = await this.handleRequest(message as WireRequest);
      } catch (error) {
        reply = { error: { code: -32603, message: error instanceof Error ? error.message : String(error) } };
      }
      this.send({ id: message.id, ...reply });
      return;
    }
    if (message.id === undefined) throw new Error(`${this.name} response lacks request ID`);
    const entry = this.pending.get(message.id);
    if (!entry) throw new Error(`${this.name} response has no original outstanding request: ${message.id}`);
    if (("result" in message) === ("error" in message)) throw new Error(`${this.name} ambiguous response`);
    this.pending.delete(message.id);
    clearTimeout(entry.timer);
    entry.resolve(message);
  }

  async stop(): Promise<number | null> {
    if (!this.ending) {
      this.ending = true;
      if (this.child.stdin.writable) this.child.stdin.end();
    }
    const wait = async (timeoutMs: number) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        return await Promise.race([
          this.closed,
          new Promise<undefined>((resolve) => { timer = setTimeout(() => resolve(undefined), timeoutMs); }),
        ]);
      } finally {
        if (timer) clearTimeout(timer);
      }
    };
    let exit = await wait(15_000);
    if (!exit) {
      this.killOwnedGroup("SIGTERM");
      exit = await wait(5_000);
    }
    if (!exit) {
      this.killOwnedGroup("SIGKILL");
      exit = await this.closed;
    }
    this.killOwnedGroup("SIGTERM");
    this.killOwnedGroup("SIGKILL");
    this.lines.close();
    return exit.code;
  }

  async join(): Promise<void> {
    await Promise.allSettled(this.tasks);
  }

  private killOwnedGroup(signal: NodeJS.Signals): void {
    if (!this.child.pid) return;
    try {
      if (process.platform === "win32") this.child.kill(signal);
      else process.kill(-this.child.pid, signal);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
    }
  }
}

function replyFrom(message: WireMessage): WireReply {
  if (message.error) return { error: message.error };
  if ("result" in message) return { result: message.result };
  throw new Error("Execution bridge response has no authoritative outcome");
}

async function trustedBun(env: Record<string, string>): Promise<string> {
  for (const directory of (env.PATH ?? "").split(path.delimiter)) {
    if (!path.isAbsolute(directory)) continue;
    const candidate = path.join(directory, process.platform === "win32" ? "bun.exe" : "bun");
    try {
      await access(candidate, constants.X_OK);
      return candidate;
    } catch {}
  }
  throw new Error("Stdio execution E2E requires Bun; no fixture admission or Rust fallback");
}

async function readRustDiagnostics(directory: string): Promise<Record<string, unknown>> {
  const names = (await readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.startsWith("stdio-execution."))
    .map((entry) => entry.name)
    .sort()
    .slice(-3);
  const files = [];
  for (const name of names) {
    const file = await open(path.join(directory, name), "r");
    try {
      const size = (await file.stat()).size;
      const buffer = Buffer.alloc(Math.min(size, 64 * 1024));
      const { bytesRead } = await file.read(buffer, 0, buffer.length, size - buffer.length);
      const lines = buffer.subarray(0, bytesRead).toString("utf8").split("\n").filter(Boolean);
      files.push({
        name,
        errors: lines.filter((line) => /"level":"(?:ERROR|WARN)"/.test(line)).slice(-16),
        rejection: lines.filter((line) => /reject|denied|permission|invocation|checkpoint|projection boundary/i.test(line)).slice(-24),
        tail: lines.slice(-16),
      });
    } finally {
      await file.close();
    }
  }
  return { directory, files };
}

export async function startStdioExecutionFixture(options: FixtureOptions): Promise<{
  call(method: string, params: any, timeoutMs?: number): Promise<WireMessage>;
  close(): Promise<number | null>;
  diagnostics(): Promise<string>;
}> {
  if (!path.isAbsolute(options.home) || options.env.HOME !== options.home) {
    throw new Error("Execution E2E requires an explicitly isolated absolute HOME");
  }
  const module = path.resolve(path.dirname(fileURLToPath(import.meta.url)),
    "../../npm-packages/@peri-sdk/src/execution/sidecar.ts");
  await access(module, constants.R_OK);
  const executable = await trustedBun(options.env);
  const database = path.join(options.home, ".peri/execution/registry.db");
  await mkdir(path.dirname(database), { recursive: true, mode: 0o700 });
  const logDirectory = path.join(options.home, ".peri/logs");
  await mkdir(logDirectory, { recursive: true, mode: 0o700 });
  const periEnv = {
    ...options.env,
    RUST_LOG_FILE: path.join(logDirectory, "stdio-execution.log"),
    RUST_LOG_FORMAT: "json",
    RUST_LOG: "info,peri_agent::agent::stages=debug,peri_middlewares::subagent=debug,peri_middlewares::mcp::middleware=debug",
  };
  const peers: JsonlPeer[] = [];
  let peri: JsonlPeer;
  let sdk: JsonlPeer;
  let closing: Promise<number | null> | undefined;
  const close = () => closing ??= (async () => {
    let periExit: number | null = null;
    const failures: unknown[] = [];
    for (const peer of peers) {
      try {
        const code = await peer.stop();
        if (peer === peri) periExit = code;
      } catch (error) {
        failures.push(error);
      }
    }
    await Promise.all(peers.map((peer) => peer.join()));
    if (failures.length) throw new AggregateError(failures, "Owned stdio execution processes could not close");
    return periExit;
  })();
  const launch = (binary: string, args: string[], cwd: string, env = options.env) => spawn(binary, args, {
    cwd,
    env,
    stdio: ["pipe", "pipe", "pipe"],
    detached: process.platform !== "win32",
  });
  try {
    sdk = new JsonlPeer(launch(executable, [module, "--database", database,
      "--instance-id", randomUUID(), "--generation-id", randomUUID()], path.dirname(module)), "sdk",
      async (request) => {
        if (!REVERSE_METHODS.has(request.method)) {
          return { error: { code: -32601, message: `unsupported SDK reverse method: ${request.method}` } };
        }
        return replyFrom(await peri.call(request.method, request.params, 180_000, request.id));
      }, () => {});
    peers.push(sdk);
    peri = new JsonlPeer(launch(options.binary, ["acp", "--cwd", options.cwd], options.cwd, periEnv), "peri",
      async (request) => {
        options.onServerRequest(request);
        if (ADMISSION_METHODS.has(request.method)) {
          const wireId = `peri-${typeof request.id}-${request.id}`;
          return replyFrom(await sdk.call(request.method, request.params, 120_000, wireId));
        }
        return options.handleServerRequest(request);
      }, async (notification) => {
        options.onNotification(notification);
        if (notification.method === "session/work/available" && !closing) {
          const response = await sdk.call("peri/execution/activate", notification.params, 180_000);
          if (response.error) throw new Error(`Actual SDK activation failed: ${JSON.stringify(response.error)}`);
        }
      });
    peers.unshift(peri);
    const ready = await sdk.call("peri/execution/ready", {}, 10_000);
    if (ready.error || ready.result?.protocolVersion !== 1 || ready.result?.durability !== "durable") {
      throw new Error(`Actual SQLite SDK dispatcher is not ready: ${JSON.stringify(ready)}`);
    }
    return {
      call: (method, params, timeoutMs) => peri.call(method, params, timeoutMs),
      close,
      diagnostics: async () => JSON.stringify({
        rust: await readRustDiagnostics(logDirectory),
        peers: peers.map((peer) => peer.diagnostics()),
      }),
    };
  } catch (error) {
    try {
      await close();
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], "Stdio execution startup and cleanup failed");
    }
    throw error;
  }
}
