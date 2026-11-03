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

describe('composer limits, history and retry', () => {
  test('over-limit drafts cannot be sent and keep the original text', async () => {
    const { MAX_COMPOSER_CHARS } = await import('./composerLimits');
    const events = setup({ draft: 'x'.repeat(MAX_COMPOSER_CHARS + 1) });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;

    expect((screen.getByLabelText('发送') as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole('alert').textContent).toContain('已超出 1 字');
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });
    expect(events.sent).toEqual([]);
    expect(events.drafts).toEqual([]);
  });

  test('the exact limit is still sendable', async () => {
    const { MAX_COMPOSER_CHARS } = await import('./composerLimits');
    const text = 'x'.repeat(MAX_COMPOSER_CHARS);
    const events = setup({ draft: text });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;

    expect((screen.getByLabelText('发送') as HTMLButtonElement).disabled).toBe(false);
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });
    expect(events.sent).toEqual([text]);
  });

  test('empty draft recalls history with ArrowUp and clears with ArrowDown', () => {
    const events = setup({ draft: '', historyPrompts: ['第一条', '第二条'] });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    textarea.setSelectionRange(0, 0);

    fireEvent.keyDown(textarea, { key: 'ArrowUp', keyCode: 38 });
    expect(events.drafts.at(-1)).toBe('第二条');
    fireEvent.keyDown(textarea, { key: 'ArrowDown', keyCode: 40 });
    expect(events.drafts.at(-1)).toBe('');
  });

  test('editing exits recall and IME composition never recalls', () => {
    const events = setup({ draft: '', historyPrompts: ['旧提示'] });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;

    const composing = new window.KeyboardEvent('keydown', {
      key: 'ArrowUp', code: 'ArrowUp', keyCode: 38, bubbles: true,
    });
    Object.defineProperty(composing, 'isComposing', { value: true });
    textarea.dispatchEvent(composing);
    expect(events.drafts).toEqual([]);

    textarea.setSelectionRange(0, 0);
    fireEvent.keyDown(textarea, { key: 'ArrowUp', keyCode: 38 });
    expect(events.drafts).toEqual(['旧提示']);

    fireEvent.change(textarea, { target: { value: '改写' } });
    // 改写后召回游标退出；再按 ArrowDown 不应清空或继续召回。
    fireEvent.keyDown(textarea, { key: 'ArrowDown', keyCode: 40 });
    expect(events.drafts).toEqual(['旧提示', '改写']);
  });

  test('a rejected send keeps the draft and exposes a retry action', async () => {
    const sent: string[] = [];
    let attempts = 0;
    const events = setup({
      onSend: async (value: string) => {
        sent.push(value);
        attempts += 1;
        return attempts === 1
          ? { delivered: false as const, sessionId: 'chat-test', message: '服务端拒绝', disposition: 'rejected' as const }
          : { delivered: true as const, sessionId: 'chat-test' };
      },
    });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });

    expect(sent).toEqual(['你好']);
    expect(events.drafts).toEqual([]);
    const retry = screen.getByRole('button', { name: '重试发送' });
    await act(async () => {
      fireEvent.click(retry);
      await Promise.resolve();
    });
    expect(sent).toEqual(['你好', '你好']);
    expect(events.drafts.at(-1)).toBe('');
  });

  test('unknown send results do not offer retry', async () => {
    setup({
      onSend: async () => ({
        delivered: false as const, sessionId: 'chat-test', message: '状态未知', disposition: 'unknown' as const,
      }),
    });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });
    expect(screen.queryByRole('button', { name: '重试发送' })).toBeNull();
  });

  test('a banner-covered failure does not repeat the message in the composer', async () => {
    setup({
      errorBanner: '服务端拒绝',
      onSend: async () => ({
        delivered: false as const, sessionId: 'chat-test', message: '服务端拒绝', disposition: 'rejected' as const,
      }),
    });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    await act(async () => { await Promise.resolve(); });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByRole('button', { name: '重试发送' })).toBeTruthy();
  });

  test('stopping disables the stop button and announces the phase', () => {
    setup({ canSend: false, canStop: true, stopping: true });
    const stop = screen.getByLabelText('停止') as HTMLButtonElement;
    expect(stop.disabled).toBe(true);
    expect(screen.getByText('正在停止…')).toBeTruthy();
  });
});

describe('composer slash menu and status wiring', () => {
  const commands = [{ name: 'help', description: '显示帮助' }];

  test('slash menu selects with Enter without sending', () => {
    const events = setup({ draft: '/he', commands });
    expect(screen.getByRole('listbox', { name: '斜杠命令' })).toBeTruthy();
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 });
    expect(events.sent).toEqual([]);
    expect(events.drafts.at(-1)).toBe('/help ');
  });

  test('Escape closes the menu and empty results show an empty state', () => {
    setup({ draft: '/he', commands });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.keyDown(textarea, { key: 'Escape', code: 'Escape', keyCode: 27 });
    expect(screen.queryByRole('listbox')).toBeNull();

    cleanup();
    const empty = setup({ draft: '/zzz', commands });
    expect(screen.getByText('没有匹配的命令')).toBeTruthy();
    fireEvent.keyDown(screen.getByLabelText('输入消息'), { key: 'Enter', code: 'Enter', keyCode: 13 });
    expect(empty.sent).toEqual([]);
  });

  test('Enter during IME composition does not pick a command', () => {
    const events = setup({ draft: '/he', commands });
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    const composing = new window.KeyboardEvent('keydown', {
      key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true,
    });
    Object.defineProperty(composing, 'isComposing', { value: true });
    textarea.dispatchEvent(composing);
    expect(events.drafts).toEqual([]);
    expect(events.sent).toEqual([]);
    expect(screen.getByRole('listbox')).toBeTruthy();
  });

  test('renders meta, usage and keeps the textarea described by hints', () => {
    setup({
      meta: {
        model: {
          id: 'model', label: '模型', currentValue: 'fast', currentLabel: 'Fast',
          options: [{ value: 'fast', name: 'Fast' }, { value: 'quality', name: 'Quality' }],
        },
      },
      usage: { used: 50, max: 100, percent: 50 },
      commands: [],
    });
    expect(screen.getByText('Fast')).toBeTruthy();
    expect(screen.getByRole('button', { name: /上下文用量 50%/ })).toBeTruthy();
    const textarea = screen.getByLabelText('输入消息');
    expect(textarea.getAttribute('aria-describedby')).toContain('-hint');
  });
});
