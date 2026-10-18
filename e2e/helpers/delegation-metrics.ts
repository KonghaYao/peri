/**
 * 自主委派评测的确定性指标读取。
 *
 * 只读隔离 HOME 下的 `~/.peri/threads/threads.db`：父会话与子会话的关系来自
 * `threads.parent_thread_id`，工具调用来自持久化消息（`tool_calls`，或兼容
 * provider content blocks 中的 `tool_use`）。指标只用于评测，不改变运行时。
 */
import { existsSync } from "node:fs";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";

export interface DelegationToolCall {
  threadId: string;
  owner: "parent" | "child";
  name: string;
  arguments: Record<string, unknown>;
}

export interface DelegationMetrics {
  parentThreadId?: string;
  childThreadIds: string[];
  agentCalls: number;
  effectiveDelegation: boolean;
  parentToolCalls: number;
  childToolCalls: number;
  /** 首次 Agent 调用之后，主 Agent 的写工具调用数（上下文指标，不直接判失败）。 */
  parentMutationCallsAfterDelegation: number;
  /** 主 Agent 重做子 Agent 已写文件的数量（Write/Edit 的 file_path 交集）。 */
  duplicateLabor: number;
  /** 父 + 子会话的 assistant 消息数；无 trace 时的模型调用代理指标。 */
  modelCalls: number;
  /** 仅 goal 模式持久化到 `thread_goals.tokens_used`；缺失为 null。 */
  tokensUsed: number | null;
}

interface ThreadRow {
  id: string;
  parent_thread_id: string | null;
  updated_at: string;
}

interface MessageRow {
  thread_id: string;
  role: string;
  content: string;
}

interface RawToolCall {
  name?: unknown;
  arguments?: unknown;
  input?: unknown;
}

function openDatabase(home: string): DatabaseSync | undefined {
  const file = path.join(home, ".peri", "threads", "threads.db");
  if (!existsSync(file)) return undefined;
  return new DatabaseSync(file, { readOnly: true });
}

/** 从持久化 envelope（或 legacy 裸 BaseMessage）提取 assistant tool calls。 */
function toolCallsFromContent(raw: string): RawToolCall[] {
  let payload: any;
  try {
    payload = JSON.parse(raw);
  } catch {
    return [];
  }
  const message = payload?.type === "message" ? payload.message : payload;
  if (!message || message.role !== "assistant") return [];
  if (Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
    return message.tool_calls;
  }
  if (Array.isArray(message.content)) {
    return message.content
      .filter((block: any) => block?.type === "tool_use" && typeof block.name === "string")
      .map((block: any) => ({ name: block.name, arguments: block.input }));
  }
  return [];
}

function asArguments(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function writeTarget(call: DelegationToolCall): string | undefined {
  if (call.name !== "Write" && call.name !== "Edit") return undefined;
  const target = call.arguments.file_path ?? call.arguments.path;
  return typeof target === "string" ? target : undefined;
}

function collectToolCalls(
  database: DatabaseSync,
  threads: ThreadRow[],
  parentId: string,
): DelegationToolCall[] {
  if (threads.length === 0) return [];
  const ids = threads.map((thread) => thread.id);
  const placeholders = ids.map(() => "?").join(", ");
  const rows = database
    .prepare(
      `SELECT thread_id, role, content FROM messages WHERE thread_id IN (${placeholders}) ORDER BY rowid`,
    )
    .all(...ids) as unknown as MessageRow[];

  const calls: DelegationToolCall[] = [];
  for (const row of rows) {
    if (row.role !== "assistant") continue;
    for (const raw of toolCallsFromContent(row.content)) {
      if (typeof raw.name !== "string") continue;
      calls.push({
        threadId: row.thread_id,
        owner: row.thread_id === parentId ? "parent" : "child",
        name: raw.name,
        arguments: asArguments(raw.arguments ?? raw.input),
      });
    }
  }
  return calls;
}

function readTokensUsed(database: DatabaseSync, threadIds: string[]): number | null {
  const table = database
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'thread_goals'")
    .get() as { name?: string } | undefined;
  if (!table || threadIds.length === 0) return null;
  const placeholders = threadIds.map(() => "?").join(", ");
  const row = database
    .prepare(
      `SELECT COALESCE(SUM(tokens_used), 0) AS total FROM thread_goals WHERE thread_id IN (${placeholders})`,
    )
    .get(...threadIds) as { total?: number } | undefined;
  const total = row?.total;
  return typeof total === "number" ? total : null;
}

/** 读取一次评测的委派指标；无数据库时返回全零（由调用方结合质量断言判断）。 */
export function readDelegationMetrics(home: string): DelegationMetrics {
  const database = openDatabase(home);
  if (!database) {
    return {
      childThreadIds: [],
      agentCalls: 0,
      effectiveDelegation: false,
      parentToolCalls: 0,
      childToolCalls: 0,
      parentMutationCallsAfterDelegation: 0,
      duplicateLabor: 0,
      modelCalls: 0,
      tokensUsed: null,
    };
  }
  try {
    database.exec("BEGIN");
    const parents = database
      .prepare(
        "SELECT id, parent_thread_id, updated_at FROM threads WHERE parent_thread_id IS NULL ORDER BY updated_at DESC",
      )
      .all() as unknown as ThreadRow[];
    const parent = parents[0];
    if (!parent) {
      return {
        childThreadIds: [],
        agentCalls: 0,
        effectiveDelegation: false,
        parentToolCalls: 0,
        childToolCalls: 0,
        parentMutationCallsAfterDelegation: 0,
        duplicateLabor: 0,
        modelCalls: 0,
        tokensUsed: null,
      };
    }
    const children = database
      .prepare("SELECT id, parent_thread_id, updated_at FROM threads WHERE parent_thread_id = ?")
      .all(parent.id) as unknown as ThreadRow[];
    const threads = [parent, ...children];
    const calls = collectToolCalls(database, threads, parent.id);
    const parentCalls = calls.filter((call) => call.owner === "parent");
    const childCalls = calls.filter((call) => call.owner === "child");

    const firstAgentIndex = parentCalls.findIndex((call) => call.name === "Agent");
    const afterDelegation =
      firstAgentIndex >= 0 ? parentCalls.slice(firstAgentIndex + 1) : [];
    const childWriteTargets = new Set(
      childCalls.map(writeTarget).filter((target): target is string => target !== undefined),
    );
    const duplicateTargets = new Set(
      afterDelegation
        .map(writeTarget)
        .filter((target): target is string => target !== undefined && childWriteTargets.has(target)),
    );

    const ids = threads.map((thread) => thread.id);
    const placeholders = ids.map(() => "?").join(", ");
    const modelCalls = (
      database
        .prepare(
          `SELECT COUNT(*) AS count FROM messages WHERE thread_id IN (${placeholders}) AND role = 'assistant'`,
        )
        .get(...ids) as { count?: number } | undefined
    )?.count ?? 0;

    const agentCalls = parentCalls.filter((call) => call.name === "Agent").length;
    return {
      parentThreadId: parent.id,
      childThreadIds: children.map((thread) => thread.id),
      agentCalls,
      effectiveDelegation: agentCalls > 0 && children.length > 0,
      parentToolCalls: parentCalls.length,
      childToolCalls: childCalls.length,
      parentMutationCallsAfterDelegation: afterDelegation.filter(
        (call) => writeTarget(call) !== undefined,
      ).length,
      duplicateLabor: duplicateTargets.size,
      modelCalls,
      tokensUsed: readTokensUsed(database, ids),
    };
  } finally {
    database.close();
  }
}
