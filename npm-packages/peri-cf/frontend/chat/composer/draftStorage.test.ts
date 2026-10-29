import { describe, expect, test } from 'bun:test';
import { CHAT_SEND_TEXT_MAX_LENGTH } from '@peri-code/sdk';
import {
  clearComposerDrafts, loadComposerDrafts, saveComposerDrafts,
  COMPOSER_DRAFT_STORAGE_KEY, COMPOSER_DRAFT_STORAGE_MAX_BYTES, COMPOSER_DRAFT_STORAGE_VERSION,
  type DraftStorageLike,
} from './draftStorage';

class FakeStorage implements DraftStorageLike {
  readonly values = new Map<string, string>();
  failWrites = false;
  failReads = false;

  getItem(key: string): string | null {
    if (this.failReads) throw new Error('read denied');
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    if (this.failWrites) throw new Error('quota exceeded');
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }
}

describe('composer draft storage', () => {
  test('round-trips drafts and ignores empty slots', () => {
    const storage = new FakeStorage();
    expect(saveComposerDrafts({ 'chat-a': '草稿 A', 'chat-b': '' }, storage, 10)).toBe(true);
    expect(loadComposerDrafts(storage)).toEqual({ 'chat-a': '草稿 A' });

    const raw = JSON.parse(storage.values.get(COMPOSER_DRAFT_STORAGE_KEY)!);
    expect(raw.version).toBe(COMPOSER_DRAFT_STORAGE_VERSION);
    expect(raw.drafts['chat-a']).toEqual({ text: '草稿 A', updatedAt: 10 });
  });

  test('falls back to empty drafts for corrupted JSON, wrong version and denied reads', () => {
    const storage = new FakeStorage();
    storage.values.set(COMPOSER_DRAFT_STORAGE_KEY, '{not json');
    expect(loadComposerDrafts(storage)).toEqual({});

    storage.values.set(COMPOSER_DRAFT_STORAGE_KEY, JSON.stringify({ version: 99, drafts: {} }));
    expect(loadComposerDrafts(storage)).toEqual({});

    storage.failReads = true;
    expect(loadComposerDrafts(storage)).toEqual({});
    expect(saveComposerDrafts({ a: 'x' }, storage)).toBe(false);
  });

  test('does not persist drafts above the SDK send limit', () => {
    const storage = new FakeStorage();
    saveComposerDrafts({ big: 'x'.repeat(CHAT_SEND_TEXT_MAX_LENGTH + 1), ok: 'small' }, storage, 1);
    expect(loadComposerDrafts(storage)).toEqual({ ok: 'small' });
  });

  test('evicts the least recently used slots when over capacity', () => {
    const storage = new FakeStorage();
    expect(saveComposerDrafts({ old: 'a'.repeat(1_000) }, storage, 1)).toBe(true);
    const drafts: Record<string, string> = { old: 'a'.repeat(1_000) };
    for (let index = 0; index < 300; index += 1) {
      drafts[`chat-${index}`] = 'b'.repeat(1_000);
    }
    expect(saveComposerDrafts(drafts, storage, 2)).toBe(true);

    const loaded = loadComposerDrafts(storage);
    expect(loaded.old).toBeUndefined();
    expect(Object.keys(loaded).length).toBeGreaterThan(0);
    expect(new TextEncoder().encode(storage.values.get(COMPOSER_DRAFT_STORAGE_KEY)!).byteLength)
      .toBeLessThanOrEqual(COMPOSER_DRAFT_STORAGE_MAX_BYTES);
  });

  test('write failures do not throw and clear removes the key', () => {
    const storage = new FakeStorage();
    storage.failWrites = true;
    expect(saveComposerDrafts({ a: 'x' }, storage)).toBe(false);

    storage.failWrites = false;
    storage.values.set(COMPOSER_DRAFT_STORAGE_KEY, '{}');
    expect(clearComposerDrafts(storage)).toBe(true);
    expect(storage.values.has(COMPOSER_DRAFT_STORAGE_KEY)).toBe(false);
    expect(clearComposerDrafts(null)).toBe(false);
    expect(loadComposerDrafts(null)).toEqual({});
    expect(saveComposerDrafts({ a: 'x' }, null)).toBe(false);
  });
});
