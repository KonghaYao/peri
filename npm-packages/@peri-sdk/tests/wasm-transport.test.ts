import { expect, test } from "bun:test";
import { WasmAcpTransport } from "../src/transport/wasm-transport";
import { startPeriWasmHost } from "../src/wasm/host";
import type { NativeWasmAcp } from "../src/wasm/loader";

function framePort() {
  const sent: string[] = [];
  const frames: string[] = [];
  const waiters: Array<(frame: string | null) => void> = [];
  let closed = false;
  const push = (frame: unknown) => {
    const text = JSON.stringify(frame);
    const waiter = waiters.shift();
    if (waiter) waiter(text);
    else frames.push(text);
  };
  const port: NativeWasmAcp = {
    async send(frame) { sent.push(frame); },
    recv() {
      const next = frames.shift();
      if (next) return Promise.resolve(next);
      if (closed) return Promise.resolve(null);
      return new Promise((resolve) => waiters.push(resolve));
    },
    async close() {
      closed = true;
      for (const waiter of waiters.splice(0)) waiter(null);
    },
  };
  return { port, sent, push };
}

test("注入 WASM 模块直接启动 ACP 并仅在关闭完成后提供停止证明", async () => {
  const wire = framePort();
  let config: string | undefined;
  const module = { PeriWasmAcp: { start: async (value: string) => { config = value; return wire.port; } } };
  const transport = await WasmAcpTransport.start({ module, configJson: "{}" });
  expect(config).toBe("{}");
  expect(await transport.executionStopped()).toBe(false);
  await transport.close();
  expect(await transport.executionStopped()).toBe(true);
  await expect(WasmAcpTransport.start({ module, configJson: "{}", env: {} }))
    .rejects.toThrow("cannot be combined");
  await expect(WasmAcpTransport.start({ module, configJson: "{}", moduleUrl: "file:///unused.js" }))
    .rejects.toThrow("cannot be combined");
});

test("宿主 factory 接收显式环境，关闭依次等待 native、网络和释放", async () => {
  const wire = framePort();
  const order: string[] = [];
  const close = wire.port.close;
  wire.port.close = async () => { order.push("native"); await close(); };
  wire.port.free = () => { order.push("free"); };
  const transport = await startPeriWasmHost({
    configJson: "{}", env: { INSTANCE: "host" },
    moduleFactory: async (options) => {
      const module = { ENV: {} as Record<string, string>, PeriWasmAcp: { start: async () => wire.port } };
      for (const hook of options.preRun as Array<(value: typeof module) => void>) hook(module);
      expect(module.ENV).toEqual({ INSTANCE: "host" });
      return module;
    },
    ports: { network: {
      module: {}, close() { order.push("network"); }, async drain() { order.push("drain"); },
    } },
  });
  await transport.close();
  expect(order).toEqual(["native", "network", "drain", "free", "network"]);
  expect(await transport.executionStopped()).toBe(true);
});

test("WASM frame port preserves ACP requests, notifications and reverse requests", async () => {
  const wire = framePort();
  const transport = WasmAcpTransport.fromPort(wire.port);
  const notifications: unknown[] = [];
  transport.subscribe((event) => notifications.push(event));
  transport.setRequestHandler((method, params) => ({ method, params }));
  try {
    const first = await transport.sendRequest("initialize", { protocolVersion: 1 });
    const second = await transport.sendRequest("session/new", { cwd: "/work" });
    expect(wire.sent.map((frame) => JSON.parse(frame).method)).toEqual(["initialize", "session/new"]);
    const firstId = JSON.parse(wire.sent[0]!).id;
    const secondId = JSON.parse(wire.sent[1]!).id;
    wire.push({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1" } });
    wire.push({ jsonrpc: "2.0", id: secondId, result: { sessionId: "s1" } });
    wire.push({ jsonrpc: "2.0", id: 91, method: "client/ask", params: { question: "?" } });
    wire.push({ jsonrpc: "2.0", id: firstId, result: { protocolVersion: 1 } });
    expect(await Promise.all([first.response, second.response])).toEqual([{ protocolVersion: 1 }, { sessionId: "s1" }]);
    expect(notifications).toEqual([{ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1" } }]);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(JSON.parse(wire.sent[2]!)).toEqual({ jsonrpc: "2.0", id: 91, result: { method: "client/ask", params: { question: "?" } } });
  } finally { await transport.close(); }
});

test("WASM port close rejects a pending request and ends event stream", async () => {
  const wire = framePort();
  const transport = WasmAcpTransport.fromPort(wire.port);
  const { response } = await transport.sendRequest("session/input/dispatch", {});
  const events = transport.events()[Symbol.asyncIterator]();
  await transport.close();
  await expect(response).rejects.toThrow("closed");
  expect((await events.next()).done).toBe(true);
});

test("existing Agent and Session APIs run over an injected WASM frame port", async () => {
  const { Sandbox } = await import("../src/sandbox/sandbox");
  const { ManagedAgents } = await import("../src/managed/managed-agents");
  const { MemoryKV } = await import("../src/kv/memory-kv");
  const wire = framePort();
  const methods: string[] = [];
  const sendFrame = wire.port.send;
  wire.port.send = async (frame) => {
    await sendFrame(frame);
    const request = JSON.parse(frame) as { id?: number; method: string; params?: Record<string, unknown> };
    // 通知没有 id：宿主只回响应，不把 notification 当请求处理。
    if (request.id === undefined) return;
    methods.push(request.method);
    let result: unknown;
    switch (request.method) {
      case "initialize": result = { protocolVersion: 1 }; break;
      case "session/new": result = { sessionId: "s1" }; break;
      case "session/input/snapshot": result = { generation: "g1" }; break;
      case "session/input/enqueue": result = { results: [{ inputId: request.params?.inputId, state: "delivered" }] }; break;
      case "session/close": result = {}; break;
      default: throw new Error(`Unexpected ACP request ${request.method}`);
    }
    wire.push({ jsonrpc: "2.0", id: request.id, result });
  };
  const sandbox = new Sandbox({ id: "wasm-test", transportFactory: () => WasmAcpTransport.fromPort(wire.port) });
  const manager = new ManagedAgents({ kv: new MemoryKV() });
  const agent = manager.createAgent({ path: "/tmp", id: "a1", sandbox, instructions: "hello" });
  try {
    await agent.session.start(null);
    expect(agent.session.id).toBe("s1");
    const receipt = agent.session.send("question");
    await receipt;
    expect(receipt.isSent).toBe(true);
    await agent.session.cancel();
    expect(methods).toEqual(["initialize", "session/new", "session/input/snapshot", "session/input/enqueue"]);
    const notifications = wire.sent.map((frame) => JSON.parse(frame) as { method?: string; params?: unknown })
      .filter((frame) => frame.id === undefined);
    expect(notifications).toEqual([{ jsonrpc: "2.0", method: "session/cancel", params: { sessionId: "s1" } }]);
  } finally { await manager.closeAll(); }
});

test("WASM receive failure settles pending requests and closes native port once", async () => {
  let failReceive!: (error: Error) => void;
  const receiving = new Promise<string | null>((_resolve, reject) => { failReceive = reject; });
  let closes = 0;
  let frees = 0;
  const transport = WasmAcpTransport.fromPort({
    free() { frees++; },
    async send() {},
    recv: () => receiving,
    async close() { closes++; },
  });
  const { response } = await transport.sendRequest("initialize", {});
  failReceive(new Error("WASM receive failed"));
  await expect(response).rejects.toThrow("WASM receive failed");
  await transport.close();
  expect(closes).toBe(1);
  expect(frees).toBe(1);
  await expect(transport.request("later")).rejects.toThrow("WASM receive failed");
});
