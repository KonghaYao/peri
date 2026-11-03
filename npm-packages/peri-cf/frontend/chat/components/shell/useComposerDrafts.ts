import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  adoptNewChatDraft, clearComposerDrafts, composerDraft, composerDraftKey, loadComposerDrafts,
  saveComposerDrafts, withComposerDraft,
  type ComposerDrafts,
} from '../../composer';
import type { ChatSendOutcome } from '../../runtime';

/** 草稿写入去抖：连续输入只在停笔后写一次 localStorage。 */
const DRAFT_PERSIST_DEBOUNCE_MS = 300;

/**
 * Composer 草稿槽：按展示会话隔离，新建期间固定在占位槽。
 *
 * 只管理草稿事实，不决定导航顺序；创建/发送结算由调用方按真实会话身份传入，
 * 切换会话不互相串稿。
 */

export interface SentDraftInput {
  /** 本次提交的原始文本；用于判断成功后是否清空新槽。 */
  content: string;
  /** 提交前的会话身份；null 表示隐式新建。 */
  selectedBefore: string | null;
  outcome: ChatSendOutcome;
  /** 结算时是否仍是同一次导航；切走时不猜测草稿目标。 */
  navigationStillCurrent: boolean;
}

export interface ComposerDraftHandle {
  /** 当前草稿槽键；创建期间为占位槽。 */
  draftKey: string;
  draft: string;
  changeDraft(value: string): void;
  /** 选择提示词写入当前草稿槽；焦点由调用方处理。 */
  selectPrompt(prompt: string): void;
  /** 导航开始：新会话写入占位槽，切换既有会话清除待迁移标记。 */
  beginNavigation(newChat: boolean): void;
  finishNavigation(): void;
  /** 隐式创建（发送时还没有会话）：标记创建成功后迁移占位草稿。 */
  beginImplicitCreate(): void;
  /** 创建成功后把占位草稿迁到当前会话；无待迁移或会话未知时不变。 */
  adoptCreatedDraft(): void;
  /** 发送结算：按投递/创建结果迁移或清理草稿，返回实际送达会话。 */
  settleSentDraft(input: SentDraftInput): string | null;
  /** 清除内存与 localStorage 中的所有草稿。 */
  clearPersistedDrafts(): void;
}

export function useComposerDrafts(
  selectedChatId: string | null,
  currentChatId: () => string | null,
): ComposerDraftHandle {
  const [drafts, setDrafts] = useState<ComposerDrafts>(() => loadComposerDrafts());
  const [creating, setCreating] = useState(false);
  /** 本次导航是否在创建新会话；创建成功后即使读面失败也要迁移草稿。 */
  const pendingNewChat = useRef(false);

  useEffect(() => {
    const timer = setTimeout(() => {
      saveComposerDrafts(drafts);
    }, DRAFT_PERSIST_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [drafts]);

  // 新建期间固定使用占位草稿槽，创建成功后再迁到新会话，加载期间输入不丢。
  const draftKey = composerDraftKey(selectedChatId, creating);
  const draft = composerDraft(drafts, draftKey);

  const adoptCreatedDraft = useCallback(() => {
    if (!pendingNewChat.current) return;
    const createdId = currentChatId();
    if (!createdId) return;
    pendingNewChat.current = false;
    setDrafts((previous) => adoptNewChatDraft(previous, createdId));
  }, [currentChatId]);

  const changeDraft = useCallback((value: string) => {
    setDrafts((previous) => withComposerDraft(previous, draftKey, value));
  }, [draftKey]);

  const selectPrompt = useCallback((prompt: string) => {
    setDrafts((previous) => withComposerDraft(previous, draftKey, prompt));
  }, [draftKey]);

  const beginNavigation = useCallback((newChat: boolean) => {
    pendingNewChat.current = newChat;
    setCreating(newChat);
  }, []);

  const finishNavigation = useCallback(() => {
    setCreating(false);
  }, []);

  const beginImplicitCreate = useCallback(() => {
    pendingNewChat.current = true;
  }, []);

  const settleSentDraft = useCallback((input: SentDraftInput): string | null => {
    const deliveredId = input.outcome.delivered ? input.outcome.sessionId : null;
    // 创建成功即使用户没切走、prompt 失败/被撤回，也要把占位草稿迁到实际会话；切走时不猜测目标。
    const createdId = !deliveredId && !input.selectedBefore && input.navigationStillCurrent
      ? currentChatId()
      : null;
    const draftTarget = deliveredId ?? createdId;
    if (!input.selectedBefore && draftTarget) {
      pendingNewChat.current = false;
      setDrafts((previous) => {
        const adopted = adoptNewChatDraft(previous, draftTarget);
        if (!deliveredId) return adopted;
        // 成功后 Composer 会用旧占位槽清空；这里把刚提交的内容从新会话槽同步清掉。
        return composerDraft(adopted, draftTarget).trim() === input.content.trim()
          ? withComposerDraft(adopted, draftTarget, '')
          : adopted;
      });
    } else if (!deliveredId && !currentChatId()) {
      pendingNewChat.current = false;
    }
    return deliveredId;
  }, [currentChatId]);

  const clearPersistedDrafts = useCallback(() => {
    clearComposerDrafts();
    setDrafts({});
  }, []);

  return useMemo(() => ({
    draftKey,
    draft,
    changeDraft,
    selectPrompt,
    beginNavigation,
    finishNavigation,
    beginImplicitCreate,
    adoptCreatedDraft,
    settleSentDraft,
    clearPersistedDrafts,
  }), [
    draftKey, draft, changeDraft, selectPrompt, beginNavigation, finishNavigation,
    beginImplicitCreate, adoptCreatedDraft, settleSentDraft, clearPersistedDrafts,
  ]);
}
