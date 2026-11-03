import { describe, expect, test } from "bun:test";
import type { ChatSessionDirectoryEntry } from "@peri-code/sdk";
import { ChatDirectoryUnavailableError, createHttpRepository } from "./repository";

/** 与服务端 `GET /api/chats` 的目录条目同形：SDK 目录事实 + 服务端派生的项目归属。 */
const directoryEntry = {
  sessionId: "session-1",
  title: null,
  cwd: "/workspace/",
  messageCount: 3,
  createdAt: "2026-10-20T12:00:00Z",
  updatedAt: "2026-10-20T13:00:00Z",
  projectId: "default",
} satisfies ChatSessionDirectoryEntry & { projectId: string };

function respond(body: unknown, status = 200): typeof fetch {
  return (async () => new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })) as unknown as typeof fetch;
}

describe("chat directory repository", () => {
  test("parses the SDK directory shape without a display id", async () => {
    const repository = createHttpRepository("http://localhost", respond({ chats: [directoryEntry], available: true }));
    const chats = await repository.list();
    expect(chats).toEqual([directoryEntry]);
    expect(chats[0]).not.toHaveProperty("id");
  });

  test("rejects entries that do not match the SDK directory contract", async () => {
    const invalid: Record<string, unknown> = { ...directoryEntry };
    delete invalid.sessionId;
    const repository = createHttpRepository("http://localhost", respond({ chats: [invalid], available: true }));
    const failure = await repository.list().then(() => undefined, (error: unknown) => error);
    expect(failure).toBeInstanceOf(ChatDirectoryUnavailableError);
    expect((failure as Error).message).toBe("会话目录不可用（HTTP 200）。");
  });
});
