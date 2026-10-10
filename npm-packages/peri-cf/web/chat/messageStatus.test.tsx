import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { PayloadReadError } from '@peri-code/sdk/view';
import { ChatApi } from '../api/client';
import { MessageList } from './MessageList';
import { httpPayloadReader, loadPayloadText } from './payloadReader';
import type { Entry, EntryBlock } from './types';
import type { SyncState } from '../../shared/sync';

const chat = { id: 'chat-status', title: '状态回归', updatedAt: '2026-10-06T00:00:00Z' };
let fetchSpy: ReturnType<typeof spyOn<typeof globalThis, 'fetch'>> | undefined;

afterEach(() => {
  fetchSpy?.mockRestore();
  fetchSpy = undefined;
});

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

function state(overrides: Partial<SyncState> = {}): SyncState {
  return {
    chat, running: false, executionBlocked: false, readiness: 'ready',
    entries: [entry()], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
    session: {}, tasks: [], plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [],
    inputQueue: null,
    ...overrides,
  };
}

function respond(payload: unknown) {
  fetchSpy = spyOn(globalThis, 'fetch').mockResolvedValue(Response.json(payload));
}

function render(entries: Entry[], phase: 'idle' | 'streaming' = 'idle') {
  return renderToStaticMarkup(<MessageList entries={entries} phase={phase} chatId={chat.id} />);
}

describe('sync DTO boundary', () => {
  test('accepts entries with nullable times and full read面', async () => {
    const payload = state({
      entries: [entry({ createdAt: null, createdAtSource: 'unknown', completedAt: null, error: null })],
      toolCalls: [{ toolCallId: 't1', turnId: 'turn:1', name: 'Read', status: 'completed',
        createdAt: null, completedAt: null, error: null, resultRef: { id: 'p1', bytes: 4, preview: 'body' } }],
      turns: [{ turnId: 'turn:1', status: 'completed', startedAt: null, completedAt: null, error: null, interactionStatus: 'none' }],
      interactions: [{ id: 'interaction:1', kind: 'permission', sessionId: chat.id, status: 'resolved',
        createdAt: null, resolvedAt: null, outcome: 'allow_once' }],
      pendingInteractions: [],
    });
    respond(payload);
    expect(await new ChatApi('test-token').detail(chat.id)).toEqual(payload);
  });

  test.each(['unknown', '', 42, {}, []].map(value => ({ value })))('rejects invalid readiness %j', async ({ value: readiness }) => {
    respond(state({ readiness: readiness as SyncState['readiness'] }));
    await expect(new ChatApi('test-token').detail(chat.id)).rejects.toThrow('会话历史格式不正确');
  });

  test.each([['status', 42], ['blocks', 'text'], ['error', 'boom'], ['kind', 'note']].map(([field, value]) => ({ field, value })))
    ('rejects invalid entry field %s', async ({ field, value }) => {
      respond(state({ entries: [{ ...entry(), [field!]: value }] }));
      await expect(new ChatApi('test-token').detail(chat.id)).rejects.toThrow('会话历史格式不正确');
    });

  test('rejects an unsupported document pair with an upgrade message', async () => {
    respond(state({ readiness: 'unsupported' }));
    await expect(new ChatApi('test-token').detail(chat.id)).rejects.toThrow('不受支持的数据版本');
  });
});

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

  function jsonResponse(body: unknown, status = 200): Response {
    return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
  }

  test('http reader calls the versioned endpoint with bearer auth and returns the validated payload', async () => {
    const json = JSON.stringify({ contentText: '全文' });
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    const reader = httpPayloadReader(chat.id, 'secret-token', (async (url: string, init?: RequestInit) => {
      calls.push({ url, init });
      return jsonResponse({ id: 'gen:1', bytes: encoder.encode(json).byteLength, json });
    }) as unknown as typeof fetch);
    await expect(reader({ id: 'gen:1' })).resolves.toEqual({ id: 'gen:1', bytes: encoder.encode(json).byteLength, json });
    expect(calls[0]!.url).toBe(`/api/v2/chats/${chat.id}/payloads/gen%3A1`);
    expect((calls[0]!.init?.headers as Record<string, string>).Authorization).toBe('Bearer secret-token');
    expect(calls[0]!.init?.cache).toBe('no-store');
  });

  test('http reader maps server failure codes onto the SDK contract', async () => {
    const cases: Array<[number, string | undefined, string]> = [
      [401, undefined, 'unauthorized'],
      [403, undefined, 'unauthorized'],
      [404, 'unavailable', 'unavailable'],
      [413, 'too_large', 'too_large'],
      [500, 'integrity_error', 'integrity_error'],
      [502, undefined, 'transport_error'],
    ];
    for (const [status, code, expected] of cases) {
      const reader = httpPayloadReader(chat.id, 'token', (async () => jsonResponse({ error: { code, message: 'reason' } }, status)) as unknown as typeof fetch);
      const failure = await reader({ id: 'gen:1' }).catch((error) => error);
      expect((failure as { code?: string }).code, `${status}`).toBe(expected);
    }
    const malformed = httpPayloadReader(chat.id, 'token', (async () => jsonResponse({ id: 'gen:1' })) as unknown as typeof fetch);
    expect(((await malformed({ id: 'gen:1' }).catch((error) => error)) as { code?: string }).code).toBe('integrity_error');
  });

  test('readPayload surfaces network failures as transport_error through the SDK helper', async () => {
    const reader = httpPayloadReader(chat.id, 'token', (async () => { throw new TypeError('Failed to fetch'); }) as unknown as typeof fetch);
    const result = await loadPayloadText({ id: 'gen:1', bytes: 4 }, reader);
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
