import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { PayloadReadError } from '@peri-code/sdk/view';
import { MessageList } from './MessageList';
import { commandPayloadReader, loadPayloadText, type PayloadCommandSource } from './payloadReader';
import type { Entry, EntryBlock } from './types';

const chat = { id: 'chat-status', title: '状态回归', updatedAt: '2026-10-06T00:00:00Z' };

function textBlock(text: string): EntryBlock {
  return { blockId: 'text:0', type: 'text', text };
}

function entry(overrides: Partial<Entry> = {}): Entry {
  return {
    entryId: 'turn:1:assistant', id: 'turn:1:assistant', kind: 'message', turnId: 'turn:1',
    role: 'assistant', status: 'completed', createdAt: '2026-10-06T00:00:00Z', createdAtSource: 'observed',
    completedAt: '2026-10-06T00:00:01Z', error: null, blocks: [textBlock('')],
    ...overrides,
  };
}

function render(entries: Entry[], phase: 'idle' | 'streaming' = 'idle') {
  return renderToStaticMarkup(<MessageList entries={entries} phase={phase} chatId={chat.id} />);
}

describe('entry rendering follows block order and terminal status', () => {
  test('shows failure instead of a generic empty answer', () => {
    const output = render([entry({ status: 'error', error: { code: 'execution_failed', message: '模型调用失败' }, blocks: [] })]);
    expect(output).toContain('生成失败');
    expect(output).toContain('模型调用失败');
    expect(output).not.toContain('这条回答没有文本内容');
  });

  test('keeps partial markdown and copy action on failure', () => {
    const output = render([entry({ status: 'error', error: { code: 'execution_failed', message: '连接中断' }, blocks: [textBlock('**已有片段**')] })]);
    expect(output).toContain('<strong>已有片段</strong>');
    expect(output).toContain('连接中断');
    expect(output).toContain('复制回答');
  });

  test('shows cancellation for an empty historical answer', () => {
    const output = render([entry({ status: 'cancelled', blocks: [] })]);
    expect(output).toContain('已停止生成');
    expect(output).toContain('本次生成已取消');
    expect(output).not.toContain('这条回答没有文本内容');
  });

  test('renders error details as escaped text, not HTML or markdown', () => {
    const output = render([entry({ status: 'error', error: { code: 'execution_failed', message: '<script>alert(1)</script> **错误**' }, blocks: [] })]);
    expect(output).toContain('&lt;script&gt;alert(1)&lt;/script&gt; **错误**');
    expect(output).not.toContain('<script>');
    expect(output).not.toContain('<strong>错误</strong>');
  });
});

describe('block rendering covers tools, reasoning and unknown objects', () => {
  test('a tool-only entry renders the card and never claims a missing answer', () => {
    const output = render([entry({ blocks: [{ blockId: 'tool:t1', type: 'tool_call', toolCallId: 't1',
      tool: { toolCallId: 't1', turnId: 'turn:1', name: 'Read', status: 'completed', createdAt: null, completedAt: null,
        error: null, result: { contentText: '文件内容预览' } } }] })]);
    expect(output).toContain('Read');
    expect(output).toContain('completed');
    expect(output).toContain('文件内容预览');
    expect(output).not.toContain('这条回答没有文本内容');
  });

  test('a reasoning-only entry stays silent about missing text and hides hidden reasoning', () => {
    const summary = render([entry({ status: 'streaming', blocks: [{ blockId: 'reasoning:0', type: 'reasoning', text: '内部推理', visibility: 'summary' }] })]);
    expect(summary).toContain('思考过程');
    expect(summary).not.toContain('这条回答没有文本内容');
    const hidden = render([entry({ status: 'streaming', blocks: [{ blockId: 'reasoning:0', type: 'reasoning', text: 'PRIVATE', visibility: 'hidden' }] })]);
    expect(hidden).not.toContain('PRIVATE');
  });

  test('orphaned tool references and unknown blocks report an explicit reason', () => {
    const output = render([entry({ blocks: [
      { blockId: 'tool:ghost', type: 'tool_call', toolCallId: 'ghost', tool: null, unavailable: 'missing_tool' },
      { blockId: 'resource:0', type: 'unknown', reason: 'unsupported_block:resource' },
    ] })]);
    expect(output).toContain('工具调用暂不可用');
    expect(output).toContain('引用的工具记录缺失');
    expect(output).toContain('暂不支持的内容块');
    expect(output).toContain('unsupported_block:resource');
    expect(output).not.toContain('这条回答没有文本内容');
  });

  test('renders entries by id in order and keeps an empty completed answer explicit', () => {
    const output = render([
      entry({ entryId: 'turn:1:user', id: 'turn:1:user', role: 'user', status: 'completed', blocks: [textBlock('问题')] }),
      entry({ entryId: 'turn:1:assistant', id: 'turn:1:assistant', blocks: [] }),
    ]);
    expect(output.indexOf('问题')).toBeGreaterThanOrEqual(0);
    expect(output).toContain('这条回答没有文本内容');
  });
});

describe('payload full-text entry point', () => {
  const encoder = new TextEncoder();
  const json = JSON.stringify({ contentText: '全文' });
  const bytes = encoder.encode(json).byteLength;

  function source(readPayload: PayloadCommandSource['readPayload']): PayloadCommandSource { return { readPayload }; }

  test('command reader reads the payload through the session command channel', async () => {
    const refs: string[] = [];
    const reader = commandPayloadReader(() => source(async (refId) => {
      refs.push(refId);
      return { ok: true, payload: { id: refId, bytes, json } };
    }));
    await expect(reader({ id: 'gen:1' })).resolves.toEqual({ id: 'gen:1', bytes, json });
    expect(refs).toEqual(['gen:1']);
  });

  test('command reader maps server failure codes onto the SDK contract', async () => {
    for (const code of ['unauthorized', 'unavailable', 'too_large', 'integrity_error', 'aborted', 'transport_error'] as const) {
      const reader = commandPayloadReader(() => source(async () => ({ ok: false, source: 'server', code, message: 'reason' })));
      const failure = await reader({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string; message?: string };
      expect(failure.code, code).toBe(code);
      expect(failure.message).toBe('reason');
    }
  });

  test('unknown server codes and missing payloads are contract mismatches, not transport failures', async () => {
    const unknown = commandPayloadReader(() => source(async () => ({ ok: false, source: 'server', code: 'busy', message: 'reason' })));
    const failure = await unknown({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string };
    expect(failure.code).toBe('integrity_error');
    const empty = commandPayloadReader(() => source(async () => ({ ok: true })));
    expect((await empty({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string }).code).toBe('integrity_error');
  });

  test('an unsynced session reports unavailable instead of fabricating content', async () => {
    const reader = commandPayloadReader(() => undefined);
    const failure = await reader({ id: 'gen:1' }).catch((error: unknown) => error) as { code?: string; message?: string };
    expect(failure.code).toBe('unavailable');
    expect(failure.message).toContain('会话尚未就绪');
  });

  test('readPayload surfaces client-side command failures through the SDK helper', async () => {
    const reader = commandPayloadReader(() => source(async () => ({ ok: false, source: 'client', code: 'transport_error', message: '载荷读取失败' })));
    const result = await loadPayloadText({ id: 'gen:1', bytes }, reader);
    expect(result).toEqual({ ok: false, error: '载荷读取失败' });
  });

  test('formats the validated public JSON for display', async () => {
    const json = JSON.stringify({ contentText: '完整内容' });
    const ref = { id: 'p1', bytes: new TextEncoder().encode(json).byteLength };
    const result = await loadPayloadText(ref, async () => ({ id: 'p1', bytes: ref.bytes, json }));
    expect(result).toEqual({ ok: true, text: JSON.stringify({ contentText: '完整内容' }, null, 2) });
  });

  test('surfaces the P3 placeholder reason without fabricating content', async () => {
    const result = await loadPayloadText({ id: 'p1', bytes: 4 }, async () => {
      throw new PayloadReadError('unavailable', '全文读取端点尚未提供（等待存储方案批准，P3）。');
    });
    expect(result).toEqual({ ok: false, error: '全文读取端点尚未提供（等待存储方案批准，P3）。' });
  });

  test('maps size and integrity failures to explicit reasons', async () => {
    const tooLarge = await loadPayloadText({ id: 'p1', bytes: 2_000_000 }, async () => ({ id: 'p1', bytes: 2, json: '{}' }));
    expect(tooLarge.ok).toBe(false);
    const broken = await loadPayloadText({ id: 'p1', bytes: 2 }, async () => ({ id: 'p1', bytes: 5, json: '{}' }));
    expect(broken).toEqual({ ok: false, error: expect.stringContaining('字节数') });
  });
});
