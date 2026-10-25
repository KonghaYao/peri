import { QueryClient, queryOptions } from '@tanstack/react-query';
import type { ChatSummary, ChatDisplayState } from './types';

/** 列表读面只返回目录事实；运行态来自选中会话的副本。 */
export interface ChatReadApi {
  list(signal?: AbortSignal): Promise<ChatSummary[]>;
}

/** 有本地活动时的高频轮询：尽快发现服务端目录变化（外部客户端新建/结算）。 */
export const LIST_POLL_INTERVAL_MS = 4_000;
/** 无本地活动时的低频轮询：没有服务端目录推送，只做事实刷新。 */
export const LIST_POLL_IDLE_INTERVAL_MS = 15_000;
/** 发送/切换后的观察窗口：给目录写入留出反映时间。 */
export const LIST_POLL_ACTIVITY_MS = 15_000;

export interface ListPollingOptions {
  /** 命令在途或刚刚活动；用时间戳保持轮询，不依赖列表伪造的运行态。 */
  activeUntil?: number;
  now?: number;
  /** 可见 tab 才轮询；隐藏 tab 交由聚焦恢复时补一次。 */
  visible?: boolean;
}

/**
 * 列表轮询节奏：只由可见性和本地活动决定。
 * 列表不携带每会话运行态，因此不能用 running 字段提高频率。
 */
export function listRefetchInterval(
  _chats: readonly ChatSummary[] | undefined, options: ListPollingOptions = {},
): number | false {
  if (options.visible === false) return false;
  if ((options.activeUntil ?? 0) > (options.now ?? Date.now())) return LIST_POLL_INTERVAL_MS;
  return LIST_POLL_IDLE_INTERVAL_MS;
}

const listKey = ['chats', 'list'] as const;
/** 展示副本按会话身份 + 绑定代次分代；换代后旧缓存不得冒充当前画面。 */
export const historyKey = (chatId: string, bindingRevision: number) =>
  ['chats', 'history', chatId, bindingRevision] as const;

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
  });

  return {
    client,
    list,
    /**
     * 写入副本物化结果；同一会话只保留当前绑定代次，旧代次缓存立即删除。
     * readiness `unsupported` 也缓存，避免 UI 把它误当成空历史。
     */
    writeHistory(state: ChatDisplayState): void {
      if (!state.sessionId || state.bindingRevision === null) return;
      const key = historyKey(state.sessionId, state.bindingRevision);
      client.setQueryData(key, state);
      for (const query of client.getQueryCache().findAll({ queryKey: ['chats', 'history', state.sessionId] })) {
        if (query.queryKey.at(-1) !== state.bindingRevision) client.removeQueries({ queryKey: query.queryKey, exact: true });
      }
    },
    readHistory(chatId: string, bindingRevision: number): ChatDisplayState | undefined {
      return client.getQueryData<ChatDisplayState>(historyKey(chatId, bindingRevision));
    },
    dispose() {
      const cancelled = client.cancelQueries();
      client.clear();
      return cancelled;
    },
  };
}
