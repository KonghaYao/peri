import { afterEach, expect, test } from 'bun:test';
import type { ChatSnapshotEvent, ChatSummary } from '@shared/chat';
import { installDom } from './dom';

installDom();
const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { act, cleanup, fireEvent, render, screen, waitFor } = await import('@testing-library/react');
const { Link, MemoryRouter, Route, Routes } = await import('react-router');
const { ChatPage } = await import('../../src/features/chats/chat-page');
const nativeFetch = globalThis.fetch;
let client: InstanceType<typeof QueryClient> | null = null;

afterEach(() => {
  cleanup();
  client?.clear();
  client = null;
  globalThis.fetch = nativeFetch;
});

function frame(event: string, data: unknown, seq = 1) {
  return new TextEncoder().encode(`id: ${seq}\nevent: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
}

function snapshot(chatId: string): ChatSnapshotEvent {
  return {
    chatId,
    sessionId: `session-${chatId}`,
    bindingRevision: 1,
    messages: [
      { id: chatId, role: 'assistant', text: `private-${chatId}`, status: 'completed', createdAt: null },
    ],
    state: { running: false, closed: false },
    lastSeq: 1,
    overflow: false,
  };
}

function setup(options: { late?: Promise<Response>; error?: Error; httpError?: string } = {}) {
  const controls = new Map<string, ReadableStreamDefaultController<Uint8Array>>();
  const canceled: string[] = [];
  const chats: ChatSummary[] = ['A', 'B', 'C'].map((id) => ({
    chatId: id,
    agentId: '00000000-0000-4000-8000-000000000001',
    agentName: `Agent${id}`,
    sessionId: null,
    createdAt: 1,
    updatedAt: 1,
    sandbox: { state: 'disabled', reason: null },
  }));
  const caps = {
    metaHarnessKeys: [],
    sendTextMaxLength: 32768,
    providers: [{ type: 'anthropic', configured: true }],
    chatRuntime: 'wasm',
    chatRuntimeReady: true,
    chatRuntimeReasons: [],
    historyPersisted: true,
    sandbox: { mode: 'disabled', reasons: [] },
  };
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 30_000 } } });
  client.setQueryData(['chats'], chats);
  client.setQueryData(['capabilities'], caps);
  globalThis.fetch = (async (input) => {
    const url = String(input);
    if (url === '/api/chats') return Response.json({ chats });
    if (url === '/api/capabilities') return Response.json(caps);
    if (url.endsWith('/A/events') && options.late) return options.late;
    if (url.includes('/events') && options.httpError)
      return Response.json(
        {
          error: {
            code: 'engine_unavailable',
            message: options.httpError,
            details: { reason: options.httpError },
            retryable: true,
          },
        },
        { status: 503 },
      );
    if (url.includes('/events') && options.error) throw options.error;
    if (url.includes('/events'))
      return new Response(
        new ReadableStream<Uint8Array>({
          start(controller) {
            controls.set(url, controller);
          },
          cancel() {
            canceled.push(url);
          },
        }),
        { headers: { 'content-type': 'text/event-stream' } },
      );
    throw new Error('unexpected route');
  }) as typeof fetch;
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/chats/A']}>
        <Link to="/chats/B">gotoB</Link>
        <Link to="/chats/C">gotoC</Link>
        <Routes>
          <Route path="/chats/:chatId" element={<ChatPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { controls, canceled };
}

test('ABV-05 actual route A → B → C isolates messages, session, closed and sequence state', async () => {
  const { controls, canceled } = setup();
  await waitFor(() => expect(controls.has('/api/chats/A/events')).toBe(true));
  await act(async () => {
    controls.get('/api/chats/A/events')?.enqueue(frame('snapshot', snapshot('A'), 90));
  });
  expect(screen.getByText('private-A')).toBeTruthy();
  await act(async () => {
    fireEvent.click(screen.getByText('gotoB'));
  });
  expect(screen.getByText('与 AgentB 的对话')).toBeTruthy();
  expect(screen.queryByText('private-A')).toBeNull();
  expect(screen.queryByText('会话 session-A')).toBeNull();
  await waitFor(() => expect(controls.has('/api/chats/B/events')).toBe(true));
  await act(async () => {
    controls.get('/api/chats/B/events')?.enqueue(frame('snapshot', snapshot('B')));
    controls.get('/api/chats/B/events')?.enqueue(frame('closed', { reason: null }, 2));
  });
  expect(screen.getByText(/当前对话不再接受消息/)).toBeTruthy();
  await act(async () => {
    fireEvent.click(screen.getByText('gotoC'));
  });
  expect(screen.queryByText('private-B')).toBeNull();
  expect(screen.queryByText(/当前对话不再接受消息/)).toBeNull();
  await waitFor(() => expect(controls.has('/api/chats/C/events')).toBe(true));
  await act(async () => {
    controls.get('/api/chats/C/events')?.enqueue(frame('snapshot', snapshot('C')));
  });
  expect(screen.getByText('private-C')).toBeTruthy();
  expect(screen.queryByText(/当前对话不再接受消息/)).toBeNull();
  cleanup();
  expect(canceled).toContain('/api/chats/A/events');
  expect(canceled).toContain('/api/chats/C/events');
});

test.each(['snapshot', '404', 'error-body'] as const)(
  'ABV-05 late %s from A cannot affect C route',
  async (kind) => {
    let resolve = (_response: Response) => {};
    const late = new Promise<Response>((done) => {
      resolve = done;
    });
    const { controls } = setup({ late });
    await act(async () => {
      fireEvent.click(screen.getByText('gotoC'));
    });
    await waitFor(() => expect(controls.has('/api/chats/C/events')).toBe(true));
    await act(async () => {
      controls.get('/api/chats/C/events')?.enqueue(frame('snapshot', snapshot('C')));
    });
    let canceled = false;
    await act(async () => {
      if (kind === 'snapshot')
        resolve(
          new Response(
            new ReadableStream({
              start(controller) {
                controller.enqueue(frame('snapshot', snapshot('A')));
              },
              cancel() {
                canceled = true;
              },
            }),
          ),
        );
      else
        resolve(
          Response.json(
            { error: { code: 'not_found', message: 'stale error', details: {}, retryable: false } },
            { status: kind === '404' ? 404 : 503 },
          ),
        );
    });
    expect(screen.getByText('与 AgentC 的对话')).toBeTruthy();
    expect(screen.getByText('private-C')).toBeTruthy();
    expect(screen.queryByText('private-A')).toBeNull();
    expect(screen.queryByText('stale error')).toBeNull();
    if (kind === 'snapshot') expect(canceled).toBe(true);
  },
);

test('ABV-05 error body already being read cannot publish after route change', async () => {
  let release = (_payload: unknown) => {};
  let started = false;
  const payload = new Promise<unknown>((resolve) => {
    release = resolve;
  });
  const response = new Response(null, { status: 503 });
  response.json = () => {
    started = true;
    return payload;
  };
  const { controls } = setup({ late: Promise.resolve(response) });
  await waitFor(() => expect(started).toBe(true));
  await act(async () => {
    fireEvent.click(screen.getByText('gotoC'));
  });
  await waitFor(() => expect(controls.has('/api/chats/C/events')).toBe(true));
  await act(async () => {
    controls.get('/api/chats/C/events')?.enqueue(frame('snapshot', snapshot('C')));
    release({ error: { code: 'engine_unavailable', message: 'stale body', details: {}, retryable: true } });
  });
  expect(screen.getByText('private-C')).toBeTruthy();
  expect(screen.queryByText('stale body')).toBeNull();
});

test('ABV-04 actual ChatPage uses stable HTTP code wording, never arbitrary error text', async () => {
  const secret = crypto.randomUUID() + crypto.randomUUID();
  setup({ httpError: secret });
  await waitFor(() => expect(screen.getByText('Chat runtime is unavailable')).toBeTruthy());
  expect(document.body.textContent?.includes(secret)).toBe(false);
});

test('ABV-04 actual ChatPage never renders random Error message/stack/cause', async () => {
  const secret = crypto.randomUUID() + crypto.randomUUID();
  const error = new Error(secret, { cause: new Error(secret) });
  setup({ error });
  await waitFor(() => expect(screen.getByText('连接中断，正在重连…')).toBeTruthy());
  expect(document.body.textContent?.includes(secret)).toBe(false);
});
