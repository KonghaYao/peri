import { fireEvent, render, screen } from '@solidjs/testing-library';
import { describe, expect, it, vi } from 'vitest';
import type { ChatEntry } from '../lib/chat-view';
import { ConversationMessage } from './ConversationMessage';

function entry(overrides: Partial<ChatEntry> = {}): ChatEntry {
  return {
    id: 'entry-1', turnId: 'turn-1', kind: 'message', role: 'assistant', status: 'completed', authorUserId: null, sourceCommandId: null,
    createdAt: '2026-08-13T12:00:00Z', completedAt: '2026-08-13T12:00:01Z', text: '', reasoning: [], toolCalls: [], resources: [], error: null,
    ...overrides,
  };
}

describe('ConversationMessage', () => {
  it('keeps user text plain and visually separate from assistant Markdown', () => {
    const view = render(() => <ConversationMessage entry={entry({ role: 'user', text: '**literal user input**' })} />);
    const message = screen.getByLabelText('你的消息');
    expect(message).toHaveClass('conversation-message--user');
    expect(message).toHaveTextContent('**literal user input**');
    expect(message.querySelector('strong')).toBeNull();
    expect(screen.queryByRole('button', { name: '复制回答' })).not.toBeInTheDocument();
    view.unmount();
  });

  it('renders completed assistant content as a coding reader with copy', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    render(() => <ConversationMessage entry={entry({ text: '## Result\n\n`cargo test` passed.' })} />);

    expect(screen.getByLabelText('助手消息')).toHaveClass('conversation-message--assistant');
    expect(screen.getByRole('heading', { name: 'Result' })).toBeInTheDocument();
    expect(screen.getByText('cargo test')).toHaveClass('md-inline-code');
    fireEvent.click(screen.getByRole('button', { name: '复制回答' }));
    expect(writeText).toHaveBeenCalledWith('## Result\n\n`cargo test` passed.');
  });

  it('keeps streaming assistant text plain and exposes no copy action yet', () => {
    render(() => <ConversationMessage entry={entry({ status: 'streaming', text: '**partial' })} />);
    expect(screen.getByText('**partial')).toHaveClass('message-plain-text');
    expect(screen.queryByRole('button', { name: '复制回答' })).not.toBeInTheDocument();
    expect(document.querySelector('.message-loading')).toHaveAttribute('aria-hidden', 'true');
  });

  it('presents reasoning, resources and errors as distinct evidence layers', () => {
    render(() => <ConversationMessage entry={entry({
      reasoning: [{ text: 'checked repository state', visibility: 'visible' }],
      resources: [{ resourceId: 'file:///workspace/src/main.rs', mediaType: 'text/rust', name: 'main.rs' }],
      error: { code: 'TOOL_FAILED', message: 'exit 1' },
    })} />);

    expect(screen.getByText('思考过程').closest('details')).not.toHaveAttribute('open');
    expect(screen.getByText('checked repository state')).toBeInTheDocument();
    expect(screen.getByLabelText('main.rs')).toHaveTextContent('text/rust');
    expect(screen.getByTitle('file:///workspace/src/main.rs')).toBeInTheDocument();
    expect(screen.getByRole('alert', { name: '消息错误' })).toHaveTextContent('TOOL_FAILED: exit 1');
  });

  it('gives an empty projected assistant entry one quiet progress announcement', () => {
    render(() => <ConversationMessage entry={entry({ status: 'pending' })} />);
    expect(screen.getByText('正在生成回答')).toHaveClass('sr-only');
    expect(document.querySelectorAll('.message-loading')).toHaveLength(1);
  });

  it('keeps a reloaded unknown user delivery visibly blocked from retry', () => {
    render(() => <ConversationMessage entry={entry({
      role: 'user',
      status: 'pending',
      text: 'potentially executed prompt',
      deliveryState: 'delivery_unknown',
      deliveryErrorCode: 'DELIVERY_UNKNOWN',
    })} />);
    expect(screen.getByRole('alert')).toHaveTextContent('投递结果未知');
    expect(screen.getByRole('alert')).toHaveTextContent('不会自动重发');
  });
});
