import { QueryClient, queryOptions } from '@tanstack/react-query';
import type { ChatSummary, ProjectSummary } from './types';

/** 列表读面只返回目录事实；运行态来自选中会话的副本。 */
export interface ChatReadApi {
  list(signal?: AbortSignal): Promise<ChatSummary[]>;
  projects(signal?: AbortSignal): Promise<ProjectSummary[]>;
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
const projectsKey = ['projects', 'list'] as const;

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

  /** 项目清单变化远低于会话目录；创建后由调用方显式刷新。 */
  const projects = queryOptions({
    queryKey: projectsKey,
    queryFn: async ({ signal }) => await api.projects(signal),
  });

  return {
    client,
    list,
    projects,
    dispose() {
      const cancelled = client.cancelQueries();
      client.clear();
      return cancelled;
    },
  };
}
