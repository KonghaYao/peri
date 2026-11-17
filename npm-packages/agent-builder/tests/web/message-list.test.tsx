import { afterEach, describe, expect, test } from 'bun:test';
import type { ChatMessage } from '@shared/chat';
import { installDom } from './dom';

installDom();

const { cleanup, render, screen } = await import('@testing-library/react');
const { MessageList } = await import('../../src/features/chats/message-list');

function assistantMessage(status: string): ChatMessage {
  return { id: 'm-1', role: 'assistant', text: 'done', status, createdAt: null };
}

afterEach(() => {
  cleanup();
});

describe('MessageList', () => {
  test('does not append a status suffix to completed messages', () => {
    render(<MessageList messages={[assistantMessage('completed')]} running={false} />);

    const article = screen.getByTestId('chat-message');
    expect(article.textContent).toContain('Agent');
    expect(article.textContent).toContain('done');
    expect(article.textContent).not.toContain('completed');
  });

  test('still shows non-terminal statuses', () => {
    render(<MessageList messages={[assistantMessage('streaming')]} running />);

    expect(screen.getByTestId('chat-message').textContent).toContain('· streaming');
  });
});
