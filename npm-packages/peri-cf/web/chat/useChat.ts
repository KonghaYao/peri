import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { InteractionAnswer } from '@peri-code/sdk/view';
import { ChatApi, errorText } from '../api/client';
import { createApplication } from '../runtime/application';
import { createChatQueries } from './queries';
import { commandPayloadReader } from './payloadReader';
import { ChatSession, type ChatView } from './session';
import type { Chat } from './types';

const initialView: ChatView = { selected: null, state: null, phase: 'idle', historyReady: true, error: '' };

export function useChat() {
  const [view, setView] = useState(initialView);
  const [dismissedListError, setDismissedListError] = useState(0);
  const application = useMemo(() => createApplication(), []);
  const api = useMemo(() => new ChatApi(application), [application]);
  const queries = useMemo(() => createChatQueries(api), [api]);
  const session = useRef<ChatSession | null>(null);
  const list = useQuery(queries.list, queries.client);

  useEffect(() => {
    queries.client.mount();
    setView(initialView);
    setDismissedListError(0);
    const active = new ChatSession(api, queries, setView, (chatId) => application.runtimeFor(chatId));
    session.current = active;
    return () => {
      active.dispose();
      if (session.current === active) session.current = null;
      queries.client.unmount();
      void queries.dispose();
    };
  }, [api, queries, application]);

  // 全文读取与交互回答都经当前会话的本地连接；这里按调用时刻取会话，切换会话不需要重建 reader。
  const payloadReader = useMemo(() => commandPayloadReader(() => session.current ?? undefined), []);
  const answerInteraction = useCallback(async (interactionId: string, answer: InteractionAnswer) => {
    const active = session.current;
    if (!active) throw new Error('会话尚未就绪，回答未提交，请稍后重试。');
    await active.answerInteraction(interactionId, answer);
  }, []);

  const refreshList = useCallback(async () => {
    await list.refetch({ cancelRefetch: true });
  }, [list.refetch]);
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
    payloadReader, answerInteraction,
    refreshList, selectChat, send, stop, dismissError,
  };
}
