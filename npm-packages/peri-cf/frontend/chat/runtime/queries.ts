import { QueryClient, queryOptions } from '@tanstack/react-query';
import type { Chat, ChatSummary } from './types';
import { turnPresentationFacts, type SyncState } from '@core/shared';

export interface ChatReadApi {
  list(signal?: AbortSignal): Promise<ChatSummary[]>;
  detail(chatId: string, signal?: AbortSignal): Promise<SyncState>;
}

/** 当前投影对清单条目的实时事实；选中会话以它覆盖列表缓存，不保留陈旧值。 */
export interface ChatLiveFacts {
  messageCount: number;
  running: boolean;
  pendingInteractionCount: number;
  lastTurnStatus: string | null;
  lastCompletedAt: string | null;
}

/** 与 `ChatProjection.summary` 同语义：共享 `turnPresentationFacts` 单一规则。 */
export function summaryFacts(state: SyncState): ChatLiveFacts {
  const facts = turnPresentationFacts(state.turns, state.activeTurn.turnId);
  return {
    messageCount: state.entries.filter(entry => entry.kind === 'message').length,
    running: state.running,
    pendingInteractionCount: state.pendingInteractions.length,
    lastTurnStatus: facts.current?.status ?? null,
    lastCompletedAt: facts.lastCompletedAt,
  };
}

/** 有本地活动或已缓存运行态时的高频轮询：尽快反映服务端投影。 */
export const LIST_POLL_INTERVAL_MS = 4_000;
/** 无本地活动时的低频轮询：发现外部客户端启动/结算的后台运行。 */
export const LIST_POLL_IDLE_INTERVAL_MS = 15_000;
/** 发送/切换后的观察窗口：给服务端投影与外部客户端留出反映运行态的时间。 */
export const LIST_POLL_ACTIVITY_MS = 15_000;

export interface ListPollingOptions {
  /** 当前选中会话在客户端已进入运行阶段（发送乐观态）。 */
  liveRunning?: boolean;
  /** 命令在途或刚刚活动；用时间戳保持轮询，不依赖缓存里的 running。 */
  activeUntil?: number;
  now?: number;
  /** 可见 tab 才轮询；隐藏 tab 交由聚焦恢复时补一次，避免后台空转。 */
  visible?: boolean;
}

/**
 * 列表轮询节奏：可见 tab 空闲时 15s 低频发现外部启动；有本地活动或缓存 running 时 4s。
 * 隐藏 tab 返回 false，不声称有服务端推送。
 */
export function listRefetchInterval(
  chats: readonly ChatSummary[] | undefined, options: ListPollingOptions = {},
): number | false {
  if (options.visible === false) return false;
  if (options.liveRunning === true) return LIST_POLL_INTERVAL_MS;
  if ((options.activeUntil ?? 0) > (options.now ?? Date.now())) return LIST_POLL_INTERVAL_MS;
  return chats?.some(chat => chat.running) ? LIST_POLL_INTERVAL_MS : LIST_POLL_IDLE_INTERVAL_MS;
}

/** 列表合并的单一权威规则：标题/时间是列表事实，计数只增，运行态由实时投影覆盖。 */
export function mergeChatSummary(
  existing: ChatSummary | undefined, chat: Chat, facts?: ChatLiveFacts,
): ChatSummary {
  return {
    id: chat.id,
    title: existing?.title ?? chat.title,
    updatedAt: existing?.updatedAt ?? chat.updatedAt,
    messageCount: Math.max(existing?.messageCount ?? 0, facts?.messageCount ?? 0),
    running: facts ? facts.running : existing?.running ?? false,
    pendingInteractionCount: facts ? facts.pendingInteractionCount : existing?.pendingInteractionCount ?? 0,
    lastTurnStatus: facts ? facts.lastTurnStatus : existing?.lastTurnStatus ?? null,
    lastCompletedAt: facts ? facts.lastCompletedAt : existing?.lastCompletedAt ?? null,
  };
}

const listKey = ['chats', 'list'] as const;
const historyKey = (chatId: string) => ['chats', 'history', chatId] as const;

export function createChatQueries(api: ChatReadApi) {
  const client = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        retryOnMount: false,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        refetchOnMount: false,
        networkMode: 'always',
        staleTime: Infinity,
      },
      mutations: { retry: false },
    },
  });

  const list = queryOptions({
    queryKey: listKey,
    queryFn: async ({ signal }) => (await api.list(signal))
      .sort((left, right) => Date.parse(right.updatedAt) - Date.parse(left.updatedAt)),
    refetchInterval: query => listRefetchInterval(query.state.data),
  });

  const history = (chatId: string) => queryOptions({
    queryKey: historyKey(chatId),
    queryFn: ({ signal }) => api.detail(chatId, signal),
  });

  return {
    client,
    list,
    history,
    setHistory(detail: SyncState) {
      client.setQueryData(historyKey(detail.chat.id), detail);
    },
    mergeChat(chat: Chat, facts?: ChatLiveFacts) {
      client.setQueryData<ChatSummary[]>(listKey, previous => {
        const existing = previous?.find(item => item.id === chat.id);
        const merged = mergeChatSummary(existing, chat, facts);
        return [...(previous || []).filter(item => item.id !== chat.id), merged]
          .sort((left, right) => Date.parse(right.updatedAt) - Date.parse(left.updatedAt));
      });
    },
    /** 发送成功后的乐观运行态：服务端投影到达前先让列表/侧栏显示运行中。 */
    markRunning(chatId: string) {
      client.setQueryData<ChatSummary[]>(listKey, previous => previous?.map(item =>
        item.id === chatId ? { ...item, running: true } : item));
    },
    async readHistory(chatId: string, signal?: AbortSignal, fresh = true): Promise<SyncState> {
      signal?.throwIfAborted();
      const cancel = () => { void client.cancelQueries({ queryKey: historyKey(chatId), exact: true }); };
      signal?.addEventListener('abort', cancel, { once: true });
      try {
        const detail = await client.fetchQuery({ ...history(chatId), staleTime: fresh ? 0 : Infinity });
        signal?.throwIfAborted();
        return detail;
      } finally {
        signal?.removeEventListener('abort', cancel);
      }
    },
    dispose() {
      const cancelled = client.cancelQueries();
      client.clear();
      return cancelled;
    },
  };
}
