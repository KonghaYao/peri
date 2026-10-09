import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";

interface HistoryRow {
  rowid: number;
  message_id: string;
  role: string;
  content: string;
}

interface Reminder {
  category: string;
  source: string;
  kind: string;
  body: string;
  metadata: { task_id?: string; child_thread_id?: string; success?: boolean };
}

export interface BgStageBoundary {
  parentSessionId?: string;
  lastMessageRow: number;
}

export interface BgCompletionEvidence {
  parentSessionId: string;
  taskId: string;
  deliveryId: string;
  responseMessageId: string;
  childSessionId?: string;
}

function read<T>(home: string, inspect: (database: DatabaseSync) => T): T | undefined {
  const file = path.join(home, ".peri", "threads", "threads.db");
  if (!existsSync(file)) return undefined;
  const database = new DatabaseSync(file, { readOnly: true });
  try {
    database.exec("BEGIN");
    return inspect(database);
  } finally {
    database.close();
  }
}

function parentId(database: DatabaseSync): string | undefined {
  const rows = database.prepare(
    "SELECT id FROM threads WHERE parent_thread_id IS NULL",
  ).all() as { id: string }[];
  if (rows.length > 1) throw new Error("BG fixture requires exactly one direct parent session");
  return rows[0]?.id;
}

function text(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content.map((block) => typeof block?.text === "string" ? block.text : "").join("\n");
}

function containsOutput(reminder: Reminder, source: "shell" | "subagent", marker: string): boolean {
  if (source === "subagent") return reminder.body.includes(marker);
  const stdout = /^stdout 输出文件：(.+)$/m.exec(reminder.body)?.[1];
  return stdout !== undefined && path.isAbsolute(stdout) && existsSync(stdout)
    && reminder.body.includes("退出码 0")
    && reminder.body.includes("完整输出已保存到文件系统")
    && readFileSync(stdout, "utf8").includes(marker);
}

export function captureBgStageBoundary(home: string): BgStageBoundary {
  return read(home, (database) => {
    const parentSessionId = parentId(database);
    const row = parentSessionId === undefined ? undefined : database.prepare(
      "SELECT COALESCE(MAX(rowid), 0) AS last_row FROM messages WHERE thread_id = ?",
    ).get(parentSessionId) as { last_row: number } | undefined;
    return { parentSessionId, lastMessageRow: row?.last_row ?? 0 };
  }) ?? { lastMessageRow: 0 };
}

export function completedBgStage(
  home: string,
  boundary: BgStageBoundary,
  source: "shell" | "subagent",
  outputMarker: string,
  responseMarker: string,
): BgCompletionEvidence | undefined {
  return read(home, (database) => {
    const parentSessionId = parentId(database);
    if (!parentSessionId || (boundary.parentSessionId && boundary.parentSessionId !== parentSessionId)) {
      return undefined;
    }
    const rows = database.prepare(
      "SELECT rowid, message_id, role, content FROM messages WHERE thread_id = ? AND rowid > ? ORDER BY rowid",
    ).all(parentSessionId, boundary.lastMessageRow) as unknown as HistoryRow[];
    for (const row of rows) {
      const payload = JSON.parse(row.content);
      if (payload.type !== "system_reminder" || payload.id !== row.message_id) continue;
      const reminder = payload.reminder as Reminder;
      if (reminder.category !== "task" || reminder.kind !== "completed"
        || (source === "subagent" && reminder.metadata.success !== true) || !reminder.metadata.task_id
        || !(reminder.source === source || (source === "shell" && reminder.source === "mcp"))
        || !containsOutput(reminder, source, outputMarker)) continue;
      const matchingDeliveries = rows.filter((candidate) => {
        const content = JSON.parse(candidate.content);
        return content.type === "system_reminder" && content.reminder?.kind === "completed"
          && content.reminder?.metadata?.task_id === reminder.metadata.task_id;
      });
      if (matchingDeliveries.length !== 1) throw new Error("BG terminal was persisted more than once");
      const response = rows.find((candidate) => {
        if (candidate.rowid <= row.rowid || candidate.role !== "assistant") return false;
        const content = JSON.parse(candidate.content);
        return content.type === "message" && content.message?.role === "assistant"
          && text(content.message.content).includes(responseMarker);
      });
      if (!response) continue;
      const evidence = {
        parentSessionId,
        taskId: reminder.metadata.task_id,
        deliveryId: row.message_id,
        responseMessageId: response.message_id,
      };
      if (source === "shell") return evidence;
      const childId = reminder.metadata.child_thread_id;
      if (!childId) continue;
      const child = database.prepare(
        "SELECT id FROM threads WHERE id = ? AND parent_thread_id = ? AND agent_status = 'done'",
      ).get(childId, parentSessionId) as { id: string } | undefined;
      if (child) return { ...evidence, childSessionId: child.id };
    }
    return undefined;
  });
}
