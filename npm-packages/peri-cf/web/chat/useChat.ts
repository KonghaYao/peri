import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { ChatApi, errorText } from '../api/client';
import { createChatQueries } from './queries';
import { ChatSession, type ChatView } from './session';
import type { Chat } from './types';

const initialView: ChatView = { selected: null, messages: [], phase: 'idle', historyReady: true, error: '' };

export function useChat(token: string) {
  const [view, setView] = useState(initialView);
  const [dismissedListError, setDismissedListError] = useState(0);
  const api = useMemo(() => new ChatApi(token), [token]);
  const queries = useMemo(() => createChatQueries(api), [api]);
  const session = useRef<ChatSession | null>(null);
  const list = useQuery({ ...queries.list, enabled: Boolean(token) }, queries.client);

  useEffect(() => {
    queries.client.mount();
    setView(initialView);
    setDismissedListError(0);
    const active = new ChatSession(api, queries, token, setView);
    session.current = active;
    return () => {
      active.dispose();
      if (session.current === active) session.current = null;
      queries.client.unmount();
      void queries.dispose();
    };
  }, [api, queries, token]);

  const refreshList = useCallback(async () => {
    if (token) await list.refetch({ cancelRefetch: true });
  }, [token, list.refetch]);
  const selectChat = useCallback(async (chat: Chat | null) => {
    await session.current?.select(chat);
  }, []);
  const send = useCallback((content: string) => session.current?.send(content) ?? Promise.resolve(false), []);
  const stop = useCallback(() => session.current?.stop() ?? Promise.resolve(false), []);
  const dismissError = useCallback(() => {
    session.current?.dismissError();
    setDismissedListError(list.errorUpdatedAt);
  }, [list.errorUpdatedAt]);

  return {
    ...view, chats: list.data ?? [], listLoading: list.isFetching,
    error: view.error || (list.error && dismissedListError !== list.errorUpdatedAt ? errorText(list.error) : ''),
    refreshList, selectChat, send, stop, dismissError,
  };
}
