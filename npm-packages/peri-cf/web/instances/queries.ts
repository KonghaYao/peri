import { QueryClient, queryOptions } from '@tanstack/react-query';
import type { InstanceList } from '../../shared/instances';

export interface InstancesReadApi {
  list(signal?: AbortSignal): Promise<InstanceList>;
}

export function createInstanceQueries(api: InstancesReadApi) {
  const client = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        retryOnMount: false,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        networkMode: 'always',
        staleTime: 0,
      },
      mutations: { retry: false },
    },
  });

  // 观测是轮询读侧：每次刷新都重新采样，不保留陈旧数据充当实时状态。
  const list = (refetchInterval: number | false) => queryOptions({
    queryKey: ['instances', 'list'] as const,
    queryFn: ({ signal }) => api.list(signal),
    refetchInterval,
  });

  return {
    client,
    list,
    dispose() {
      const cancelled = client.cancelQueries();
      client.clear();
      return cancelled;
    },
  };
}
