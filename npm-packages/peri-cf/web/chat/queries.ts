import { QueryClient, queryOptions } from '@tanstack/react-query';
import type { Chat, ChatDetail } from './types';

export interface ChatReadApi {
  list(signal?: AbortSignal): Promise<Chat[]>;
  detail(chatId: string, signal?: AbortSignal): Promise<ChatDetail>;
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
  });

  const history = (chatId: string) => queryOptions({
    queryKey: historyKey(chatId),
    queryFn: ({ signal }) => api.detail(chatId, signal),
  });

  return {
    client,
    list,
    history,
    setHistory(detail: ChatDetail) {
      client.setQueryData(historyKey(detail.chat.id), detail);
    },
    mergeChat(chat: Chat) {
      client.setQueryData<Chat[]>(listKey, previous => [...(previous || []).filter(item => item.id !== chat.id), chat]
        .sort((left, right) => Date.parse(right.updatedAt) - Date.parse(left.updatedAt)));
    },
    async readHistory(chatId: string, signal?: AbortSignal, fresh = true): Promise<ChatDetail> {
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
