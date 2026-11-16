import { afterEach, describe, expect, test } from 'bun:test';
import type { Agent, AgentCapabilities, AgentPreviewResponse } from '@shared/agent';
import type { ReactNode } from 'react';
import { installDom } from './dom';

installDom();

const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { cleanup, fireEvent, render, screen } = await import('@testing-library/react');
const { MemoryRouter, Route, Routes } = await import('react-router');
const { AgentPreviewPage } = await import('../../src/features/agents/agent-preview-page');
const { AgentToolsPage } = await import('../../src/features/agents/agent-tools-page');

const agent: Agent = {
  id: '00000000-0000-4000-8000-000000000001',
  name: 'Reviewer',
  description: 'Checks changes',
  createdAt: 1,
  updatedAt: 2,
  revision: 1,
  config: {
    instructions: 'Be concise',
    provider: { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' },
    metaHarness: { '01_intro': true },
    mcpServers: { docs: { type: 'http', url: 'https://example.com/mcp' } },
  },
};

const capabilities: AgentCapabilities = {
  metaHarnessKeys: ['01_intro', 'McpMiddleware'],
  sendTextMaxLength: 32_768,
  providers: [{ type: 'anthropic', configured: true }],
  chatRuntime: 'not_configured',
  chatRuntimeReasons: ['store_token_missing'],
  historyPersisted: false,
  sandbox: { mode: 'disabled', reasons: [] },
};

const preview: AgentPreviewResponse = {
  target: {
    chatId: 'preview',
    workspace: 'peri+sandbox://00000000-0000-4000-8000-000000000001/tmp/agent-builder-workspace',
    agent: {
      settings: {
        config: {
          active_alias: 'sonnet',
          providers: [{ id: 'anthropic-main', type: 'anthropic', apiKey: '[redacted]' }],
          profiles: { sonnet: { provider: 'anthropic-main', model: 'claude-sonnet-4-5' } },
          meta_harness: { '01_intro': true },
        },
      },
    },
    storage: { url: 'http://127.0.0.1:8091/db/local_default/agent_builder/main', authToken: '[redacted]' },
    session: { instructions: 'Be concise' },
  },
  warnings: [
    { code: 'mcp_not_connected', message: '真实运行时未配置，MCP 服务器不会连接；预览只验证配置。' },
    { code: 'history_not_persisted', message: '当前运行时不持久化会话历史；重启后历史为空。' },
  ],
};

const jsonResponse = (payload: unknown, status = 200) =>
  new Response(JSON.stringify(payload), {
    status,
    headers: { 'content-type': 'application/json' },
  });

/** Stubs must not outlive the file: other test files rely on the real fetch. */
const nativeFetch = globalThis.fetch;

function stubFetch(): void {
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
    if (url === '/api/agents/preview') return jsonResponse(preview);
    if (url === '/api/agents/00000000-0000-4000-8000-000000000001') return jsonResponse({ agent });
    if (url === '/api/capabilities') return jsonResponse(capabilities);
    return jsonResponse(
      { error: { code: 'not_found', message: 'missing', details: {}, retryable: false } },
      404,
    );
  }) as unknown as typeof fetch;
}

function renderRoute(entry: string, element: ReactNode) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/agents/:agentId/preview" element={element} />
          <Route path="/agents/:agentId/tools" element={element} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  globalThis.fetch = nativeFetch;
});

describe('AgentPreviewPage', () => {
  test('renders the redacted target, warnings and capability overrides', async () => {
    stubFetch();
    renderRoute('/agents/00000000-0000-4000-8000-000000000001/preview', <AgentPreviewPage />);

    expect(await screen.findByText('运行目标')).toBeTruthy();
    expect((await screen.findAllByText(/\[redacted\]/)).length).toBeGreaterThan(0);
    expect(screen.getByText('mcp_not_connected')).toBeTruthy();
    expect(screen.getByText('history_not_persisted')).toBeTruthy();
    expect(screen.getByText('01_intro')).toBeTruthy();
  });

  test('shows an error state instead of a stuck skeleton when the agent request fails', async () => {
    globalThis.fetch = (async () =>
      jsonResponse(
        { error: { code: 'not_found', message: 'Agent not found', details: {}, retryable: false } },
        404,
      )) as unknown as typeof fetch;

    renderRoute('/agents/00000000-0000-4000-8000-000000000001/preview', <AgentPreviewPage />);

    expect(await screen.findByText('无法生成预览')).toBeTruthy();
    expect(screen.getByText('Requested resource not found')).toBeTruthy();
    expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
    expect(screen.getByRole('link', { name: '返回列表' }).getAttribute('href')).toBe('/agents');
  });

  test('shows an error state when the preview request fails', async () => {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
      if (url === '/api/agents/00000000-0000-4000-8000-000000000001') return jsonResponse({ agent });
      return jsonResponse(
        { error: { code: 'internal_error', message: 'Preview failed', details: {}, retryable: true } },
        500,
      );
    }) as unknown as typeof fetch;

    renderRoute('/agents/00000000-0000-4000-8000-000000000001/preview', <AgentPreviewPage />);

    expect(await screen.findByText('无法生成预览')).toBeTruthy();
    expect(screen.getByText('Internal server error')).toBeTruthy();
  });
});

describe('AgentToolsPage', () => {
  test('searches capabilities and toggles a tri-state value', async () => {
    stubFetch();
    renderRoute('/agents/00000000-0000-4000-8000-000000000001/tools', <AgentToolsPage />);

    expect(await screen.findByText('docs')).toBeTruthy();
    expect(screen.getByText('01_intro')).toBeTruthy();
    expect(screen.getByText('McpMiddleware')).toBeTruthy();

    fireEvent.change(screen.getByLabelText('搜索能力键'), { target: { value: 'mcp' } });
    expect(screen.queryByText('01_intro')).toBeNull();
    expect(screen.getByText('McpMiddleware')).toBeTruthy();

    fireEvent.change(screen.getByLabelText('搜索能力键'), { target: { value: '' } });
    const offButtons = screen.getAllByRole('button', { name: '关' });
    fireEvent.click(offButtons[0] as HTMLElement);

    expect((offButtons[0] as HTMLElement).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByRole('button', { name: /保存工具配置/ }).hasAttribute('disabled')).toBe(false);
  });
});
