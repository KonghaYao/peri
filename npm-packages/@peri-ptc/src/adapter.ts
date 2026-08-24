import readline from "node:readline";
import type { Writable } from "node:stream";

import {
  PTC_EXECUTE_METHOD,
  PTC_PROTOCOL_VERSION,
  PTC_START_METHOD,
  type PtcExecuteParams,
  type PtcStartParams,
  type ToolCallErrorCode,
  type ToolCallOptions,
  type ToolCallParams,
  type Tools,
} from "./types.js";

export { PTC_EXECUTE_METHOD, PTC_PROTOCOL_VERSION, PTC_START_METHOD } from "./types.js";

const TOOL_FAILED = "TOOL_FAILED";
const RESOURCE_LIMIT = "RESOURCE_LIMIT";
export const PTC_BUILD_ID = "@peri-code/ptc@0.2.3";
const HANDSHAKE_REQUIRED = "PTC handshake required";
const PROTOCOL_MISMATCH = "Unsupported PTC protocol version";
const EXECUTION_MESSAGES: Record<typeof TOOL_FAILED | typeof RESOURCE_LIMIT, string> = Object.freeze({
  [TOOL_FAILED]: "JavaScript execution failed",
  [RESOURCE_LIMIT]: "JavaScript resource limit exceeded",
});
const ABORT_MESSAGE = "The operation was aborted";
const ADAPTER_FAILURE_MESSAGE = "PTC adapter message handling failed";
const resourceLimitErrors = new WeakSet<object>();

type JsonRpcId = string | number;
interface JsonRpcError {
  code: number;
  message: string;
  data?: { code?: unknown };
}
interface IncomingMessage {
  id?: JsonRpcId;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: JsonRpcError;
}
interface PendingRequest {
  resolve(value: unknown): void;
  reject(reason: unknown): void;
}
type ConsoleProxy = Record<"log" | "info" | "warn" | "error", (...args: unknown[]) => void>;
type ExecuteFunction = (tools: Tools, input: unknown, console: ConsoleProxy) => Promise<unknown>;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isStartParams(value: unknown): value is PtcStartParams {
  return isRecord(value) && "protocolVersion" in value;
}

function isExecuteParams(value: unknown): value is PtcExecuteParams {
  return isRecord(value) && typeof value.source === "string" && "input" in value;
}

function resourceLimitError(): ToolCallError {
  const error = new ToolCallError(RESOURCE_LIMIT, "JavaScript resource limit exceeded");
  resourceLimitErrors.add(error);
  return error;
}

function classifyExecutionError(error: unknown): typeof RESOURCE_LIMIT | typeof TOOL_FAILED {
  return isRecord(error) && resourceLimitErrors.has(error) ? RESOURCE_LIMIT : TOOL_FAILED;
}

export class ToolCallError extends Error {
  readonly code: ToolCallErrorCode;

  constructor(code: ToolCallErrorCode, message: string) {
    super(message);
    this.name = "ToolCallError";
    this.code = code;
  }
}

export interface PtcAdapter {
  handleMessage(message: unknown): Promise<void>;
  tools: Tools;
}

export function createPtcAdapter(write: (line: string) => void): PtcAdapter {
  let nextId = 1;
  let started = false;
  const pending = new Map<JsonRpcId, PendingRequest>();
  let maxFrameBytes = Number.MAX_SAFE_INTEGER;

  const send = (message: unknown): void => {
    const encoded = JSON.stringify(message);
    if (Buffer.byteLength(encoded, "utf8") > maxFrameBytes) throw resourceLimitError();
    write(`${encoded}\n`);
  };

  const request = (method: string, params: ToolCallParams, signal?: AbortSignal): Promise<unknown> => {
    const id = nextId++;
    if (signal?.aborted) return Promise.reject(new DOMException(ABORT_MESSAGE, "AbortError"));
    return new Promise((resolve, reject) => {
      const requestSignal = signal;
      const onAbort = requestSignal
        ? (): void => {
            if (!pending.delete(id)) return;
            send({ jsonrpc: "2.0", method: "tool/cancel", params: { invocationId: params.invocationId } });
            reject(new DOMException(ABORT_MESSAGE, "AbortError"));
          }
        : undefined;
      if (onAbort) requestSignal?.addEventListener("abort", onAbort, { once: true });
      const settle = (callback: (value: unknown) => void) => (value: unknown): void => {
        if (onAbort) requestSignal?.removeEventListener("abort", onAbort);
        callback(value);
      };
      pending.set(id, { resolve: settle(resolve), reject: settle(reject) });
      try {
        send({ jsonrpc: "2.0", id, method, params });
      } catch (error) {
        pending.delete(id);
        reject(error);
      }
    });
  };

  const tools: Tools = new Proxy({}, {
    get(_target, name): Tools[string] | undefined {
      if (typeof name !== "string") return undefined;
      return (input?: unknown, options: ToolCallOptions = {}) => request(
        "tool/call",
        { invocationId: `ptc-${nextId}`, toolName: name, input: input ?? null },
        options.signal,
      );
    },
  });

  const handleResponse = (message: IncomingMessage): void => {
    if (message.id === undefined) return;
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.error) {
      const rawCode = message.error.data?.code;
      const code = typeof rawCode === "string" ? rawCode as ToolCallErrorCode : TOOL_FAILED;
      waiter.reject(new ToolCallError(code, message.error.message));
    } else {
      waiter.resolve(message.result);
    }
  };

  const execute = async (id: JsonRpcId | undefined, params: PtcExecuteParams): Promise<void> => {
    const limits = params.limits ?? {};
    maxFrameBytes = limits.maxFrameBytes ?? Number.MAX_SAFE_INTEGER;
    const maxLogsBytes = limits.maxLogsBytes ?? Number.MAX_SAFE_INTEGER;
    const maxResultBytes = limits.maxResultBytes ?? Number.MAX_SAFE_INTEGER;
    const logs: string[] = [];
    let logBytes = 0;
    const capture = (...args: unknown[]): void => {
      const text = args.map((arg) => typeof arg === "string" ? arg : JSON.stringify(arg)).join(" ");
      logBytes += Buffer.byteLength(text, "utf8");
      if (logBytes > maxLogsBytes) throw resourceLimitError();
      logs.push(text);
    };
    const consoleProxy: ConsoleProxy = { log: capture, info: capture, warn: capture, error: capture };

    try {
      const run = new Function("tools", "input", "console", `return (async () => { ${params.source}\n })()`) as ExecuteFunction;
      const value = await run(tools, params.input, consoleProxy);
      const normalized = value ?? null;
      const encodedValue = JSON.stringify(normalized);
      if (Buffer.byteLength(encodedValue, "utf8") > maxResultBytes) throw resourceLimitError();
      send({ jsonrpc: "2.0", id, result: { value: normalized, logs } });
    } catch (error) {
      const code = classifyExecutionError(error);
      write(`${JSON.stringify({
        jsonrpc: "2.0",
        id,
        error: { code: -32001, message: EXECUTION_MESSAGES[code], data: { code } },
      })}\n`);
    } finally {
      for (const waiter of pending.values()) {
        waiter.reject(new ToolCallError(TOOL_FAILED, "JavaScript execution ended"));
      }
      pending.clear();
    }
  };

  const handleStart = (id: JsonRpcId | undefined, params: unknown): void => {
    if (!isStartParams(params) || params.protocolVersion !== PTC_PROTOCOL_VERSION) {
      send({ jsonrpc: "2.0", id, error: { code: -32602, message: PROTOCOL_MISMATCH } });
      return;
    }
    started = true;
    send({ jsonrpc: "2.0", id, result: { protocolVersion: PTC_PROTOCOL_VERSION, buildId: PTC_BUILD_ID } });
  };

  const handleMessage = async (value: unknown): Promise<void> => {
    if (!isRecord(value)) return;
    const message = value as IncomingMessage;
    if (message.id != null && !message.method) return handleResponse(message);
    if (message.method === PTC_START_METHOD) return handleStart(message.id, message.params);
    if (message.method === PTC_EXECUTE_METHOD && !started) {
      send({ jsonrpc: "2.0", id: message.id, error: { code: -32000, message: HANDSHAKE_REQUIRED } });
      return;
    }
    if (message.method === PTC_EXECUTE_METHOD && isExecuteParams(message.params)) {
      await execute(message.id, message.params);
    }
  };

  return { handleMessage, tools };
}

export function startPtcAdapter(): PtcAdapter {
  const adapter = createPtcAdapter((line) => process.stdout.write(line));
  const waitDrain = async (): Promise<void> => {
    if (!(process.stdout as Writable).writableNeedDrain) return;
    await new Promise<void>((resolve) => process.stdout.once("drain", resolve));
  };
  const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  lines.on("line", (line) => {
    let message: unknown;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }
    void adapter.handleMessage(message)
      .then(waitDrain)
      .catch(() => {
        process.stderr.write(`${ADAPTER_FAILURE_MESSAGE}\n`);
        process.exitCode = 1;
      });
  });
  return adapter;
}
