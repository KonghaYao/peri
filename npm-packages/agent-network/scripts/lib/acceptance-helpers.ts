/**
 * 端到端验收共享类型与流程辅助。
 *
 * 验收主体保持单条线性流程；这里只放跨用例复用的类型和工具调用，
 * 避免 `acceptance.ts` 超过单文件行数上限，不改变验收顺序。
 */

import type { McpSubscription } from '@modelcontextprotocol/client';

import { callTool, check, newRequestId, sleep, type McpConnection } from './harness';

export interface Identity {
  actor_id: string;
  kind: string;
  name: string;
  pending_count: number;
  resources: string[];
}

export interface Delivery {
  delivery_id: string;
  recipient_id: string;
  message_id: string;
  room_id: string;
  state: 'pending' | 'claimed' | 'completed' | 'failed' | 'cancelled';
  attempt: number;
  claim_id: string | null;
  claimed_at: string | null;
  updated_at: string;
  result_message_id: string | null;
}

export interface InboxPage {
  items: Delivery[];
  next_cursor: string | null;
  has_more: boolean;
  pending_count: number;
  claimed_delivery_id: string | null;
}

export interface MessageView {
  message_id: string;
  sender_id: string;
  created_seq: string;
  preview: string;
  body_bytes: number;
  reply_to: string | null;
  root_id: string | null;
  hop: number;
  delivery_states: Record<string, number>;
  delivery_claimed_at: string | null;
  delivery_updated_at: string | null;
}

export interface HistoryPage {
  items: MessageView[];
  next_cursor: string | null;
  has_more: boolean;
  snapshot: string;
  head_seq: string;
}

export interface SendResult {
  message: MessageView;
  delivery_ids: string[];
}

export interface RoomDetail {
  room: {
    room_id: string;
    kind: string;
    owner_id: string | null;
    head_seq: string;
    member_count: number;
    my_role: string;
    last_message: { message_id: string; preview: string } | null;
  };
  members: { actor_id: string; role: string; kind: string; online: boolean | null }[];
}

export interface ReadChunk {
  chunk: string;
  next_cursor: string | null;
  eof: boolean;
  body_bytes: number;
}

export interface LoginResult {
  actor_id: string;
  online: true;
  logged_in_at: string;
  pending_count: number;
  inbox_head_seq: string;
}

export async function registerIdentity(
  connection: McpConnection,
  input: { kind: 'human' | 'agent'; name: string; sessionId?: string },
): Promise<Identity> {
  return callTool<Identity>(connection.client, 'network_register', {
    request_id: newRequestId('reg'),
    kind: input.kind,
    name: input.name,
    ...(input.sessionId ? { agent_session_id: input.sessionId } : {}),
  });
}

export async function sendMessage(
  connection: McpConnection,
  roomId: string,
  body: string,
  options: { invoke?: string[]; clientMessageId?: string; requestId?: string } = {},
): Promise<SendResult> {
  return callTool<SendResult>(connection.client, 'network_message_send', {
    request_id: options.requestId ?? newRequestId('send'),
    client_message_id: options.clientMessageId ?? `cm_${crypto.randomUUID()}`,
    room_id: roomId,
    body,
    ...(options.invoke ? { invoke: options.invoke } : {}),
  });
}

export async function inbox(
  connection: McpConnection,
  state: 'pending' | 'claimed' | 'completed' | 'cancelled' | 'all' = 'all',
): Promise<InboxPage> {
  return callTool<InboxPage>(connection.client, 'network_inbox_list', { state });
}

export async function readWholeMessage(
  connection: McpConnection,
  messageId: string,
): Promise<string> {
  let cursor: string | null = null;
  let text = '';
  for (let round = 0; round < 20; round += 1) {
    const chunk: ReadChunk = await callTool<ReadChunk>(connection.client, 'network_message_read', {
      message_id: messageId,
      ...(cursor ? { chunk_cursor: cursor } : {}),
    });
    text += chunk.chunk;
    if (!chunk.next_cursor) {
      check(`分块读取在第 ${round + 1} 块结束`, chunk.eof, chunk);
      return text;
    }
    cursor = chunk.next_cursor;
  }
  throw new Error('消息分块读取超过 20 轮');
}

export async function waitClosed(
  subscription: McpSubscription,
  timeoutMs = 15_000,
): Promise<string> {
  return Promise.race([
    subscription.closed,
    sleep(timeoutMs).then(() => 'timeout' as const),
  ]);
}
