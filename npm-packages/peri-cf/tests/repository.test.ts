import { describe, expect, test } from "bun:test";
import type { SessionStorage, SessionSummary } from "../worker/sdk";
import { TursoChatRepository } from "../worker/chat/repository";

const session: SessionSummary = {
  id: "00000000-0000-4000-8000-000000000001", title: "Persisted title", cwd: "/workspace",
  messageCount: 4, createdAt: "2026-10-05T00:00:00.000Z", updatedAt: "2026-10-06T00:00:00.000Z",
};

function fixture() {
  const summaries = new Map<string, SessionSummary>([[session.id, structuredClone(session)]]);
  const calls: string[] = [];
  const storage: Pick<SessionStorage, "getSessions" | "getSession"> = {
    async getSessions(cwd) {
      calls.push(`list:${cwd}`);
      return structuredClone([...summaries.values()].filter((summary) => summary.cwd === cwd));
    },
    async getSession(id) { calls.push(`get:${id}`); return structuredClone(summaries.get(id) ?? null); },
  };
  const createSession = async (title: string) => { calls.push(`create:${title}`); return session.id; };
  return { summaries, calls, storage, repository: new TursoChatRepository(storage, createSession) };
}

describe("TursoChatRepository using the SDK storage read seam", () => {
  test("lists workspace sessions using actual ACP identities and authoritative metadata", async () => {
    const app = fixture();
    app.summaries.set("other", { ...session, id: "other", cwd: "/another-workspace" });
    expect(await app.repository.list()).toEqual([{ id: session.id, title: session.title!, updatedAt: session.updatedAt }]);
    expect(app.calls).toEqual(["list:/workspace"]);
  });

  test("preserves the SDK storage ordering instead of maintaining a separate directory", async () => {
    const app = fixture();
    const newer = { ...session, id: "00000000-0000-4000-8000-000000000002", updatedAt: "2026-10-07T00:00:00.000Z" };
    app.storage.getSessions = async () => [newer, session];
    expect((await app.repository.list()).map(({ id }) => id)).toEqual([newer.id, session.id]);
  });

  test("maps a null stored title to the display fallback", async () => {
    const app = fixture();
    app.summaries.set(session.id, { ...session, title: null });
    expect(await app.repository.get(session.id)).toEqual({ id: session.id, title: "New chat", updatedAt: session.updatedAt });
  });

  test("returns null for a missing or differently scoped persisted session", async () => {
    const app = fixture();
    expect(await app.repository.get("missing")).toBeNull();
    app.summaries.set(session.id, { ...session, cwd: "/another-workspace" });
    expect(await app.repository.get(session.id)).toBeNull();
  });

  test("creation reads the ACP-created row instead of minting a presentation identity or metadata", async () => {
    const app = fixture();
    expect(await app.repository.create("Requested title")).toEqual({
      id: session.id, title: session.title!, updatedAt: session.updatedAt,
    });
    expect(app.calls).toEqual(["create:Requested title", `get:${session.id}`]);
  });

  test("creation fails if the created session is not visible in the authoritative Store", async () => {
    const app = fixture();
    app.summaries.clear();
    await expect(app.repository.create("New")).rejects.toThrow("Created ACP session is not visible in Turso");
    expect(app.calls).toEqual(["create:New", `get:${session.id}`]);
  });

  test("a reconstructed repository reads new persisted title and timestamp without cached metadata", async () => {
    const app = fixture();
    await app.repository.get(session.id);
    app.summaries.set(session.id, { ...session, title: "Changed in Rust", updatedAt: "2026-10-07T00:00:00.000Z" });
    const restored = new TursoChatRepository(app.storage, async () => { throw new Error("Unexpected creation"); });
    expect(await restored.get(session.id)).toEqual({ id: session.id, title: "Changed in Rust", updatedAt: "2026-10-07T00:00:00.000Z" });
  });

  test("storage read failures propagate instead of becoming empty history", async () => {
    const app = fixture();
    app.storage.getSessions = async () => { throw new Error("Turso list unavailable"); };
    app.storage.getSession = async () => { throw new Error("Turso detail unavailable"); };
    await expect(app.repository.list()).rejects.toThrow("Turso list unavailable");
    await expect(app.repository.get(session.id)).rejects.toThrow("Turso detail unavailable");
  });

  test("creation failure does not fall back to any existing session", async () => {
    const app = fixture();
    const repository = new TursoChatRepository(app.storage, async () => { throw new Error("ACP creation failed"); });
    await expect(repository.create("New")).rejects.toThrow("ACP creation failed");
    expect(app.calls).toEqual([]);
  });
});
