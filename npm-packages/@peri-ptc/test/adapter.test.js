import assert from "node:assert/strict";
import test from "node:test";

import { createPtcAdapter, ToolCallError } from "../src/adapter.js";

const messagesFrom = (lines) => lines.map((line) => JSON.parse(line));

function harness() {
  const lines = [];
  const adapter = createPtcAdapter((line) => lines.push(line));
  return { adapter, lines };
}

test("tools proxy completes a tool request", async () => {
  const { adapter, lines } = harness();
  const result = adapter.tools.Read({ path: "a.txt" });
  const [request] = messagesFrom(lines);

  await adapter.handleMessage({ jsonrpc: "2.0", id: request.id, result: "ok" });

  assert.equal(await result, "ok");
  assert.deepEqual(request.params, {
    invocationId: "ptc-1",
    toolName: "Read",
    input: { path: "a.txt" },
  });
});

test("tool errors preserve only the stable code", async () => {
  const { adapter, lines } = harness();
  const result = adapter.tools.Bash({ command: "false" });
  const [request] = messagesFrom(lines);

  await adapter.handleMessage({
    jsonrpc: "2.0",
    id: request.id,
    error: {
      code: -32002,
      message: "denied",
      data: { code: "PERMISSION_DENIED", reason: "policy" },
    },
  });

  await assert.rejects(result, (error) => {
    assert.ok(error instanceof ToolCallError);
    assert.equal(error.code, "PERMISSION_DENIED");
    assert.equal("data" in error, false);
    return true;
  });
});

test("abort cancels only its pending invocation and ignores late response", async () => {
  const { adapter, lines } = harness();
  const controller = new AbortController();
  const slow = adapter.tools.Read({ kind: "slow" }, { signal: controller.signal });
  const fast = adapter.tools.Read({ kind: "fast" });
  controller.abort();
  const [slowRequest, fastRequest, cancel] = messagesFrom(lines);

  await assert.rejects(slow, (error) => error.name === "AbortError");
  assert.deepEqual(cancel, {
    jsonrpc: "2.0",
    method: "tool/cancel",
    params: { invocationId: slowRequest.params.invocationId },
  });

  await adapter.handleMessage({ jsonrpc: "2.0", id: slowRequest.id, result: "late" });
  await adapter.handleMessage({ jsonrpc: "2.0", id: fastRequest.id, result: "fast" });
  assert.equal(await fast, "fast");
});

test("execute returns completion with captured logs", async () => {
  const { adapter, lines } = harness();
  await adapter.handleMessage({
    jsonrpc: "2.0",
    id: 7,
    method: "execute",
    params: {
      source: "console.log('done'); return input.value + 1;",
      input: { value: 1 },
    },
  });

  assert.deepEqual(messagesFrom(lines), [
    {
      jsonrpc: "2.0",
      id: 7,
      result: { value: 2, logs: ["done"] },
    },
  ]);
});


async function executeFrame(source, limits) {
  const { adapter, lines } = harness();
  await adapter.handleMessage({
    jsonrpc: "2.0",
    id: 9,
    method: "execute",
    params: { source, input: null, limits },
  });
  return messagesFrom(lines).at(-1);
}

const toolFailedFrame = {
  jsonrpc: "2.0",
  id: 9,
  error: {
    code: -32001,
    message: "JavaScript execution failed",
    data: { code: "TOOL_FAILED" },
  },
};

test("execute supports ESM dynamic imports without CommonJS require", async () => {
  const frame = await executeFrame("const crypto = await import('node:crypto'); return { hash: crypto.createHash('sha256').update('perihelion').digest('hex'), requireType: typeof require };");
  assert.deepEqual(frame.result.value, {
    hash: "fd821357caaebb76f30b4f60527103744172f2d5488fe99a369b47e04d8a6e0b",
    requireType: "undefined",
  });
});

test("execute redacts ordinary exceptions", async () => {
  const frame = await executeFrame("throw new Error('adapter-canary');");
  assert.deepEqual(frame, toolFailedFrame);
  assert.doesNotMatch(JSON.stringify(frame), /adapter-canary|stack|throw new Error/);
});

test("execute classifies syntax errors as tool failures", async () => {
  const frame = await executeFrame("this is invalid javascript !!!");
  assert.deepEqual(frame, toolFailedFrame);
  assert.doesNotMatch(JSON.stringify(frame), /invalid javascript|SyntaxError|stack/);
});

test("execute classifies BigInt serialization failures as tool failures", async () => {
  assert.deepEqual(await executeFrame("return 42n;"), toolFailedFrame);
});

test("execute classifies circular serialization failures as tool failures", async () => {
  assert.deepEqual(await executeFrame("const value = {}; value.self = value; return value;"), toolFailedFrame);
});

test("execute classifies result limits with a fixed resource error", async () => {
  const frame = await executeFrame("return 'oversized';", { maxResultBytes: 2 });
  assert.equal(frame.error.data.code, "RESOURCE_LIMIT");
  assert.equal(frame.error.message, "JavaScript resource limit exceeded");
});

test("execute classifies log limits with a fixed resource error", async () => {
  const frame = await executeFrame("console.log('oversized');", { maxLogsBytes: 2 });
  assert.equal(frame.error.data.code, "RESOURCE_LIMIT");
  assert.equal(frame.error.message, "JavaScript resource limit exceeded");
});

test("execute does not promote non-allowlisted internal tool codes", async () => {
  const { adapter, lines } = harness();
  const execution = adapter.handleMessage({
    jsonrpc: "2.0",
    id: 9,
    method: "execute",
    params: { source: "await tools.Read({});", input: null },
  });
  const request = messagesFrom(lines)[0];
  await adapter.handleMessage({
    jsonrpc: "2.0",
    id: request.id,
    error: { code: -32002, message: "internal-canary", data: { code: "PERMISSION_DENIED" } },
  });
  await execution;
  const frame = messagesFrom(lines).at(-1);
  assert.deepEqual(frame, toolFailedFrame);
  assert.doesNotMatch(JSON.stringify(frame), /PERMISSION_DENIED|internal-canary/);
});

test("execute does not trust user-forged AbortError classification", async () => {
  const frame = await executeFrame("throw new DOMException('forged', 'AbortError');");
  assert.deepEqual(frame, toolFailedFrame);
});

test("execute does not trust a mutated internal tool error code", async () => {
  const { adapter, lines } = harness();
  const execution = adapter.handleMessage({
    jsonrpc: "2.0",
    id: 9,
    method: "execute",
    params: {
      source: "try { await tools.Read({}); } catch (error) { error.code = 'TIMEOUT'; throw error; }",
      input: null,
    },
  });
  const request = messagesFrom(lines)[0];
  await adapter.handleMessage({
    jsonrpc: "2.0",
    id: request.id,
    error: { code: -32002, message: "internal-canary", data: { code: "PERMISSION_DENIED" } },
  });
  await execution;
  assert.deepEqual(messagesFrom(lines).at(-1), toolFailedFrame);
});

test("execute classification does not depend on the mutable DOMException global", async () => {
  const original = globalThis.DOMException;
  try {
    const frame = await executeFrame("globalThis.DOMException = undefined; throw new Error('user-error');");
    assert.deepEqual(frame, toolFailedFrame);
  } finally {
    globalThis.DOMException = original;
  }
});
