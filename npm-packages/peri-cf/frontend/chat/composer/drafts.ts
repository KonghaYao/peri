/** 新建会话尚未拿到 ID 时的草稿槽；切换会话后各槽互不影响。 */
export const NEW_CHAT_DRAFT_KEY = '__new__';

export type ComposerDrafts = Readonly<Record<string, string>>;

/** 新建过程中始终使用占位槽，避免创建响应到达后草稿换槽而丢失输入。 */
export function composerDraftKey(chatId: string | null, creating: boolean): string {
  return creating || chatId === null ? NEW_CHAT_DRAFT_KEY : chatId;
}

export function composerDraft(drafts: ComposerDrafts, key: string): string {
  return drafts[key] ?? '';
}

export function withComposerDraft(drafts: ComposerDrafts, key: string, value: string): ComposerDrafts {
  if ((drafts[key] ?? '') === value) return drafts;
  return { ...drafts, [key]: value };
}

/**
 * 新建成功：把占位草稿迁到真实会话并清空占位槽。
 * 创建期间所有输入都写入占位槽；理论上真实会话槽不存在，若存在则保留它。
 */
export function adoptNewChatDraft(drafts: ComposerDrafts, chatId: string): ComposerDrafts {
  const pending = drafts[NEW_CHAT_DRAFT_KEY] ?? '';
  const existing = drafts[chatId] ?? '';
  const next = { ...drafts };
  delete next[NEW_CHAT_DRAFT_KEY];
  if (existing) next[chatId] = existing;
  else if (pending) next[chatId] = pending;
  else delete next[chatId];
  return next;
}
