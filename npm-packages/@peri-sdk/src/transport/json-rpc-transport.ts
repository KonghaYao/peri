import { EventQueue } from "./event-queue";
import { RpcError } from "./rpc-error";
import type { JsonRpcNotification, ReverseRequestHandler, Transport } from "./types";

type Pending = {
  resolve(value: unknown): void;
  reject(error: Error): void;
};

function errorFrom(value: unknown, fallback: string): Error {
  return value instanceof Error ? value : new Error(fallback);
}

/** Shared ACP JSON-RPC client behavior. A wire only moves complete JSON frames. */
export class JsonRpcTransport implements Transport {
  private readonly pending = new Map<number, Pending>();
  private readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  private readonly queues = new Set<EventQueue>();
  private nextId = 1;
  private writeTail: Promise<void> = Promise.resolve();
  private terminalError: Error | undefined;
  private requestHandler: ReverseRequestHandler | undefined;
  private closePromise: Promise<void> | undefined;

  constructor(
    private readonly sendFrame: (frame: string) => Promise<void>,
    private readonly closeWire: () => Promise<void>,
  ) {}

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return this.sendRequest<T>(method, params).then(({ response }) => response);
  }

  async sendRequest<T = unknown>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
    this.assertOpen();
    const id = this.nextId++;
    if (!Number.isSafeInteger(id)) throw new Error("ACP request id exhausted");
    let resolve!: (value: T) => void;
    let reject!: (error: Error) => void;
    const response = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
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
    this.fail(new Error("ACP transport closed"));
    this.closePromise = Promise.resolve().then(() => this.closeWire()).catch((error) => {
      this.closePromise = undefined;
      throw error;
    });
    return this.closePromise;
  }

  protected fail(error: Error): void {
    if (this.terminalError) return;
    this.terminalError = error;
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
    for (const queue of [...this.queues]) queue.finish();
    this.queues.clear();
    this.listeners.clear();
  }

  private assertOpen(): void {
    if (this.terminalError) throw this.terminalError;
  }

  private writeJson(value: unknown): Promise<void> {
    this.assertOpen();
    const frame = JSON.stringify(value);
    const write = this.writeTail.then(async () => {
      this.assertOpen();
      await this.sendFrame(frame);
    });
    this.writeTail = write.catch(() => {});
    return write;
  }

  /** Accept exactly one complete JSON-RPC frame from the transport. */
  acceptFrame(frame: string): void {
    let message: Record<string, unknown>;
    try {
      const value = JSON.parse(frame);
      if (!value || typeof value !== "object" || Array.isArray(value) || value.jsonrpc !== "2.0") return;
      message = value;
    } catch (error) {
      console.error("invalid ACP JSON-RPC frame", error);
      throw new Error("invalid ACP JSON-RPC frame", { cause: error });
    }
    if (typeof message.method === "string") {
      if ("id" in message) void this.replyToReverseRequest(message);
      else this.publish({ jsonrpc: "2.0", method: message.method, params: message.params });
      return;
    }
    if (typeof message.id !== "number") return;
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    if ("error" in message) {
      const detail = message.error;
      const description = detail && typeof detail === "object" && "message" in detail
        ? String(detail.message) : "ACP request failed";
      const fields = detail && typeof detail === "object" ? detail as { code?: unknown; data?: unknown } : {};
      pending.reject(new RpcError(typeof fields.code === "number" ? fields.code : -32603, description, fields.data));
    } else pending.resolve(message.result);
  }

  private publish(notification: JsonRpcNotification): void {
    for (const listener of this.listeners) {
      try { listener(notification); } catch { /* one consumer cannot break transport */ }
    }
    for (const queue of this.queues) queue.push(notification);
  }

  private async replyToReverseRequest(message: Record<string, unknown>): Promise<void> {
    const id = message.id;
    if (typeof id !== "number" && typeof id !== "string") return;
    try {
      if (!this.requestHandler) throw new Error("unsupported ACP client request");
      const result = await this.requestHandler(message.method as string, message.params);
      await this.writeJson({ jsonrpc: "2.0", id, result });
    } catch {
      try {
        await this.writeJson({ jsonrpc: "2.0", id, error: { code: -32603, message: "ACP client request failed" } });
      } catch { /* transport already closed */ }
    }
  }
}
