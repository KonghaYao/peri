import {
  McpServer,
  ResourceNotFoundError,
  ResourceTemplate,
  type Variables,
} from '@modelcontextprotocol/server';

import { headCursor } from '../domain/sync';
import { actorResources } from '../domain/principal';
import { identityView } from '../domain/identity';
import { getClaimedDelivery } from '../storage/queries';
import {
  getMember,
  getMessage,
  getRoom,
} from '../storage/queries';
import type { Env } from '../env';
import type { Principal } from '../domain/principal';
import { messagePreviewOf } from '../domain/views';

const CACHE_HINT = { ttlMs: 0, cacheScope: 'private' as const };

function withUri(uri: URL, value: unknown) {
  return {
    contents: [
      {
        uri: uri.href,
        mimeType: 'application/json',
        text: JSON.stringify(value),
      },
    ],
  };
}

function variable(variables: Variables, name: string): string {
  const value = variables[name];
  if (typeof value === 'string') return decodeURIComponent(value);
  if (Array.isArray(value) && typeof value[0] === 'string') return decodeURIComponent(value[0]);
  throw new Error(`missing URI variable ${name}`);
}

export function registerResources(
  server: McpServer,
  env: Env,
  principal: Principal,
): void {
  const db = env.DB;

  server.registerResource(
    'me',
    'agentnet://me',
    { mimeType: 'application/json', description: '当前身份、入口与限制', cacheHint: CACHE_HINT },
    async (uri) => {
      if (!principal.actor) throw new ResourceNotFoundError(uri.href, '凭据尚未注册');
      return withUri(uri, identityView(principal.actor, env.PUBLIC_ORIGIN));
    },
  );

  server.registerResource(
    'inbox',
    new ResourceTemplate('agentnet://actors/{actor_id}/inbox', { list: undefined }),
    {
      mimeType: 'application/json',
      description: '自己的待办、领取状态与同步入口',
      cacheHint: CACHE_HINT,
    },
    async (uri, variables) => {
      const actor = principal.actor;
      const target = variable(variables, 'actor_id');
      if (!actor || target !== actor.actor_id) {
        throw new ResourceNotFoundError(uri.href, 'inbox 不存在');
      }

      const pending = await db
        .prepare(
          `SELECT d.*, m.preview AS m_preview, m.body_bytes AS m_body_bytes,
                  m.sender_id AS m_sender_id, m.created_at AS m_created_at
             FROM deliveries d JOIN messages m ON m.message_id = d.message_id
            WHERE d.recipient_id = ? AND d.state = 'pending'
            ORDER BY d.created_seq ASC LIMIT 10`,
        )
        .bind(actor.actor_id)
        .all<Record<string, unknown>>();
      const claimed = await getClaimedDelivery(db, actor.actor_id);

      return withUri(uri, {
        actor_id: actor.actor_id,
        pending_count: actor.pending_count,
        inbox_head_seq: String(actor.inbox_head_seq),
        pending: pending.results.map((row) => ({
          delivery_id: row.delivery_id,
          message_id: row.message_id,
          room_id: row.room_id,
          created_seq: String(row.created_seq),
          message: {
            message_id: row.message_id,
            sender_id: row.m_sender_id,
            created_seq: String(row.created_seq),
            preview: row.m_preview,
            body_bytes: row.m_body_bytes,
            created_at: String(row.m_created_at),
          },
        })),
        claimed: claimed
          ? {
              delivery_id: claimed.delivery_id,
              message_id: claimed.message_id,
              room_id: claimed.room_id,
              attempt: claimed.attempt,
              claim_id: claimed.claim_id,
              claimed_at: claimed.claimed_at === null ? null : String(claimed.claimed_at),
              progress_at: claimed.progress_at === null ? null : String(claimed.progress_at),
            }
          : null,
        resources: actorResources(actor.actor_id),
        sync_cursor: headCursor('inbox', actor.inbox_head_seq),
      });
    },
  );

  server.registerResource(
    'room-head',
    new ResourceTemplate('agentnet://rooms/{room_id}/head', { list: undefined }),
    {
      mimeType: 'application/json',
      description: '房间 head、成员版本与最近消息摘要',
      cacheHint: CACHE_HINT,
    },
    async (uri, variables) => {
      const actor = principal.actor;
      if (!actor) throw new ResourceNotFoundError(uri.href, '凭据尚未注册');
      const roomId = variable(variables, 'room_id');
      const room = await getRoom(db, roomId);
      if (!room) throw new ResourceNotFoundError(uri.href, '房间不存在');
      const member = await getMember(db, room.room_id, actor.actor_id);
      if (!member || member.status !== 'active') {
        throw new ResourceNotFoundError(uri.href, '房间不存在');
      }
      const last = room.last_message_id ? await getMessage(db, room.last_message_id) : null;

      return withUri(uri, {
        room_id: room.room_id,
        kind: room.kind,
        title: room.title,
        head_seq: String(room.head_seq),
        member_seq: String(room.member_seq),
        events_min_seq: String(room.events_min_seq),
        my_last_read_seq: String(member.last_read_seq),
        last_message: messagePreviewOf(last),
        sync_cursor: headCursor(`room:${room.room_id}`, room.head_seq),
      });
    },
  );
}
