import { afterEach, describe, expect, mock, test } from 'bun:test';
import type { AgentDraft } from '@shared/agent';
import { installDom } from './dom';

installDom();

const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { cleanup, fireEvent, render, screen, waitFor } = await import('@testing-library/react');
const { MemoryRouter } = await import('react-router');
const { AgentForm } = await import('../../src/features/agents/agent-form');
const { EMPTY_AGENT_DRAFT } = await import('../../src/features/agents/draft');

const capabilities = {
  metaHarnessKeys: ['01_intro', 'McpMiddleware'],
  sendTextMaxLength: 32_768,
  providers: [
    { type: 'anthropic' as const, configured: true },
    { type: 'openai' as const, configured: false },
  ],
  chatRuntime: 'not_configured' as const,
  chatRuntimeReasons: ['store_token_missing'],
  historyPersisted: false,
  sandbox: { mode: 'disabled' as const, reasons: [] },
};

const nativeFetch = globalThis.fetch;
function renderForm(onSubmit: (draft: AgentDraft) => Promise<void>) {
  globalThis.fetch = (async () =>
    new Response(JSON.stringify({ providers: [] }), {
      headers: { 'content-type': 'application/json' },
    })) as unknown as typeof fetch;
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <AgentForm
          defaultValues={EMPTY_AGENT_DRAFT}
          submitLabel="创建 Agent"
          capabilities={capabilities}
          onSubmit={onSubmit}
        />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  globalThis.fetch = nativeFetch;
});

describe('AgentForm', () => {
  test('blocks submission and marks the name field invalid when it is empty', async () => {
    const onSubmit = mock(async (_draft: AgentDraft) => {});
    renderForm(onSubmit);

    const name = screen.getByLabelText('名称');
    expect(name).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: /创建 Agent/ }));

    await waitFor(() => {
      expect(name.getAttribute('aria-invalid')).toBe('true');
    });
    expect(onSubmit).not.toHaveBeenCalled();
  });

  test('submits a valid draft built from the form values and capabilities', async () => {
    const onSubmit = mock(async (_draft: AgentDraft) => {});
    renderForm(onSubmit);

    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'Reviewer' } });
    fireEvent.click(screen.getByText(/高级设置/));
    fireEvent.change(screen.getByLabelText('描述'), { target: { value: 'Checks changes' } });
    fireEvent.click(screen.getByRole('button', { name: /创建 Agent/ }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const draft = onSubmit.mock.calls[0]?.[0];
    expect(draft?.name).toBe('Reviewer');
    expect(draft?.description).toBe('Checks changes');
    expect(draft?.config.provider).toBeNull();
    expect(draft?.config.mcpServers).toEqual({});
    expect(screen.getByText('01_intro')).toBeTruthy();
  });
});
