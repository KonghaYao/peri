import { fireEvent, render, screen } from '@solidjs/testing-library';
import { describe, expect, it, vi } from 'vitest';
import type { MessageSubmission } from '../lib/message-delivery';
import { MessageOutbox } from './MessageOutbox';

const submission = (phase: MessageSubmission['phase']): MessageSubmission => ({
  commandId: 'command-1',
  text: 'do not lose this work',
  sessionId: 'session-1',
  chatId: 'chat-1',
  phase,
  detail: phase === 'uncertain' ? '服务器尚未确认结果。' : null,
  retryable: phase === 'uncertain',
  projected: false,
});

describe('MessageOutbox', () => {
  it('keeps the full message visible and offers only the identity-safe retry', () => {
    const retry = vi.fn();
    render(() => <MessageOutbox submission={submission('uncertain')} onRetry={retry} onEdit={vi.fn()} />);
    expect(screen.getByRole('alert', { name: '你的待确认消息' })).toHaveTextContent('do not lose this work');
    expect(screen.getByText('结果尚未确认')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '使用同一请求重新确认' }));
    expect(retry).toHaveBeenCalledOnce();
    expect(screen.queryByRole('button', { name: '返回编辑' })).not.toBeInTheDocument();
  });

  it('returns a definite failure to editing without offering unsafe retry', () => {
    const edit = vi.fn();
    render(() => <MessageOutbox submission={submission('failed')} onRetry={vi.fn()} onEdit={edit} />);
    expect(screen.queryByRole('button', { name: '使用同一请求重新确认' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '返回编辑' }));
    expect(edit).toHaveBeenCalledOnce();
  });

  it('keeps delivery-unknown visible but offers neither retry nor edit', () => {
    render(() => <MessageOutbox submission={submission('delivery_unknown')} onRetry={vi.fn()} onEdit={vi.fn()} />);
    expect(screen.getByText('投递结果未知，请勿重发')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '复制原文' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '使用同一请求重新确认' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '返回编辑' })).not.toBeInTheDocument();
  });
});
