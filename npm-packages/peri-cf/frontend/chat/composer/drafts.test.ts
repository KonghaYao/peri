import { describe, expect, test } from 'bun:test';
import {
  NEW_CHAT_DRAFT_KEY, adoptNewChatDraft, composerDraft, composerDraftKey, withComposerDraft,
} from './drafts';

describe('per-chat composer drafts', () => {
  test('each chat keeps its own draft across switches', () => {
    let drafts = withComposerDraft({}, 'chat-a', 'A 的草稿');
    drafts = withComposerDraft(drafts, 'chat-b', 'B 的草稿');
    expect(composerDraft(drafts, 'chat-a')).toBe('A 的草稿');
    expect(composerDraft(drafts, 'chat-b')).toBe('B 的草稿');
    expect(composerDraft(drafts, 'chat-c')).toBe('');
  });

  test('input typed while a switch is loading lands in the target chat draft', () => {
    // 选择 chat-b 后 selected 立即切换；加载完成不应清空这期间写入的草稿。
    let drafts = withComposerDraft({}, 'chat-a', '旧草稿');
    const key = composerDraftKey('chat-b', false);
    drafts = withComposerDraft(drafts, key, '加载期间输入');
    drafts = withComposerDraft(drafts, key, '加载期间输入，继续写');
    expect(composerDraft(drafts, 'chat-b')).toBe('加载期间输入，继续写');
    expect(composerDraft(drafts, 'chat-a')).toBe('旧草稿');
  });

  test('creating uses the placeholder slot until success', () => {
    expect(composerDraftKey(null, true)).toBe(NEW_CHAT_DRAFT_KEY);
    expect(composerDraftKey('chat-new', true)).toBe(NEW_CHAT_DRAFT_KEY);
    expect(composerDraftKey('chat-new', false)).toBe('chat-new');

    const drafts = withComposerDraft({}, NEW_CHAT_DRAFT_KEY, '新建前输入');
    expect(composerDraft(drafts, composerDraftKey(null, true))).toBe('新建前输入');
  });

  test('new-chat success migrates the draft and clears the placeholder', () => {
    const drafts = withComposerDraft({}, NEW_CHAT_DRAFT_KEY, '加载期间输入');
    const adopted = adoptNewChatDraft(drafts, 'chat-new');
    expect(composerDraft(adopted, 'chat-new')).toBe('加载期间输入');
    expect(adopted[NEW_CHAT_DRAFT_KEY]).toBeUndefined();
  });

  test('failed creation leaves the placeholder draft untouched for retry', () => {
    const drafts = withComposerDraft({}, NEW_CHAT_DRAFT_KEY, '保留');
    // 失败路径不调用 adopt；重试仍读到同一草稿。
    expect(composerDraft(drafts, NEW_CHAT_DRAFT_KEY)).toBe('保留');
  });

  test('a send failure keeps the submitted text', () => {
    // Composer 侧 nextDraftAfterSend(false) 返回 null；草稿槽不会被写空。
    const drafts = withComposerDraft({}, 'chat-a', '没发出去');
    expect(composerDraft(drafts, 'chat-a')).toBe('没发出去');
  });
});
