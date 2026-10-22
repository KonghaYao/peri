import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { InteractionAnswer } from '@peri-code/sdk/view';
import { createRemoteAgent, type AgentManager } from '../../remote';
import { errorText } from '@core/agent';
import { createChatQueries, LIST_POLL_ACTIVITY_MS, listRefetchInterval, summaryFacts } from './queries';
import { commandPayloadReader } from './payloadReader';
import type { Chat } from './types';

/** 错误横幅的重试路由：列表错误刷新列表；只有读面失败才重新加载会话。 */
export type ChatRetryAction = 'refresh-list' | 'reload-chat' | 'none';

export interface ChatErrorBanner {
  message: string;
  action: ChatRetryAction;
  /** 读面失败必须保留重试入口：关闭横幅后仍显示失败态。 */
  dismissible: boolean;
}

export interface ChatErrorBanners {
  list?: ChatErrorBanner;
  chat?: ChatErrorBanner;
}

/**
 * 横幅与重试动作的唯一映射：列表错误与会话错误分别展示，互不覆盖。
 * 执行错误不是读面事实，不能靠 reload 重读同一错误，因此不提供重试。
 */
export function errorBanners(input: {
  listError: string;
  chatError: string;
  loadState: 'idle' | 'loading' | 'ready' | 'failed';
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
    banners.chat = { message: input.chatError, action: 'none', dismissible: true };
  }
  return banners;
}

/** 实时事件流中断的入口：已有内容时也保留重连，不让 Header 永远停在“同步中”。 */
export function syncRetryAction(input: {
  loadState: 'idle' | 'loading' | 'ready' | 'failed';
  liveSync: 'connecting' | 'synced' | 'failed';
}): ChatRetryAction {
  if (input.loadState === 'failed') return 'reload-chat';
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
  const agentRef = useRef<AgentManager | null>(null);
  agentRef.current ??= createRemoteAgent();
  const agent = agentRef.current;
  const api = useMemo(() => ({
    list: (signal?: AbortSignal) => agent.listChats(signal),
    detail: (chatId: string, signal?: AbortSignal) => agent.readChat(chatId, signal),
  }), [agent]);
  const queries = useMemo(() => createChatQueries(api), [api]);
  const view = useSyncExternalStore(agent.subscribe, agent.getState, agent.getState);
  const visible = useSyncExternalStore(subscribeVisibility, documentVisible, () => true);
  const runningPhase = view.phase === 'streaming' || view.phase === 'cancelling' || view.phase === 'blocked';
  const list = useQuery({
    ...queries.list,
    refetchInterval: query => listRefetchInterval(query.state.data, {
      liveRunning: runningPhase, activeUntil: activityUntil, visible,
    }),
    // 隐藏 tab 不轮询；可见 tab 空闲时 15s 低频轮询，用于发现外部客户端启动的后台运行。
    // 没有服务端推送，因此外部启动最多要等一个空闲轮询周期才会出现在列表里。
    refetchIntervalInBackground: false,
    // 回到页面时立即补一次清单事实，缩短外部启动的发现延迟。
    refetchOnWindowFocus: true,
  }, queries.client);
  const cleanupTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const bumpActivity = useCallback(() => {
    setActivityUntil(Date.now() + LIST_POLL_ACTIVITY_MS);
  }, []);

  useEffect(() => {
    // StrictMode 会连续执行 setup → cleanup → setup：把释放推迟到下一轮事件循环，
    // 重挂载时取消，真实卸载时才释放宿主连接与查询缓存。
    if (cleanupTimer.current !== null) {
      clearTimeout(cleanupTimer.current);
      cleanupTimer.current = null;
    }
    queries.client.mount();
    return () => {
      cleanupTimer.current = setTimeout(() => {
        cleanupTimer.current = null;
        agent.dispose();
        queries.client.unmount();
        void queries.dispose();
      }, 0);
    };
  }, [agent, queries]);

  // 会话元数据与实时运行事实变化都同步到列表缓存；列表仍是读侧缓存，不是状态权威。
  useEffect(() => {
    if (view.selected) queries.mergeChat(view.selected, view.state ? summaryFacts(view.state) : undefined);
  }, [queries, view.selected, view.state]);

  // 全文读取按调用时刻取当前会话；切换会话不需要重建 reader。
  const payloadReader = useMemo(() => commandPayloadReader(() => agent), [agent]);
  const answerInteraction = useCallback(async (interactionId: string, answer: InteractionAnswer) => {
    await agent.answerInteraction(interactionId, answer);
  }, [agent]);

  const refreshList = useCallback(async () => {
    await list.refetch({ cancelRefetch: true });
  }, [list.refetch]);
  const selectChat = useCallback(async (chat: Chat | null) => {
    const opened = await agent.openChat(chat);
    // 切换会话时刷新列表：发现外部客户端启动或刚结算的后台运行。
    bumpActivity();
    void list.refetch({ cancelRefetch: true });
    return opened;
  }, [agent, bumpActivity, list.refetch]);
  const reloadChat = useCallback(async () => {
    const opened = await agent.reloadChat();
    bumpActivity();
    void list.refetch({ cancelRefetch: true });
    return opened;
  }, [agent, bumpActivity, list.refetch]);
  const currentChatId = useCallback(() => agent.getState().selected?.id ?? null, [agent]);
  const send = useCallback(async (content: string) => {
    // 身份以 AgentManager 实际提交的会话为准：隐式创建期间即使视图已切走，也不会记错会话。
    const chatId = await agent.send(content);
    if (chatId) {
      queries.markRunning(chatId);
      bumpActivity();
    }
    return chatId;
  }, [agent, bumpActivity, queries]);
  const stop = useCallback(() => agent.stop(), [agent]);
  const dismissChatError = useCallback(() => agent.dismissError(), [agent]);
  const dismissListError = useCallback(() => {
    setDismissedListError(list.errorUpdatedAt);
  }, [list.errorUpdatedAt]);
  const dismissError = useCallback(() => {
    agent.dismissError();
    setDismissedListError(list.errorUpdatedAt);
  }, [agent, list.errorUpdatedAt]);

  const chatError = view.error;
  const listError = list.error && dismissedListError !== list.errorUpdatedAt ? errorText(list.error) : '';
  const banners = errorBanners({ listError, chatError, loadState: view.loadState });

  return {
    ...view, chats: list.data ?? [], listLoading: list.isFetching,
    chatError, listError, error: chatError || listError, banners,
    payloadReader, answerInteraction,
    refreshList, selectChat, reloadChat, currentChatId, send, stop,
    dismissError, dismissChatError, dismissListError,
  };
}
