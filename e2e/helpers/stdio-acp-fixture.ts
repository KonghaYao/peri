import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdir, open, readdir } from "node:fs/promises";
import path from "node:path";
import { createInterface } from "node:readline";

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
    this.wire.push({
      direction,
      id: message.id,
      method: message.method,
      sessionId: params?.sessionId,
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

  async call(method: string, params: any, timeoutMs = 120_000): Promise<WireMessage> {
    if (this.failure) throw this.failure;
    const id = `fixture-${this.name}-${++this.sequence}`;
    if (this.pending.has(id)) throw new Error(`${this.name} duplicate outstanding request ID: ${id}`);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.fail(new Error(`${this.name} 请求超时：${method}`));
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

async function readRustDiagnostics(directory: string): Promise<Record<string, unknown>> {
  const names = (await readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.startsWith("stdio-acp."))
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

/** e2e 自有 stdio transport；只驱动当前 ACP，不提供 SDK 执行准入或持久 registry。 */
export async function startStdioAcpFixture(options: FixtureOptions): Promise<{
  call(method: string, params: any, timeoutMs?: number): Promise<WireMessage>;
  close(): Promise<number | null>;
  diagnostics(): Promise<string>;
}> {
  if (!path.isAbsolute(options.home) || options.env.HOME !== options.home) {
    throw new Error("ACP E2E 必须显式提供隔离的绝对 HOME");
  }
  const logDirectory = path.join(options.home, ".peri/logs");
  await mkdir(logDirectory, { recursive: true, mode: 0o700 });
  const peri = new JsonlPeer(spawn(options.binary, ["acp", "--cwd", options.cwd], {
    cwd: options.cwd,
    env: {
      ...options.env,
      RUST_LOG_FILE: path.join(logDirectory, "stdio-acp.log"),
      RUST_LOG_FORMAT: "json",
      RUST_LOG: "info,peri_agent::agent::stages=debug,peri_middlewares::subagent=debug,peri_middlewares::mcp::middleware=debug",
    },
    stdio: ["pipe", "pipe", "pipe"],
    detached: process.platform !== "win32",
  }), "peri", async (request) => {
    options.onServerRequest(request);
    return options.handleServerRequest(request);
  }, options.onNotification);
  let closing: Promise<number | null> | undefined;
  return {
    call: (method, params, timeoutMs) => peri.call(method, params, timeoutMs),
    close: () => closing ??= (async () => {
      const code = await peri.stop();
      await peri.join();
      return code;
    })(),
    diagnostics: async () => JSON.stringify({
      rust: await readRustDiagnostics(logDirectory),
      peers: [peri.diagnostics()],
    }),
  };
}
