import { describe, expect, spyOn, test } from "bun:test";
import { ChatProjection } from "../worker/chat/projection";
import { SessionDocReplica } from "../worker/sdk";
import { readSyncState } from "../shared/sync-state";
import { syncStateSchema } from "../shared/sync";
import type { SessionRecord } from "../worker/types";

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

function chunk(projection: ChatProjection, text: string, messageId = "streamed-answer"): void {
  projection.accept({ jsonrpc: "2.0", method: "session/update", params: {
    sessionId: chatId, update: { sessionUpdate: "agent_message_chunk", messageId,
      content: { type: "text", text } },
  } });
}

describe("SDK-backed projection materialization boundaries", () => {
  test("500 historical turns and 200 chunks do not materialize history per chunk", async () => {
    const projection = new ChatProjection();
    const session = record(500);
    await projection.restore(session);
    session.running = true;
    await projection.start("New question");
    const persisted = session.messages;
    let metadataPublications = 0;
    projection.docs.session.getMap("peri-cf").observe(() => { metadataPublications++; });
    const parse = spyOn(syncStateSchema, "parse");
    try {
      for (let index = 0; index < 200; index++) {
        chunk(projection, "x");
        await Promise.resolve();
      }
      expect(parse).toHaveBeenCalledTimes(0);
      expect(metadataPublications).toBe(0);
      expect(session.messages).toBe(persisted);
      const visible = readSyncState(projection.docs.chat, projection.docs.session);
      expect(visible.messages.at(-1)?.content).toBe("x".repeat(200));
      expect(visible.messages.at(-1)?.status).toBe("running");
      expect(visible.messages.at(-1)?.id).toBe(persisted.at(-1)?.id);
      parse.mockClear();
      await projection.flush();
      expect(parse).toHaveBeenCalledTimes(1);
      expect(session.messages.at(-1)?.content).toBe("x".repeat(200));
      expect(session.messages[0]?.id).toBe("user-0");
      expect(session.messages[999]?.id).toBe("assistant-499");
    } finally {
      parse.mockRestore();
    }
  });

  test("read boundaries and terminal errors never return stale text or status", async () => {
    const projection = new ChatProjection();
    const session = record(1);
    await projection.restore(session);
    session.running = true;
    await projection.start("Question");
    chunk(projection, "first");
    await projection.flush();
    const answerId = session.messages.at(-1)?.id;
    expect(session.messages.at(-1)?.content).toBe("first");
    chunk(projection, " second");
    session.running = false;
    session.executionBlocked = true;
    await projection.complete("error", "Provider failure");
    expect(session.messages.at(-1)).toMatchObject({ id: answerId, content: "first second", status: "error", error: "Provider failure" });
    const visible = readSyncState(projection.docs.chat, projection.docs.session);
    expect(visible.running).toBe(false);
    expect(visible.executionBlocked).toBe(true);
    expect(visible.messages).toEqual(session.messages);
    const restored = new ChatProjection();
    await restored.restore(session);
    expect(readSyncState(restored.docs.chat, restored.docs.session).messages).toEqual(session.messages);
  });

  test("cancellation, new assistant entries and command-input deduplication retain SDK identity", async () => {
    const projection = new ChatProjection();
    const session = record();
    await projection.restore(session);
    session.running = true;
    await projection.start("Question");
    projection.accept({ jsonrpc: "2.0", method: "session/update", params: {
      sessionId: chatId, update: { sessionUpdate: "user_message_chunk", content: { type: "text", text: "Question" } },
    } });
    const firstAnswerId = session.messages.at(-1)!.id;
    chunk(projection, "One", "answer-one");
    await Promise.resolve();
    chunk(projection, "Two", "answer-two");
    session.running = false;
    await projection.complete("cancelled");
    expect(session.messages.map((message) => message.content)).toEqual(["Question", "One", "Two"]);
    expect(session.messages.map((message) => message.id).slice(1)).toEqual([firstAnswerId, "answer-two"]);
    expect(session.messages.slice(1).map((message) => message.status)).toEqual(["cancelled", "cancelled"]);
  });

  test("batched chunks reach an SDK replica before transcript persistence", async () => {
    const projection = new ChatProjection();
    const session = record();
    await projection.restore(session);
    await projection.start("Question");
    const persisted = session.messages;
    const replica = new SessionDocReplica();
    const subscription = projection.sync.subscribe((update) => replica.applyUpdate(update));
    replica.applySnapshot(subscription.snapshot);
    try {
      chunk(projection, "first");
      chunk(projection, " second");
      projection.sync.flush();
      await Promise.resolve();
      expect(readSyncState(replica.chat, replica.session).messages.at(-1)?.content).toBe("first second");
      expect(session.messages).toBe(persisted);
      await projection.flush();
      expect(session.messages.at(-1)?.content).toBe("first second");
    } finally {
      subscription.unsubscribe();
      replica.destroy();
    }
  });

  test("same-length compacted histories replace presentation metadata correctly", async () => {
    const projection = new ChatProjection();
    const session = record(1);
    await projection.restore(session);
    projection.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: {
      sessionId: chatId, event_json: JSON.stringify({ type: "compact_completed", value: {
        messages_json: JSON.stringify([
          { id: "replacement-user", role: "user", content: "Compacted question" },
          { id: "replacement-assistant", role: "assistant", content: "Compacted answer" },
        ]),
      } }),
    } });
    await projection.flush();
    expect(session.messages.map((message) => message.id)).toEqual(["replacement-user", "replacement-assistant"]);
    expect(session.messages.map((message) => message.content)).toEqual(["Compacted question", "Compacted answer"]);
  });
});
