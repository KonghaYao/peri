import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { cleanup, fireEvent, render, screen, act } = await import('@testing-library/react');
const { ChatComposer } = await import('./ChatComposer');
const { createElement } = await import('react');

afterEach(() => cleanup());

function setup(overrides: Record<string, unknown> = {}) {
  const events = { sent: [] as string[], stopped: 0, drafts: [] as string[] };
  render(createElement(ChatComposer, {
    draft: '你好', onDraftChange: (value: string) => events.drafts.push(value),
    canSend: true, canStop: false, stopping: false,
    onSend: async (value: string) => {
      events.sent.push(value);
      return { delivered: true as const, sessionId: 'chat-test' };
    },
    onStop: async () => { events.stopped += 1; return true; },
    focusSignal: 0,
    ...overrides,
  }));
  return events;
}

describe('ChatComposer real DOM keyboard interactions', () => {
  test('Enter sends and Shift+Enter or IME composition does not', async () => {
    const events = setup();
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;

    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });
    expect(events.sent).toEqual(['你好']);

    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13, shiftKey: true });
    await act(async () => { await Promise.resolve(); });
    expect(events.sent).toEqual(['你好']);

    const composing = new window.KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true });
    Object.defineProperty(composing, 'isComposing', { value: true });
    textarea.dispatchEvent(composing);
    await act(async () => { await Promise.resolve(); });
    expect(events.sent).toEqual(['你好']);
  });

  test('clicking send submits, clicking stop calls onStop while running', async () => {
    const events = setup({ canSend: false, canStop: true });
    fireEvent.click(screen.getByLabelText('停止'));
    await act(async () => { await Promise.resolve(); });
    expect(events.stopped).toBe(1);

    cleanup();
    const idle = setup();
    fireEvent.click(screen.getByLabelText('发送'));
    await act(async () => { await Promise.resolve(); });
    expect(idle.sent).toEqual(['你好']);
  });

  test('an empty draft cannot be sent and a typing change reaches the draft callback', () => {
    const events = setup({ draft: '' });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    expect((screen.getByLabelText('发送') as HTMLButtonElement).disabled).toBe(true);
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    expect(events.sent).toEqual([]);

    fireEvent.change(textarea, { target: { value: '新内容' } });
    expect(events.drafts).toEqual(['新内容']);
  });
});

describe('send delivery outcomes', () => {
  test('a withdrawn delivery keeps the draft and shows the reason', async () => {
    const drafts: string[] = [];
    const sent: string[] = [];
    setup({
      onDraftChange: (value: string) => drafts.push(value),
      onSend: async (value: string) => {
        sent.push(value);
        return { delivered: false as const, sessionId: 'chat-test', message: '输入已撤回，消息未送达。' };
      },
    });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });

    expect(sent).toEqual(['你好']);
    // 202 不算送达：撤回时不发清空指令，草稿原样保留。
    expect(drafts).toEqual([]);
    expect(screen.getByRole('alert').textContent).toContain('输入已撤回');
  });

  test('a confirmed delivery clears only the submitted draft', async () => {
    const drafts: string[] = [];
    setup({
      onDraftChange: (value: string) => drafts.push(value),
      onSend: async () => ({ delivered: true as const, sessionId: 'chat-test' }),
    });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });

    expect(drafts).toEqual(['']);
    expect(screen.queryByRole('alert')).toBeNull();
  });
});
