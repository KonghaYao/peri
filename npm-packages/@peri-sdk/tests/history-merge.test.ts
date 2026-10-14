import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { AWAITING_RESULT_STATUS } from "../src/sdk/index";
import { readSessionView } from "../src/view/session-view";
import type { JsonRpcNotification } from "../src/transport/types";

// 形状来自 peri-acp/src/host/history_page.rs 的模块文档（响应 {sessionId, snapshotToken,
// nextCursor, items[{entryId, position, oversize, subItems[{position, update}]}]}）；
// 子项的 `update` 与回放路径同一份投影。字段名照 wire 原样，不在测试里改名。

const TOKEN = "1:524288:range-token";

function update(sessionId: string, sessionUpdate: string, fields: Record<string, unknown>): JsonRpcNotification {
  return { jsonrpc: "2.0", method: "session/update", params: { sessionId, update: { sessionUpdate, ...fields } } };
}

/** 回放路径的历史投影通知：带条目身份 / 位置戳。 */
function replay(
  sessionId: string,
  entryId: string,
  fields: Record<string, unknown>,
  position?: number,
): JsonRpcNotification {
  return update(sessionId, fields.sessionUpdate as string, {
    ...fields,
    _meta: {
      periReplay: true, periLoadGeneration: "gen-1", periEntryId: entryId,
      ...(position === undefined ? {} : { periPosition: position }),
    },
  });
}

type SubItem = { position: number; update: Record<string, unknown> };
function item(entryId: string, position: number, subItems: SubItem[], oversize = false) {
  return { entryId, position, oversize, subItems };
}
function text(sessionUpdate: string, value: string, messageId?: string): Record<string, unknown> {
  return { sessionUpdate, content: { type: "text", text: value }, ...(messageId ? { messageId } : {}) };
}

function historyPage(items: ReturnType<typeof item>[], nextCursor: string | null = null) {
  return { sessionId: "s1", snapshotToken: TOKEN, nextCursor, items };
}

function entryIds(docs: SessionDocs): string[] {
  const root = docs.chat.getMap<unknown>("root");
  const order = root.get("entryOrder") as Y.Array<string>;
  const entries = root.get("entries") as Y.Map<Y.Map<unknown>>;
  return order.toArray().map((id) => String(entries.get(id)?.get("entryId")));
}

function texts(docs: SessionDocs): string[] {
  return docs.chat.getMap<unknown>("root").get("entryOrder")
    ? readSessionView(docs.chat, docs.session).entries.flatMap((entry) =>
        entry.blocks.flatMap((block) => block.type === "text" || block.type === "reasoning" ? [block.text] : []))
    : [];
}

function withWindow(): SessionDocs {
  const docs = new SessionDocs();
  docs.seedHistoryWindow({ snapshotToken: TOKEN, nextCursor: "cursor-1" });
  return docs;
}

describe("history projection merge", () => {
  test("a sparse first screen keeps the inherited prefix in front of the older own page (A20)", () => {
    const docs = withWindow();
    // 首屏：[继承 I（本线程没有行，无位置）, own 摘要 S]
    docs.accept(replay("s1", "s1:inherited", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "I" } }, undefined));
    docs.accept(replay("s1", "s1:summary", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "S" } }, 20));
    expect(entryIds(docs)).toEqual(["s1:inherited", "s1:summary"]);

    const merged = docs.mergeHistoryPage("s1", historyPage([
      item("s1:old-own", 10, [{ position: 0, update: text("agent_message_chunk", "H") }]),
    ]));

    expect(merged.status).toBe("merged");
    expect(merged.applied).toBe(1);
    expect(entryIds(docs)).toEqual(["s1:inherited", "s1:old-own", "s1:summary"]);
    expect(texts(docs)).toEqual(["I", "H", "S"]);
    docs.destroy();
  });

  test("a page fills the gap between first-screen entries and stays in ascending order", () => {
    const docs = withWindow();
    docs.accept(replay("s1", "s1:summary", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "S" } }, 30));
    docs.accept(replay("s1", "s1:recent", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "R" } }, 60));

    docs.mergeHistoryPage("s1", historyPage([
      item("s1:mid-a", 40, [{ position: 0, update: text("agent_message_chunk", "40") }]),
      item("s1:mid-b", 50, [{ position: 0, update: text("agent_message_chunk", "50") }]),
    ]));

    expect(entryIds(docs)).toEqual(["s1:summary", "s1:mid-a", "s1:mid-b", "s1:recent"]);
    docs.destroy();
  });

  test("a whole older page is inserted before the current earliest entry, still ascending", () => {
    const docs = withWindow();
    docs.accept(replay("s1", "s1:summary", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "S" } }, 50));
    docs.accept(replay("s1", "s1:recent", { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "R" } }, 60));

    docs.mergeHistoryPage("s1", historyPage([
      item("s1:older-a", 10, [{ position: 0, update: text("agent_message_chunk", "10") }]),
      item("s1:older-b", 20, [{ position: 0, update: text("agent_message_chunk", "20") }]),
    ]));

    expect(entryIds(docs)).toEqual(["s1:older-a", "s1:older-b", "s1:summary", "s1:recent"]);
    expect(texts(docs)).toEqual(["10", "20", "S", "R"]);
    docs.destroy();
  });

  test("repeated and overlapping pages merge idempotently", () => {
    const docs = withWindow();
    const page = historyPage([
      item("s1:a", 10, [{ position: 0, update: text("agent_message_chunk", "A") }]),
      item("s1:b", 20, [{ position: 0, update: text("agent_message_chunk", "B") }]),
    ]);
    docs.mergeHistoryPage("s1", page);
    docs.mergeHistoryPage("s1", page);
    docs.mergeHistoryPage("s1", historyPage([
      item("s1:b", 20, [{ position: 0, update: text("agent_message_chunk", "B") }]),
      item("s1:c", 30, [{ position: 0, update: text("agent_message_chunk", "C") }]),
    ]));

    expect(entryIds(docs)).toEqual(["s1:a", "s1:b", "s1:c"]);
    expect(texts(docs)).toEqual(["A", "B", "C"]);
    docs.destroy();
  });

  test("sub items of one entry keep their own identity when they share a messageId (A23)", () => {
    const docs = withWindow();
    const messageId = "00000000-0000-4000-8000-000000000001";
    const entry = item("s1:multi", 10, [
      { position: 0, update: text("agent_thought_chunk", "thinking", messageId) },
      { position: 1, update: text("agent_message_chunk", "answer", messageId) },
    ]);
    docs.mergeHistoryPage("s1", historyPage([entry]));
    const view = readSessionView(docs.chat, docs.session);
    expect(view.entries[0]!.blocks.map((block) => block.type)).toEqual(["reasoning", "text"]);
    expect(view.entries[0]!.blocks.map((block) => block.type === "tool_call" ? "" : block.text)).toEqual(["thinking", "answer"]);

    // 重试同一页：两个 block 都不得被对方覆盖或重复。
    docs.mergeHistoryPage("s1", historyPage([entry]));
    const retried = readSessionView(docs.chat, docs.session);
    expect(retried.entries).toHaveLength(1);
    expect(retried.entries[0]!.blocks.map((block) => block.type)).toEqual(["reasoning", "text"]);
    expect(texts(docs)).toEqual(["thinking", "answer"]);
    docs.destroy();
  });

  test("assistant and tool entries render without a Human prefix or an active turn (A23)", () => {
    const docs = withWindow();
    docs.seedInputQueue({ generation: "g1", revision: 1, items: [{ inputId: "live", state: "delivered" }] });
    docs.mergeHistoryPage("s1", historyPage([
      item("s1:summary", 10, [{ position: 0, update: text("agent_message_chunk", "compacted summary") }]),
      item("s1:tool", 20, [{
        position: 0,
        update: { sessionUpdate: "tool_call", toolCallId: "call-1", title: "Read", status: "in_progress", kind: "read" },
      }]),
    ]));

    const view = readSessionView(docs.chat, docs.session);
    expect(view.entries.map((entry) => entry.role)).toEqual(["assistant", "assistant"]);
    expect(view.activeTurnId).toBeNull();
    expect(view.activeTurnStatus).toBeNull();
    expect(view.entries[0]!.blocks[0]).toMatchObject({ type: "text", text: "compacted summary" });
    expect(view.entries[1]!.blocks[0]).toMatchObject({ type: "tool_call", tool: { name: "Read", kind: "read" } });
    // 旧历史不重新启动工具动画，也不改输入队列。
    expect(view.entries[1]!.blocks[0]!.type === "tool_call" && view.entries[1]!.blocks[0]!.tool?.status).toBe("awaiting_result");
    expect(view.inputQueue?.items).toEqual([{ inputId: "live", state: "delivered" }]);
    docs.destroy();
  });

  test("a tool result that arrives in a later page completes the earlier call without faking failure (A10)", () => {
    const docs = withWindow();
    docs.mergeHistoryPage("s1", historyPage([
      item("s1:call", 10, [{
        position: 0,
        update: { sessionUpdate: "tool_call", toolCallId: "call-9", title: "Bash", status: "in_progress" },
      }]),
    ]));
    expect(readSessionView(docs.chat, docs.session).entries[0]!.blocks[0])
      .toMatchObject({ tool: { status: "awaiting_result" } });

    docs.mergeHistoryPage("s1", historyPage([
      item("s1:result", 20, [{ position: 0, update: {
        sessionUpdate: "tool_call_update", toolCallId: "call-9", status: "completed",
        content: [{ type: "text", text: "tests passed" }],
      } }]),
    ]));
    expect(readSessionView(docs.chat, docs.session).entries[0]!.blocks[0])
      .toMatchObject({ tool: { status: "completed", result: { contentText: "tests passed" } } });
    docs.destroy();
  });

  test("the same sub item merged from a page and from a replay keeps one block", () => {
    const docs = withWindow();
    const messageId = "00000000-0000-4000-8000-00000000000a";
    const subItem = { position: 0, update: text("agent_message_chunk", "Answer", messageId) };

    // 页路径先落库（带子项下标），随后回放路径带着同一条目身份到达（没有子项下标）。
    docs.mergeHistoryPage("s1", historyPage([item("s1:m1", 10, [subItem])]));
    expect(texts(docs)).toEqual(["Answer"]);
    docs.accept(replay("s1", "s1:m1", subItem.update, 10));

    // 同一条目身份的两条路径必须收敛到同一个块，不能各建一个。
    expect(texts(docs)).toEqual(["Answer"]);
    expect(readSessionView(docs.chat, docs.session).entries[0]!.blocks).toHaveLength(1);
    docs.destroy();
  });

  test("a page adopts the block a replay already created for the same sub item", () => {
    const docs = withWindow();
    const messageId = "00000000-0000-4000-8000-00000000000b";
    const update = text("agent_message_chunk", "Answer", messageId);

    docs.accept(replay("s1", "s1:m1", update, 10));
    docs.mergeHistoryPage("s1", historyPage([item("s1:m1", 10, [{ position: 0, update }])]));

    expect(texts(docs)).toEqual(["Answer"]);
    expect(readSessionView(docs.chat, docs.session).entries[0]!.blocks).toHaveLength(1);
    docs.destroy();
  });

  test("a page flags an oversized entry, and the marker is readable from the view (REQ §5.2)", () => {
    const docs = withWindow();
    docs.mergeHistoryPage("s1", historyPage([
      item("s1:big", 10, [{ position: 0, update: text("agent_message_chunk", "huge") }], true),
      item("s1:small", 20, [{ position: 0, update: text("agent_message_chunk", "small") }]),
    ]));

    const view = readSessionView(docs.chat, docs.session);
    expect(view.entries.map((entry) => entry.historyOversize)).toEqual([true, undefined]);
    docs.destroy();
  });

  test("the awaiting-result tool status is consumable as a constant (REQ-C01-6)", () => {
    const docs = withWindow();
    docs.mergeHistoryPage("s1", historyPage([
      item("s1:call", 10, [{
        position: 0,
        update: { sessionUpdate: "tool_call", toolCallId: "call-awaiting", title: "Read", status: "in_progress" },
      }]),
    ]));

    expect(AWAITING_RESULT_STATUS).toBe("awaiting_result");
    const view = readSessionView(docs.chat, docs.session);
    expect(view.entries[0]!.blocks[0]).toMatchObject({ tool: { status: AWAITING_RESULT_STATUS } });
    docs.destroy();
  });

  test("history pages never advance the live plan, tasks, queue, or current turn (A24)", () => {
    const docs = new SessionDocs();
    docs.seedHistoryWindow({ snapshotToken: TOKEN, nextCursor: "cursor-1" });
    docs.accept(update("s1", "user_message_chunk", { messageId: "live-user", content: { type: "text", text: "current work" } }));
    docs.accept(update("s1", "plan", { entries: [{ content: "Current todo", status: "in_progress" }] }));
    docs.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: "s1", event: "bg-task-started", data: {
      task_id: "bg-1", kind: "shell", summary: "Build", started_at: "2026-10-13T00:00:00Z", revision: 1,
    } } });
    const info = docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
    const liveTurn = info.get("activeTurnId");
    const planBefore = JSON.stringify(info.get("plan"));
    const queueBefore = JSON.stringify(info.get("inputQueue") ?? null);

    docs.mergeHistoryPage("s1", historyPage([
      item("s1:old-todo", 5, [{
        position: 0,
        update: { sessionUpdate: "tool_call", toolCallId: "call-todo", title: "TodoWrite", status: "in_progress",
          rawInput: { todos: [{ content: "Old todo", status: "pending" }] } },
      }]),
      item("s1:old-answer", 6, [{ position: 0, update: text("agent_message_chunk", "older answer") }]),
    ]));

    // 历史卡片完整展示并排在实时尾部之前，但当前 Todo／任务／队列／实时 turn 一个都不动。
    expect(entryIds(docs)).toEqual(["s1:old-todo", "s1:old-answer", "turn:live-user:user", "turn:live-user:assistant"]);
    expect(JSON.stringify(info.get("plan"))).toBe(planBefore);
    expect(JSON.stringify(info.get("inputQueue") ?? null)).toBe(queueBefore);
    expect(info.get("activeTurnId")).toBe(liveTurn);
    expect(readSessionView(docs.chat, docs.session).tasks.map((task) => task.taskId)).toEqual(["bg-1"]);
    docs.destroy();
  });
});
