import { describe, expect, test } from 'bun:test';
import type { ExternalStoreThreadData, ExternalStoreThreadListAdapter, ThreadMessageLike } from '@assistant-ui/react';
import {
  convertMessage, promptText, threadListAdapter, UNKNOWN_BLOCK_PART_NAME, UNAVAILABLE_TOOL_NAME,
} from './adapter';
import type { Chat, Entry, EntryBlock, ToolCall } from './types';

type Part = Exclude<ThreadMessageLike['content'], string>[number];

/** 分组用的 lastMessageAt 不在 assistant-ui 的公开输入类型里（运行期透传），断言时按原始字段读取。 */
function lastMessageAtOf(data: ExternalStoreThreadData<'regular'> | undefined): Date | undefined {
  return (data as { lastMessageAt?: Date } | undefined)?.lastMessageAt;
}

function contentParts(message: ThreadMessageLike): Part[] {
  return typeof message.content === 'string' ? [] : Array.from(message.content);
}

function partOf<T extends Part['type']>(message: ThreadMessageLike, type: T): Extract<Part, { type: T }> {
  const found = contentParts(message).find((part): part is Extract<Part, { type: T }> => part.type === type);
  if (!found) throw new Error(`缺少 ${type} 部分`);
  return found;
}

function textBlock(text: string): EntryBlock {
  return { blockId: 'text:0', type: 'text', text };
}

function tool(overrides: Partial<ToolCall> = {}): ToolCall {
  return {
    toolCallId: 't1', turnId: 'turn:1', name: 'Read', status: 'completed',
    createdAt: null, completedAt: null, error: null, ...overrides,
  };
}

function entry(overrides: Partial<Entry> = {}): Entry {
  return {
    entryId: 'turn:1:assistant', id: 'turn:1:assistant', kind: 'message', turnId: 'turn:1',
    role: 'assistant', status: 'completed', createdAt: '2026-10-13T00:00:00Z', createdAtSource: 'observed',
    completedAt: '2026-10-13T00:00:01Z', error: null, blocks: [textBlock('回答')],
    ...overrides,
  };
}

describe('convertMessage maps entry blocks onto assistant-ui messages', () => {
  test('keeps the stable entry id and omits createdAt when the projection has none', () => {
    const message = convertMessage(entry({ id: 'turn:1:assistant', createdAt: null }));
    expect(message.id).toBe('turn:1:assistant');
    expect(message.role).toBe('assistant');
    expect('createdAt' in message).toBe(false);
    expect(convertMessage(entry()).createdAt).toEqual(new Date('2026-10-13T00:00:00Z'));
  });

  test('keeps block order across text, reasoning, tool calls and unknown blocks', () => {
    const message = convertMessage(entry({ blocks: [
      textBlock('先看结论'),
      { blockId: 'reasoning:0', type: 'reasoning', text: '内部推理', visibility: 'summary' },
      { blockId: 'tool:t1', type: 'tool_call', toolCallId: 't1', tool: tool({ result: { contentText: '文件内容' } }) },
      { blockId: 'resource:0', type: 'unknown', reason: 'unsupported_block:resource' },
      textBlock('最后补充'),
    ] }));
    expect(contentParts(message).map((part) => part.type))
      .toEqual(['text', 'reasoning', 'tool-call', `data-${UNKNOWN_BLOCK_PART_NAME}`, 'text']);
  });

  test('skips hidden reasoning and keeps its summary', () => {
    const hidden = convertMessage(entry({ blocks: [{ blockId: 'reasoning:0', type: 'reasoning', text: 'PRIVATE', visibility: 'hidden' }] }));
    expect(contentParts(hidden)).toEqual([]);
    const summary = convertMessage(entry({ blocks: [{ blockId: 'reasoning:0', type: 'reasoning', text: '思考过程', visibility: 'summary' }] }));
    expect(partOf(summary, 'reasoning').text).toBe('思考过程');
  });

  test('maps tool name, arguments and result, and flags tool failures', () => {
    const message = convertMessage(entry({ blocks: [{ blockId: 'tool:t1', type: 'tool_call', toolCallId: 't1',
      tool: tool({ name: 'Bash', arguments: { command: 'ls' }, result: { contentText: 'a.txt' } }) }] }));
    const part = partOf(message, 'tool-call');
    expect(part.toolName).toBe('Bash');
    expect(part.toolCallId).toBe('t1');
    expect(part.args).toEqual({ command: 'ls' });
    expect(part.result).toEqual({ contentText: 'a.txt' });
    expect(part.isError).toBeUndefined();

    const failed = convertMessage(entry({ blocks: [{ blockId: 'tool:t1', type: 'tool_call', toolCallId: 't1',
      tool: tool({ name: 'Bash', status: 'error', error: { code: 'tool_failed', message: '命令失败' } }) }] }));
    const failedPart = partOf(failed, 'tool-call');
    expect(failedPart.isError).toBe(true);
    expect(failedPart.result).toEqual({ error: { code: 'tool_failed', message: '命令失败' } });
  });

  test('keeps non-object tool arguments as argsText instead of inventing structure', () => {
    const message = convertMessage(entry({ blocks: [{ blockId: 'tool:t1', type: 'tool_call', toolCallId: 't1',
      tool: tool({ arguments: 'raw input' }) }] }));
    const part = partOf(message, 'tool-call');
    expect(part.args).toBeUndefined();
    expect(part.argsText).toBe('raw input');
  });

  test('renders a missing tool record as an explicit failure instead of dropping it', () => {
    const message = convertMessage(entry({ blocks: [
      { blockId: 'tool:ghost', type: 'tool_call', toolCallId: 'ghost', tool: null, unavailable: 'missing_tool' },
    ] }));
    const part = partOf(message, 'tool-call');
    expect(part.toolName).toBe(UNAVAILABLE_TOOL_NAME);
    expect(part.toolCallId).toBe('ghost');
    expect(part.isError).toBe(true);
    expect(part.result).toEqual({ unavailable: 'missing_tool' });
  });

  test('keeps the unknown block reason in a data part', () => {
    const message = convertMessage(entry({ blocks: [{ blockId: 'resource:0', type: 'unknown', reason: 'unsupported_block:resource' }] }));
    expect(contentParts(message)).toEqual([
      { type: `data-${UNKNOWN_BLOCK_PART_NAME}`, id: 'resource:0', data: { reason: 'unsupported_block:resource' } },
    ]);
  });

  test('maps terminal and running statuses and keeps the raw projection status in metadata', () => {
    const completed = convertMessage(entry({ status: 'completed' }));
    expect(completed.status).toEqual({ type: 'complete', reason: 'stop' });
    expect(completed.metadata).toEqual({ custom: { periTurnId: 'turn:1', periStatus: 'completed' } });

    expect(convertMessage(entry({ status: 'streaming' })).status).toEqual({ type: 'running' });
    expect(convertMessage(entry({ status: 'pending' })).status).toEqual({ type: 'running' });
    expect(convertMessage(entry({ status: 'cancelled' })).status).toEqual({ type: 'incomplete', reason: 'cancelled' });
    // 旧检查点恢复出的未确认状态不能伪装成运行中。
    expect(convertMessage(entry({ status: 'running' })).status).toEqual({ type: 'incomplete', reason: 'other' });
  });

  test('carries the entry error on the message status and in metadata', () => {
    const failure = { code: 'execution_failed', message: '模型调用失败' };
    const message = convertMessage(entry({ status: 'error', error: failure, blocks: [] }));
    expect(message.status).toEqual({ type: 'incomplete', reason: 'error', error: failure });
    expect(message.metadata).toEqual({ custom: { periTurnId: 'turn:1', periStatus: 'error', periError: failure } });
  });

  test('never attaches status to user messages', () => {
    const message = convertMessage(entry({ id: 'turn:1:user', entryId: 'turn:1:user', role: 'user', blocks: [textBlock('问题')] }));
    expect(message.role).toBe('user');
    expect('status' in message).toBe(false);
    expect(partOf(message, 'text').text).toBe('问题');
  });
});

describe('promptText extracts the command text from an append message', () => {
  test('joins text parts in order and ignores other parts', () => {
    expect(promptText({ content: [
      { type: 'text', text: '第一行' },
      { type: 'image', image: 'data:image/png;base64,AA' },
      { type: 'text', text: '第二行' },
    ] })).toBe('第一行\n第二行');
  });

  test('returns an empty string when there is no text to send', () => {
    expect(promptText({ content: [] })).toBe('');
    expect(promptText({ content: [{ type: 'text', text: '   ' }] })).toBe('');
  });
});

describe('threadListAdapter maps chats onto assistant-ui threads', () => {
  const chats: Chat[] = [
    { id: 'chat-a', title: '会话 A', updatedAt: '2026-10-13T01:00:00Z' },
    { id: 'chat-b', title: '', updatedAt: '2026-10-13T00:00:00Z' },
  ];

  function build(selected: Chat | null, onSelect: (chat: Chat | null) => Promise<void>, listLoading = false): ExternalStoreThreadListAdapter {
    return threadListAdapter({ chats, selected, selectChat: onSelect, listLoading });
  }

  test('exposes regular threads with the selected thread id and a title fallback', async () => {
    const adapter = build(chats[0], async () => undefined);
    expect(adapter.threadId).toBe('chat-a');
    expect(adapter.threads).toMatchObject([
      { status: 'regular', id: 'chat-a', remoteId: 'chat-a', title: '会话 A' },
      { status: 'regular', id: 'chat-b', remoteId: 'chat-b', title: '新对话' },
    ]);
    // 未提供的能力没有 handler，assistant-ui 的 rename / archive / delete 保持关闭。
    expect(adapter.onRename).toBeUndefined();
    expect(adapter.onArchive).toBeUndefined();
    expect(adapter.onDelete).toBeUndefined();
  });

  test('carries updatedAt as lastMessageAt so the list can group by day, and drops unparsable values', () => {
    const adapter = threadListAdapter({
      chats: [...chats, { id: 'chat-c', title: '坏时间', updatedAt: 'not-a-date' }],
      selected: null, selectChat: async () => {}, listLoading: false,
    });
    expect(lastMessageAtOf(adapter.threads?.[0])).toEqual(new Date('2026-10-13T01:00:00Z'));
    // 时间不可解析时不猜时刻：该条不带 lastMessageAt，分组逻辑会把它排到末尾。
    expect(adapter.threads?.[2]).not.toHaveProperty('lastMessageAt');
  });

  test('only reports loading while there is nothing to show yet', () => {
    const empty = { chats: [] as const, selected: null, selectChat: async () => {} };
    // 首次加载且无数据：进骨架屏。已有数据时刷新不闪空列表。
    expect(threadListAdapter({ ...empty, listLoading: true }).isLoading).toBe(true);
    expect(threadListAdapter({ ...empty, listLoading: false }).isLoading).toBe(false);
    expect(build(chats[0], async () => {}, true).isLoading).toBe(false);
  });

  test('switches by chat id, ignores unknown ids, and opens a new chat on null', async () => {
    const selected: (Chat | null)[] = [];
    const adapter = build(chats[0], async (chat) => { selected.push(chat); });
    await adapter.onSwitchToThread?.('chat-b');
    expect(selected).toEqual([chats[1]]);
    await adapter.onSwitchToThread?.('chat-gone');
    expect(selected).toEqual([chats[1]]);
    await adapter.onSwitchToNewThread?.();
    expect(selected).toEqual([chats[1], null]);
  });
});
