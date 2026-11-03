import { CHAT_SEND_TEXT_MAX_LENGTH } from '@peri-code/sdk';
import type { ComposerDrafts } from './drafts';

/**
 * 草稿持久化：版本化 key、按会话分槽、容量上限与损坏回退。
 *
 * 只缓存非空且不超过发送上限的草稿；超限草稿留在当前输入框，不写入浏览器。
 */
export const COMPOSER_DRAFT_STORAGE_KEY = 'peri-cf:composer-drafts:v1';
export const COMPOSER_DRAFT_STORAGE_VERSION = 1;
export const COMPOSER_DRAFT_STORAGE_MAX_BYTES = 256 * 1024;

export interface DraftStorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

interface StoredDraft {
  text: string;
  updatedAt: number;
}

interface StoredEnvelope {
  version: number;
  drafts: Record<string, StoredDraft>;
}

function defaultStorage(): DraftStorageLike | null {
  try {
    if (globalThis.localStorage) return globalThis.localStorage;
  } catch {
    // 访问被策略拒绝时继续尝试 window。
  }
  try {
    return typeof window === 'undefined' ? null : window.localStorage;
  } catch {
    return null;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function parseEnvelope(raw: string | null): StoredEnvelope | null {
  if (!raw) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (!isRecord(value) || value.version !== COMPOSER_DRAFT_STORAGE_VERSION) return null;
    if (!isRecord(value.drafts)) return null;
    const drafts: Record<string, StoredDraft> = {};
    for (const [key, item] of Object.entries(value.drafts)) {
      if (!isRecord(item)) continue;
      if (typeof item.text !== 'string' || typeof item.updatedAt !== 'number') continue;
      if (
        key.length === 0 || item.text.length === 0
        || item.text.length > CHAT_SEND_TEXT_MAX_LENGTH || !Number.isFinite(item.updatedAt)
      ) continue;
      drafts[key] = { text: item.text, updatedAt: item.updatedAt };
    }
    return { version: COMPOSER_DRAFT_STORAGE_VERSION, drafts };
  } catch {
    return null;
  }
}

function byteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

/** 读取损坏、版本不符或存储不可用时返回空草稿，不阻断输入。 */
export function loadComposerDrafts(storage: DraftStorageLike | null = defaultStorage()): ComposerDrafts {
  if (!storage) return {};
  try {
    const envelope = parseEnvelope(storage.getItem(COMPOSER_DRAFT_STORAGE_KEY));
    if (!envelope) return {};
    return Object.fromEntries(Object.entries(envelope.drafts).map(([key, item]) => [key, item.text]));
  } catch {
    return {};
  }
}

/**
 * 保存草稿快照：未变化的槽保留原时间戳，变化/新增的槽刷新时间戳；
 * 总字节超过上限时按最久未使用淘汰。返回是否写入成功。
 */
export function saveComposerDrafts(
  drafts: ComposerDrafts,
  storage: DraftStorageLike | null = defaultStorage(),
  now: number = Date.now(),
): boolean {
  if (!storage) return false;
  try {
    const previous = parseEnvelope(storage.getItem(COMPOSER_DRAFT_STORAGE_KEY))?.drafts ?? {};
    const next: Record<string, StoredDraft> = {};
    for (const [key, text] of Object.entries(drafts)) {
      if (key.length === 0 || text.length === 0 || text.length > CHAT_SEND_TEXT_MAX_LENGTH) continue;
      next[key] = { text, updatedAt: previous[key]?.text === text ? previous[key]!.updatedAt : now };
    }
    if (Object.keys(next).length === 0) {
      storage.removeItem(COMPOSER_DRAFT_STORAGE_KEY);
      return true;
    }

    const encode = (value: Record<string, StoredDraft>): string =>
      JSON.stringify({ version: COMPOSER_DRAFT_STORAGE_VERSION, drafts: value });
    let serialized = encode(next);
    if (byteLength(serialized) > COMPOSER_DRAFT_STORAGE_MAX_BYTES) {
      const oldestFirst = Object.entries(next).sort((left, right) => left[1].updatedAt - right[1].updatedAt);
      for (const [key] of oldestFirst) {
        delete next[key];
        serialized = encode(next);
        if (byteLength(serialized) <= COMPOSER_DRAFT_STORAGE_MAX_BYTES) break;
      }
    }
    if (byteLength(serialized) > COMPOSER_DRAFT_STORAGE_MAX_BYTES) return false;
    storage.setItem(COMPOSER_DRAFT_STORAGE_KEY, serialized);
    return true;
  } catch {
    return false;
  }
}

/** 清除所有持久化草稿；存储不可用时返回 false。 */
export function clearComposerDrafts(storage: DraftStorageLike | null = defaultStorage()): boolean {
  if (!storage) return false;
  try {
    storage.removeItem(COMPOSER_DRAFT_STORAGE_KEY);
    return true;
  } catch {
    return false;
  }
}
