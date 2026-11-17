import { afterEach, describe, expect, test } from 'bun:test';
import type { ChatMessage, ChatSnapshotEvent } from '@shared/chat';
import { installDom } from './dom';

installDom();

const { act, cleanup, render, screen, waitFor } = await import('@testing-library/react');
const { useChatStream } = await import('../../src/features/chats/use-chat-stream');

/** Stubs must not outlive the file: other test files rely on the real fetch. */
const nativeFetch = globalThis.fetch;

interface FakeSse {
  readonly response: Response;
  send(chunk: string): void;
  end(): void;
}

function createSse(): FakeSse {
  let controller: ReadableStreamDefaultController<Uint8Array> | null = null;
  const body = new ReadableStream<Uint8Array>({
    start(next) {
      controller = next;
    },
  });
  const encoder = new TextEncoder();
  return {
    response: new Response(body, {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    }),
    send(chunk) {
      controller?.enqueue(encoder.encode(chunk));
    },
    end() {
      controller?.close();
    },
  };
}

function frame(seq: number, event: string, data: unknown): string {
  return `id: ${seq}\nevent: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

function snapshot(messages: ChatMessage[], lastSeq: number): ChatSnapshotEvent {
  return {
    chatId: 'chat-1',
    sessionId: 'session-1',
    bindingRevision: 1,
    messages,
    state: { running: false, closed: false },
    lastSeq,
    overflow: false,
  };
}

function message(id: string, role: ChatMessage['role'], text: string): ChatMessage {
  return { id, role, text, status: 'streaming', createdAt: null };
}

let fetches: string[] = [];
let streams: FakeSse[] = [];

function stubFetch(): void {
  fetches = [];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
    fetches.push(url);
    const next = streams.shift();
    if (!next) throw new Error(`unexpected stream request: ${url}`);
    return next.response;
  }) as unknown as typeof fetch;
}

function Harness({ retryDelayMs = 10 }: { readonly retryDelayMs?: number }) {
  const stream = useChatStream({ chatId: 'chat-1', retryDelayMs });
  return (
    <div>
      <span data-testid="connection">{stream.connection}</span>
      <span data-testid="last-seq">{stream.lastSeq}</span>
      <ul>
        {stream.messages.map((item) => (
          <li key={item.id} data-testid="message">
            {item.id}|{item.role}|{item.text}
          </li>
        ))}
      </ul>
    </div>
  );
}

afterEach(() => {
  cleanup();
  streams = [];
  fetches = [];
  globalThis.fetch = nativeFetch;
});

describe('useChatStream', () => {
  test('appends chunks, updates the same message id and ignores duplicates', async () => {
    streams = [createSse()];
    const sse = streams[0];
    if (!sse) throw new Error('missing stream');
    stubFetch();
    render(<Harness />);

    await waitFor(() => expect(screen.getByTestId('connection').textContent).toBe('open'));

    await act(async () => {
      sse.send(frame(1, 'snapshot', snapshot([message('turn:1:user', 'user', 'hello')], 1)));
    });
    await waitFor(() => expect(screen.getAllByTestId('message')).toHaveLength(1));

    await act(async () => {
      sse.send(frame(2, 'message', { message: message('turn:1:assistant', 'assistant', 'echo: ') }));
    });
    await act(async () => {
      sse.send(frame(3, 'message', { message: message('turn:1:assistant', 'assistant', 'echo: hello') }));
    });

    await waitFor(() => expect(screen.getAllByTestId('message')).toHaveLength(2));
    expect(screen.getByText('turn:1:assistant|assistant|echo: hello')).toBeTruthy();
    expect(screen.getByTestId('last-seq').textContent).toBe('3');

    await act(async () => {
      sse.send(frame(3, 'message', { message: message('turn:1:assistant', 'assistant', 'echo: hello') }));
    });
    expect(screen.getAllByTestId('message')).toHaveLength(2);
  });

  test('reconnects with sinceSeq after an abrupt stream end and keeps one copy of messages', async () => {
    const first = createSse();
    const second = createSse();
    streams = [first, second];
    stubFetch();
    render(<Harness />);

    await waitFor(() => expect(screen.getByTestId('connection').textContent).toBe('open'));
    await act(async () => {
      first.send(frame(1, 'snapshot', snapshot([message('turn:1:user', 'user', 'hello')], 1)));
    });
    await waitFor(() => expect(screen.getAllByTestId('message')).toHaveLength(1));

    first.end();

    await waitFor(() => expect(fetches).toHaveLength(2));
    expect(fetches[1]).toContain('sinceSeq=1');
    await waitFor(() => expect(screen.getByTestId('connection').textContent).toBe('open'));

    await act(async () => {
      second.send(
        frame(
          2,
          'snapshot',
          snapshot(
            [
              message('turn:1:user', 'user', 'hello'),
              message('turn:1:assistant', 'assistant', 'echo: hello'),
            ],
            2,
          ),
        ),
      );
    });

    await waitFor(() => expect(screen.getByText('turn:1:assistant|assistant|echo: hello')).toBeTruthy());
    expect(screen.getAllByTestId('message')).toHaveLength(2);
  });

  test('does not reconnect after a closed event', async () => {
    const sse = createSse();
    streams = [sse];
    stubFetch();
    render(<Harness />);

    await waitFor(() => expect(screen.getByTestId('connection').textContent).toBe('open'));
    await act(async () => {
      sse.send(frame(1, 'closed', { reason: null }));
    });
    await waitFor(() => expect(screen.getByTestId('connection').textContent).toBe('closed'));

    sse.end();
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(fetches).toHaveLength(1);
  });
});
