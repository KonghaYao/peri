import { describe, expect, test } from "bun:test";
import { StdioTransport } from "../src/transport/stdio-transport.ts";

// A real child process consumes the trusted frame before it reads JSON-RPC lines.
const MOCK = String.raw`
const fs = require('node:fs');
function bytes(length) {
  const out = Buffer.alloc(length);
  let position = 0;
  while (position < length) {
    const count = fs.readSync(0, out, position, length - position, null);
    if (!count) process.exit(0);
    position += count;
  }
  return out;
}
const size = bytes(4).readUInt32BE(0);
const bootstrap = JSON.parse(bytes(size).toString('utf8'));
function line() {
  const data = [];
  while (true) {
    const byte = bytes(1)[0];
    if (byte === 10) return JSON.parse(Buffer.from(data).toString('utf8'));
    data.push(byte);
  }
}
function write(value) { fs.writeSync(1, JSON.stringify(value) + '\n'); }
const mode = process.env.MOCK_MODE;
if (mode === 'eof') { const request = line(); process.exit(0); }
if (mode === 'bootstrap') {
  const request = line();
  write({jsonrpc:'2.0',id:request.id,result:bootstrap});
}
if (mode === 'concurrent') {
  const first = line();
  const second = line();
  write({jsonrpc:'2.0',id:second.id,result:second.params.value});
  write({jsonrpc:'2.0',id:first.id,result:first.params.value});
}
if (mode === 'split') {
  const request = line();
  const response = JSON.stringify({jsonrpc:'2.0',id:request.id,result:'拆分✓'}) + '\n';
  const data = Buffer.from(response);
  for (const byte of data) fs.writeSync(1, Buffer.from([byte]));
}
if (mode === 'reverse') {
  const request = line();
  write({jsonrpc:'2.0',id:89,method:'client/ask',params:{question:'yes?'}});
  const reply = line();
  write({jsonrpc:'2.0',id:request.id,result:reply.result});
}
if (mode === 'notify') {
  const request = line();
  write({jsonrpc:'2.0',method:'session/update',params:{sessionId:'s1'}});
  write({jsonrpc:'2.0',id:request.id,result:true});
}
if (mode === 'pending') { line(); setTimeout(() => {}, 10000); }
`;

async function start(mode: string, settings: unknown = { config: { active_alias: "sonnet" } }) {
  return StdioTransport.start({
    command: process.execPath,
    args: ["-e", MOCK],
    env: { MOCK_MODE: mode },
    settings,
  });
}

describe("ACP stdio transport", () => {
  test("writes raw settings before the first ACP line", async () => {
    const transport = await start("bootstrap");
    try {
      expect(await transport.request("initialize", {})).toEqual({ config: { active_alias: "sonnet" } });
    } finally { await transport.close(); }
  });

  test("correlates concurrent requests when responses arrive out of order", async () => {
    const transport = await start("concurrent");
    try {
      const first = transport.request("first", { value: "a" });
      const second = transport.request("second", { value: "b" });
      expect(await Promise.all([first, second])).toEqual(["a", "b"]);
    } finally { await transport.close(); }
  });

  test("assembles split UTF-8 response bytes", async () => {
    const transport = await start("split");
    try { expect(await transport.request("split", {})).toBe("拆分✓"); }
    finally { await transport.close(); }
  });

  test("handles reverse ACP requests and returns the handler result", async () => {
    const transport = await start("reverse");
    transport.setRequestHandler((method, params) => ({ method, params }));
    try {
      expect(await transport.request("initialize", {})).toEqual({ method: "client/ask", params: { question: "yes?" } });
    } finally { await transport.close(); }
  });

  test("delivers notifications to subscription and stream", async () => {
    const transport = await start("notify");
    const seen: string[] = [];
    const unsubscribe = transport.subscribe((event) => seen.push(event.method));
    const stream = transport.events()[Symbol.asyncIterator]();
    try {
      await transport.request("initialize", {});
      expect(seen).toEqual(["session/update"]);
      expect((await stream.next()).value).toEqual({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1" } });
    } finally { unsubscribe(); await stream.return?.(); await transport.close(); }
  });

  test("rejects pending and later requests after EOF", async () => {
    const transport = await start("eof");
    try {
      await expect(transport.request("initialize", {})).rejects.toThrow("ACP process exited (code 0)");
      await expect(transport.request("after", {})).rejects.toThrow("ACP process exited (code 0)");
    } finally { await transport.close(); }
  });

  test("close rejects a pending request and ends event iterators", async () => {
    const transport = await start("pending");
    const { response } = await transport.sendRequest("session/prompt", {});
    const events = transport.events()[Symbol.asyncIterator]();
    await transport.close();
    await expect(response).rejects.toThrow("closed");
    expect((await events.next()).done).toBe(true);
  });

  test("rejects oversized settings before spawning", async () => {
    await expect(start("bootstrap", { data: "x".repeat(1024 * 1024) })).rejects.toThrow("size");
  });
});
