import { SUPPORTED_SCHEMA_VERSIONS, SessionDocs, type DisplayCheckpoint, type CheckpointEntry, type CheckpointTurn } from "../sdk";
import type { Chat, SessionRecord } from "../types";

/**
 * DO 记录结构（P3 已批准）：`recordVersion: 2` 单键索引 + `payload:<id>:<n>` 分段。
 * 写入顺序固定为「全部分段写成功 → 索引提交」；段与索引非原子，靠孤儿清扫收敛。
 * v1 纯文本记录（key `session`）在加载时转换，转换前备份到 `record:v1-backup`（7 天窗口）。
 */

export const RECORD_VERSION = 2 as const;
export const RECORD_KEY = "record";
export const LEGACY_RECORD_KEY = "session";
export const BACKUP_KEY = "record:v1-backup";
export const PAYLOAD_KEY_PREFIX = "payload:";
/** 分段上限（已批准）：单段 1 MiB。 */
export const PAYLOAD_PART_BYTES = 1024 * 1024;
/** 索引容量目标（已批准）：新 turn 接纳与转换裁剪都以此为界。 */
export const CHECKPOINT_INDEX_MAX_BYTES = 1024 * 1024;
/** 实现护栏（非容量承诺）：低于平台文档的单值硬上限 2 MB，避免把 key 与 JSON 开销算漏。 */
export const INDEX_PLATFORM_SAFETY_BYTES = 1_900_000;
/** 备份回滚窗口（已批准）：7 天。 */
export const RECORD_BACKUP_TTL_MS = 7 * 24 * 60 * 60 * 1000;
/** 接纳预留：索引接近上限时仍允许一次 turn 的写入，之后拒绝新 turn。 */
export const INDEX_ADMISSION_HEADROOM_BYTES = 64 * 1024;

export type PayloadIndexEntry = { id: string; bytes: number; parts: number };
export type StoredCheckpoint = Omit<DisplayCheckpoint, "payloadValues">;
export type StoredRecord = {
  recordVersion: typeof RECORD_VERSION;
  chat: Chat;
  running: boolean;
  executionBlocked: boolean;
  checkpoint: StoredCheckpoint;
  payloadIndex: PayloadIndexEntry[];
};
export type StoredBackup = { record: SessionRecord; convertedAt: number };
export type PayloadReadResult =
  | { status: "ok"; bytes: number; json: string }
  | { status: "unknown" }
  | { status: "missing" }
  | { status: "mismatch" };

/** DO 存储端口的应用侧最小契约；真实实现是 DurableObjectStorage（成组 put / 数组 delete / prefix list）。 */
export interface RecordStorage {
  get<T>(key: string): Promise<T | undefined>;
  put(entries: Record<string, unknown>): Promise<void>;
  delete(keys: string[]): Promise<boolean | void>;
  list(options?: { prefix?: string; limit?: number }): Promise<Map<string, unknown>>;
}

export class RecordCorruptionError extends Error {
  constructor(readonly payloadId: string, readonly reason: string) {
    super(`Display payload ${payloadId} is not resolvable: ${reason}`);
    this.name = "RecordCorruptionError";
  }
}
export class RecordCapacityError extends Error {
  constructor(message: string) { super(message); this.name = "RecordCapacityError"; }
}
export class RecordFormatError extends Error {
  constructor(message: string) { super(message); this.name = "RecordFormatError"; }
}

const encoder = new TextEncoder();
export function recordBytes(value: unknown): number {
  return encoder.encode(JSON.stringify(value)).byteLength;
}
function bytesOf(text: string): number { return encoder.encode(text).byteLength; }

/** 按 UTF-8 字节上限切分，不切开代理对或多字节字符。 */
export function splitPayload(json: string, partBytes = PAYLOAD_PART_BYTES): string[] {
  if (partBytes <= 0) throw new RangeError("partBytes must be positive");
  const parts: string[] = [];
  let start = 0;
  let bytes = 0;
  for (let index = 0; index < json.length;) {
    const code = json.codePointAt(index)!;
    const width = code > 0xffff ? 4 : code > 0x7ff ? 3 : code > 0x7f ? 2 : 1;
    if (bytes + width > partBytes) {
      parts.push(json.slice(start, index));
      start = index;
      bytes = 0;
    }
    bytes += width;
    index += code > 0xffff ? 2 : 1;
  }
  parts.push(json.slice(start));
  return parts;
}

export function partKeys(id: string, parts: number): string[] {
  return Array.from({ length: parts }, (_, index) => `${PAYLOAD_KEY_PREFIX}${id}:${index}`);
}

/** 从 `payload:<id>:<n>` 键还原载荷 id（id 内可含冒号，按最后一个冒号切）。 */
export function payloadIdFromKey(key: string): string | null {
  if (!key.startsWith(PAYLOAD_KEY_PREFIX)) return null;
  const body = key.slice(PAYLOAD_KEY_PREFIX.length);
  const split = body.lastIndexOf(":");
  return split > 0 ? body.slice(0, split) : null;
}

/** v1 行 → 目标形状 checkpoint：保留旧 id/time，文本只生成 text 块，不补造 reasoning/tool/usage。 */
export function checkpointFromLegacyRecord(record: SessionRecord, schemaVersions: { chat: number; session: number }): DisplayCheckpoint {
  const entries: CheckpointEntry[] = [];
  const turns: CheckpointTurn[] = [];
  const pending = new Map<string, { running: boolean; cancelled: boolean; error: CheckpointEntry["error"] }>();
  let turnId: string | null = null;
  let assistantIndex = 0;
  const openTurn = (id: string): void => {
    turnId = id;
    assistantIndex = 0;
    turns.push({ turnId: id, status: "completed", startedAt: null, completedAt: null, error: null, interactionStatus: "none" });
    pending.set(id, { running: false, cancelled: false, error: null });
  };
  const noteRow = (id: string, status: CheckpointEntry["status"], error: CheckpointEntry["error"]): void => {
    const state = pending.get(id) ?? { running: false, cancelled: false, error: null };
    if (status === "streaming" || status === "pending") state.running = true;
    if (status === "cancelled") state.cancelled = true;
    if (status === "error" && !state.error) state.error = error;
    pending.set(id, state);
  };
  for (const message of record.messages) {
    if (message.role === "user") {
      openTurn(`turn:${message.id}`);
      entries.push({
        entryId: `${turnId}:user`, id: message.id, kind: "message", turnId, role: "user", status: "completed",
        createdAt: message.createdAt, createdAtSource: "observed", completedAt: null, error: null,
        blocks: message.content ? [{ blockId: "text:0", type: "text", text: message.content }] : [],
      });
      continue;
    }
    if (!turnId) openTurn(`turn:${message.id}`);
    const owner = turnId!;
    const entryId = assistantIndex === 0 ? `${owner}:assistant` : `${owner}:assistant:${message.id}`;
    assistantIndex++;
    const status = message.status === "completed" || message.status === "cancelled" || message.status === "error"
      ? message.status : "streaming";
    const error = status === "error"
      ? { code: "execution_failed" as const, message: message.error?.trim().slice(0, 512) || "执行失败" } : null;
    noteRow(owner, status, error);
    entries.push({
      entryId, id: message.id, kind: "message", turnId: owner, role: "assistant", status,
      createdAt: message.createdAt, createdAtSource: "observed", completedAt: null, error,
      blocks: message.content ? [{ blockId: "text:0", type: "text", text: message.content }] : [],
    });
  }
  // turn 状态与其行一致：未完成行保留 running 由导入收敛为中断，错误行提升为 turn error。
  for (const turn of turns) {
    const state = pending.get(turn.turnId);
    if (!state) continue;
    if (state.running) turn.status = "running";
    else if (state.error) { turn.status = "error"; turn.error = state.error; }
    else if (state.cancelled) turn.status = "cancelled";
  }
  return {
    checkpointVersion: 1, schemaVersions, session: {},
    entries, toolCalls: [], turns, tasks: [], plansByTurn: {}, interactions: [], payloadValues: [],
  };
}

/** 索引超目标时从最旧 turn 起裁剪展示材料；最后一个 turn 本身超限时保留，不销毁内容。 */
export function fitCheckpointToIndex(
  record: Omit<StoredRecord, "payloadIndex">,
): { record: Omit<StoredRecord, "payloadIndex">; trimmedTurns: number } {
  let checkpoint = record.checkpoint;
  let trimmed = 0;
  while (recordBytes({ ...record, checkpoint }) > CHECKPOINT_INDEX_MAX_BYTES) {
    if (checkpoint.turns.length <= 1) break;
    const dropCount = Math.max(1, Math.floor(checkpoint.turns.length / 10));
    const drop = new Set(checkpoint.turns.slice(0, dropCount).map((turn) => turn.turnId));
    checkpoint = {
      ...checkpoint,
      entries: checkpoint.entries.filter((entry) => !drop.has(entry.turnId ?? "")),
      turns: checkpoint.turns.filter((turn) => !drop.has(turn.turnId)),
      legacyIncomplete: true,
      session: { ...checkpoint.session, legacyIncomplete: true },
    };
    trimmed += drop.size;
  }
  return { record: { ...record, checkpoint }, trimmedTurns: trimmed };
}

export function splitCheckpoint(checkpoint: DisplayCheckpoint): {
  index: StoredCheckpoint; payloads: Array<{ id: string; bytes: number; json: string }>;
} {
  const { payloadValues, ...index } = checkpoint;
  return { index, payloads: payloadValues.map(({ id, json }) => ({ id, bytes: bytesOf(json), json })) };
}

function parseStoredRecord(value: unknown): StoredRecord {
  const record = value as StoredRecord | null;
  if (!record || typeof record !== "object" || (record as { recordVersion?: unknown }).recordVersion !== RECORD_VERSION)
    throw new RecordFormatError(`Unsupported DO record version: ${String((record as { recordVersion?: unknown })?.recordVersion)}`);
  if (!record.chat || !record.checkpoint || !Array.isArray(record.payloadIndex))
    throw new RecordFormatError("DO record is missing required fields");
  return record;
}

export function emptyStoredRecord(chat: Chat): StoredRecord {
  return {
    recordVersion: RECORD_VERSION, chat, running: false, executionBlocked: false,
    checkpoint: {
      checkpointVersion: 1, schemaVersions: { ...SUPPORTED_SCHEMA_VERSIONS }, session: {},
      entries: [], toolCalls: [], turns: [], tasks: [], plansByTurn: {}, interactions: [],
    },
    payloadIndex: [],
  };
}

/**
 * recordVersion 适配器：v2 直读；v1 加载时转换（幂等、先备份）；分段与索引提交都在此收敛。
 */
export class ChatRecordStore {
  constructor(private readonly storage: RecordStorage, private readonly now: () => number = Date.now) {}

  async load(): Promise<{ record: StoredRecord | null; source: "v2" | "converted" | "none"; interrupted: boolean }> {
    const existing = await this.storage.get<unknown>(RECORD_KEY);
    if (existing !== undefined && existing !== null) {
      const record = parseStoredRecord(existing);
      // 转换中断遗留的旧键：转换已完成，旧键按垃圾回收。
      if (await this.storage.get(LEGACY_RECORD_KEY) !== undefined) await this.storage.delete([LEGACY_RECORD_KEY]);
      return { record, source: "v2", interrupted: false };
    }
    const legacy = await this.storage.get<SessionRecord>(LEGACY_RECORD_KEY);
    if (!legacy || typeof legacy !== "object" || !Array.isArray(legacy.messages) || !legacy.chat)
      return { record: null, source: "none", interrupted: false };
    const record = await this.convert(legacy);
    return { record, source: "converted", interrupted: legacy.running === true };
  }

  /** v1 → v2：备份、裁剪到索引目标、导入校验、分段与索引提交、删除旧键；重复调用幂等。 */
  async convert(legacy: SessionRecord): Promise<StoredRecord> {
    const existing = await this.storage.get<unknown>(RECORD_KEY);
    if (existing !== undefined && existing !== null) return parseStoredRecord(existing);
    if (await this.storage.get<StoredBackup>(BACKUP_KEY) === undefined) {
      await this.storage.put({ [BACKUP_KEY]: { record: legacy, convertedAt: this.now() } satisfies StoredBackup });
    }
    let checkpoint: DisplayCheckpoint;
    try {
      const built = checkpointFromLegacyRecord(legacy, { ...SUPPORTED_SCHEMA_VERSIONS });
      const fitted = fitCheckpointToIndex({
        recordVersion: RECORD_VERSION, chat: legacy.chat, running: false,
        executionBlocked: legacy.running === true || legacy.executionBlocked === true, checkpoint: built,
      }).record;
      const imported = SessionDocs.fromCheckpoint(fitted.checkpoint);
      checkpoint = imported.exportCheckpoint();
      imported.destroy();
    } catch (error) {
      throw new RecordFormatError(`Legacy record cannot be converted: ${error instanceof Error ? error.message : String(error)}`);
    }
    const record = await this.commit({
      chat: legacy.chat, running: false,
      executionBlocked: legacy.running === true || legacy.executionBlocked === true, checkpoint,
    });
    await this.storage.delete([LEGACY_RECORD_KEY]);
    return record;
  }

  /** 写入顺序：新分段 → 索引提交；随后按新旧索引差删除被替换的段。 */
  async commit(input: {
    chat: Chat; running: boolean; executionBlocked: boolean; checkpoint: DisplayCheckpoint;
  }): Promise<StoredRecord> {
    const previous = await this.storage.get<StoredRecord>(RECORD_KEY).catch(() => undefined);
    const previousIds = new Set((previous?.payloadIndex ?? []).map((entry) => entry.id));
    const { index, payloads } = splitCheckpoint(input.checkpoint);
    const entries: Record<string, unknown> = {};
    const payloadIndex: PayloadIndexEntry[] = [];
    for (const payload of payloads) {
      const parts = splitPayload(payload.json);
      partKeys(payload.id, parts.length).forEach((key, partIndex) => { entries[key] = parts[partIndex]; });
      payloadIndex.push({ id: payload.id, bytes: payload.bytes, parts: parts.length });
    }
    const next: StoredRecord = {
      recordVersion: RECORD_VERSION, chat: input.chat, running: input.running,
      executionBlocked: input.executionBlocked, checkpoint: index, payloadIndex,
    };
    const indexBytes = recordBytes(next);
    if (indexBytes > INDEX_PLATFORM_SAFETY_BYTES) {
      throw new RecordCapacityError(`展示索引 ${indexBytes} 字节超过平台单值安全上限 ${INDEX_PLATFORM_SAFETY_BYTES}`);
    }
    if (Object.keys(entries).length) await this.storage.put(entries);
    await this.storage.put({ [RECORD_KEY]: next });
    const dropped = [...previousIds].filter((id) => !payloadIndex.some((entry) => entry.id === id));
    if (dropped.length) await this.removePayloads(dropped);
    return next;
  }

  /** 加载时按索引回填分段全文：引用必须可解析，否则显式失败（不伪装空历史）。 */
  async hydrate(record: StoredRecord): Promise<DisplayCheckpoint> {
    const payloadValues: Array<{ id: string; json: string }> = [];
    for (const entry of record.payloadIndex) {
      const result = await this.readPayload(entry.id);
      if (result.status !== "ok") throw new RecordCorruptionError(entry.id, result.status);
      payloadValues.push({ id: entry.id, json: result.json });
    }
    return { ...record.checkpoint, payloadValues };
  }

  async readPayload(id: string): Promise<PayloadReadResult> {
    const record = await this.storage.get<StoredRecord>(RECORD_KEY);
    const entry = record?.payloadIndex?.find((item) => item.id === id);
    if (!entry) return { status: "unknown" };
    const stored = await this.storage.list({ prefix: `${PAYLOAD_KEY_PREFIX}${id}:` });
    const parts = [...stored.entries()]
      .sort(([left], [right]) => Number(left.slice(left.lastIndexOf(":") + 1)) - Number(right.slice(right.lastIndexOf(":") + 1)))
      .map(([, value]) => String(value));
    if (parts.length !== entry.parts) return { status: "missing" };
    const json = parts.join("");
    if (bytesOf(json) !== entry.bytes) return { status: "mismatch" };
    return { status: "ok", bytes: entry.bytes, json };
  }

  /** 孤儿段与过期备份回收；返回删除计数。 */
  async sweep(): Promise<{ orphanParts: number; expiredBackup: boolean }> {
    const record = await this.storage.get<StoredRecord>(RECORD_KEY);
    const referenced = new Set((record?.payloadIndex ?? []).map((entry) => entry.id));
    const stored = await this.storage.list({ prefix: PAYLOAD_KEY_PREFIX });
    const orphans = [...stored.keys()].filter((key) => {
      const id = payloadIdFromKey(key);
      return id === null || !referenced.has(id);
    });
    if (orphans.length) await this.storage.delete(orphans);
    let expiredBackup = false;
    const backup = await this.storage.get<StoredBackup>(BACKUP_KEY);
    if (backup && typeof backup.convertedAt === "number" && this.now() - backup.convertedAt > RECORD_BACKUP_TTL_MS) {
      await this.storage.delete([BACKUP_KEY]);
      expiredBackup = true;
    }
    return { orphanParts: orphans.length, expiredBackup };
  }

  async hasBackup(): Promise<boolean> {
    return (await this.storage.get<StoredBackup>(BACKUP_KEY)) !== undefined;
  }

  /** 回滚还原：备份写回 v1 键，删除 v2 索引与其全部分段。 */
  async restoreBackup(): Promise<boolean> {
    const backup = await this.storage.get<StoredBackup>(BACKUP_KEY);
    if (!backup || !backup.record) return false;
    await this.storage.put({ [LEGACY_RECORD_KEY]: backup.record });
    const stored = await this.storage.list({ prefix: PAYLOAD_KEY_PREFIX });
    await this.storage.delete([RECORD_KEY, ...stored.keys()]);
    return true;
  }

  private async removePayloads(ids: string[]): Promise<void> {
    const keys: string[] = [];
    for (const id of ids) {
      const stored = await this.storage.list({ prefix: `${PAYLOAD_KEY_PREFIX}${id}:` });
      keys.push(...stored.keys());
    }
    if (keys.length) await this.storage.delete(keys);
  }
}
