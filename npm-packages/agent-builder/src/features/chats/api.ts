import {
  ChatCancelResponse,
  ChatCloseResponse,
  ChatListResponse,
  ChatResponse,
  type ChatSendRequest,
  ChatSendResponse,
  type ChatSummary,
} from '@shared/chat';
import { type QueryClient, skipToken, useQuery } from '@tanstack/react-query';
import { apiRequest, apiRequestVoid, jsonInit } from '@/lib/api-client';

export async function listChats(agentId?: string): Promise<ChatSummary[]> {
  const query = agentId === undefined ? '' : `?agentId=${encodeURIComponent(agentId)}`;
  const body = await apiRequest(`/api/chats${query}`, undefined, ChatListResponse);
  return body.chats;
}

export async function createChat(agentId: string): Promise<ChatSummary> {
  const body = await apiRequest('/api/chats', jsonInit('POST', { agentId }), ChatResponse);
  return body.chat;
}

export async function sendChat(chatId: string, request: ChatSendRequest): Promise<ChatSendResponse> {
  return apiRequest(
    `/api/chats/${encodeURIComponent(chatId)}/send`,
    jsonInit('POST', request),
    ChatSendResponse,
  );
}

export async function cancelChat(chatId: string): Promise<ChatCancelResponse> {
  return apiRequest(
    `/api/chats/${encodeURIComponent(chatId)}/cancel`,
    jsonInit('POST', {}),
    ChatCancelResponse,
  );
}

export async function closeChat(chatId: string): Promise<ChatCloseResponse> {
  return apiRequest(`/api/chats/${encodeURIComponent(chatId)}/close`, { method: 'POST' }, ChatCloseResponse);
}

/** Only the host mapping is deleted; the ACP history stays in the Store. */
export async function deleteChat(chatId: string): Promise<void> {
  await apiRequestVoid(`/api/chats/${encodeURIComponent(chatId)}`, { method: 'DELETE' });
}

export function useChatList() {
  return useQuery({ queryKey: ['chats'], queryFn: () => listChats() });
}

export function chatQueryKey(chatId: string) {
  return ['chat', chatId] as const;
}

/** The create response is the freshest authority for a single chat; the list may lag behind. */
export function cacheChat(queryClient: QueryClient, chat: ChatSummary): void {
  queryClient.setQueryData(chatQueryKey(chat.chatId), chat);
}

/**
 * Single-chat cache seeded by `POST /api/chats`. There is no GET-by-id endpoint: the list is the
 * only fallback, so a page must not turn a list miss into a 404 while this cache still has the chat.
 */
export function useCachedChat(chatId: string | undefined): ChatSummary | undefined {
  const query = useQuery<ChatSummary>({
    queryKey: chatQueryKey(chatId ?? ''),
    queryFn: skipToken,
    staleTime: Number.POSITIVE_INFINITY,
    gcTime: Number.POSITIVE_INFINITY,
  });
  return query.data;
}
