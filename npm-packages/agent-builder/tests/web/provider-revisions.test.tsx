import { afterEach, expect, test } from 'bun:test';
import type { Agent, AgentCapabilities } from '@shared/agent';
import type { Provider } from '@shared/provider';
import { installDom } from './dom';

installDom();
const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { act, cleanup, fireEvent, render, screen, waitFor, within } = await import('@testing-library/react');
const { MemoryRouter, Route, Routes, createMemoryRouter, RouterProvider } = await import('react-router');
const { AgentEditPage } = await import('../../src/features/agents/agent-edit-page');
const { AgentCreatePage } = await import('../../src/features/agents/agent-create-page');
const { ProviderForm } = await import('../../src/features/providers/provider-form');
const nativeFetch = globalThis.fetch;
const clients: InstanceType<typeof QueryClient>[] = [];
afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  globalThis.fetch = nativeFetch;
});
const caps: AgentCapabilities = {
  metaHarnessKeys: [],
  sendTextMaxLength: 32768,
  providers: [],
  chatRuntime: 'not_configured',
  chatRuntimeReady: false,
  chatRuntimeReasons: ['store_token_missing'],
  historyPersisted: false,
  sandbox: { mode: 'disabled', reasons: [] },
};
const agent: Agent = {
  id: '00000000-0000-4000-8000-000000000001',
  name: 'Initial',
  description: '',
  revision: 1,
  createdAt: 1,
  updatedAt: 1,
  config: { provider: null, instructions: '', metaHarness: {}, mcpServers: {} },
};
const provider: Provider = {
  id: agent.id,
  name: 'Initial',
  type: 'openai',
  baseUrl: null,
  credentialSource: 'local',
  hasCredential: false,
  revision: 1,
  agentCount: 0,
  runtimeCount: 0,
};
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
const conflict = () =>
  json({ error: { code: 'revision_conflict', message: 'safe', details: {}, retryable: false } }, 409);
function client() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  return client;
}

test('Agent accepts server revision, preserves stale dirty draft on conflict, and explicitly reloads before next CAS', async () => {
  let stored = structuredClone(agent);
  const seen: string[] = [];
  globalThis.fetch = (async (url: RequestInfo | URL, init?: RequestInit) => {
    if (String(url) === '/api/capabilities') return json(caps);
    if (String(url) === '/api/providers') return json({ providers: [] });
    if (init?.method === 'PUT') {
      const revision = new Headers(init.headers).get('If-Match') ?? '';
      seen.push(revision);
      if (Number(revision) !== stored.revision) return conflict();
      stored = { ...stored, ...JSON.parse(String(init.body)), revision: stored.revision + 1 };
    }
    return json({ agent: stored });
  }) as unknown as typeof fetch;
  const c = client();
  render(
    <QueryClientProvider client={c}>
      <MemoryRouter initialEntries={[`/agents/${agent.id}/edit`]}>
        <Routes>
          <Route path="/agents/:agentId/edit" element={<AgentEditPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  await screen.findByLabelText('名称');
  fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'First save' } });
  fireEvent.click(screen.getByRole('button', { name: '保存修改' }));
  await waitFor(() => expect(stored.revision).toBe(2));
  await waitFor(() => expect(screen.queryByText('保存中…')).toBeNull());
  stored = { ...stored, name: 'External change', revision: 3 };
  fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'Dirty local' } });
  await act(async () => {
    c.setQueryData(['agent', agent.id], stored);
  });
  expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('Dirty local');
  fireEvent.click(screen.getByRole('button', { name: '保存修改' }));
  await screen.findByRole('alert');
  expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('Dirty local');
  expect(stored.name).toBe('External change');
  expect((c.getQueryData(['agent', agent.id]) as Agent).name).toBe('External change');
  fireEvent.click(screen.getByRole('button', { name: '重新加载服务端版本' }));
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '重新加载' }));
  await waitFor(() =>
    expect((screen.getByLabelText('名称') as HTMLInputElement).value).toBe('External change'),
  );
  fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'After reload' } });
  fireEvent.click(screen.getByRole('button', { name: '保存修改' }));
  await waitFor(() => expect(stored.revision).toBe(4));
  expect(seen).toEqual(['1', '2', '3']);
});

test('Provider success updates revision; conflict keeps metadata but clears password without caching candidate', async () => {
  let stored = { ...provider };
  const seen: string[] = [];
  let reject = false;
  globalThis.fetch = (async (_url: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      seen.push(new Headers(init.headers).get('If-Match') ?? '');
      if (reject) return conflict();
      stored = { ...stored, name: JSON.parse(String(init.body)).name, revision: stored.revision + 1 };
    }
    return json({ provider: stored });
  }) as unknown as typeof fetch;
  const c = client();
  c.setQueryData(['providers'], [provider]);
  render(
    <QueryClientProvider client={c}>
      <ProviderForm initial={provider} onSaved={() => {}} onCancel={() => {}} onPending={() => {}} />
    </QueryClientProvider>,
  );
  fireEvent.change(screen.getByLabelText('Provider 名称'), { target: { value: 'Saved name' } });
  fireEvent.click(screen.getByRole('button', { name: '保存 Provider' }));
  await waitFor(() => expect(stored.revision).toBe(2));
  await waitFor(() => expect(screen.queryByText('保存中…')).toBeNull());
  reject = true;
  const key = crypto.randomUUID();
  fireEvent.change(screen.getByLabelText('Provider 名称'), { target: { value: 'Failed candidate' } });
  fireEvent.change(screen.getByLabelText(/API key/), { target: { value: key } });
  fireEvent.click(screen.getByRole('button', { name: '保存 Provider' }));
  await screen.findByRole('alert');
  expect((screen.getByLabelText('Provider 名称') as HTMLInputElement).value).toBe('Failed candidate');
  expect((screen.getByLabelText(/API key/) as HTMLInputElement).value).toBe('');
  expect(JSON.stringify(c.getQueryData(['providers'])).includes('Failed candidate')).toBe(false);
  expect(JSON.stringify(c.getMutationCache().getAll()).includes(key)).toBe(false);
  expect(seen).toEqual(['1', '2']);
});

test('actual create page navigates after explicit save without a dirty-navigation prompt or automatic Chat', async () => {
  const writes: string[] = [];
  globalThis.fetch = (async (url: RequestInfo | URL, init?: RequestInit) => {
    if (String(url) === '/api/capabilities') return json(caps);
    if (String(url) === '/api/providers') return json({ providers: [] });
    if (init?.method === 'POST') {
      writes.push(String(url));
      return json({ agent: { ...agent, ...JSON.parse(String(init.body)) } }, 201);
    }
    return json({ agents: [] });
  }) as unknown as typeof fetch;
  const c = client();
  const router = createMemoryRouter(
    [
      { path: '/agents/new', element: <AgentCreatePage /> },
      { path: '/agents/:id/edit', element: <p>Saved editor</p> },
    ],
    { initialEntries: ['/agents/new'] },
  );
  render(
    <QueryClientProvider client={c}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  fireEvent.click(await screen.findByRole('button', { name: '应用模板' }));
  expect(writes).toEqual([]);
  fireEvent.click(screen.getByRole('button', { name: '创建 Agent' }));
  await screen.findByText('Saved editor');
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(writes).toEqual(['/api/agents']);
});
