import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ChatApi } from '../api/client';
import { MessageList } from './MessageList';
import { messageStatuses } from './types';
import type { Message, Phase } from './types';

const chat = { id: 'chat-status', title: '状态回归', updatedAt: '2026-10-06T00:00:00Z' };
const message: Message = {
  id: 'assistant-status', role: 'assistant', content: '', createdAt: '2026-10-06T00:00:00Z',
};
let fetchSpy: ReturnType<typeof spyOn<typeof globalThis, 'fetch'>> | undefined;

afterEach(() => {
  fetchSpy?.mockRestore();
  fetchSpy = undefined;
});

function respond(messages: unknown[]) {
  fetchSpy = spyOn(globalThis, 'fetch').mockResolvedValue(Response.json({ chat, messages }));
}

function render(overrides: Partial<Message>, phase: Phase = 'idle') {
  return renderToStaticMarkup(<MessageList messages={[{ ...message, ...overrides }]} phase={phase} chatId={chat.id} />);
}

describe('message status API boundary', () => {
  test('accepts messages with omitted optional fields', async () => {
    respond([message]);
    expect((await new ChatApi('test-token').detail(chat.id)).messages).toEqual([message]);
  });

  test.each(messageStatuses.map(status => ({ status })))('preserves valid status %j and error text', async ({ status }) => {
    const stored = { ...message, content: '已保存的部分回答', status, error: '错误详情' };
    respond([stored]);
    expect((await new ChatApi('test-token').detail(chat.id)).messages).toEqual([stored]);
  });

  test.each(['unknown', '', null, 42, {}, []].map(value => ({ value })))('rejects invalid status %j', async ({ value: status }) => {
    respond([{ ...message, status }]);
    await expect(new ChatApi('test-token').detail(chat.id)).rejects.toThrow('会话历史格式不正确');
  });

  test.each([null, 42, {}, []].map(value => ({ value })))('rejects invalid error %j', async ({ value: error }) => {
    respond([{ ...message, status: 'error', error }]);
    await expect(new ChatApi('test-token').detail(chat.id)).rejects.toThrow('会话历史格式不正确');
  });
});

describe('historical assistant status rendering', () => {
  test('shows failure instead of a generic empty answer', () => {
    const output = render({ status: 'error', error: '模型调用失败' });
    expect(output).toContain('生成失败');
    expect(output).toContain('模型调用失败');
    expect(output).not.toContain('这条回答没有文本内容');
  });

  test('keeps partial markdown and copy action on failure', () => {
    const output = render({ status: 'error', content: '**已有片段**', error: '连接中断' });
    expect(output).toContain('<strong>已有片段</strong>');
    expect(output).toContain('连接中断');
    expect(output).toContain('复制回答');
  });

  test('shows cancellation for an empty historical answer', () => {
    const output = render({ status: 'cancelled' });
    expect(output).toContain('已停止生成');
    expect(output).toContain('本次生成已取消');
    expect(output).not.toContain('这条回答没有文本内容');
  });

  test('keeps partial content and suppresses thinking for cancelled terminal messages', () => {
    const output = render({ status: 'cancelled', content: '停止前的片段' }, 'streaming');
    expect(output).toContain('停止前的片段');
    expect(output).toContain('已停止生成');
    expect(output).toContain('复制回答');
    expect(output).not.toContain('class="thinking"');
  });

  test('uses a fallback for missing or blank error details', () => {
    expect(render({ status: 'error' })).toContain('这次回答未能完成');
    expect(render({ status: 'error', error: '  ' })).toContain('这次回答未能完成');
  });

  test('renders error details as escaped text, not HTML or markdown', () => {
    const output = render({ status: 'error', error: '<script>alert(1)</script> **错误**' });
    expect(output).toContain('&lt;script&gt;alert(1)&lt;/script&gt; **错误**');
    expect(output).not.toContain('<script>');
    expect(output).not.toContain('<strong>错误</strong>');
  });

  test('supports error text when optional status is absent', () => {
    expect(render({ error: '执行失败' })).toContain('生成失败');
  });

  test('does not imply a live stream for a historical running message', () => {
    const output = render({ status: 'running' });
    expect(output).toContain('生成尚未完成');
    expect(output).not.toContain('class="thinking"');
    expect(output).not.toContain('这条回答没有文本内容');
  });

  test('preserves completed answers without interrupted status labels', () => {
    const output = render({ status: 'completed', content: '完整回答' });
    expect(output).toContain('完整回答');
    expect(output).not.toContain('生成失败');
    expect(output).not.toContain('已停止生成');
  });
});
