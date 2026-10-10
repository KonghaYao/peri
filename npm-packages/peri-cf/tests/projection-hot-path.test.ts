import { describe, expect, test } from "bun:test";
import { ChatProjection } from "../worker/chat/projection";
import { SessionDocReplica, type JsonRpcNotification } from "../worker/sdk";
import { replayChunk } from "./helpers/runtime";
import { readSyncState } from "../shared/sync-state";
import type { SyncState } from "../shared/sync";

/**
 * 投影热路径边界：历史现在只能由 ACP 重放喂入（不再有 checkpoint 导入/导出）。
 * 这些用例守住「流式期间不重写元数据」「读取时才物化」「身份来自 SDK Entry」三条。
 */

const createdAt = "2026-10-07T00:00:00.000Z";
const chatId = "8d68cd01-33d3-4df4-9779-bd3031a09184";
const chat = { id: chatId, title: "Projection fixture", updatedAt: createdAt };

function entryText(entry: SyncState["entries"][number] | undefined): string {
  return entry ? entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])).join("") : "";
}

/** 重放 N 轮历史（每轮用户消息 + 助手回答），与 ACP 的 `session/load` 重放同形。 */
function replayed(projection: ChatProjection, turns: number): void {
  for (let index = 0; index < turns; index++) {
    projection.accept(replayChunk(chatId, `user-${index}`, `Question ${index}`, { role: "user", replay: true }));
    projection.accept(replayChunk(chatId, `assistant-${index}`, `Answer ${index}`, { role: "assistant", replay: true }));
  }
}

function chunk(projection: ChatProjection, text: string, messageId = "streamed-answer"): void {
  projection.accept({ jsonrpc: "2.0", method: "session/update", params: {
    sessionId: chatId, update: { sessionUpdate: "agent_message_chunk", messageId,
      content: { type: "text", text } },
  } });
}

describe("SDK-backed projection materialization boundaries", () => {
  test("500 historical turns and 200 chunks do not materialize history per chunk", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    replayed(projection, 500);
    projection.closeReplayedTurn();
    await projection.flush();
    const historical = projection.state();
    expect(historical.entries).toHaveLength(1000);
    await projection.start("New question");
    let metadataPublications = 0;
    projection.docs.session.getMap("peri-cf").observe(() => { metadataPublications++; });
    for (let index = 0; index < 200; index++) {
      chunk(projection, "x");
      await Promise.resolve();
    }
    // 流式期间不重写元数据（Yjs 事务保持最小）；读取时才物化完整状态。
    expect(metadataPublications).toBe(0);
    const visible = projection.state();
    expect(entryText(visible.entries.at(-1))).toBe("x".repeat(200));
    expect(visible.entries.at(-1)?.status).toBe("streaming");
    expect(visible.entries).toHaveLength(1002);
    await projection.flush();
    expect(entryText(projection.state().entries.at(-1))).toBe("x".repeat(200));
    // 历史身份来自 SDK Entry：重放的 messageId 决定条目身份，不被流式内容改写。
    expect(visible.entries[0]?.id).toBe("turn:user-0:user");
    expect(visible.entries.map((entry) => entry.messageId)).toContain("assistant-499");
    expect(visible.entries.at(-1)?.id.startsWith("turn:")).toBe(true);
  });

  test("read boundaries and terminal errors never return stale text or status", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    replayed(projection, 1);
    projection.closeReplayedTurn();
    await projection.start("Question");
    chunk(projection, "first");
    await projection.flush();
    expect(entryText(projection.state().entries.at(-1))).toBe("first");
    chunk(projection, " second");
    projection.setRunState({ chat, running: false, executionBlocked: true });
    await projection.complete("error", "Provider failure");
    const visible = projection.state();
    expect(visible.running).toBe(false);
    expect(visible.executionBlocked).toBe(true);
    expect(visible.error).toBe("Provider failure");
    expect(visible.entries.map(entryText)).toEqual(["Question 0", "Answer 0", "Question", "first second"]);
    expect(visible.entries.at(-1)).toMatchObject({ status: "error", error: { code: "execution_failed" } });
  });

  test("cancellation, new assistant entries and command-input deduplication retain SDK identity", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    await projection.start("Question");
    projection.accept({ jsonrpc: "2.0", method: "session/update", params: {
      sessionId: chatId, update: { sessionUpdate: "user_message_chunk", content: { type: "text", text: "Question" } },
    } });
    // 已投递的用户输入只投影一次：回显被 commandInputDelivered 去重。
    const echoed = projection.state().entries;
    expect(echoed.filter((entry) => entry.role === "user").map(entryText)).toEqual(["Question"]);
    expect(echoed).toHaveLength(2);
    const firstAnswerId = projection.state().entries.at(-1)!.id;
    chunk(projection, "One", "answer-one");
    await Promise.resolve();
    chunk(projection, "Two", "answer-two");
    await projection.complete("cancelled");
    const visible = projection.state();
    expect(visible.entries.map(entryText)).toEqual(["Question", "One", "Two"]);
    // 新内容身份来自 SDK Entry（稳定 id），不再由应用按 messageId 生成。
    expect(visible.entries.map((entry) => entry.id)).toEqual(visible.entries.map((entry) => entry.entryId));
    expect(visible.entries[1]!.id).toBe(firstAnswerId);
    expect(visible.entries.at(-1)!.messageId).toBe("answer-two");
    expect(visible.entries.at(-1)!.id).not.toBe(visible.entries[1]!.id);
    // 已批准契约（对象设计 4.4.3）：取消只收敛未终态 Entry，前序已完成消息保持 completed。
    expect(visible.entries.slice(1).map((entry) => entry.status)).toEqual(["completed", "cancelled"]);
  });

  test("batched chunks reach an SDK replica with the current text", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    await projection.start("Question");
    const replica = new SessionDocReplica();
    const subscription = projection.sync.subscribe((update) => replica.applyUpdate(update));
    replica.applySnapshot(subscription.snapshot);
    try {
      chunk(projection, "first");
      chunk(projection, " second");
      projection.sync.flush();
      await Promise.resolve();
      expect(entryText(readSyncState(replica.chat, replica.session).entries.at(-1))).toBe("first second");
      await projection.flush();
      expect(entryText(projection.state().entries.at(-1))).toBe("first second");
    } finally {
      subscription.unsubscribe();
      replica.destroy();
    }
  });

  test("same-length compacted histories replace history with SDK-owned identities", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: false, executionBlocked: false });
    replayed(projection, 1);
    projection.closeReplayedTurn();
    await projection.flush();
    projection.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: {
      sessionId: chatId, event_json: JSON.stringify({ type: "compact_completed", value: {
        messages_json: JSON.stringify([
          { id: "replacement-user", role: "user", content: "Compacted question" },
          { id: "replacement-assistant", role: "assistant", content: "Compacted answer" },
        ]),
      } }),
    } });
    await projection.flush();
    const entries = projection.state().entries;
    expect(entries.map((entry) => entry.id)).toEqual(["message:replacement-user", "message:replacement-assistant"]);
    expect(entries.map(entryText)).toEqual(["Compacted question", "Compacted answer"]);
  });

  test("reset 丢弃全部展示材料：重放复原重试不得叠加", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: false, executionBlocked: false });
    replayed(projection, 2);
    projection.closeReplayedTurn();
    await projection.flush();
    expect(projection.state().entries).toHaveLength(4);
    projection.reset();
    expect(projection.state().entries).toEqual([]);
    replayed(projection, 2);
    projection.closeReplayedTurn();
    await projection.flush();
    expect(projection.state().entries).toHaveLength(4);
  });

  test("重放内容进入投影前不需要任何持久材料", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    const notifications: JsonRpcNotification[] = [
      replayChunk(chatId, "user-1", "恢复的问题", { role: "user", replay: true }),
      replayChunk(chatId, "assistant-1", "恢复的回答", { role: "assistant", replay: true }),
    ];
    for (const notification of notifications) projection.accept(notification);
    projection.closeReplayedTurn();
    await projection.flush();
    const visible = projection.state();
    expect(visible.entries.map(entryText)).toEqual(["恢复的问题", "恢复的回答"]);
    expect(visible.entries.map((entry) => entry.status)).toEqual(["completed", "completed"]);
    expect(visible.turns.map((turn) => turn.status)).toEqual(["completed"]);
  });
});
