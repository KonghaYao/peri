import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { captureBgStageBoundary, completedBgStage } from "./bg-task-boundary.js";

describe("background completion canonical evidence", () => {
  let home: string;
  let database: DatabaseSync;

  beforeEach(() => {
    home = mkdtempSync(path.join(os.tmpdir(), "peri-bg-boundary-"));
    const directory = path.join(home, ".peri", "threads");
    mkdirSync(directory, { recursive: true });
    database = new DatabaseSync(path.join(directory, "threads.db"));
    database.exec(`
      CREATE TABLE threads (id TEXT PRIMARY KEY, parent_thread_id TEXT, agent_status TEXT);
      CREATE TABLE messages (message_id TEXT PRIMARY KEY, thread_id TEXT, role TEXT, content TEXT);
      INSERT INTO threads VALUES ('parent', NULL, 'active'), ('child', 'parent', 'done');
    `);
  });

  afterEach(() => {
    database.close();
    rmSync(home, { recursive: true, force: true });
  });

  function append(id: string, type: "message" | "system_reminder", value: unknown): void {
    const payload = type === "message"
      ? { version: 1, type, message: { id, role: "assistant", content: [{ type: "text", text: value }] } }
      : { version: 1, type, id, reminder: value };
    database.prepare("INSERT INTO messages VALUES (?, 'parent', ?, ?)")
      .run(id, type === "message" ? "assistant" : "system_reminder", JSON.stringify(payload));
  }

  function reminder(source = "subagent", body = "BG_OUTPUT") {
    return { category: "task", source, kind: "completed", body,
      metadata: { task_id: "task", child_thread_id: "child", success: true } };
  }

  function evidence() {
    return completedBgStage(home, { parentSessionId: "parent", lastMessageRow: 0 },
      "subagent", "BG_OUTPUT", "BG_PROCESSED");
  }

  it("uses only current threads/messages and requires a later parent response", () => {
    append("delivery", "system_reminder", reminder());
    expect(evidence()).toBeUndefined();
    append("response", "message", "BG_PROCESSED");
    expect(evidence()).toEqual({ parentSessionId: "parent", taskId: "task",
      deliveryId: "delivery", responseMessageId: "response", childSessionId: "child" });
  });

  it("does not count an earlier assistant response as result consumption", () => {
    append("response", "message", "BG_PROCESSED");
    append("delivery", "system_reminder", reminder());
    expect(evidence()).toBeUndefined();
  });

  it("excludes messages before the captured boundary", () => {
    append("delivery", "system_reminder", reminder());
    append("response", "message", "BG_PROCESSED");
    expect(completedBgStage(home, captureBgStageBoundary(home), "subagent", "BG_OUTPUT", "BG_PROCESSED"))
      .toBeUndefined();
  });

  it.each(["active", "failed"])("does not accept child status %s", (status) => {
    database.prepare("UPDATE threads SET agent_status = ? WHERE id = 'child'").run(status);
    append("delivery", "system_reminder", reminder());
    append("response", "message", "BG_PROCESSED");
    expect(evidence()).toBeUndefined();
  });

  it("requires the child to belong to the direct parent", () => {
    database.exec("UPDATE threads SET parent_thread_id = 'other' WHERE id = 'child'");
    append("delivery", "system_reminder", reminder());
    append("response", "message", "BG_PROCESSED");
    expect(evidence()).toBeUndefined();
  });

  it("rejects duplicate terminal deliveries", () => {
    append("delivery", "system_reminder", reminder());
    append("duplicate", "system_reminder", reminder());
    append("response", "message", "BG_PROCESSED");
    expect(evidence).toThrow("persisted more than once");
  });

  it("verifies shell result files and subsequent model response", () => {
    const stdout = path.join(home, "stdout.txt");
    writeFileSync(stdout, "BG_OUTPUT");
    append("delivery", "system_reminder", {
      ...reminder("shell", `退出码 0\n完整输出已保存到文件系统\nstdout 输出文件：${stdout}`),
      metadata: { task_id: "task", server: "workspace", initiator: "parent" },
    });
    append("response", "message", "BG_PROCESSED");
    expect(completedBgStage(home, { lastMessageRow: 0 }, "shell", "BG_OUTPUT", "BG_PROCESSED"))
      .toMatchObject({ taskId: "task", responseMessageId: "response" });
    writeFileSync(stdout, "other output");
    expect(completedBgStage(home, { lastMessageRow: 0 }, "shell", "BG_OUTPUT", "BG_PROCESSED"))
      .toBeUndefined();
  });

  it("surfaces malformed persisted canonical payloads", () => {
    database.exec("INSERT INTO messages VALUES ('bad', 'parent', 'system_reminder', 'not-json')");
    expect(evidence).toThrow();
  });
});
