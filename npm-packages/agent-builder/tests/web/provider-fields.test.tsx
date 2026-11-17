import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from './dom';

installDom();

const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
const { cleanup, render, screen } = await import('@testing-library/react');
const { ProviderFields } = await import('../../src/features/agents/provider-fields');

afterEach(() => {
  cleanup();
});

describe('ProviderFields', () => {
  test('warns about the missing key without the removed echo runtime wording', () => {
    render(
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        <ProviderFields
          value={{ id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' }}
          onChange={() => {}}
        />
      </QueryClientProvider>,
    );

    expect(screen.getByText(/旧内嵌配置仅可编辑/)).toBeTruthy();
    expect(document.body.textContent ?? '').not.toContain('echo');
  });
});
