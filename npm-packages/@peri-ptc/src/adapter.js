import readline from "node:readline";

const TOOL_FAILED = "TOOL_FAILED";
const RESOURCE_LIMIT = "RESOURCE_LIMIT";
const EXECUTION_MESSAGES = Object.freeze({
  [TOOL_FAILED]: "JavaScript execution failed",
  [RESOURCE_LIMIT]: "JavaScript resource limit exceeded",
});
const ABORT_MESSAGE = "The operation was aborted";
const resourceLimitErrors = new WeakSet();

function resourceLimitError() {
  const error = new ToolCallError(RESOURCE_LIMIT, "JavaScript resource limit exceeded");
  resourceLimitErrors.add(error);
  return error;
}

function classifyExecutionError(error) {
  return resourceLimitErrors.has(error) ? RESOURCE_LIMIT : TOOL_FAILED;
}

export class ToolCallError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "ToolCallError";
    this.code = code;
  }
}

export function createPtcAdapter(write) {
  let nextId = 1;
  const pending = new Map();
  let maxFrameBytes = Number.MAX_SAFE_INTEGER;

  const send = (message) => {
    const encoded = JSON.stringify(message);
    if (Buffer.byteLength(encoded, "utf8") > maxFrameBytes) {
      throw resourceLimitError();
    }
    write(`${encoded}\n`);
  };

  const request = (method, params, signal) => {
    const id = nextId++;
    if (signal?.aborted) {
      return Promise.reject(new DOMException(ABORT_MESSAGE, "AbortError"));
    }
    return new Promise((resolve, reject) => {
      const onAbort = signal
        ? () => {
            if (!pending.delete(id)) return;
            send({ jsonrpc: "2.0", method: "tool/cancel", params: { invocationId: params.invocationId } });
            reject(new DOMException(ABORT_MESSAGE, "AbortError"));
          }
        : undefined;
      if (onAbort) signal.addEventListener("abort", onAbort, { once: true });
      const settle = (callback) => (value) => {
        if (onAbort) signal.removeEventListener("abort", onAbort);
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

  const tools = new Proxy({}, {
    get(_target, name) {
      if (typeof name !== "string") return undefined;
      return (input, options = {}) => request(
        "tool/call",
        { invocationId: `ptc-${nextId}`, toolName: name, input: input ?? null },
        options.signal,
      );
    },
  });

  const handleResponse = (message) => {
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.error) {
      const code = typeof message.error.data?.code === "string"
        ? message.error.data.code
        : TOOL_FAILED;
      waiter.reject(new ToolCallError(code, message.error.message));
    } else {
      waiter.resolve(message.result);
    }
  };

  const execute = async (message) => {
    const limits = message.params.limits ?? {};
    maxFrameBytes = limits.maxFrameBytes ?? Number.MAX_SAFE_INTEGER;
    const maxLogsBytes = limits.maxLogsBytes ?? Number.MAX_SAFE_INTEGER;
    const maxResultBytes = limits.maxResultBytes ?? Number.MAX_SAFE_INTEGER;
    const logs = [];
    let logBytes = 0;
    const consoleProxy = {};
    for (const level of ["log", "info", "warn", "error"]) {
      consoleProxy[level] = (...args) => {
        const text = args.map((arg) => typeof arg === "string" ? arg : JSON.stringify(arg)).join(" ");
        logBytes += Buffer.byteLength(text, "utf8");
        if (logBytes > maxLogsBytes) {
          throw resourceLimitError();
        }
        logs.push(text);
      };
    }

    try {
      const run = new Function("tools", "input", "console", `return (async () => { ${message.params.source}\n })()`);
      const value = await run(tools, message.params.input, consoleProxy);
      const normalized = value ?? null;
      const encodedValue = JSON.stringify(normalized);
      if (Buffer.byteLength(encodedValue, "utf8") > maxResultBytes) {
        throw resourceLimitError();
      }
      send({ jsonrpc: "2.0", id: message.id, result: { value: normalized, logs } });
    } catch (error) {
      const code = classifyExecutionError(error);
      const errorFrame = {
        jsonrpc: "2.0",
        id: message.id,
        error: { code: -32001, message: EXECUTION_MESSAGES[code], data: { code } },
      };
      const encoded = JSON.stringify(errorFrame);
      write(`${encoded}\n`);
    } finally {
      for (const waiter of pending.values()) {
        waiter.reject(new ToolCallError(TOOL_FAILED, "JavaScript execution ended"));
      }
      pending.clear();
    }
  };

  const handleMessage = async (message) => {
    if (message.id != null && !message.method) {
      handleResponse(message);
      return;
    }
    if (message.method === "execute") await execute(message);
  };

  return { handleMessage, tools };
}

export function startPtcAdapter() {
  const adapter = createPtcAdapter((line) => process.stdout.write(line));
  const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  lines.on("line", (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }
    void adapter.handleMessage(message);
  });
  return adapter;
}
