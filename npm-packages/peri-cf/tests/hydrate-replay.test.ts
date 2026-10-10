import { describe, expect, test } from "bun:test";
import { replayChunk, replayToolCall, runtimeFixture, type TransportPlan } from "./helpers/runtime";
import type { SyncState } from "../shared/sync";

/**
 * 无 DO 的核心行为：展示态不落盘，冷 isolate 只能靠 `session/load` 重放复原。
 * 这些用例证明「订阅先于 load」「失败不粘住」「观测不唤醒」三条。
 */

function entryText(entry: SyncState["entries"][number] | undefined): string {
  return entry ? entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])).join("") : "";
}

function viewSummary(state: SyncState) {
  return {
    chat: state.chat,
    readiness: state.readiness,
    running: state.running,
    executionBlocked: state.executionBlocked,
    entries: state.entries.map((entry) => ({ id: entry.id, role: entry.role, status: entry.status,
      messageId: entry.messageId, createdAt: entry.createdAt, createdAtSource: entry.createdAtSource, text: entryText(entry) })),
    turns: state.turns.map((turn) => ({ turnId: turn.turnId, status: turn.status, startedAt: turn.startedAt })),
    activeTurn: state.activeTurn,
    toolCalls: state.toolCalls.map((tool) => ({ toolCallId: tool.toolCallId, status: tool.status,
      bytes: tool.resultRef?.bytes, preview: tool.resultRef?.preview })),
  };
}

const chatId = "00000000-0000-4000-8000-000000000001";

/** 一段带工具结果的历史：用户消息 + 助手回答 + 大载荷工具调用（重放顺序即 ACP 顺序）。 */
const history = [
  replayChunk(chatId, "user-1", "第一问", { role: "user", replay: true }),
  replayChunk(chatId, "assistant-1", "第一答", { role: "assistant", replay: true }),
  replayChunk(chatId, "user-2", "第二问", { role: "user", replay: true }),
  replayToolCall(chatId, "tool-1", "P".repeat(6000)),
  replayChunk(chatId, "assistant-2", "第二答", { role: "assistant", replay: true }),
];

describe("cold runtime replay hydration", () => {
  test("重放复原出与重放前一致的展示 DTO（仅载荷引用 id 随会话代次更新）", async () => {
    const plan: TransportPlan = { replay: history };
    // limit=1：第二个会话的复原会按 LRU 回收第一个会话的运行期，模拟 isolate 内冷启动。
    const app = runtimeFixture(plan, { limit: 1 });
    const first = await app.create("First");
    const warm = await app.detail(first.id);
    expect(viewSummary(warm).entries.map((entry) => entry.text))
      .toEqual(["第一问", "第一答", "第二问", "第二答"]);
    // 重放的历史全部是终态：不得停留在 streaming，也不得用当前时钟冒充源时间。
    expect(warm.entries.map((entry) => entry.status)).toEqual(["completed", "completed", "completed", "completed"]);
    expect(warm.entries.map((entry) => entry.createdAt)).toEqual([null, null, null, null]);
    expect(warm.entries.map((entry) => entry.createdAtSource)).toEqual(["unknown", "unknown", "unknown", "unknown"]);
    expect(warm.turns.map((turn) => ({ status: turn.status, startedAt: turn.startedAt })))
      .toEqual([{ status: "completed", startedAt: null }, { status: "completed", startedAt: null }]);
    expect(warm.activeTurn).toEqual({ turnId: "turn:user-2", status: "completed" });
    expect(warm.readiness).toBe("ready");
    const warmRef = warm.toolCalls[0]!.resultRef!;
    expect(warmRef.bytes).toBeGreaterThan(4096);

    const second = await app.create("Second");
    await app.detail(second.id);
    expect(app.registry.peek(first.id)).toBeUndefined();
    // 冷运行期：没有任何持久展示材料，只能由重放重建。
    const cold = await app.detail(first.id);
    expect(viewSummary(cold)).toEqual(viewSummary(warm));
    expect(app.transports).toHaveLength(3);
    // 载荷引用按会话代次重新发号（SDK 的 ToolPayloads generation 每次复原都变）：
    // 客户端必须用最新快照里的引用，旧引用按 unavailable 处理。
    const coldRef = cold.toolCalls[0]!.resultRef!;
    expect(coldRef.id).not.toBe(warmRef.id);
    expect(coldRef.bytes).toBe(warmRef.bytes);
    expect(coldRef.preview).toBe(warmRef.preview);
  });

  test("订阅必须先于 session/load：重放通知在 load 响应前到达也全部进入投影", async () => {
    const plan: TransportPlan = { replay: history };
    const app = runtimeFixture(plan);
    const chat = await app.create("Replay order");
    await app.detail(chat.id);
    // 加载那一刻必须有订阅者，否则 ACP 的重放（响应前发送）会被整段丢弃。
    expect(app.transports[0]!.listenersAtLoad).toBe(1);
    expect((await app.detail(chat.id)).entries).toHaveLength(4);
  });

  test("并发触发共享同一次复原：只启动一个宿主，重放不叠加", async () => {
    const app = runtimeFixture({ replay: history });
    const chat = await app.create("Concurrent");
    const [detail, attached] = await Promise.all([app.detail(chat.id), app.attached(chat.id)]);
    expect(app.transports).toHaveLength(1);
    expect(detail.entries).toHaveLength(4);
    expect(attached.socket.state().entries).toHaveLength(4);
    expect(attached.socket.state().entries.map(entryText)).toEqual(["第一问", "第一答", "第二问", "第二答"]);
  });

  test("复原失败不粘住：重试丢弃上次残留后重放一次，内容不重复", async () => {
    const plan: TransportPlan = { replay: history, loadFailure: new Error("host unavailable") };
    const app = runtimeFixture(plan);
    const chat = await app.create("Retry");
    const failed = await app.request(`/api/v2/chats/${chat.id}`);
    expect(failed.status).toBe(500);
    // 上次尝试已经吃到部分重放：重试必须从空文档重建。
    plan.loadFailure = undefined;
    const recovered = await app.detail(chat.id);
    expect(recovered.entries.map(entryText)).toEqual(["第一问", "第一答", "第二问", "第二答"]);
    expect(recovered.entries.map((entry) => entry.id)).toEqual(["turn:user-1:user", "turn:user-1:assistant",
      "turn:user-2:user", "turn:user-2:assistant"]);
  });

  test("仓库里没有的会话在启动宿主前就以 404 结束", async () => {
    const app = runtimeFixture({ replay: history });
    const response = await app.request(`/api/v2/chats/${chatId}`);
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({ error: "Chat not found" });
    expect(app.transports).toEqual([]);
  });

  test("重放后宿主关闭未确认：展示态仍可读，但本实例阻断后续执行", async () => {
    const app = runtimeFixture({ replay: history, closeFailure: new Error("close unconfirmed") });
    const chat = await app.create("Close failure");
    const detail = await app.detail(chat.id);
    expect(detail.entries).toHaveLength(4);
    expect(detail.executionBlocked).toBe(true);
    const { socket } = await app.attached(chat.id);
    const result = await app.send(socket, { type: "command", id: "p1", op: "prompt", content: "Hello" });
    expect(result).toEqual({ type: "command_result", id: "p1", ok: false,
      error: { code: "blocked", message: "Previous ACP host did not close; this instance cannot run again" } });
  });

  test("只读观测不唤醒会话：peek 不创建运行期也不启动宿主", async () => {
    const app = runtimeFixture({ replay: history });
    const chat = await app.create("Observation");
    const first = await app.resources(chat.id);
    expect(first).toEqual({ sessionId: chat.id, running: false, executionBlocked: false, instance: null });
    expect(app.registry.peek(chat.id)).toBeUndefined();
    expect(app.transports).toEqual([]);
  });
});
