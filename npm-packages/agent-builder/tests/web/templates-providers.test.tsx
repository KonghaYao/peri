import { afterEach, describe, expect, mock, test } from 'bun:test';
import type { AgentCapabilities, AgentDraft } from '@shared/agent';
import type { Provider } from '@shared/provider';
import { installDom } from './dom';

installDom();
const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { act, cleanup, fireEvent, render, screen, waitFor, within } = await import('@testing-library/react');
const { MemoryRouter, createMemoryRouter, RouterProvider } = await import('react-router');
const { AgentForm } = await import('../../src/features/agents/agent-form');
const { EMPTY_AGENT_DRAFT } = await import('../../src/features/agents/draft');
const { ProviderForm, ProviderDialog } = await import('../../src/features/providers/provider-form');
const { ProvidersPage } = await import('../../src/features/providers/providers-page');
const { AppShell } = await import('../../src/components/app-shell');

const nativeFetch = globalThis.fetch;
const capabilities: AgentCapabilities = {
  metaHarnessKeys: ['McpMiddleware'],
  sendTextMaxLength: 32768,
  providers: [],
  chatRuntime: 'not_configured',
  chatRuntimeReasons: ['store_token_missing'],
  historyPersisted: false,
  sandbox: { mode: 'disabled', reasons: [] },
};
const provider: Provider = {
  id: '00000000-0000-4000-8000-000000000001',
  name: 'Shared',
  type: 'anthropic',
  baseUrl: null,
  credentialSource: 'local',
  hasCredential: true,
  revision: 1,
  agentCount: 0,
  runtimeCount: 0,
};
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
const clients: InstanceType<typeof QueryClient>[] = [];
afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  globalThis.fetch = nativeFetch;
});
function client() {
  const c = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(c);
  return c;
}
function form(
  draft = EMPTY_AGENT_DRAFT,
  providers: Provider[] = [],
  onSubmit = mock(async (_draft: AgentDraft) => {}),
) {
  globalThis.fetch = (async () => json({ providers })) as unknown as typeof fetch;
  const c = client();
  const view = render(
    <QueryClientProvider client={c}>
      <MemoryRouter>
        <AgentForm
          defaultValues={draft}
          capabilities={capabilities}
          submitLabel="创建 Agent"
          showTemplate
          onSubmit={onSubmit}
        />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { view, c, onSubmit };
}

describe('official template and short form', () => {
  test('applies runtime defaults, shows only short fields, and does not save or create a Chat', async () => {
    const { onSubmit, view } = form();
    expect(view.container.querySelector('details')?.open).toBe(false);
    fireEvent.click(screen.getByRole('button', { name: '应用模板' }));
    expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('Peri Agent');
    expect(onSubmit).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: '创建 Agent' }));
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0]?.[0].config).toEqual({
      provider: null,
      instructions: '',
      metaHarness: {},
      mcpServers: {},
    });
  });

  test('dirty template cancellation is unchanged, confirmation resets only advanced settings, and undo is one-step', async () => {
    const draft = {
      ...EMPTY_AGENT_DRAFT,
      name: 'Mine',
      description: 'Keep',
      config: { ...EMPTY_AGENT_DRAFT.config, instructions: 'custom', metaHarness: { McpMiddleware: false } },
    };
    form(draft);
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'My name' } });
    fireEvent.click(screen.getByRole('button', { name: '应用模板' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '取消' }));
    expect((screen.getByLabelText('Instructions') as HTMLTextAreaElement).value).toBe('custom');
    fireEvent.click(screen.getByRole('button', { name: '应用模板' }));
    fireEvent.click(screen.getByRole('button', { name: '确认应用' }));
    expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('My name');
    expect((screen.getByLabelText('描述') as HTMLInputElement).value).toBe('Keep');
    expect((screen.getByLabelText('Instructions') as HTMLTextAreaElement).value).toBe('');
    fireEvent.click(screen.getByRole('button', { name: '撤销本次应用' }));
    expect((screen.getByLabelText('Instructions') as HTMLTextAreaElement).value).toBe('custom');
    fireEvent.click(screen.getByRole('button', { name: '应用模板' }));
    fireEvent.click(screen.getByRole('button', { name: '确认应用' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'Later input' } });
    expect(screen.queryByRole('button', { name: '撤销本次应用' })).toBeNull();
  });

  test('advanced validation expands and focuses error without losing fields', async () => {
    const { view, onSubmit } = form({
      ...EMPTY_AGENT_DRAFT,
      name: 'Invalid',
      config: { ...EMPTY_AGENT_DRAFT.config, instructions: 'x'.repeat(32769) },
    });
    fireEvent.click(screen.getByRole('button', { name: '创建 Agent' }));
    await waitFor(() => expect(view.container.querySelector('details')?.open).toBe(true));
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText('Instructions')));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  test('auto selects only one configured Provider, multiple remain explicit, late refresh never erases typed values', async () => {
    const { view, c } = form(EMPTY_AGENT_DRAFT, [provider]);
    await waitFor(() =>
      expect((screen.getByLabelText('模型 ID') as HTMLInputElement).value).toBe('claude-sonnet-4-5'),
    );
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'Unchanged' } });
    fireEvent.change(screen.getByLabelText('模型 ID'), { target: { value: 'custom-model' } });
    await act(async () => c.setQueryData(['providers'], [{ ...provider, name: 'Renamed' }]));
    expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('Unchanged');
    expect((screen.getByLabelText('模型 ID') as HTMLInputElement).value).toBe('custom-model');
    view.unmount();
    form(EMPTY_AGENT_DRAFT, [provider, { ...provider, id: '00000000-0000-4000-8000-000000000002' }]);
    await screen.findAllByText('未选择 Provider');
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    expect((screen.getByLabelText('模型 ID') as HTMLInputElement).value).toBe('');
  });

  test('adding Provider in the draft returns selected and preserves name/model/advanced fields', async () => {
    const { onSubmit } = form({
      ...EMPTY_AGENT_DRAFT,
      name: 'Keep name',
      config: {
        ...EMPTY_AGENT_DRAFT.config,
        provider: {
          kind: 'reference',
          providerId: '00000000-0000-4000-8000-000000000002',
          model: 'keep-custom-model',
        },
        instructions: 'Keep instructions',
      },
    });
    let savedCalls = 0;
    globalThis.fetch = (async (_path: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'POST') {
        savedCalls++;
        return json({ provider }, 201);
      }
      return json({ providers: [] });
    }) as unknown as typeof fetch;
    fireEvent.click(screen.getByRole('button', { name: '添加 Provider' }));
    fireEvent.change(screen.getByLabelText('Provider 名称'), { target: { value: 'Shared' } });
    expect(screen.queryByLabelText('凭据来源（明确选择）')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '保存 Provider' }));
    await waitFor(() => expect(savedCalls).toBe(1));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    expect(screen.queryAllByRole('dialog').length).toBe(0);
    expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('Keep name');
    expect((screen.getByLabelText('Instructions') as HTMLTextAreaElement).value).toBe('Keep instructions');
    fireEvent.click(screen.getByRole('button', { name: '创建 Agent' }));
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0]?.[0].config.provider).toEqual({
      kind: 'reference',
      providerId: provider.id,
      model: 'keep-custom-model',
    });
  });
});

describe('Provider management safety and navigation', () => {
  test('Provider deletion failure keeps the row, retry reloads its revision, and confirmed deletion reaches empty state', async () => {
    let rows = [provider];
    let deletes = 0;
    globalThis.fetch = (async (_url: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'DELETE') {
        deletes++;
        if (deletes === 1)
          return json(
            { error: { code: 'provider_in_use', message: 'safe', details: {}, retryable: false } },
            409,
          );
        rows = [];
        return new Response(null, { status: 204 });
      }
      return json({ providers: rows });
    }) as unknown as typeof fetch;
    const c = client();
    render(
      <QueryClientProvider client={c}>
        <ProvidersPage />
      </QueryClientProvider>,
    );
    fireEvent.click(await screen.findByRole('button', { name: '删除 Shared' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '删除' }));
    await screen.findByText('未删除 Provider');
    expect(screen.getByText('Shared')).toBeTruthy();
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '取消' }));
    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    fireEvent.click(screen.getByRole('button', { name: '删除 Shared' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '删除' }));
    await screen.findByText('还没有 Provider');
    expect(deletes).toBe(2);
    expect(screen.queryByText('Shared')).toBeNull();
  });
  test('write-only password is not cached, clears after success, and origin cancel does not submit', async () => {
    const secret = crypto.randomUUID();
    const c = client();
    let calls = 0;
    globalThis.fetch = (async (url: RequestInfo | URL, init?: RequestInit) => {
      calls++;
      expect(String(url).includes(secret)).toBe(false);
      expect(init?.cache).toBe('no-store');
      return json({ provider: { ...provider, revision: 2 } });
    }) as unknown as typeof fetch;
    render(
      <QueryClientProvider client={c}>
        <ProviderForm initial={provider} onSaved={() => {}} onCancel={() => {}} onPending={() => {}} />
      </QueryClientProvider>,
    );
    expect((screen.getByLabelText(/API key/) as HTMLInputElement).value).toBe('');
    expect((screen.getByLabelText(/API key/) as HTMLInputElement).type).toBe('password');
    expect(screen.queryByLabelText('凭据来源（明确选择）')).toBeNull();
    fireEvent.change(screen.getByLabelText(/API key/), { target: { value: secret } });
    fireEvent.click(screen.getByRole('button', { name: '保存 Provider' }));
    await waitFor(() => expect((screen.getByLabelText(/API key/) as HTMLInputElement).value).toBe(''));
    expect(JSON.stringify(c.getMutationCache().getAll()).includes(secret)).toBe(false);
    expect(JSON.stringify(c.getQueryCache().getAll()).includes(secret)).toBe(false);
    expect(window.localStorage.length + window.sessionStorage.length).toBe(0);
    fireEvent.change(screen.getByLabelText('Base URL（可选）'), { target: { value: 'https://new.example' } });
    fireEvent.click(screen.getByRole('button', { name: '保存 Provider' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '取消' }));
    expect(calls).toBe(1);
    fireEvent.click(screen.getByRole('button', { name: '保存 Provider' }));
    fireEvent.click(screen.getByRole('button', { name: '确认并保存' }));
    await waitFor(() => expect(calls).toBe(2));
  });

  test('closing Dialog removes password state; failed save preserves draft and displays only safe error', async () => {
    const secret = crypto.randomUUID();
    const c = client();
    globalThis.fetch = (async () =>
      json(
        { error: { code: 'internal_error', message: secret, details: {}, retryable: false } },
        500,
      )) as unknown as typeof fetch;
    const view = render(
      <QueryClientProvider client={c}>
        <ProviderDialog open initial={provider} onOpenChange={() => {}} onSaved={() => {}} />
      </QueryClientProvider>,
    );
    fireEvent.change(screen.getByLabelText(/API key/), { target: { value: secret } });
    fireEvent.click(screen.getByRole('button', { name: '保存 Provider' }));
    await screen.findByRole('alert');
    expect((screen.getByLabelText(/API key/) as HTMLInputElement).value).toBe('');
    expect(JSON.stringify(c.getMutationCache().getAll()).includes(secret)).toBe(false);
    expect(JSON.stringify(c.getQueryCache().getAll()).includes(secret)).toBe(false);
    expect(document.body.textContent?.includes(secret)).toBe(false);
    view.rerender(
      <QueryClientProvider client={c}>
        <ProviderDialog open={false} initial={provider} onOpenChange={() => {}} onSaved={() => {}} />
      </QueryClientProvider>,
    );
    view.rerender(
      <QueryClientProvider client={c}>
        <ProviderDialog open initial={provider} onOpenChange={() => {}} onSaved={() => {}} />
      </QueryClientProvider>,
    );
    expect((screen.getByLabelText(/API key/) as HTMLInputElement).value).toBe('');
  });

  test('Provider page loads, retries, edits and restricts deletion; sidebar/mobile routes are real', async () => {
    let failed = true;
    globalThis.fetch = (async (url: RequestInfo | URL) =>
      String(url) === '/api/health'
        ? json({
            ok: true,
            engine: { mode: 'not_configured', status: 'unknown' },
            runtime: { name: 'bun', version: 'test' },
          })
        : failed
          ? json({ error: { code: 'internal_error', message: 'safe', details: {}, retryable: true } }, 500)
          : json({ providers: [{ ...provider, agentCount: 1 }] })) as unknown as typeof fetch;
    const c = client();
    const router = createMemoryRouter(
      [{ path: '/', element: <AppShell />, children: [{ path: 'providers', element: <ProvidersPage /> }] }],
      { initialEntries: ['/providers'] },
    );
    render(
      <QueryClientProvider client={c}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    await screen.findByText('无法加载 Providers');
    failed = false;
    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    await screen.findByText('Shared');
    expect((screen.getByRole('button', { name: '删除 Shared' }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getAllByRole('link', { name: 'Providers' }).length).toBe(2);
    fireEvent.click(screen.getByRole('button', { name: '编辑 Shared' }));
    expect((screen.getByLabelText(/API key/) as HTMLInputElement).value).toBe('');
  });

  test('data router prompts before leaving a dirty template and cancel keeps the same draft', async () => {
    globalThis.fetch = (async () => json({ providers: [] })) as unknown as typeof fetch;
    const c = client();
    const router = createMemoryRouter([
      {
        path: '/',
        element: (
          <AgentForm
            defaultValues={EMPTY_AGENT_DRAFT}
            submitLabel="创建 Agent"
            capabilities={capabilities}
            onSubmit={async () => {}}
            showTemplate
          />
        ),
      },
      { path: '/agents', element: <p>Agents destination</p> },
    ]);
    render(
      <QueryClientProvider client={c}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
    fireEvent.click(screen.getByRole('button', { name: '应用模板' }));
    fireEvent.click(screen.getByRole('button', { name: '取消' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '取消' }));
    expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('Peri Agent');
    fireEvent.click(screen.getByRole('button', { name: '取消' }));
    fireEvent.click(screen.getByRole('button', { name: '放弃并离开' }));
    await screen.findByText('Agents destination');
  });
});
