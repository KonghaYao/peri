import { cleanup, render, screen } from '@solidjs/testing-library';
import { afterEach, describe, expect, it } from 'vitest';
import { PromptRecoveryNotice } from './PromptRecoveryNotice';

afterEach(cleanup);

describe('PromptRecoveryNotice', () => {
  it('shows safe evidence without inventing message content or runtime recovery', () => {
    render(() => <PromptRecoveryNotice recovery={{
      sessionId: 'session-1',
      loading: false,
      truncated: false,
      evidenceIncomplete: false,
      error: null,
      prompts: [{
        commandId: '12345678-1234-1234-1234-123456789abc',
        status: 'delivery_unknown',
        createdAt: '2026-08-14T00:00:00Z',
        updatedAt: '2026-08-14T00:00:01Z',
      }],
    }} />);
    expect(screen.getByText('投递结果尚未确认')).toBeTruthy();
    expect(screen.getByText('旧运行实例没有被恢复')).toBeTruthy();
  });

  it('does not turn completed history into persistent UI noise', () => {
    const { container } = render(() => <PromptRecoveryNotice recovery={{
      sessionId: 'session-1', loading: false, truncated: false, evidenceIncomplete: false, error: null,
      prompts: [{ commandId: 'cmd-1', status: 'completed', createdAt: '2026-08-14T00:00:00Z', updatedAt: '2026-08-14T00:00:01Z' }],
    }} />);
    expect(container.textContent).toBe('');
  });

  it('never turns missing historical evidence into an all-clear', () => {
    render(() => <PromptRecoveryNotice recovery={{
      sessionId: 'session-1', loading: false, truncated: false, evidenceIncomplete: true, error: null, prompts: [],
    }} />);
    expect(screen.getByText('部分历史证据已不可用，系统无法证明更早消息的最终状态。')).toBeTruthy();
  });
});
