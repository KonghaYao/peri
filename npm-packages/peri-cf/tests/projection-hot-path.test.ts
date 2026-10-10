import { describe, expect, test } from "bun:test";
import { ChatProjection } from "../worker/chat/projection";
import { checkpointFromLegacyRecord } from "../worker/chat/record";
import { SUPPORTED_SCHEMA_VERSIONS, SessionDocReplica } from "../worker/sdk";
import { readSyncState } from "../shared/sync-state";
import type { SessionRecord } from "../worker/types";
import type { SyncState } from "../shared/sync";

function entryText(entry: { blocks: Array<{ type: string; text?: string }> } | undefined): string {
  return entry ? entry.blocks.flatMap((block) => (block.type === "text" ? [block.text ?? ""] : [])).join("") : "";
}

const createdAt = "2026-10-07T00:00:00.000Z";
const chatId = "8d68cd01-33d3-4df4-9779-bd3031a09184";

function record(turns = 0): SessionRecord {
  return {
    chat: { id: chatId, title: "Projection fixture", updatedAt: createdAt },
    messages: Array.from({ length: turns }, (_, index) => [
      { id: `user-${index}`, role: "user" as const, content: `Question ${index}`, createdAt, status: "completed" as const },
      { id: `assistant-${index}`, role: "assistant" as const, content: `Answer ${index}`, createdAt, status: "completed" as const },
    ]).flat(),
  };
}

/** v1 记录 → v2 checkpoint 导入（与 DO 转换同一路径）。 */
function restore(projection: ChatProjection, session: SessionRecord): void {
  projection.restoreCheckpoint(checkpointFromLegacyRecord(session, { ...SUPPORTED_SCHEMA_VERSIONS }));
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
    await projection.setRunState({ chat: record().chat, running: true, executionBlocked: false });
    restore(projection, record(500));
    await projection.start("New question");
    const indexBefore = JSON.stringify(projection.checkpoint()).length;
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
    expect(visible.entries.length).toBe(1002);
    await projection.flush();
    expect(entryText(projection.state().entries.at(-1))).toBe("x".repeat(200));
    // 索引随流式增长但保持在接纳上限内（P3 预算：1 MiB）。
    expect(JSON.stringify(projection.checkpoint()).length).toBeGreaterThan(indexBefore);
    expect(JSON.stringify(projection.checkpoint()).length).toBeLessThan(1_048_576);
    expect(projection.checkpoint().entries[0]?.id).toBe("user-0");
    expect(projection.checkpoint().entries.some((entry) => entry.id === "assistant-499")).toBe(true);
    expect(projection.checkpoint().entries.at(-1)?.id.startsWith("turn:")).toBe(true);
  });

  test("read boundaries and terminal errors never return stale text or status", async () => {
    const projection = new ChatProjection();
    await projection.setRunState({ chat: record().chat, running: true, executionBlocked: false });
    restore(projection, record(1));
    await projection.start("Question");
    chunk(projection, "first");
    await projection.flush();
    expect(entryText(projection.state().entries.at(-1))).toBe("first");
    chunk(projection, " second");
    await projection.setRunState({ chat: record().chat, running: false, executionBlocked: true });
    await projection.complete("error", "Provider failure");
    const visible = projection.state();
    expect(visible.running).toBe(false);
    expect(visible.executionBlocked).toBe(true);
    expect(visible.error).toBe("Provider failure");
    expect(visible.entries.map(entryText)).toEqual(["Question 0", "Answer 0", "Question", "first second"]);
    expect(visible.entries.at(-1)).toMatchObject({ status: "error", error: { code: "execution_failed" } });
    // 从同一 checkpoint 再导入：文本与状态一致（转换/导入可重复）。
    const restored = new ChatProjection();
    restored.restoreCheckpoint(projection.checkpoint());
    const replayed = restored.state();
    expect(replayed.entries.map(entryText)).toEqual(visible.entries.map(entryText));
    expect(replayed.entries.map((entry) => entry.status)).toEqual(visible.entries.map((entry) => entry.status));
  });

  test("cancellation, new assistant entries and command-input deduplication retain SDK identity", async () => {
    const projection = new ChatProjection();
    await projection.setRunState({ chat: record().chat, running: true, executionBlocked: false });
    restore(projection, record());
    await projection.start("Question");
    projection.accept({ jsonrpc: "2.0", method: "session/update", params: {
      sessionId: chatId, update: { sessionUpdate: "user_message_chunk", content: { type: "text", text: "Question" } },
    } });
    const firstAnswerId = projection.checkpoint().entries.at(-1)!.id;
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

  test("batched chunks reach an SDK replica before the checkpoint is exported", async () => {
    const projection = new ChatProjection();
    await projection.setRunState({ chat: record().chat, running: true, executionBlocked: false });
    restore(projection, record());
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
      expect(entryText(projection.checkpoint().entries.at(-1))).toBe("first second");
    } finally {
      subscription.unsubscribe();
      replica.destroy();
    }
  });

  test("same-length compacted histories replace history with SDK-owned identities", async () => {
    const projection = new ChatProjection();
    await projection.setRunState({ chat: record().chat, running: false, executionBlocked: false });
    restore(projection, record(1));
    projection.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: {
      sessionId: chatId, event_json: JSON.stringify({ type: "compact_completed", value: {
        messages_json: JSON.stringify([
          { id: "replacement-user", role: "user", content: "Compacted question" },
          { id: "replacement-assistant", role: "assistant", content: "Compacted answer" },
        ]),
      } }),
    } });
    await projection.flush();
    const entries = projection.checkpoint().entries;
    expect(entries.map((entry) => entry.id)).toEqual(["message:replacement-user", "message:replacement-assistant"]);
    expect(entries.map((entry) => entryText(entry))).toEqual(["Compacted question", "Compacted answer"]);
  });
});
