import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { InteractionAnswer } from '@peri-code/sdk/view';
import { createChatApplication, type ChatController } from '../../remote';
import { createChatQueries, LIST_POLL_ACTIVITY_MS, listRefetchInterval } from './queries';
import { chatPayloadReader } from './payloadReader';
import type { Chat, ChatRecovery, ChatSendOutcome, ChatSummary } from './types';

/** 错误文案单一出口：宿主字符串与 Error 用同一条清洗规则，避免控制字符进入视图。 */
function errorText(error: unknown): string {
  const text = typeof error === "string" ? error
    : error instanceof Error ? error.message : "本地宿主调用失败，请稍后重试。";
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "").slice(0, 512);
}

/** 错误横幅的重试路由：列表错误刷新列表；只有读面失败才重新加载会话。 */
export type ChatRetryAction = 'refresh-list' | 'reload-chat' | 'rebuild-chat' | 'none';

interface ChatErrorBanner {
  message: string;
  action: ChatRetryAction;
  /** 读面失败必须保留重试入口：关闭横幅后仍显示失败态。 */
  dismissible: boolean;
}

interface ChatErrorBanners {
  list?: ChatErrorBanner;
  chat?: ChatErrorBanner;
}

/**
 * 横幅与重试动作的唯一映射：列表错误与会话错误分别展示，互不覆盖。
 * 有恢复入口的错误（未知执行结果、绑定过期）必须保留动作且不可 dismiss；
 * 普通执行拒绝不是读面事实，不能靠 reload 重读同一错误，因此不提供重试。
 */
export function errorBanners(input: {
  listError: string;
  chatError: string;
  loadState: 'idle' | 'loading' | 'ready' | 'failed' | 'unsupported';
  recovery?: ChatRecovery;
}): ChatErrorBanners {
  const banners: ChatErrorBanners = {};
  if (input.listError) {
    banners.list = { message: input.listError, action: 'refresh-list', dismissible: true };
  }
  if (input.loadState === 'failed') {
    banners.chat = {
      message: input.chatError || '会话加载失败，请重新加载。',
      action: 'reload-chat',
      dismissible: false,
    };
  } else if (input.chatError) {
    const retry = input.recovery === 'reload';
    banners.chat = { message: input.chatError, action: retry ? 'reload-chat' : 'none', dismissible: !retry };
  }
  return banners;
}

/** 实时同步中断的入口：已有内容时也保留重连，不让 Header 永远停在“同步中”。 */
export function syncRetryAction(input: {
  loadState: 'idle' | 'loading' | 'ready' | 'failed' | 'unsupported';
  liveSync: 'connecting' | 'synced' | 'failed';
}): ChatRetryAction {
  if (input.loadState === 'failed') return 'reload-chat';
  if (input.loadState === 'unsupported') return 'none';
  if (input.liveSync === 'failed') return 'reload-chat';
  return 'none';
}

/** 可见性订阅：隐藏 tab 暂停低频轮询，回到页面时由 focus refetch 补一次。 */
function subscribeVisibility(listener: () => void): () => void {
  if (typeof document === 'undefined') return () => {};
  document.addEventListener('visibilitychange', listener);
  return () => document.removeEventListener('visibilitychange', listener);
}

function documentVisible(): boolean {
  return typeof document === 'undefined' || document.visibilityState !== 'hidden';
}

export function useChat() {
  const [dismissedListError, setDismissedListError] = useState(0);
  const [activityUntil, setActivityUntil] = useState(0);
  const controllerRef = useRef<ChatController | null>(null);
  const api = useMemo(() => ({
    list: (signal?: AbortSignal) => controllerRef.current?.listChats(signal) ?? Promise.resolve([]),
  }), []);
  const queries = useMemo(() => createChatQueries(api), [api]);
  const controller = useMemo(() => createChatApplication(), []);
  controllerRef.current = controller;
  const view = useSyncExternalStore(controller.subscribe, controller.getState, controller.getState);
  const visible = useSyncExternalStore(subscribeVisibility, documentVisible, () => true);
  const list = useQuery({
    ...queries.list,
    refetchInterval: query => listRefetchInterval(query.state.data, { activeUntil: activityUntil, visible }),
    // 隐藏 tab 不轮询；可见 tab 空闲时低频刷新目录事实。
    refetchIntervalInBackground: false,
    // 回到页面时立即补一次目录事实。
    refetchOnWindowFocus: true,
  }, queries.client);
  const cleanupTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const bumpActivity = useCallback(() => {
    setActivityUntil(Date.now() + LIST_POLL_ACTIVITY_MS);
  }, []);

  useEffect(() => {
    // StrictMode 会连续执行 setup → cleanup → setup：把释放推迟到下一轮事件循环，
    // 重挂载时取消，真实卸载时才释放连接与查询缓存。
    if (cleanupTimer.current !== null) {
      clearTimeout(cleanupTimer.current);
      cleanupTimer.current = null;
    }
    queries.client.mount();
    return () => {
      cleanupTimer.current = setTimeout(() => {
        cleanupTimer.current = null;
        void controller.dispose();
        queries.client.unmount();
        void queries.dispose();
      }, 0);
    };
  }, [controller, queries]);

  // 全文读取按调用时刻取当前会话；切换会话不需要重建 reader。
  const payloadReader = useMemo(() => chatPayloadReader(() => controller), [controller]);
  const answerInteraction = useCallback(async (interactionId: string, answer: InteractionAnswer) => {
    await controller.answerInteraction(interactionId, answer);
  }, [controller]);

  const refreshList = useCallback(async () => {
    await list.refetch({ cancelRefetch: true });
  }, [list.refetch]);
  const selectChat = useCallback(async (chat: ChatSummary | null) => {
    const opened = await controller.openChat(chat);
    // 切换会话时刷新目录：发现外部客户端新建/结算的后台会话。
    bumpActivity();
    void list.refetch({ cancelRefetch: true });
    return opened;
  }, [controller, bumpActivity, list.refetch]);
  const reloadChat = useCallback(async () => {
    const opened = await controller.reloadChat();
    bumpActivity();
    void list.refetch({ cancelRefetch: true });
    return opened;
  }, [controller, bumpActivity, list.refetch]);
  const rebuildChat = useCallback(async () => {
    const opened = await controller.rebuildChat();
    bumpActivity();
    void list.refetch({ cancelRefetch: true });
    return opened;
  }, [controller, bumpActivity, list.refetch]);
  const currentChatId = useCallback(() => controller.currentChatId(), [controller]);
  const send = useCallback(async (content: string): Promise<ChatSendOutcome> => {
    // 身份以控制器实际提交的会话为准：隐式创建期间即使视图已切走，也不会记错会话。
    const outcome = await controller.send(content);
    if (outcome.sessionId) bumpActivity();
    return outcome;
  }, [controller, bumpActivity]);
  const stop = useCallback(() => controller.stop(), [controller]);
  const dismissChatError = useCallback(() => controller.dismissError(), [controller]);
  const dismissListError = useCallback(() => {
    setDismissedListError(list.errorUpdatedAt);
  }, [list.errorUpdatedAt]);

  const chatError = view.error;
  const listError = list.error && dismissedListError !== list.errorUpdatedAt ? errorText(list.error) : '';
  const banners = errorBanners({
    listError, chatError, loadState: view.loadState, recovery: view.recovery,
  });

  return {
    ...view, chats: list.data ?? [], listLoading: list.isFetching,
    chatError, listError, error: chatError || listError, banners,
    payloadReader, answerInteraction,
    refreshList, selectChat, reloadChat, rebuildChat, currentChatId, send, stop,
    dismissChatError, dismissListError,
  };
}
