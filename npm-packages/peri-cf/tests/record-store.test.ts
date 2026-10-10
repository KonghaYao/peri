import { describe, expect, test } from "bun:test";
import {
  BACKUP_KEY, CHECKPOINT_INDEX_MAX_BYTES, LEGACY_RECORD_KEY, PAYLOAD_KEY_PREFIX, RECORD_BACKUP_TTL_MS,
  RECORD_KEY, ChatRecordStore, RecordFormatError, checkpointFromLegacyRecord, fitCheckpointToIndex,
  partKeys, payloadIdFromKey, recordBytes, splitPayload, type RecordStorage, type StoredRecord, type StoredBackup,
} from "../worker/chat/record";
import type { Chat, SessionRecord } from "../worker/types";
import type { DisplayCheckpoint } from "../worker/sdk";

const chat: Chat = { id: "00000000-0000-4000-8000-000000000001", title: "Record fixture", updatedAt: "2026-10-10T00:00:00Z" };
const encoder = new TextEncoder();

class MemoryStorage implements RecordStorage {
  readonly map = new Map<string, unknown>();
  failOn: ((entries: Record<string, unknown>) => boolean) | undefined;
  puts = 0;

  async get<T>(key: string): Promise<T | undefined> { return this.map.get(key) as T | undefined; }
  async put(entries: Record<string, unknown>): Promise<void> {
    this.puts++;
    if (this.failOn?.(entries)) throw new Error("storage put failed");
    for (const [key, value] of Object.entries(entries)) this.map.set(key, structuredClone(value));
  }
  async delete(keys: string[]): Promise<boolean> {
    let removed = false;
    for (const key of keys) removed = this.map.delete(key) || removed;
    return removed;
  }
  async list(options: { prefix?: string } = {}): Promise<Map<string, unknown>> {
    const result = new Map<string, unknown>();
    for (const [key, value] of [...this.map].sort(([left], [right]) => left.localeCompare(right))) {
      if (options.prefix && !key.startsWith(options.prefix)) continue;
      result.set(key, value);
    }
    return result;
  }
}

function legacy(messages: SessionRecord["messages"], extra: Partial<SessionRecord> = {}): SessionRecord {
  return { chat, messages, ...extra };
}

describe("payload splitting", () => {
  test("cuts on UTF-8 boundaries and never splits surrogate pairs", () => {
    const json = JSON.stringify({ text: "中文🌙".repeat(400) });
    const parts = splitPayload(json, 512);
    expect(parts.length).toBeGreaterThan(1);
    expect(parts.join("")).toBe(json);
    for (const part of parts) expect(encoder.encode(part).byteLength).toBeLessThanOrEqual(512);
    expect(parts.every((part) => !/[\uD800-\uDBFF]$/.test(part))).toBe(true);
  });

  test("payload keys round-trip ids that contain colons", () => {
    for (const key of partKeys("gen:7", 3)) expect(payloadIdFromKey(key)).toBe("gen:7");
    expect(payloadIdFromKey("other:1")).toBeNull();
    expect(payloadIdFromKey(`${PAYLOAD_KEY_PREFIX}broken`)).toBeNull();
  });
});

describe("v1 to v2 conversion", () => {
  test("converts rows once, keeps old ids/times, backs up and deletes the legacy key", async () => {
    const storage = new MemoryStorage();
    const store = new ChatRecordStore(storage, () => 1_000);
    storage.map.set(LEGACY_RECORD_KEY, legacy([
      { id: "user-1", role: "user", content: "问题", createdAt: "2026-10-01T00:00:00Z", status: "completed" },
      { id: "assistant-1", role: "assistant", content: "回答", createdAt: "2026-10-01T00:00:01Z", status: "error", error: "provider exploded" },
      { id: "user-2", role: "user", content: "追问", createdAt: "2026-10-01T00:01:00Z", status: "completed" },
      { id: "assistant-2", role: "assistant", content: "部分", createdAt: "2026-10-01T00:01:01Z", status: "running" },
    ], { running: true }));

    const loaded = await store.load();
    expect(loaded.source).toBe("converted");
    expect(loaded.interrupted).toBe(true);
    const record = loaded.record!;
    expect(record.recordVersion).toBe(2);
    expect(record.running).toBe(false);
    expect(record.executionBlocked).toBe(true);
    expect(record.payloadIndex).toEqual([]);
    expect(storage.map.has(LEGACY_RECORD_KEY)).toBe(false);
    const backup = storage.map.get(BACKUP_KEY) as StoredBackup;
    expect(backup.convertedAt).toBe(1_000);
    expect(backup.record.messages).toHaveLength(4);

    const entries = record.checkpoint.entries;
    expect(entries.map((entry) => entry.id)).toEqual(["user-1", "assistant-1", "user-2", "assistant-2"]);
    expect(entries[0]).toMatchObject({ turnId: "turn:user-1", role: "user", status: "completed", createdAt: "2026-10-01T00:00:00Z" });
    expect(entries[1]).toMatchObject({ status: "error", error: { code: "execution_failed", message: "provider exploded" } });
    // 未完成行由导入收敛为中断，不伪装完成。
    expect(entries[3]).toMatchObject({ entryId: "turn:user-2:assistant", status: "error", error: { code: "execution_interrupted" } });
    expect(record.checkpoint.turns.map((turn) => turn.turnId)).toEqual(["turn:user-1", "turn:user-2"]);
    expect(record.checkpoint.turns[1]).toMatchObject({ status: "error" });

    // 幂等：再次 load 直读 v2，不再写备份。
    const again = await store.load();
    expect(again.source).toBe("v2");
    expect(again.record).toEqual(record);
    expect((storage.map.get(BACKUP_KEY) as StoredBackup).convertedAt).toBe(1_000);
  });

  test("trims oldest display material when the index exceeds the target and marks legacyIncomplete", () => {
    const messages: SessionRecord["messages"] = [];
    for (let index = 0; index < 400; index++) {
      messages.push({ id: `u-${index}`, role: "user", content: `问 ${index} ${"x".repeat(2000)}`, createdAt: "2026-10-01T00:00:00Z", status: "completed" });
      messages.push({ id: `a-${index}`, role: "assistant", content: `答 ${index} ${"y".repeat(2000)}`, createdAt: "2026-10-01T00:00:01Z", status: "completed" });
    }
    const built = checkpointFromLegacyRecord(legacy(messages), { chat: 3, session: 2 });
    expect(recordBytes(built)).toBeGreaterThan(CHECKPOINT_INDEX_MAX_BYTES);
    const { record: fitted, trimmedTurns } = fitCheckpointToIndex({
      recordVersion: 2, chat, running: false, executionBlocked: false, checkpoint: built,
    });
    expect(trimmedTurns).toBeGreaterThan(0);
    expect(recordBytes(fitted.checkpoint)).toBeLessThanOrEqual(CHECKPOINT_INDEX_MAX_BYTES);
    expect(fitted.checkpoint.legacyIncomplete).toBe(true);
    expect(fitted.checkpoint.entries.at(-1)!.id).toBe("a-399");
    expect(fitted.checkpoint.entries[0]!.id).not.toBe("u-0");
  });
});

describe("segmented payload storage", () => {
  async function committed(): Promise<{ storage: MemoryStorage; store: ChatRecordStore; record: StoredRecord; json: string }> {
    const storage = new MemoryStorage();
    const store = new ChatRecordStore(storage, () => 1_000);
    const json = JSON.stringify({ contentText: "结果🌙".repeat(400_000) });
    const checkpoint: DisplayCheckpoint = {
      checkpointVersion: 1, schemaVersions: { chat: 3, session: 2 }, session: {},
      entries: [{
        entryId: "turn:1:assistant", id: "turn:1:assistant", kind: "message" as const, turnId: "turn:1",
        role: "assistant" as const, status: "completed", createdAt: null, createdAtSource: "observed",
        completedAt: null, error: null,
        blocks: [{ blockId: "tool:t1", type: "tool_call" as const, toolCallId: "t1" }],
      }],
      toolCalls: [{
        toolCallId: "t1", turnId: "turn:1", name: "Read", status: "completed", createdAt: null, completedAt: null,
        error: null, resultRef: { id: "gen:1", bytes: encoder.encode(json).byteLength, preview: "结果" },
      }],
      turns: [], tasks: [], plansByTurn: {}, interactions: [],
      payloadValues: [{ id: "gen:1", json }],
    };
    const record = await store.commit({ chat, running: false, executionBlocked: false, checkpoint });
    return { storage, store, record, json };
  }

  test("writes parts before the index, exposes the exact bytes and reads back", async () => {
    const { storage, store, json } = await committed();
    expect(recordBytes(storage.map.get(RECORD_KEY))).toBeLessThan(4096);
    expect(storage.map.size).toBeGreaterThan(2);
    const index = storage.map.get(RECORD_KEY) as StoredRecord;
    expect(index.payloadIndex).toEqual([{ id: "gen:1", bytes: encoder.encode(json).byteLength, parts: expect.any(Number) }]);
    const read = await store.readPayload("gen:1");
    expect(read).toEqual({ status: "ok", bytes: index.payloadIndex[0]!.bytes, json });
    expect(await store.readPayload("missing")).toEqual({ status: "unknown" });
  });

  test("reports missing and corrupted parts instead of returning another version", async () => {
    const { storage, store } = await committed();
    storage.map.delete(`${PAYLOAD_KEY_PREFIX}gen:1:0`);
    expect((await store.readPayload("gen:1")).status).toBe("missing");
    storage.map.set(`${PAYLOAD_KEY_PREFIX}gen:1:0`, "truncated");
    expect((await store.readPayload("gen:1")).status).toBe("mismatch");
  });

  test("replaces and drops retired payloads, and sweeps orphan parts", async () => {
    const storage = new MemoryStorage();
    const store = new ChatRecordStore(storage, () => 1_000);
    const checkpointFor = (id: string, json: string): DisplayCheckpoint => ({
      checkpointVersion: 1, schemaVersions: { chat: 3, session: 2 }, session: {}, entries: [],
      toolCalls: [{ toolCallId: "t1", turnId: "turn:1", name: "Read", status: "completed", createdAt: null, completedAt: null,
        error: null, resultRef: { id, bytes: encoder.encode(json).byteLength, preview: "" } }],
      turns: [], tasks: [], plansByTurn: {}, interactions: [], payloadValues: [{ id, json }],
    });
    await store.commit({ chat, running: false, executionBlocked: false, checkpoint: checkpointFor("gen:1", "x".repeat(4096)) });
    expect((await store.sweep()).orphanParts).toBe(0);
    await store.commit({ chat, running: false, executionBlocked: false, checkpoint: checkpointFor("gen:2", "y".repeat(4096)) });
    // 被替换版本立即回收。
    expect([...storage.map.keys()].some((key) => key.startsWith(`${PAYLOAD_KEY_PREFIX}gen:1:`))).toBe(false);
    expect((await store.readPayload("gen:1")).status).toBe("unknown");
    // 孤儿段（无索引引用）由清扫回收。
    storage.map.set(`${PAYLOAD_KEY_PREFIX}ghost:5:0`, "leftover");
    expect((await store.sweep()).orphanParts).toBe(1);
    expect(storage.map.has(`${PAYLOAD_KEY_PREFIX}ghost:5:0`)).toBe(false);
  });

  test("an index commit failure leaves readable orphans and a recoverable legacy record", async () => {
    const storage = new MemoryStorage();
    const store = new ChatRecordStore(storage, () => 1_000);
    storage.map.set(LEGACY_RECORD_KEY, legacy([
      { id: "user-1", role: "user", content: "hello", createdAt: "2026-10-01T00:00:00Z", status: "completed" },
    ]));
    // 第一次 put 是备份，第二次是索引 → 索引提交失败。
    let puts = 0;
    storage.failOn = () => ++puts === 2;
    await expect(store.load()).rejects.toThrow("storage put failed");
    expect(storage.map.has(RECORD_KEY)).toBe(false);
    expect(storage.map.has(LEGACY_RECORD_KEY)).toBe(true);
    expect((await store.sweep()).orphanParts).toBe(0);
    storage.failOn = undefined;
    const recovered = await store.load();
    expect(recovered.source).toBe("converted");
    expect(storage.map.has(LEGACY_RECORD_KEY)).toBe(false);
  });

  test("rejects unknown record versions instead of misreading them", async () => {
    const storage = new MemoryStorage();
    storage.map.set(RECORD_KEY, { recordVersion: 3, chat, checkpoint: {}, payloadIndex: [] });
    await expect(new ChatRecordStore(storage).load()).rejects.toThrow(RecordFormatError);
  });
});

describe("backup window and rollback", () => {
  test("expires the backup after seven days and restores it on demand", async () => {
    const storage = new MemoryStorage();
    let now = 1_000;
    const store = new ChatRecordStore(storage, () => now);
    storage.map.set(LEGACY_RECORD_KEY, legacy([
      { id: "user-1", role: "user", content: "hello", createdAt: "2026-10-01T00:00:00Z", status: "completed" },
    ]));
    await store.load();
    expect(await store.hasBackup()).toBe(true);
    now = 1_000 + RECORD_BACKUP_TTL_MS - 1;
    expect((await store.sweep()).expiredBackup).toBe(false);
    now = 1_000 + RECORD_BACKUP_TTL_MS + 1;
    expect((await store.sweep()).expiredBackup).toBe(true);
    expect(await store.hasBackup()).toBe(false);
    expect(await store.restoreBackup()).toBe(false);
  });

  test("restoreBackup reinstates the v1 record and removes the v2 index with its parts", async () => {
    const storage = new MemoryStorage();
    const store = new ChatRecordStore(storage, () => 1_000);
    storage.map.set(LEGACY_RECORD_KEY, legacy([
      { id: "user-1", role: "user", content: "hello", createdAt: "2026-10-01T00:00:00Z", status: "completed" },
      { id: "assistant-1", role: "assistant", content: "world", createdAt: "2026-10-01T00:00:01Z", status: "completed" },
    ]));
    await store.load();
    storage.map.set(`${PAYLOAD_KEY_PREFIX}orphan:1:0`, "x");
    expect(await store.restoreBackup()).toBe(true);
    expect(storage.map.has(RECORD_KEY)).toBe(false);
    expect([...storage.map.keys()].some((key) => key.startsWith(PAYLOAD_KEY_PREFIX))).toBe(false);
    const restored = storage.map.get(LEGACY_RECORD_KEY) as SessionRecord;
    expect(restored.messages.map((message) => message.content)).toEqual(["hello", "world"]);
    // 还原后可再次转换（回滚演练的可逆性）。
    const reloaded = await new ChatRecordStore(storage, () => 2_000).load();
    expect(reloaded.source).toBe("converted");
    expect(reloaded.record!.checkpoint.entries.map((entry) => entry.id)).toEqual(["user-1", "assistant-1"]);
  });
});
