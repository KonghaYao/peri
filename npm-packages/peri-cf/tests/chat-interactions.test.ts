import { describe, expect, test } from "bun:test";
import { promptCommand, runtimeFixture, type TransportPlan } from "./helpers/runtime";
import { untilAsync } from "./helpers/sockets";
import type { SyncState } from "../shared/sync";

/**
 * 反向交互与配置读面（P4 契约保持）：反向请求经 SDK 投影进入读面、回答经 WS 控制帧回灌，
 * 未接线/不支持的请求保持安全默认；这些状态都只在内存，随运行期回收消失。
 */

const elicitationParams = (sessionId: string) => ({
  sessionId, mode: "form", message: "需要你的输入",
  requestedSchema: {
    properties: {
      name: { type: "string", title: "Name" },
      level: { type: "string", enum: ["a", "b"], title: "Level" },
    },
    required: ["name"],
  },
});

function detailEntries(state: SyncState) {
  return state.entries.map((entry) => entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])).join(""));
}

async function running(plan: TransportPlan = {}) {
  const app = runtimeFixture({ chunks: [], blocked: true, ...plan });
  const chat = await app.create("Fixture chat");
  const { socket } = await app.attached(chat.id);
  expect(await app.send(socket, promptCommand("Hello"))).toMatchObject({ ok: true });
  const transport = await app.runTransport();
  await transport.promptStarted;
  return { app, chat, socket, transport };
}

describe("reverse interactions (P4)", () => {
  test("elicitation round trip validates the form window before accepting", async () => {
    const { app, chat, socket, transport } = await running();
    const first = transport.handler!("elicitation/create", elicitationParams(chat.id));
    await untilAsync(async () => (await app.detail(chat.id)).pendingInteractions.length === 1);
    const view = await app.detail(chat.id);
    expect(view.pendingInteractions[0]!.fields).toEqual([
      { key: "name", title: "Name", description: "", type: "string", required: true },
      { key: "level", title: "Level", description: "", type: "string", required: false, enum: ["a", "b"] },
    ]);
    const id = view.pendingInteractions[0]!.id;
    const accepted = await app.send(socket, { type: "command", id: "a1", op: "interaction", interactionId: id,
      answer: { kind: "elicitation", decision: { action: "accept", content: { name: "Ada", level: "a" } } } });
    expect(accepted).toMatchObject({ ok: true });
    await expect(first).resolves.toEqual({ action: "accept", content: { name: "Ada", level: "a" } });
    // 表单内容不进 Yjs：读面只暴露交互身份与字段窗口。
    expect(JSON.stringify(await app.detail(chat.id))).not.toContain("Ada");

    // 缺必填 / enum 越界的内容不误发 accept，按 decline 结算。
    const second = transport.handler!("elicitation/create", elicitationParams(chat.id));
    await untilAsync(async () => (await app.detail(chat.id)).pendingInteractions.length === 1);
    const secondId = (await app.detail(chat.id)).pendingInteractions[0]!.id;
    expect(await app.send(socket, { type: "command", id: "a2", op: "interaction", interactionId: secondId,
      answer: { kind: "elicitation", decision: { action: "accept", content: { level: "z" } } } })).toMatchObject({ ok: true });
    await expect(second).resolves.toEqual({ action: "decline" });
    await app.send(socket, { type: "command", id: "c1", op: "cancel" });
    await app.drain();
  });

  test("unsupported forms decline without a callback and cannot be answered", async () => {
    const { app, chat, socket, transport } = await running();
    const nested = transport.handler!("elicitation/create", {
      sessionId: chat.id, message: "嵌套表单",
      requestedSchema: { properties: { profile: { type: "object", title: "Profile" } } },
    });
    // 不支持结构默认拒绝：不会出现 pending，也不需要浏览器回答。
    await expect(nested).resolves.toEqual({ action: "decline" });
    await untilAsync(async () => (await app.detail(chat.id)).interactions.length > 0);
    const view = await app.detail(chat.id);
    expect(view.pendingInteractions).toEqual([]);
    expect(view.interactions.at(-1)).toMatchObject({ status: "resolved", outcome: "decline" });
    expect(await app.send(socket, { type: "command", id: "a3", op: "interaction",
      interactionId: view.interactions.at(-1)!.id,
      answer: { kind: "elicitation", decision: { action: "accept", content: {} } } }))
      .toMatchObject({ ok: false, error: { code: "settled" } });
    await app.send(socket, { type: "command", id: "c2", op: "cancel" });
    await app.drain();
  });

  test("unsupported reverse capabilities stay denied", async () => {
    const { app, socket, transport } = await running();
    expect(() => transport.handler!("fs/read_text_file", {})).toThrow("not authorized");
    expect(await app.send(socket, { type: "command", id: "a4", op: "interaction", interactionId: "interaction:none",
      answer: { kind: "permission", decision: "allow_once" } }))
      .toMatchObject({ ok: false, error: { code: "settled" } });
    await app.send(socket, { type: "command", id: "c3", op: "cancel" });
    await app.drain();
  });
});

describe("capability declaration and configuration read face", () => {
  test("declares replay/task capabilities, seeds load configuration and repairs background tasks", async () => {
    const app = runtimeFixture({ chunks: [], blocked: true,
      loadConfig: [{ id: "model", name: "Model", type: "select", currentValue: "sonnet",
        options: [{ value: "sonnet", name: "Sonnet" }] }],
      tasks: [{ task_id: "task-1", kind: "shell", summary: "Checking", status: "completed",
        started_at: "2026-10-10T00:00:00Z" }] });
    const chat = await app.create("Capabilities");
    const { socket } = await app.attached(chat.id);
    const hydrate = app.transports[0]!;
    const initialize = hydrate.calls.find(({ method }) => method === "initialize");
    expect((initialize?.params as { clientCapabilities?: { _meta?: Record<string, unknown> } }).clientCapabilities?._meta)
      .toMatchObject({ "peri.userInputQueue": true, "peri.unstableEvent": true, "peri.replay": true });
    // 配置源进入会话读面（load 响应与通知都来自同一 seedConfig 路径）。
    await untilAsync(async () => ((await app.detail(chat.id)).session as { configOptions?: unknown[] })
      .configOptions?.length === 1);
    expect(await app.send(socket, promptCommand("Hello"))).toMatchObject({ ok: true });
    const transport = await app.runTransport();
    await transport.promptStarted;
    // 后台任务修订缺口触发 session/bg-tasks 快照修复（沿用现有 SDK 投影规则）。
    transport.emitRaw({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: chat.id,
      event: "bg-task-updated", data: { task_id: "task-1", revision: 1, status: "running" } } });
    transport.emitRaw({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: chat.id,
      event: "bg-task-updated", data: { task_id: "task-1", revision: 5, status: "completed" } } });
    await untilAsync(async () => transport.calls.some(({ method }) => method === "session/bg-tasks"));
    await untilAsync(async () => (await app.detail(chat.id)).tasks.some((task) => task.taskId === "task-1"));
    // 读面里只有用户输入与尚未产出的助手条目（turn 已开启），没有反向请求原文。
    expect(detailEntries(await app.detail(chat.id))).toEqual(["Hello", ""]);
    await app.send(socket, { type: "command", id: "c4", op: "cancel" });
    await app.drain();
  });
});
