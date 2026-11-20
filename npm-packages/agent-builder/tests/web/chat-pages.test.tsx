import { afterEach, describe, expect, test } from 'bun:test';
import type { AgentCapabilities, AgentSummary } from '@shared/agent';
import type { EngineHealth } from '@shared/api';
import type { ChatMessage, ChatSnapshotEvent, ChatSummary } from '@shared/chat';
import type { ReactNode } from 'react';
import { installDom } from './dom';

installDom();

const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { act, cleanup, fireEvent, render, screen, waitFor } = await import('@testing-library/react');
const { Link, MemoryRouter, Route, Routes } = await import('react-router');
const { AppShell } = await import('../../src/components/app-shell');
const { ChatListPage } = await import('../../src/features/chats/chat-list-page');
const { ChatNewPage } = await import('../../src/features/chats/chat-new-page');
const { ChatPage } = await import('../../src/features/chats/chat-page');

const AGENT_ID = '00000000-0000-4000-8000-000000000001';

/** Stubs must not outlive the file: other test files rely on the real fetch. */
const nativeFetch = globalThis.fetch;

const agent: AgentSummary = {
  id: AGENT_ID,
  name: 'Reviewer',
  description: 'Checks changes',
  revision: 1,
  createdAt: 1,
  updatedAt: 2,
};

const chat: ChatSummary = {
  chatId: 'chat-1',
  agentId: AGENT_ID,
  agentName: 'Reviewer',
  sessionId: null,
  createdAt: 1,
  updatedAt: 2,
  sandbox: { state: 'disabled', reason: null },
};

function capabilities(
  mode: AgentCapabilities['chatRuntime'],
  reasons: string[] = [],
  ready = mode === 'wasm',
): AgentCapabilities {
  return {
    metaHarnessKeys: [],
    sendTextMaxLength: 32_768,
    providers: [{ type: 'anthropic', configured: true }],
    chatRuntime: mode,
    chatRuntimeReady: ready,
    chatRuntimeReasons: reasons,
    historyPersisted: ready,
    sandbox: { mode: 'disabled', reasons: [] },
  };
}

interface FakeSse {
  readonly response: Response;
  send(chunk: string): void;
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
  };
}

function frame(seq: number, event: string, data: unknown): string {
  return `id: ${seq}\nevent: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

function message(id: string, role: ChatMessage['role'], text: string): ChatMessage {
  return { id, role, text, status: 'streaming', createdAt: null };
}

function snapshot(messages: ChatMessage[]): ChatSnapshotEvent {
  return {
    chatId: chat.chatId,
    sessionId: 'session-1',
    bindingRevision: 1,
    messages,
    state: { running: false, closed: false },
    lastSeq: 1,
    overflow: false,
  };
}

const jsonResponse = (payload: unknown, status = 200) =>
  new Response(JSON.stringify(payload), {
    status,
    headers: { 'content-type': 'application/json' },
  });

interface FetchState {
  readonly capabilities: AgentCapabilities | (() => AgentCapabilities | Response);
  readonly chats?: ChatSummary[] | (() => ChatSummary[]);
  readonly agents?: AgentSummary[];
  readonly events?: FakeSse;
  readonly eventsStatus?: number;
  readonly create?: () => Response | Promise<Response>;
  readonly send?: () => Response | Promise<Response>;
  readonly list?: () => Response | Promise<Response>;
  readonly calls?: string[];
}

function stubFetch(state: FetchState): void {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
    state.calls?.push(`${init?.method ?? 'GET'} ${url}`);
    if (url === '/api/capabilities') {
      const capabilitiesPayload =
        typeof state.capabilities === 'function' ? state.capabilities() : state.capabilities;
      return capabilitiesPayload instanceof Response
        ? capabilitiesPayload
        : jsonResponse(capabilitiesPayload);
    }
    if (url === '/api/agents') return jsonResponse({ agents: state.agents ?? [] });
    if (url === '/api/chats') {
      if (init?.method === 'POST') {
        return state.create ? state.create() : jsonResponse({ chat }, 201);
      }
      if (state.list) return state.list();
      const chats = typeof state.chats === 'function' ? state.chats() : (state.chats ?? []);
      return jsonResponse({ chats });
    }
    if (url.startsWith('/api/chats/') && url.endsWith('/events')) {
      if (state.eventsStatus !== undefined) {
        return jsonResponse(
          {
            error: {
              code: 'not_found',
              message: 'Chat not found',
              details: {},
              retryable: false,
            },
          },
          state.eventsStatus,
        );
      }
      if (!state.events) throw new Error('missing events stream');
      return state.events.response;
    }
    if (url.startsWith('/api/chats/') && url.endsWith('/send')) {
      if (!state.send) throw new Error('missing send response');
      return state.send();
    }
    throw new Error(`unexpected fetch: ${url}`);
  }) as unknown as typeof fetch;
}

function renderPage(
  element: ReactNode,
  entry = '/chats',
  seedChats?: ChatSummary[],
  seedChat?: ChatSummary,
  staleTime = 30_000,
  seedCapabilities?: AgentCapabilities,
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime } },
  });
  if (seedChats !== undefined) queryClient.setQueryData(['chats'], seedChats);
  if (seedChat !== undefined) queryClient.setQueryData(['chat', seedChat.chatId], seedChat);
  if (seedCapabilities !== undefined) {
    // Mark the seed as old so the page's staleTime does not suppress the refresh this test needs.
    queryClient.setQueryData(['capabilities'], seedCapabilities, { updatedAt: 0 });
  }
  render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/chats" element={element} />
          <Route path="/chats/new" element={element} />
          <Route path="/chats/:chatId" element={element} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return queryClient;
}

function renderChatFlow(entry: string): void {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: 30_000 } },
  });
  render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[entry]}>
        <Link to="/chats">导航到列表</Link>
        <Routes>
          <Route path="/chats" element={<ChatListPage />} />
          <Route path="/chats/new" element={<ChatNewPage />} />
          <Route path="/chats/:chatId" element={<ChatPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function renderShell(engine: EngineHealth): void {
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
    if (url === '/api/health') {
      return jsonResponse({ ok: true, engine, runtime: { name: 'bun', version: 'test' } });
    }
    throw new Error(`unexpected fetch: ${url}`);
  }) as unknown as typeof fetch;
  renderPage(<AppShell />, '/chats');
}

afterEach(() => {
  cleanup();
  globalThis.fetch = nativeFetch;
});

describe('chat pages', () => {
  test.each(['inline', 'reference'] as const)(
    'ChatNewPage ignores protocol capabilities for an unconfigured %s Provider',
    async (kind) => {
      const providerId = '00000000-0000-4000-8000-000000000002';
      globalThis.fetch = (async (url: RequestInfo | URL) => {
        if (String(url) === '/api/capabilities') return jsonResponse(capabilities('wasm'));
        if (String(url) === '/api/agents') return jsonResponse({ agents: [agent] });
        if (String(url) === '/api/providers')
          return jsonResponse({
            providers: [
              {
                id: providerId,
                name: 'Shared',
                type: 'anthropic',
                baseUrl: null,
                credentialSource: 'local',
                hasCredential: false,
                revision: 1,
                agentCount: 1,
                runtimeCount: 0,
              },
            ],
          });
        return jsonResponse({
          agent: {
            ...agent,
            config: {
              instructions: '',
              metaHarness: {},
              mcpServers: {},
              provider:
                kind === 'inline'
                  ? { id: providerId, type: 'anthropic', model: 'm' }
                  : { kind: 'reference', providerId, model: 'm' },
            },
          },
        });
      }) as unknown as typeof fetch;
      const client = renderPage(<ChatNewPage />, '/chats/new');
      try {
        await screen.findByText(
          kind === 'inline' ? /旧内嵌配置不能启动新运行/ : /此 Provider 待填写 SQLite API key/,
        );
        expect(screen.queryByText(/Provider 凭据已存在/)).toBeNull();
        expect(document.body.textContent).not.toContain('ANTHROPIC_API_KEY');
        expect(document.body.textContent).not.toContain('OPENAI_API_KEY');
      } finally {
        client.clear();
      }
    },
  );

  test('ChatListPage shows an empty state and the runtime reasons', async () => {
    stubFetch({
      capabilities: capabilities('not_configured', ['store_token_missing', 'provider_key_missing']),
      chats: [],
    });
    renderPage(<ChatListPage />);

    expect(await screen.findByText('还没有对话')).toBeTruthy();
    expect(await screen.findByText('对话未配置')).toBeTruthy();
    expect(
      screen.getByText('缺少 Store 认证令牌（访问会话存储用，与模型 API key 不同）：设置 PERI_STORE_TOKEN'),
    ).toBeTruthy();
    expect(screen.getByText('缺少模型 API key：请在 Providers 管理页填写 SQLite 凭据')).toBeTruthy();
  });

  test('ChatListPage reports an unknown runtime state without inventing a configuration cause', async () => {
    stubFetch({
      capabilities: capabilities('not_configured', []),
      chats: [],
    });
    renderPage(<ChatListPage />);

    expect(await screen.findByText('对话状态未知')).toBeTruthy();
    expect(screen.getByText('未提供具体原因，请查看 API 日志。')).toBeTruthy();
    expect(screen.queryByText('对话未配置')).toBeNull();
  });

  test('ChatListPage does not turn an unrecognized reason into a configuration claim', async () => {
    stubFetch({
      capabilities: capabilities('not_configured', ['unexpected_reason']),
      chats: [],
    });
    renderPage(<ChatListPage />);

    expect(await screen.findByText('对话状态未知')).toBeTruthy();
    expect(screen.getByText('unexpected_reason')).toBeTruthy();
    expect(screen.queryByText('对话未配置')).toBeNull();
  });

  test('ChatListPage shows a full-page error only when no list was loaded', async () => {
    stubFetch({
      capabilities: capabilities('wasm'),
      list: () =>
        jsonResponse(
          {
            error: {
              code: 'internal_error',
              message: '列表读取失败',
              details: {},
              retryable: false,
            },
          },
          500,
        ),
    });
    renderPage(<ChatListPage />);

    expect(await screen.findByText('无法加载对话列表')).toBeTruthy();
    expect(screen.queryByText('对话列表刷新失败')).toBeNull();
  });

  test('ChatListPage keeps the loaded list when a refresh fails', async () => {
    stubFetch({
      capabilities: capabilities('wasm'),
      list: () =>
        jsonResponse(
          {
            error: {
              code: 'internal_error',
              message: '列表读取失败',
              details: {},
              retryable: false,
            },
          },
          500,
        ),
    });
    // Seeded data plus staleTime 0 forces exactly one refresh, which fails.
    renderPage(<ChatListPage />, '/chats', [chat], undefined, 0);

    expect(await screen.findByText('Reviewer')).toBeTruthy();
    expect(await screen.findByText('对话列表刷新失败')).toBeTruthy();
    expect(screen.queryByText('无法加载对话列表')).toBeNull();
    expect(screen.getByText('继续对话')).toBeTruthy();
  });

  test('ChatListPage keeps the runtime notice when the capabilities refresh fails', async () => {
    stubFetch({
      capabilities: () =>
        jsonResponse(
          {
            error: {
              code: 'internal_error',
              message: 'capabilities refresh failed',
              details: {},
              retryable: false,
            },
          },
          500,
        ),
      chats: [],
    });
    // Seeded data plus staleTime 0 forces one background refresh that fails; the notice must stay.
    const queryClient = renderPage(
      <ChatListPage />,
      '/chats',
      undefined,
      undefined,
      0,
      capabilities('not_configured', ['store_auth_failed']),
    );

    expect(await screen.findByText('对话暂不可用')).toBeTruthy();
    expect(
      await screen.findByText(
        'Store 认证失败（401/403）：检查 PERI_STORE_TOKEN 是否有效；修正后需重启本地 API，不会自动重试',
      ),
    ).toBeTruthy();
    await waitFor(() => expect(queryClient.getQueryState(['capabilities'])?.status).toBe('error'));
    // The failed refresh kept the old data, so the notice must survive.
    expect(screen.getByText('对话暂不可用')).toBeTruthy();
    expect(screen.queryByText('对话未配置')).toBeNull();
  });

  test('ChatListPage renders distinct auth and database hints instead of a retry hint', async () => {
    stubFetch({
      capabilities: capabilities('not_configured', ['store_auth_failed', 'store_database_missing']),
      chats: [],
    });
    renderPage(<ChatListPage />);

    expect(await screen.findByText('对话暂不可用')).toBeTruthy();
    expect(screen.getByText(/Store 认证失败/)).toBeTruthy();
    expect(screen.getByText(/Store 数据库不存在/)).toBeTruthy();
    expect(screen.queryByText('对话未配置')).toBeNull();
    expect(screen.queryByText(/Store 恢复后会自动重试/)).toBeNull();
  });

  test('ChatNewPage explains a capabilities failure when no runtime status was ever loaded', async () => {
    const calls: string[] = [];
    stubFetch({
      capabilities: () =>
        jsonResponse(
          {
            error: {
              code: 'internal_error',
              message: 'capabilities unavailable',
              details: {},
              retryable: false,
            },
          },
          500,
        ),
      agents: [agent],
      calls,
    });
    renderPage(<ChatNewPage />, '/chats/new');

    expect(await screen.findByText('无法加载运行时状态')).toBeTruthy();
    expect(screen.getByText('Internal server error')).toBeTruthy();
    expect(screen.queryByText(/对话(未配置|暂不可用|状态未知)/)).toBeNull();
    const button = await screen.findByRole('button', { name: /开始对话/ });
    expect((button as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    await waitFor(() => expect(calls.filter((call) => call === 'GET /api/capabilities')).toHaveLength(2));
  });

  test('ChatNewPage disables creation for store auth failures without a retry', async () => {
    stubFetch({
      capabilities: capabilities('not_configured', ['store_auth_failed']),
      agents: [agent],
    });
    renderPage(<ChatNewPage />, '/chats/new');

    expect(await screen.findByText('对话暂不可用')).toBeTruthy();
    expect(await screen.findByText(/修正后需重启本地 API/)).toBeTruthy();
    expect(screen.queryByText('对话未配置')).toBeNull();
    const button = await screen.findByRole('button', { name: /开始对话/ });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  test('ChatNewPage disables creation while the runtime is not configured', async () => {
    stubFetch({
      capabilities: capabilities('not_configured', ['store_token_missing']),
      agents: [agent],
    });
    renderPage(<ChatNewPage />, '/chats/new');

    expect(await screen.findByText('Reviewer')).toBeTruthy();
    expect(await screen.findByText('对话未配置')).toBeTruthy();
    expect(screen.getByText('缺少 Store 认证令牌', { exact: false })).toBeTruthy();
    const button = await screen.findByRole('button', { name: /开始对话/ });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  test('ChatNewPage offers a retry when the Store is the only blocker', async () => {
    stubFetch({
      capabilities: capabilities('not_configured', ['store_unreachable']),
      agents: [agent],
    });
    renderPage(<ChatNewPage />, '/chats/new');

    expect(
      await screen.findByText(
        '无法连接 Peri Store（网络错误、超时或 5xx）：检查 PERI_STORE_URL 与网络；Store 恢复后会自动重试',
      ),
    ).toBeTruthy();
    const button = await screen.findByRole('button', { name: /开始对话/ });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
  });

  test('ChatNewPage disables creation when the runtime is configured but the engine is not ready', async () => {
    stubFetch({
      capabilities: capabilities('wasm', [], false),
      agents: [agent],
    });
    renderPage(<ChatNewPage />, '/chats/new');

    expect(await screen.findByText('对话引擎暂不可用')).toBeTruthy();
    expect(await screen.findByText(/引擎尚未就绪/)).toBeTruthy();
    const button = await screen.findByRole('button', { name: /开始对话/ });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  test('ChatNewPage clears the store outage notice after a successful create', async () => {
    const calls: string[] = [];
    let runtime: AgentCapabilities['chatRuntime'] = 'not_configured';
    stubFetch({
      capabilities: () => capabilities(runtime, runtime === 'wasm' ? [] : ['store_unreachable']),
      agents: [agent],
      chats: [],
      calls,
      events: createSse(),
      create: () => {
        runtime = 'wasm';
        return jsonResponse({ chat }, 201);
      },
    });
    renderChatFlow('/chats/new');

    expect(
      await screen.findByText(
        '无法连接 Peri Store（网络错误、超时或 5xx）：检查 PERI_STORE_URL 与网络；Store 恢复后会自动重试',
      ),
    ).toBeTruthy();
    const createButton = await screen.findByRole('button', { name: /开始对话/ });
    await waitFor(() => expect((createButton as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(createButton);

    expect(await screen.findByText('与 Reviewer 的对话')).toBeTruthy();
    // The create response proves the Store recovered; the invalidated capabilities hide the notice.
    await waitFor(() => expect(screen.queryByText('对话暂不可用')).toBeNull());
    expect(calls.filter((call) => call === 'GET /api/capabilities').length).toBeGreaterThanOrEqual(2);
  });

  test('ChatNewPage surfaces the reasons from a 503 when creation is refused', async () => {
    stubFetch({
      capabilities: capabilities('wasm'),
      agents: [agent],
      create: () =>
        jsonResponse(
          {
            error: {
              code: 'engine_unavailable',
              message: 'Chat runtime is not configured',
              details: { reasons: ['store_token_missing'] },
              retryable: false,
            },
          },
          503,
        ),
    });
    renderPage(<ChatNewPage />, '/chats/new');

    fireEvent.click(await screen.findByRole('button', { name: /开始对话/ }));

    expect(await screen.findByText('无法创建对话')).toBeTruthy();
    expect(await screen.findByText('对话未配置')).toBeTruthy();
    expect(
      await screen.findByText(
        '缺少 Store 认证令牌（访问会话存储用，与模型 API key 不同）：设置 PERI_STORE_TOKEN',
      ),
    ).toBeTruthy();
  });

  test('ChatPage renders the not_configured hint and disables the composer', async () => {
    const sse = createSse();
    stubFetch({
      capabilities: capabilities('not_configured', ['store_token_missing']),
      chats: [chat],
      events: sse,
    });
    renderPage(<ChatPage />, '/chats/chat-1');

    expect(await screen.findByText('对话未配置')).toBeTruthy();
    const textarea = await screen.findByLabelText('消息内容');
    await waitFor(() => expect((textarea as HTMLTextAreaElement).disabled).toBe(true));
  });

  test('ChatPage leaves the composer enabled for a retryable store outage', async () => {
    const sse = createSse();
    stubFetch({
      capabilities: capabilities('not_configured', ['store_unreachable']),
      chats: [chat],
      events: sse,
    });
    renderPage(<ChatPage />, '/chats/chat-1');

    const textarea = await screen.findByLabelText('消息内容');
    await waitFor(() => expect((textarea as HTMLTextAreaElement).disabled).toBe(false));
  });

  test('ChatPage explains a configured but not-ready engine and the lazy session binding', async () => {
    const sse = createSse();
    stubFetch({
      capabilities: capabilities('wasm', [], false),
      chats: [chat],
      events: sse,
    });
    renderPage(<ChatPage />, '/chats/chat-1');

    expect(await screen.findByText('对话引擎暂不可用')).toBeTruthy();
    expect(await screen.findByText(/首次订阅事件流或首次发送消息后建立/)).toBeTruthy();
    const textarea = await screen.findByLabelText('消息内容');
    await waitFor(() => expect((textarea as HTMLTextAreaElement).disabled).toBe(true));
  });

  test('ChatPage appends streamed chunks without duplicating a message id', async () => {
    const sse = createSse();
    stubFetch({ capabilities: capabilities('wasm'), chats: [chat], events: sse });
    renderPage(<ChatPage />, '/chats/chat-1');

    const textarea = await screen.findByLabelText('消息内容');
    await waitFor(() => expect((textarea as HTMLTextAreaElement).disabled).toBe(false));

    await act(async () => {
      sse.send(frame(1, 'snapshot', snapshot([message('turn:1:user', 'user', 'hello')])));
    });
    await act(async () => {
      sse.send(frame(2, 'message', { message: message('turn:1:a', 'assistant', 'echo: ') }));
    });
    await act(async () => {
      sse.send(frame(3, 'message', { message: message('turn:1:a', 'assistant', 'echo: hello') }));
    });

    expect(await screen.findByText('echo: hello')).toBeTruthy();
    expect(screen.getAllByTestId('chat-message')).toHaveLength(2);
  });

  test('ChatPage disables the send button while the request is pending', async () => {
    const sse = createSse();
    const pendingSend: { release: ((response: Response) => void) | null } = { release: null };
    stubFetch({
      capabilities: capabilities('wasm'),
      chats: [chat],
      events: sse,
      send: () =>
        new Promise<Response>((resolve) => {
          pendingSend.release = resolve;
        }),
    });
    renderPage(<ChatPage />, '/chats/chat-1');

    const textarea = await screen.findByLabelText('消息内容');
    await waitFor(() => expect((textarea as HTMLTextAreaElement).disabled).toBe(false));
    fireEvent.change(textarea, { target: { value: 'hello' } });
    fireEvent.click(screen.getByRole('button', { name: '发送' }));

    const pending = await screen.findByRole('button', { name: '发送中…' });
    expect((pending as HTMLButtonElement).disabled).toBe(true);

    if (pendingSend.release === null) throw new Error('send request was not issued');
    pendingSend.release(
      jsonResponse({
        requestId: 'request-1',
        inputId: 'input-1',
        sessionId: 'session-1',
        bindingRevision: 1,
      }),
    );
    expect(await screen.findByRole('button', { name: '发送' })).toBeTruthy();
  });

  test('ChatPage refreshes capabilities after a successful send', async () => {
    const calls: string[] = [];
    const sse = createSse();
    stubFetch({
      capabilities: capabilities('wasm'),
      chats: [chat],
      events: sse,
      calls,
      send: () =>
        jsonResponse({
          requestId: 'request-1',
          inputId: 'input-1',
          sessionId: 'session-1',
          bindingRevision: 1,
        }),
    });
    renderPage(<ChatPage />, '/chats/chat-1');

    const textarea = await screen.findByLabelText('消息内容');
    await waitFor(() => expect((textarea as HTMLTextAreaElement).disabled).toBe(false));
    fireEvent.change(textarea, { target: { value: 'hello' } });
    fireEvent.click(screen.getByRole('button', { name: '发送' }));

    await waitFor(() =>
      expect(calls.filter((call) => call === 'GET /api/capabilities').length).toBeGreaterThanOrEqual(2),
    );
  });

  test('ChatPage drops a cached chat when its stream answers 404', async () => {
    stubFetch({
      capabilities: capabilities('wasm'),
      chats: [],
      eventsStatus: 404,
    });
    renderPage(<ChatPage />, '/chats/chat-1', undefined, chat);

    expect(await screen.findByText('与 Reviewer 的对话')).toBeTruthy();
    // Without a delete UI, the SSE 404 is the server's confirmation that the row is gone.
    expect(await screen.findByText('对话不存在')).toBeTruthy();
  });

  test('creating a chat shows it immediately and refreshes the list cache', async () => {
    const calls: string[] = [];
    let chats: ChatSummary[] = [];
    stubFetch({
      capabilities: capabilities('wasm'),
      agents: [agent],
      chats: () => chats,
      calls,
      events: createSse(),
      create: () => {
        chats = [chat];
        return jsonResponse({ chat }, 201);
      },
    });
    renderChatFlow('/chats/new');

    const createButton = await screen.findByRole('button', { name: /开始对话/ });
    await waitFor(() => expect((createButton as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(createButton);

    // The cache write renders the new chat without waiting for the list round trip.
    expect(await screen.findByText('与 Reviewer 的对话')).toBeTruthy();
    expect(screen.queryByText('对话不存在')).toBeNull();

    // The invalidated list is refetched, so returning to it is not a stale empty state.
    await waitFor(() => expect(calls).toContain('GET /api/chats'));
    expect(calls.indexOf('GET /api/chats')).toBeGreaterThan(calls.indexOf('POST /api/chats'));
    fireEvent.click(screen.getByText('导航到列表'));
    expect(await screen.findByText('继续对话')).toBeTruthy();
    expect(screen.queryByText('还没有对话')).toBeNull();
  });

  test('creating a chat is not undone by a list that does not include it yet', async () => {
    const calls: string[] = [];
    stubFetch({
      capabilities: capabilities('wasm'),
      agents: [agent],
      chats: [],
      calls,
      events: createSse(),
      create: () => jsonResponse({ chat }, 201),
    });
    renderChatFlow('/chats/new');

    const createButton = await screen.findByRole('button', { name: /开始对话/ });
    await waitFor(() => expect((createButton as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(createButton);

    // The POST response seeds ['chat', id]; the list still returns [] on every refetch.
    expect(await screen.findByText('与 Reviewer 的对话')).toBeTruthy();
    expect(screen.queryByText('对话不存在')).toBeNull();
    await waitFor(() => expect(calls).toContain('GET /api/chats'));
    await waitFor(() => expect(calls).toContain('GET /api/chats/chat-1/events'));
  });

  test('ChatPage shows loading instead of 404 while a stale cache is confirmed', async () => {
    const pending: { release: ((response: Response) => void) | null } = { release: null };
    stubFetch({
      capabilities: capabilities('wasm'),
      chats: [],
      list: () =>
        new Promise<Response>((resolve) => {
          pending.release = resolve;
        }),
      events: createSse(),
    });
    renderPage(<ChatPage />, '/chats/chat-1', []);

    expect(await screen.findByRole('status')).toBeTruthy();
    expect(screen.queryByText('对话不存在')).toBeNull();

    if (pending.release === null) throw new Error('chat list was not requested');
    pending.release(jsonResponse({ chats: [chat] }));
    expect(await screen.findByText('与 Reviewer 的对话')).toBeTruthy();
  });

  test('ChatPage keeps a cached chat and its stream when the chat list refresh fails', async () => {
    const calls: string[] = [];
    const sse = createSse();
    stubFetch({
      capabilities: capabilities('wasm'),
      calls,
      list: () =>
        jsonResponse(
          {
            error: {
              code: 'internal_error',
              message: '列表读取失败',
              details: {},
              retryable: false,
            },
          },
          500,
        ),
      events: sse,
    });
    renderPage(<ChatPage />, '/chats/chat-1', undefined, chat);

    expect(await screen.findByText('与 Reviewer 的对话')).toBeTruthy();
    expect(await screen.findByText('对话列表暂时不可用')).toBeTruthy();
    expect(screen.queryByText('无法加载对话')).toBeNull();
    await waitFor(() => expect(calls).toContain('GET /api/chats/chat-1/events'));

    // The stream is still attached: a live snapshot must render after the list failure.
    await act(async () => {
      sse.send(frame(1, 'snapshot', snapshot([message('turn:1:user', 'user', '缓存对话仍可继续')])));
    });
    expect(await screen.findByText('缓存对话仍可继续')).toBeTruthy();
  });

  test('ChatPage reports 404 only after a fresh lookup returns no chat', async () => {
    const calls: string[] = [];
    stubFetch({
      capabilities: capabilities('wasm'),
      chats: [],
      calls,
      events: createSse(),
    });
    renderPage(<ChatPage />, '/chats/chat-1', []);

    expect(await screen.findByText('对话不存在')).toBeTruthy();
    expect(calls.filter((call) => call === 'GET /api/chats')).toHaveLength(1);
  });

  test('ChatPage reports a lookup failure instead of 404', async () => {
    stubFetch({
      capabilities: capabilities('wasm'),
      chats: [],
      list: () =>
        jsonResponse(
          {
            error: {
              code: 'internal_error',
              message: '列表读取失败',
              details: {},
              retryable: false,
            },
          },
          500,
        ),
      events: createSse(),
    });
    renderPage(<ChatPage />, '/chats/chat-1', []);

    expect(await screen.findByText('无法加载对话')).toBeTruthy();
    expect(screen.queryByText('对话不存在')).toBeNull();
  });
});

describe('app shell runtime status', () => {
  test('shows the concrete missing-token cause instead of a vague engine label', async () => {
    renderShell({ mode: 'not_configured', status: 'unknown', reasons: ['store_token_missing'] });

    expect(await screen.findByText('对话未配置')).toBeTruthy();
    expect(screen.getByText('缺少 Store 认证令牌')).toBeTruthy();
    expect(screen.queryByText(/Engine 未接入/)).toBeNull();
  });

  test.each([
    ['store_unreachable', '无法连接 Store'],
    ['store_auth_failed', 'Store 认证失败'],
    ['store_database_missing', 'Store 数据库不存在'],
  ] as const)('reports %s as a Store failure, not a missing configuration', async (reason, detail) => {
    renderShell({ mode: 'not_configured', status: 'unknown', reasons: [reason] });

    expect(await screen.findByText('对话暂不可用')).toBeTruthy();
    expect(screen.getByText(detail)).toBeTruthy();
    expect(screen.queryByText('对话未配置')).toBeNull();
  });

  test('keeps an unknown state honest when health has no reasons', async () => {
    renderShell({ mode: 'not_configured', status: 'unknown' });

    expect(await screen.findByText('对话状态未知')).toBeTruthy();
    expect(screen.queryByText('对话未配置')).toBeNull();
    expect(screen.queryByText('缺少 Store 认证令牌')).toBeNull();
  });

  test.each([
    ['ready', '对话就绪'],
    ['starting', '对话启动中'],
    ['unavailable', '对话启动失败'],
  ] as const)('labels %s without claiming a configuration problem', async (status, label) => {
    renderShell({ mode: 'embedded', status });

    expect(await screen.findByText(label)).toBeTruthy();
    expect(screen.queryByText(/对话(未配置|暂不可用|状态未知)/)).toBeNull();
  });
});
