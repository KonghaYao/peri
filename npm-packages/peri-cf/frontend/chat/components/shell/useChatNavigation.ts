import { useCallback, useMemo, useRef, useState } from 'react';
import type { ChatRetryAction, ChatSendOutcome, ChatSummary } from '../../runtime';
import type { TimelineScrollSignal } from '../../timeline';
import type { ComposerDraftHandle } from './useComposerDrafts';

/**
 * 会话导航编排：新会话、切换会话、重载/关闭重建与 409 恢复入口。
 *
 * `navigationRequest` 保留 open-token 竞态保护：被更晚导航取代的旧请求不迁移
 * 草稿、不抢焦点；发送期间的会话归属以控制器实际提交的会话为准。
 */

/** 外壳需要的最小命令面；DOM 测试可注入同形对象。 */
export interface ChatNavigationTarget {
  selectChat(target: ChatSummary | null): Promise<boolean>;
  reloadChat(): Promise<boolean>;
  rebuildChat(): Promise<boolean>;
  refreshList(): Promise<void>;
  currentChatId(): string | null;
  send(content: string): Promise<ChatSendOutcome>;
}

export interface ChatNavigationHandle {
  /** 选择/新建会话；调用方负责先关闭 sidebar。 */
  select(target: ChatSummary | null): Promise<void>;
  runAction(action: ChatRetryAction): void;
  send(content: string): Promise<ChatSendOutcome>;
  focusComposer(): void;
  focusSignal: number;
  scrollSignal: TimelineScrollSignal | null;
}

export function useChatNavigation(
  chat: ChatNavigationTarget,
  drafts: ComposerDraftHandle,
): ChatNavigationHandle {
  const [focusSignal, setFocusSignal] = useState(0);
  const [scrollSignal, setScrollSignal] = useState<TimelineScrollSignal | null>(null);
  const navigationRequest = useRef(0);
  const { selectChat, reloadChat, rebuildChat, refreshList, currentChatId, send: sendToChat } = chat;
  // 只依赖稳定的草稿命令，避免键入时重建导航回调。
  const { beginNavigation, finishNavigation, beginImplicitCreate, adoptCreatedDraft, settleSentDraft } = drafts;

  const focusComposer = useCallback(() => {
    setFocusSignal((value) => value + 1);
  }, []);

  const select = useCallback(async (target: ChatSummary | null) => {
    beginNavigation(target === null);
    const request = ++navigationRequest.current;
    const opened = await selectChat(target);
    // 被更晚的导航取代时，由后一次选择负责草稿槽与焦点。
    if (request !== navigationRequest.current) return;
    finishNavigation();
    // 创建成功但 read/watch 失败时 selected 已是新会话，草稿同样要迁移。
    adoptCreatedDraft();
    if (opened) focusComposer();
  }, [adoptCreatedDraft, beginNavigation, finishNavigation, focusComposer, selectChat]);

  const runAction = useCallback((action: ChatRetryAction) => {
    if (action === 'refresh-list') void refreshList();
    else if (action === 'reload-chat') void reloadChat().then(adoptCreatedDraft);
    else if (action === 'rebuild-chat') {
      // 关闭未确认的逃生入口：显式关闭句柄后新建会话，关闭失败保留当前会话与原因。
      beginNavigation(true);
      void rebuildChat().then((opened) => {
        finishNavigation();
        adoptCreatedDraft();
        if (opened) focusComposer();
      });
    }
  }, [adoptCreatedDraft, beginNavigation, finishNavigation, focusComposer, rebuildChat, refreshList, reloadChat]);

  const send = useCallback(async (content: string) => {
    const selectedBefore = currentChatId();
    const navigation = navigationRequest.current;
    if (!selectedBefore) beginImplicitCreate();
    // 以实际提交会话为准：隐式创建期间切走，草稿仍迁到真正收到消息的会话。
    const outcome = await sendToChat(content);
    const deliveredId = settleSentDraft({
      content,
      selectedBefore,
      outcome,
      navigationStillCurrent: navigation === navigationRequest.current,
    });
    // 信号携带实际接收会话；迟到结算由 Timeline 按当前会话过滤，不能移动别的会话。
    if (deliveredId) {
      setScrollSignal((previous) => ({ token: (previous?.token ?? 0) + 1, chatId: deliveredId }));
    }
    return outcome;
  }, [beginImplicitCreate, currentChatId, sendToChat, settleSentDraft]);

  return useMemo(() => ({
    select, runAction, send, focusComposer, focusSignal, scrollSignal,
  }), [select, runAction, send, focusComposer, focusSignal, scrollSignal]);
}
