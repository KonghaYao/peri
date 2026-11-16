import { ApiErrorBody, PUBLIC_ERROR_MESSAGES } from '@shared/api';
import {
  ChatClosedEvent as ChatClosedEventSchema,
  type ChatMessage,
  type ChatMessageEvent,
  ChatMessageEvent as ChatMessageEventSchema,
  type ChatResetEvent,
  ChatResetEvent as ChatResetEventSchema,
  type ChatSnapshotEvent,
  ChatSnapshotEvent as ChatSnapshotEventSchema,
  type ChatStateEvent,
  ChatStateEvent as ChatStateEventSchema,
} from '@shared/chat';
import { useCallback, useEffect, useMemo, useReducer, useState } from 'react';

export type ChatConnection = 'connecting' | 'open' | 'reconnecting' | 'closed' | 'error';

export interface ChatStreamState {
  readonly messages: ChatMessage[];
  readonly running: boolean;
  readonly closed: boolean;
  readonly sessionId: string | null;
  readonly bindingRevision: number | null;
  readonly overflow: boolean;
  readonly lastSeq: number;
  readonly connection: ChatConnection;
  readonly error: string | null;
}

export interface ChatStreamView extends ChatStreamState {
  /** Manual retry, e.g. after an HTTP failure that is not auto-retried. */
  reconnect(): void;
}

type State = ChatStreamState & { readonly scope?: object };

type Action =
  | { readonly type: 'connecting'; readonly reconnecting: boolean }
  | { readonly type: 'open' }
  | { readonly type: 'snapshot'; readonly data: ChatSnapshotEvent; readonly seq: number | null }
  | { readonly type: 'message'; readonly data: ChatMessageEvent; readonly seq: number | null }
  | { readonly type: 'state'; readonly data: ChatStateEvent; readonly seq: number | null }
  | { readonly type: 'reset'; readonly data: ChatResetEvent; readonly seq: number | null }
  | { readonly type: 'closed'; readonly seq: number | null }
  | { readonly type: 'error'; readonly message: string };

const initialState: State = {
  messages: [],
  running: false,
  closed: false,
  sessionId: null,
  bindingRevision: null,
  overflow: false,
  lastSeq: 0,
  connection: 'connecting',
  error: null,
};

function upsertMessage(messages: ChatMessage[], next: ChatMessage): ChatMessage[] {
  const index = messages.findIndex((message) => message.id === next.id);
  if (index === -1) return [...messages, next];
  const updated = messages.slice();
  updated[index] = next;
  return updated;
}

function maxSeq(current: number, ...values: Array<number | null>): number {
  return values.reduce<number>((max, value) => (value !== null && value > max ? value : max), current);
}

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case 'connecting':
      return {
        ...state,
        connection: action.reconnecting ? 'reconnecting' : 'connecting',
        error: null,
      };
    case 'open':
      return { ...state, connection: 'open', error: null };
    case 'snapshot':
      return {
        ...state,
        messages: action.data.messages,
        running: action.data.state.running,
        closed: action.data.state.closed,
        sessionId: action.data.sessionId,
        bindingRevision: action.data.bindingRevision,
        overflow: action.data.overflow,
        lastSeq: maxSeq(state.lastSeq, action.data.lastSeq, action.seq),
        error: null,
      };
    case 'message':
      return {
        ...state,
        messages: upsertMessage(state.messages, action.data.message),
        lastSeq: maxSeq(state.lastSeq, action.seq),
      };
    case 'state':
      return {
        ...state,
        running: action.data.state.running,
        closed: action.data.state.closed,
        lastSeq: maxSeq(state.lastSeq, action.seq),
      };
    case 'reset':
      return {
        ...state,
        messages: [],
        overflow: action.data.reason === 'overflow',
        lastSeq: maxSeq(state.lastSeq, action.seq),
      };
    case 'closed':
      return { ...state, closed: true, connection: 'closed', lastSeq: maxSeq(state.lastSeq, action.seq) };
    case 'error':
      return { ...state, connection: 'error', error: action.message };
  }
}

interface ParsedChunk {
  readonly name: string;
  readonly data: unknown;
  readonly seq: number | null;
}

/** Minimal SSE field parser: enough for `id`, `event` and multi-line `data`. */
function parseChunk(chunk: string): ParsedChunk | null {
  let name = 'message';
  let seq: number | null = null;
  const data: string[] = [];

  for (const line of chunk.split(/\r?\n/)) {
    if (line.length === 0 || line.startsWith(':')) continue;
    if (line.startsWith('id:')) {
      const value = Number(line.slice(3).trim());
      if (Number.isInteger(value) && value >= 0) seq = value;
    } else if (line.startsWith('event:')) {
      name = line.slice(6).trim();
    } else if (line.startsWith('data:')) {
      data.push(line.slice(5).replace(/^ /, ''));
    }
  }

  if (data.length === 0) return null;
  try {
    return { name, data: JSON.parse(data.join('\n')) as unknown, seq };
  } catch {
    return { name, data: undefined, seq };
  }
}

async function readError(response: Response): Promise<string> {
  const payload: unknown = await response.json().catch(() => null);
  const parsed = ApiErrorBody.safeParse(payload);
  return parsed.success
    ? PUBLIC_ERROR_MESSAGES[parsed.data.error.code]
    : `对话流请求失败（HTTP ${response.status}）`;
}

function isAbort(error: unknown): boolean {
  return error instanceof Error && (error.name === 'AbortError' || error.name === 'TimeoutError');
}

function applyChunk(
  dispatch: (action: Action) => void,
  chunk: string,
  onSeq: (seq: number) => void,
  onClosed: () => void,
  chatId: string,
): void {
  const parsed = parseChunk(chunk);
  if (parsed === null) return;
  if (parsed.seq !== null) onSeq(parsed.seq);

  switch (parsed.name) {
    case 'snapshot': {
      const result = ChatSnapshotEventSchema.safeParse(parsed.data);
      if (result.success && result.data.chatId === chatId)
        dispatch({ type: 'snapshot', data: result.data, seq: parsed.seq });
      return;
    }
    case 'message': {
      const result = ChatMessageEventSchema.safeParse(parsed.data);
      if (result.success) dispatch({ type: 'message', data: result.data, seq: parsed.seq });
      return;
    }
    case 'state': {
      const result = ChatStateEventSchema.safeParse(parsed.data);
      if (result.success) dispatch({ type: 'state', data: result.data, seq: parsed.seq });
      return;
    }
    case 'reset': {
      const result = ChatResetEventSchema.safeParse(parsed.data);
      if (result.success) dispatch({ type: 'reset', data: result.data, seq: parsed.seq });
      return;
    }
    case 'closed': {
      const result = ChatClosedEventSchema.safeParse(parsed.data);
      if (result.success) {
        onClosed();
        dispatch({ type: 'closed', seq: parsed.seq });
      }
      return;
    }
    default:
      // Unknown event names are ignored so the server can add events without breaking the UI.
      return;
  }
}

async function readStream(
  body: ReadableStream<Uint8Array>,
  signal: AbortSignal,
  onChunk: (chunk: string) => void,
): Promise<void> {
  const reader = body.getReader();
  if (signal.aborted) {
    await reader.cancel();
    reader.releaseLock();
    return;
  }
  const decoder = new TextDecoder();
  const cancel = () => {
    void reader.cancel().catch(() => {});
  };
  signal.addEventListener('abort', cancel);
  let buffer = '';

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return;
      buffer += decoder.decode(value, { stream: true });
      let match = /\r?\n\r?\n/.exec(buffer);
      while (match !== null) {
        const chunk = buffer.slice(0, match.index);
        buffer = buffer.slice(match.index + match[0].length);
        onChunk(chunk);
        match = /\r?\n\r?\n/.exec(buffer);
      }
    }
  } finally {
    signal.removeEventListener('abort', cancel);
    reader.releaseLock();
  }
}

export interface UseChatStreamOptions {
  readonly chatId: string | undefined;
  readonly enabled?: boolean;
  readonly retryDelayMs?: number;
  /** The endpoint answered 404: the chat no longer exists server-side. */
  readonly onNotFound?: () => void;
}

type ScopedAction = { readonly scope: object; readonly action: Action | null };

function scopedReducer(state: State, { scope, action }: ScopedAction): State {
  if (action === null) return { ...initialState, scope };
  if (state.scope !== scope) return state;
  return reducer(state, action);
}

export function useChatStream(options: UseChatStreamOptions): ChatStreamView {
  const { chatId, enabled = true, retryDelayMs = 1_000, onNotFound } = options;
  const [generation, setGeneration] = useState(0);
  const scope = useMemo(() => ({ chatId, generation, enabled }), [chatId, generation, enabled]);
  const [state, scopedDispatch] = useReducer(scopedReducer, initialState);

  const reconnect = useCallback(() => {
    setGeneration((current) => current + 1);
  }, []);

  useEffect(() => {
    scopedDispatch({ scope, action: null });
    if (chatId === undefined || !enabled) return;

    let disposed = false;
    const dispatch = (action: Action) => {
      if (!disposed) scopedDispatch({ scope, action });
    };
    let controller: AbortController | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let lastSeq = 0;
    let closed = false;
    let retries = 0;

    const schedule = () => {
      if (disposed || closed) return;
      const delay = retryDelayMs * Math.min(2 ** retries, 8);
      retries += 1;
      dispatch({ type: 'connecting', reconnecting: true });
      timer = setTimeout(() => {
        timer = null;
        void run();
      }, delay);
    };

    const run = async (): Promise<void> => {
      if (disposed) return;
      const current = new AbortController();
      controller = current;
      if (retries === 0) dispatch({ type: 'connecting', reconnecting: false });

      const query = lastSeq > 0 ? `?sinceSeq=${lastSeq}` : '';
      const url = `/api/chats/${encodeURIComponent(chatId)}/events${query}`;

      try {
        const response = await fetch(url, {
          signal: current.signal,
          headers: { accept: 'text/event-stream' },
        });
        if (disposed || current.signal.aborted) {
          await response.body?.cancel();
          return;
        }
        if (!response.ok) {
          const message = await readError(response);
          if (disposed || current.signal.aborted) return;
          if (response.status === 404) onNotFound?.();
          dispatch({ type: 'error', message });
          return;
        }
        if (response.body === null) {
          dispatch({ type: 'error', message: '对话流没有响应内容' });
          return;
        }

        retries = 0;
        dispatch({ type: 'open' });
        await readStream(response.body, current.signal, (chunk) => {
          if (disposed) return;
          applyChunk(
            dispatch,
            chunk,
            (seq) => {
              lastSeq = Math.max(lastSeq, seq);
            },
            () => {
              closed = true;
            },
            chatId,
          );
        });

        if (disposed || current.signal.aborted) return;
        if (!closed) schedule();
      } catch (error) {
        if (disposed || current.signal.aborted || isAbort(error)) return;
        dispatch({
          type: 'error',
          message: '对话流连接中断',
        });
        schedule();
      }
    };

    void run();

    return () => {
      disposed = true;
      if (timer !== null) clearTimeout(timer);
      controller?.abort();
    };
  }, [chatId, enabled, retryDelayMs, scope, onNotFound]);

  return { ...(state.scope === scope ? state : initialState), reconnect };
}
