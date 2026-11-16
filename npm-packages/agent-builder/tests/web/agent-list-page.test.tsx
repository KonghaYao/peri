import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from './dom';

installDom();

const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { cleanup, fireEvent, render, screen } = await import('@testing-library/react');
const { MemoryRouter } = await import('react-router');
const { AgentListPage } = await import('../../src/features/agents/agent-list-page');

const jsonResponse = (payload: unknown, status = 200) =>
  new Response(JSON.stringify(payload), {
    status,
    headers: { 'content-type': 'application/json' },
  });

/** Stubs must not outlive the file: other test files rely on the real fetch. */
const nativeFetch = globalThis.fetch;

function stubFetch(handler: () => Response): void {
  globalThis.fetch = (async () => handler()) as unknown as typeof fetch;
}

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const view = render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <AgentListPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { view, queryClient };
}

afterEach(() => {
  cleanup();
  globalThis.fetch = nativeFetch;
});

describe('AgentListPage', () => {
  test('shows an empty state and a create action when there are no agents', async () => {
    stubFetch(() => jsonResponse({ agents: [] }));
    renderPage();

    expect(await screen.findByText('还没有 Agent')).toBeTruthy();
    expect(screen.getAllByRole('link', { name: /新建 Agent/ }).length).toBeGreaterThan(0);
  });

  test('shows an error state and retries the query', async () => {
    let calls = 0;
    stubFetch(() => {
      calls += 1;
      if (calls === 1) {
        return jsonResponse(
          { error: { code: 'internal_error', message: '列表加载失败', details: {}, retryable: true } },
          500,
        );
      }
      return jsonResponse({
        agents: [
          {
            id: '00000000-0000-4000-8000-000000000000',
            name: 'Reviewer',
            description: 'Checks changes',
            createdAt: 1,
            updatedAt: 2,
            revision: 1,
          },
        ],
      });
    });

    renderPage();

    expect(await screen.findByText('无法加载 Agent 列表')).toBeTruthy();
    expect(await screen.findByText('Internal server error')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '重试' }));

    expect(await screen.findByText('Reviewer')).toBeTruthy();
    expect(calls).toBe(2);
  });
});
