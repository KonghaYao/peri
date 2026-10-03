import { EventQueue } from "./event-queue";
import { ProcessBroker } from "./process-broker";
import { randomBytes } from "node:crypto";
import type {
  JsonRpcNotification,
  ReverseRequestHandler,
  StdioTransportOptions,
  Transport,
} from "./types";

type Pending = {
  resolve(value: unknown): void;
  reject(error: Error): void;
};

const MAX_SETTINGS_BYTES = 1024 * 1024;
const MAX_ACP_LINE_BYTES = 8 * 1024 * 1024;
const encoder = new TextEncoder();

function bootstrapFrame(options: StdioTransportOptions): Uint8Array {
  if (
    options.settings === null ||
    Array.isArray(options.settings) ||
    typeof options.settings !== "object"
  ) {
    throw new Error("settings bootstrap must be a JSON object");
  }
  let json: string;
  try {
    json = JSON.stringify(options.settings);
  } catch {
    throw new Error("settings bootstrap is not JSON serializable");
  }
  if (!json) throw new Error("settings bootstrap is not JSON serializable");
  const bytes = encoder.encode(json);
  if (bytes.length === 0 || bytes.length > MAX_SETTINGS_BYTES) {
    throw new Error("settings bootstrap size is invalid");
  }
  const frame = new Uint8Array(4 + bytes.length);
  new DataView(frame.buffer).setUint32(0, bytes.length, false);
  frame.set(bytes, 4);
  return frame;
}

function errorFrom(value: unknown, fallback: string): Error {
  if (value instanceof Error) return value;
  return new Error(fallback);
}

export class StdioTransport implements Transport {
  private readonly process: Bun.Subprocess<"pipe", "pipe", "pipe">;
  private readonly pending = new Map<number, Pending>();
  private readonly listeners = new Set<
    (notification: JsonRpcNotification) => void
  >();
  private readonly queues = new Set<EventQueue>();
  private nextId = 1;
  private writeTail: Promise<void> = Promise.resolve();
  private terminalError: Error | undefined;
  private requestHandler: ReverseRequestHandler | undefined;
  private closePromise: Promise<void> | undefined;
  private readonly terminationProof: Promise<boolean>;
  private readonly hasProcessBroker: boolean;
  readonly generationId: string;

  private constructor(process: Bun.Subprocess<"pipe", "pipe", "pipe">, generationId: string, broker?: ProcessBroker) {
    this.process = process;
    this.generationId = generationId;
    this.hasProcessBroker = broker !== undefined;
    this.terminationProof = process.exited.then(
      () => broker?.settleAfterAgentExit() ?? false,
      () => broker?.settleAfterAgentExit() ?? false,
    );
    void this.readOutput();
    void this.drainStderr();
    void process.exited.then(
      (code) => this.terminate(new Error(`ACP process exited (code ${code})`)),
      () => this.terminate(new Error("ACP process exited")),
    );
  }

  static async start(options: StdioTransportOptions): Promise<StdioTransport> {
    const frame =
      options.settings === undefined ? undefined : bootstrapFrame(options);
    const env = { ...process.env, ...options.env };
    const generationId = randomBytes(16).toString("hex");
    env.PERI_AGENT_GENERATION_ID = generationId;
    const broker = process.platform === "win32" ? undefined : await ProcessBroker.start();
    if (broker) {
      env.PERI_PROCESS_BROKER_SOCKET = broker.socketPath;
      env.PERI_PROCESS_BROKER_TOKEN = broker.token;
    }
    for (const [key, value] of Object.entries(env))
      if (value === undefined) delete env[key];
    let child: Bun.Subprocess<"pipe", "pipe", "pipe">;
    try {
      child = Bun.spawn([options.command, ...(options.args ?? [])], {
        cwd: options.cwd,
        env: env as Record<string, string>,
        stdin: "pipe",
        stdout: "pipe",
        stderr: "pipe",
      });
    } catch (error) {
      await broker?.settleAfterAgentExit();
      throw error;
    }
    const transport = new StdioTransport(child, generationId, broker);
    if (frame) {
      try {
        await transport.writeBytes(frame);
      } catch {
        await transport.close();
        throw new Error("failed to write settings bootstrap");
      }
    }
    return transport;
  }

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return this.sendRequest<T>(method, params).then(({ response }) => response);
  }

  async sendRequest<T = unknown>(
    method: string,
    params?: unknown,
  ): Promise<{ response: Promise<T> }> {
    this.assertOpen();
    const id = this.nextId++;
    if (!Number.isSafeInteger(id)) throw new Error("ACP request id exhausted");
    let resolve!: (value: T) => void;
    let reject!: (error: Error) => void;
    const response = new Promise<T>((yes, no) => {
      resolve = yes;
      reject = no;
    });
    // Delivery-only callers can ignore the later response without an unhandled rejection.
    void response.catch(() => {});
    this.pending.set(id, { resolve: (value) => resolve(value as T), reject });
    try {
      await this.writeJson({ jsonrpc: "2.0", id, method, params });
    } catch (error) {
      this.pending.delete(id);
      reject(errorFrom(error, "ACP request write failed"));
      throw error;
    }
    return { response };
  }

  notify(method: string, params?: unknown): Promise<void> {
    return this.writeJson({ jsonrpc: "2.0", method, params });
  }

  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  events(): AsyncIterable<JsonRpcNotification> {
    const queue = new EventQueue(() => this.queues.delete(queue));
    if (this.terminalError) queue.finish();
    else this.queues.add(queue);
    return queue;
  }

  setRequestHandler(handler: ReverseRequestHandler): void {
    this.requestHandler = handler;
  }

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    this.terminate(new Error("ACP transport closed"));
    this.closePromise = (async () => {
      this.process.stdin.end();
      // Give ACP its EOF shutdown path so it can drain and release its Store
      // execution owner. A stuck child still has a bounded forced exit.
      const exited = await Promise.race([
        this.process.exited.then(() => true),
        Bun.sleep(5_000).then(() => false),
      ]);
      if (!exited) this.process.kill();
      await this.process.exited;
      if (this.hasProcessBroker && !(await this.terminationProof)) {
        throw new Error("ACP subprocess cleanup could not be confirmed");
      }
    })();
    return this.closePromise;
  }

  /** Trusted supervisor evidence for this exact spawned ACP generation. */
  waitForTerminationProof(): Promise<boolean> {
    return this.terminationProof;
  }

  /** OS identity for host supervision and crash testing. */
  get pid(): number { return this.process.pid; }

  private assertOpen(): void {
    if (this.terminalError) throw this.terminalError;
  }

  private terminate(error: Error): void {
    if (this.terminalError) return;
    this.terminalError = error;
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
    for (const queue of [...this.queues]) queue.finish();
    this.queues.clear();
    this.listeners.clear();
  }

  private writeJson(value: unknown): Promise<void> {
    return this.writeBytes(encoder.encode(JSON.stringify(value) + "\n"));
  }

  private writeBytes(bytes: Uint8Array): Promise<void> {
    this.assertOpen();
    const write = this.writeTail.then(async () => {
      this.assertOpen();
      await this.process.stdin.write(bytes);
      await this.process.stdin.flush();
    });
    this.writeTail = write.catch(() => {});
    return write;
  }

  private async readOutput(): Promise<void> {
    try {
      const stream = this.process.stdout;
      if (!stream) throw new Error("ACP stdout is unavailable");
      let fragments: Uint8Array[] = [];
      let length = 0;
      for await (const chunk of stream) {
        let start = 0;
        for (let index = 0; index < chunk.length; index++) {
          if (chunk[index] !== 10) continue;
          const part = chunk.subarray(start, index);
          length += part.length;
          if (length > MAX_ACP_LINE_BYTES)
            throw new Error("ACP line exceeds size limit");
          if (length) {
            const line = new Uint8Array(length);
            let offset = 0;
            for (const fragment of fragments) {
              line.set(fragment, offset);
              offset += fragment.length;
            }
            line.set(part, offset);
            this.acceptLine(line);
          }
          fragments = [];
          length = 0;
          start = index + 1;
        }
        const rest = chunk.subarray(start);
        if (rest.length) {
          length += rest.length;
          if (length > MAX_ACP_LINE_BYTES)
            throw new Error("ACP line exceeds size limit");
          fragments.push(rest);
        }
      }
      const code = await this.process.exited;
      this.terminate(new Error(`ACP process exited (code ${code})`));
    } catch {
      this.terminate(new Error("ACP stdout failed"));
      this.process.kill();
    }
  }

  private async drainStderr(): Promise<void> {
    try {
      for await (const _chunk of this.process.stderr) {
        /* never expose secrets from stderr */
      }
    } catch {
      /* exit path settles pending requests */
    }
  }

  private acceptLine(line: Uint8Array): void {
    let message: Record<string, unknown>;
    try {
      const value = JSON.parse(
        new TextDecoder("utf-8", { fatal: true }).decode(line),
      );
      if (
        !value ||
        typeof value !== "object" ||
        Array.isArray(value) ||
        value.jsonrpc !== "2.0"
      )
        return;
      message = value;
    } catch {
      throw new Error("invalid ACP JSON-RPC line");
    }
    if (typeof message.method === "string") {
      if ("id" in message) void this.replyToReverseRequest(message);
      else
        this.publish({
          jsonrpc: "2.0",
          method: message.method,
          params: message.params,
        });
      return;
    }
    if (typeof message.id !== "number") return;
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    if ("error" in message) {
      const detail = message.error;
      const description =
        detail && typeof detail === "object" && "message" in detail
          ? String(detail.message)
          : "ACP request failed";
      pending.reject(new Error(description));
    } else pending.resolve(message.result);
  }

  private publish(notification: JsonRpcNotification): void {
    for (const listener of this.listeners) {
      try {
        listener(notification);
      } catch {
        /* one consumer cannot break transport */
      }
    }
    for (const queue of this.queues) queue.push(notification);
  }

  private async replyToReverseRequest(
    message: Record<string, unknown>,
  ): Promise<void> {
    const id = message.id;
    if (typeof id !== "number" && typeof id !== "string") return;
    try {
      if (!this.requestHandler)
        throw new Error("unsupported ACP client request");
      const result = await this.requestHandler(
        message.method as string,
        message.params,
      );
      await this.writeJson({ jsonrpc: "2.0", id, result });
    } catch {
      try {
        await this.writeJson({
          jsonrpc: "2.0",
          id,
          error: { code: -32603, message: "ACP client request failed" },
        });
      } catch {
        /* transport already closed */
      }
    }
  }
}
