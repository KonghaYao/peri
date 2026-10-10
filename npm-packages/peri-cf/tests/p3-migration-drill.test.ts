import { describe, expect, test } from "bun:test";
import { ChatRecordStore, RECORD_KEY, LEGACY_RECORD_KEY, BACKUP_KEY, type RecordStorage } from "../worker/chat/record";
import { ChatSessionCore } from "../worker/chat/session";
import { fetchApi } from "../worker/api/router";
import type { AcpTransport, Chat, Env, SessionRecord, SessionState } from "../worker/types";
import type { JsonRpcNotification } from "../worker/sdk";

/**
 * P3 维护窗口演练（本地可演练范围）：冻结 → 转换 → 切入口 → 回滚还原。
 * 部署端才能验证的部分（真实平台 2 MB 强制点、生产窗口、DNS/流量切换）在 issue 中标注「需部署端验证」。
 */

const chat: Chat = { id: "00000000-0000-4000-8000-000000000001", title: "Drill", updatedAt: "2026-10-10T00:00:00Z" };

class DrillTransport implements AcpTransport {
  readonly generationId = "drill";
  async request<Result>(method: string): Promise<Result> { return (method === "initialize" ? {} : {}) as Result; }
  async sendRequest<Result>() {
    return { response: new Promise<{ stopReason: string }>(() => {}) as Promise<Result> };
  }
  async notify(): Promise<void> {}
  async *events(): AsyncIterable<JsonRpcNotification> {}
  subscribe() { return () => {}; }
  setRequestHandler(): void {}
  async close(): Promise<void> {}
}

function memoryStorage(): { buckets: Map<string, unknown>; state: SessionState; jobs: Promise<unknown>[] } {
  const buckets = new Map<string, unknown>();
  const jobs: Promise<unknown>[] = [];
  return { buckets, jobs, state: {
    storage: {
      async get<Value>(key: string) { return structuredClone(buckets.get(key)) as Value | undefined; },
      async put(entries: Record<string, unknown>) { for (const [key, value] of Object.entries(entries)) buckets.set(key, structuredClone(value)); },
      async delete(keys: string[]) { let removed = false; for (const key of keys) removed = buckets.delete(key) || removed; return removed; },
      async list(options: { prefix?: string } = {}) {
        const result = new Map<string, unknown>();
        for (const [key, value] of [...buckets].sort(([left], [right]) => left.localeCompare(right))) {
          if (options.prefix && !key.startsWith(options.prefix)) continue;
          result.set(key, structuredClone(value));
        }
        return result;
      },
    },
    waitUntil(promise) { jobs.push(promise); },
  } };
}

const repository = {
  async get(id: string) { return id === chat.id ? chat : null; },
  async list() { return [chat]; },
  async create() { return chat; },
};

/** 经应用入口请求；每个请求重建 DO 核心，模拟实例被回收后的真实唤醒。 */
function request(storage: SessionState, path: string, method = "GET", body?: unknown): Promise<Response> {
  const env: Env = { APP_AUTH_TOKEN: "drill-token", CHAT_SESSIONS: {
    idFromName(name) { return name; },
    get() { return { fetch: (incoming: Request) => new ChatSessionCore(storage, env, async () => new DrillTransport(), repository).fetch(incoming) }; },
  } };
  return fetchApi(new Request(`https://app.invalid${path}`, {
    method, headers: { Authorization: "Bearer drill-token", ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  }), env, repository);
}

describe("P3 维护窗口演练（本地）", () => {
  test("冻结旧入口 → 转换 → 切 v2 → 回滚还原", async () => {
    // 1) 冻结：旧记录就位，旧入口在切换后对新客户端返回升级错误。
    const { buckets, state, jobs } = memoryStorage();
    const legacy: SessionRecord = {
      chat, running: false,
      messages: [
        { id: "user-1", role: "user", content: "冻结前的问题", createdAt: "2026-10-09T00:00:00Z", status: "completed" },
        { id: "assistant-1", role: "assistant", content: "冻结前的回答", createdAt: "2026-10-09T00:00:01Z", status: "completed" },
      ],
    };
    buckets.set(LEGACY_RECORD_KEY, structuredClone(legacy));
    const legacyPath = await request(state, `/api/chats/${chat.id}`);
    expect(legacyPath.status).toBe(426);
    expect(await legacyPath.json()).toMatchObject({ upgrade: true });

    // 2) 转换：首次 v2 访问触发 v1 → v2（备份 + 分段 + 索引），旧键删除。
    const detail = await (await request(state, `/api/v2/chats/${chat.id}`)).json() as { readiness: string; entries: Array<{ id: string; blocks: Array<{ text?: string }> }> };
    expect(detail.readiness).toBe("ready");
    expect(detail.entries.map((entry) => entry.id)).toEqual(["user-1", "assistant-1"]);
    expect(detail.entries.map((entry) => entry.blocks[0]?.text)).toEqual(["冻结前的问题", "冻结前的回答"]);
    expect(buckets.has(LEGACY_RECORD_KEY)).toBe(false);
    const record = buckets.get(RECORD_KEY) as { recordVersion: number; checkpoint: { schemaVersions: unknown } };
    expect(record.recordVersion).toBe(2);
    expect(record.checkpoint.schemaVersions).toEqual({ chat: 3, session: 2 });
    const backup = buckets.get(BACKUP_KEY) as { record: SessionRecord; convertedAt: number };
    expect(backup.record.messages.map((message) => message.content)).toEqual(["冻结前的问题", "冻结前的回答"]);
    expect(typeof backup.convertedAt).toBe("number");

    // 3) 切入口：v2 继续可用，旧入口对全部方法给升级错误。
    expect((await request(state, `/api/v2/chats/${chat.id}/resources`)).status).toBe(200);
    expect([400, 415]).toContain((await request(state, `/api/v2/chats/${chat.id}/messages`, "POST")).status);
    expect((await request(state, `/api/chats/${chat.id}`)).status).toBe(426);

    // 4) 回滚：备份还原 v1 记录并清掉 v2 索引；旧读取路径重新可读（生产上还需部署旧构建）。
    const store = new ChatRecordStore({
      get: async <T>(key: string) => structuredClone(buckets.get(key)) as T | undefined,
      put: async (entries: Record<string, unknown>) => { for (const [key, value] of Object.entries(entries)) buckets.set(key, structuredClone(value)); },
      delete: async (keys: string[]) => { for (const key of keys) buckets.delete(key); return true; },
      list: async (options = {}) => {
        const result = new Map<string, unknown>();
        for (const [key, value] of buckets) if (!options.prefix || key.startsWith(options.prefix)) result.set(key, structuredClone(value));
        return result;
      },
    } as RecordStorage);
    expect(await store.restoreBackup()).toBe(true);
    expect(buckets.has(RECORD_KEY)).toBe(false);
    const rolled = buckets.get(LEGACY_RECORD_KEY) as SessionRecord;
    expect(rolled.messages.map((message) => message.content)).toEqual(["冻结前的问题", "冻结前的回答"]);
    // 回滚后的旧构建可读 v1 记录；若再次升级，转换可重复执行（可逆性）。
    const again = await (await request(state, `/api/v2/chats/${chat.id}`)).json() as { entries: Array<{ id: string }> };
    expect(again.entries.map((entry) => entry.id)).toEqual(["user-1", "assistant-1"]);
  });
});
