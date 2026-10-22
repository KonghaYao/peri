import { useCallback, useMemo } from 'react';
import {
  MessageNotSentError, useExternalStoreRuntime,
  type AppendMessage, type AssistantRuntime, type ExternalStoreAdapter, type ExternalStoreThreadData,
  type ExternalStoreThreadListAdapter, type MessageStatus, type ThreadMessageLike,
} from '@assistant-ui/react';
import type { Chat, Entry, EntryBlock, Phase } from './types';

/**
 * assistant-ui 与 SyncState 的唯一结合点：只做「投影 DTO → assistant-ui 消息」的单向转换与回调接线。
 * 状态权威仍在 core；不提供 onEdit / onReload / queue / 附件，对应 UI 特性随 handler 缺失自动关闭。
 */

/** 缺失工具记录的占位工具名：渲染层按此名注册「工具调用暂不可用」的 tool UI。 */
export const UNAVAILABLE_TOOL_NAME = 'peri-unavailable-tool';

/** unknown 块的数据部分名；assistant-ui 把 `data-*` 前缀解析为 DataMessagePart（name 去掉前缀）。 */
export const UNKNOWN_BLOCK_PART_NAME = 'peri-unknown-block';

type MessagePart = Exclude<ThreadMessageLike['content'], string>[number];
type ToolCallPart = Extract<MessagePart, { type: 'tool-call' }>;
type ToolCallBlock = Extract<EntryBlock, { type: 'tool_call' }>;
type UnknownBlock = Extract<EntryBlock, { type: 'unknown' }>;

/** createdAt 为 null 或不可解析时省略，交给渲染层按缺失处理，不伪造时间。 */
function createdAtOf(createdAt: string | null): Date | undefined {
  if (!createdAt) return undefined;
  const value = new Date(createdAt);
  return Number.isNaN(value.getTime()) ? undefined : value;
}

/** 工具入参：JSON 对象直传；其它形态保留为 argsText 原文，既不伪造结构也不静默丢弃。 */
function toolArgsOf(argument: unknown): Pick<ToolCallPart, 'args' | 'argsText'> {
  if (argument === undefined) return {};
  if (typeof argument === 'object' && argument !== null && !Array.isArray(argument)) return { args: argument as ToolCallPart['args'] };
  return { argsText: typeof argument === 'string' ? argument : JSON.stringify(argument) ?? String(argument) };
}

/** 工具块 → tool-call 部分；记录缺失时保持调用身份与失败语义，由渲染层展示可读原因。 */
function toolCallPart(block: ToolCallBlock): MessagePart {
  const tool = block.tool;
  if (!tool) {
    return {
      type: 'tool-call', toolCallId: block.toolCallId, toolName: UNAVAILABLE_TOOL_NAME,
      result: { unavailable: block.unavailable ?? 'object_unavailable' }, isError: true,
    };
  }
  // 工具自身失败时以 isError 标注；结果位缺失即用公开错误补位，避免失败原因在映射中丢失。
  const failed = tool.error !== null;
  const result = failed ? tool.result ?? { error: tool.error } : tool.result;
  return {
    type: 'tool-call', toolCallId: tool.toolCallId, toolName: tool.name,
    ...toolArgsOf(tool.arguments),
    ...(result === undefined ? {} : { result }),
    ...(failed ? { isError: true } : {}),
  };
}

/** unknown 块 → data 部分：保留不可读原因，不做静默丢弃（对齐现有 block-unavailable 展示）。 */
function unknownPart(block: UnknownBlock): MessagePart {
  return { type: `data-${UNKNOWN_BLOCK_PART_NAME}`, id: block.blockId, data: { reason: block.reason } };
}

/**
 * 投影状态 → assistant-ui 消息状态；`pending`/`streaming` 映射为 running 以驱动运行指示。
 */
function messageStatusOf(entry: Entry): MessageStatus {
  switch (entry.status) {
    case 'completed': return { type: 'complete', reason: 'stop' };
    case 'cancelled': return { type: 'incomplete', reason: 'cancelled' };
    case 'error': return { type: 'incomplete', reason: 'error', ...(entry.error ? { error: entry.error } : {}) };
    case 'pending':
    case 'streaming': return { type: 'running' };
    // 其余字面量（含旧检查点恢复出的 "running"）语义未确认：不进入运行态，避免伪造「生成中」。
    default: return { type: 'incomplete', reason: 'other' };
  }
}

/**
 * 单条 Entry → assistant-ui 消息：id 用 Entry.id 保持消息身份，blocks 保序，createdAt 缺失时省略。
 */
export function convertMessage(entry: Entry): ThreadMessageLike {
  const content = entry.blocks.flatMap((block): MessagePart[] => {
    switch (block.type) {
      case 'text': return [{ type: 'text', text: block.text }];
      case 'reasoning': return block.visibility === 'hidden' ? [] : [{ type: 'reasoning', text: block.text }];
      case 'tool_call': return [toolCallPart(block)];
      case 'unknown': return [unknownPart(block)];
    }
  });
  const createdAt = createdAtOf(entry.createdAt);
  return {
    id: entry.id,
    role: entry.role,
    content,
    ...(createdAt ? { createdAt } : {}),
    metadata: {
      custom: {
        periTurnId: entry.turnId, periStatus: entry.status,
        ...(entry.error ? { periError: entry.error } : {}),
      },
    },
    // status 只对 assistant 消息合法：user 消息携带会被 fromThreadMessageLike 拒绝。
    ...(entry.role === 'assistant' ? { status: messageStatusOf(entry) } : {}),
  };
}

const RUNNING_STATUSES = new Set<string>(['pending', 'streaming']);

/** 把同一 turn 的后续 assistant Entry 并入已聚合条目；状态按任一步运行/错误优先收敛。 */
function mergeAssistantEntry(current: Entry, next: Entry): Entry {
  const running = [current.status, next.status].some((status) => RUNNING_STATUSES.has(status));
  const error: Entry['error'] = next.error ?? current.error;
  return {
    ...current,
    status: running ? 'streaming' : error && next.status === 'completed' ? 'error' : next.status,
    error,
    completedAt: next.completedAt ?? current.completedAt,
    blocks: [...current.blocks, ...next.blocks],
    ...(next.messageId ? { messageId: next.messageId } : {}),
    ...(next.tokenUsage ? { tokenUsage: next.tokenUsage } : {}),
  };
}

/**
 * 同一 turn 的 assistant Entry 合并为一条视图消息：实时按 turnId 合并，历史按用户消息切段合并；
 * 消息落在最后一条 Entry 的位置，id 取首条，保证新 step 不重建身份。
 */
export function mergeAssistantTurns(entries: readonly Entry[]): Entry[] {
  // 历史条目的 turnId 不可比较（逐 payload 唯一）：用用户消息切段，
  // 段内的连续 assistant 条目属于同一个展示 turn。
  const groupKeyByEntry = new Map<Entry, string>();
  let historySegment = 0;
  for (const entry of entries) {
    if (entry.role === 'user') {
      historySegment += 1;
      continue;
    }
    if (entry.role !== 'assistant') continue;
    groupKeyByEntry.set(
      entry,
      entry.turnId.startsWith('history:') ? `history-segment:${historySegment}` : entry.turnId,
    );
  }

  const mergedByKey = new Map<string, Entry>();
  const lastAssistantIndex = new Map<string, number>();

  entries.forEach((entry, index) => {
    if (entry.role !== 'assistant') return;
    const key = groupKeyByEntry.get(entry)!;
    const current = mergedByKey.get(key);
    mergedByKey.set(key, current ? mergeAssistantEntry(current, entry) : entry);
    lastAssistantIndex.set(key, index);
  });

  const merged: Entry[] = [];
  entries.forEach((entry, index) => {
    if (entry.role !== 'assistant') {
      merged.push(entry);
      return;
    }
    const key = groupKeyByEntry.get(entry)!;
    if (lastAssistantIndex.get(key) === index) {
      merged.push(mergedByKey.get(key)!);
    }
  });
  return merged;
}

/** 适配层输入：当前会话投影与命令入口。 */
export interface PeriRuntimeSource {
  /** 当前会话条目，沿用投影顺序（entryOrder），不在此重排。 */
  entries: readonly Entry[];
  phase: Phase;
  historyReady: boolean;
  selected: Chat | null;
  chats: readonly Chat[];
/** 会话清单请求中且无数据时显示骨架屏；已渲染的列表不因刷新闪烁。 */
  listLoading: boolean;
  send: (content: string) => Promise<boolean>;
  stop: () => Promise<boolean>;
  selectChat: (chat: Chat | null) => Promise<void>;
}

/** 运行指示只覆盖可取消的三态；发送门禁仍按 phase 非 idle 判断。 */
const RUNNING_PHASES: readonly Phase[] = ['streaming', 'cancelling', 'blocked'];

/** AppendMessage → 命令文本：只取文本部分（附件、语音当前未启用）。 */
export function promptText(message: Pick<AppendMessage, 'content'>): string {
  const parts: readonly { type: string; text?: string }[] = message.content;
  return parts.filter((part) => part.type === 'text').map((part) => part.text ?? '').join('\n').trim();
}

/**
 * 线程数据 + 列表分组所需的活动时间。
 * 外部 store 在运行期把传入对象原样展开进 threadListItem 状态，日期分组从那里读取 lastMessageAt。
 */
type PeriThreadData = ExternalStoreThreadData<'regular'> & { lastMessageAt?: Date };

/** updatedAt 不可解析时省略时间：不伪造「刚刚」，让列表按运行时顺序落位。 */
function updatedAtOf(updatedAt: string): Date | undefined {
  const value = new Date(updatedAt);
  return Number.isNaN(value.getTime()) ? undefined : value;
}

/** 会话列表适配器：线程身份即现有 Chat.id，选择与新建都复用现有流程。 */
export function threadListAdapter(
  source: Pick<PeriRuntimeSource, 'chats' | 'selected' | 'selectChat' | 'listLoading'>,
): ExternalStoreThreadListAdapter {
  const threads = source.chats.map((chat): PeriThreadData => {
    const lastMessageAt = updatedAtOf(chat.updatedAt);
    return {
      status: 'regular', id: chat.id, remoteId: chat.id, title: chat.title || '新对话',
      ...(lastMessageAt ? { lastMessageAt } : {}),
    };
  });
  return {
    threadId: source.selected?.id,
    threads,
    // 初次加载且无数据时才进骨架屏：刷新期间保留已渲染条目，避免列表闪空。
    isLoading: source.listLoading && source.chats.length === 0,
    onSwitchToThread: (threadId) => {
      const target = source.chats.find((chat) => chat.id === threadId);
      // 列表与投影可能瞬时不同步：未知线程不猜测目标，交给下一次列表刷新收敛。
      return target ? source.selectChat(target) : Promise.resolve();
    },
    onSwitchToNewThread: () => source.selectChat(null),
  };
}

/**
 * useExternalStoreRuntime 的适配 props：messages 先按 turn 聚合 Entry，再交给 convertMessage。
 * 不提供编辑、重新生成、队列与附件 handler，对应 UI 特性随 handler 缺失自动关闭。
 */
export function usePeriRuntime(source: PeriRuntimeSource): ExternalStoreAdapter<Entry> {
  const { entries, phase, historyReady, selected, chats, listLoading, send, stop, selectChat } = source;

  // 同一 turn 的多条 assistant Entry 只产生一条线程消息。
  const messages = useMemo(() => mergeAssistantTurns(entries), [entries]);

  const onNew = useCallback(async (message: AppendMessage) => {
    const text = promptText(message);
    // send() 被门禁或服务端拒绝时返回 false：抛错让 composer 还原草稿。
    if (text === '' || !await send(text)) throw new MessageNotSentError('消息未发送。');
  }, [send]);

  // 取消失败的原因由现有错误横幅展示（session.stop 已记录），这里不重复抛错。
  const onCancel = useCallback(async () => { await stop(); }, [stop]);

  const threadList = useMemo(
    () => threadListAdapter({ chats, selected, selectChat, listLoading }),
    [chats, selected, selectChat, listLoading],
  );

  return useMemo(() => ({
    messages,
    convertMessage,
    isRunning: RUNNING_PHASES.includes(phase),
    // 历史重放中是「读取中」而不是「空会话」：交给 Thread 显示加载骨架，避免闪出欢迎页。
    isLoading: phase === 'loading',
    // 运行中只禁发送、不禁输入：草稿保留，与 busy 门禁语义一致。
    isSendDisabled: !historyReady || phase !== 'idle',
    onNew,
    onCancel,
    adapters: { threadList },
  }), [messages, phase, historyReady, onNew, onCancel, threadList]);
}

/** 组合出应用的 AssistantRuntime；Provider 同时在 ChatApp 层覆盖会话列表与消息区。 */
export function usePeriAssistantRuntime(source: PeriRuntimeSource): AssistantRuntime {
  return useExternalStoreRuntime(usePeriRuntime(source));
}
