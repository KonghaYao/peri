import { BusinessError } from '../contracts/errors';
import { decodeCursor, encodeCursor } from '../contracts/cursor';
import { clampPage, LIMITS } from '../contracts/limits';
import type { SyncInput } from '../contracts/schemas';
import { getActor, getMember, getRoom } from '../storage/queries';
import type { EventRow } from '../storage/rows';
import { actorId, type Principal } from './principal';

export interface EventRef {
  seq: string;
  scope: string;
  type: string;
  ref_id: string | null;
  metadata: Record<string, unknown>;
  created_at: string;
}

export interface SyncPage {
  scope: string;
  items: EventRef[];
  next_cursor: string;
  has_more: boolean;
  head_seq: string;
  min_seq: string;
  reset_required: boolean;
}

interface ScopeInfo {
  kind: 'room' | 'inbox';
  id: string;
  head: number;
  min: number;
}

async function resolveScope(
  db: D1Database,
  viewer: string,
  scope: string,
): Promise<ScopeInfo> {
  if (scope === 'inbox') {
    const actor = await getActor(db, viewer);
    if (!actor) throw new BusinessError('NOT_FOUND', 'actor 不存在');
    return { kind: 'inbox', id: viewer, head: actor.inbox_head_seq, min: actor.inbox_min_seq };
  }
  if (scope.startsWith('room:')) {
    const roomId = scope.slice('room:'.length);
    if (!roomId) throw new BusinessError('INVALID_ARGUMENT', 'scope 缺少 room_id');
    const room = await getRoom(db, roomId);
    if (!room) throw new BusinessError('NOT_FOUND', '房间不存在');
    const member = await getMember(db, roomId, viewer);
    if (!member || member.status !== 'active') {
      throw new BusinessError('NOT_MEMBER', '当前身份不是该房间的有效成员');
    }
    return { kind: 'room', id: roomId, head: room.head_seq, min: room.events_min_seq };
  }
  throw new BusinessError('INVALID_ARGUMENT', 'scope 只支持 inbox 或 room:<room_id>');
}

function parseMetadata(raw: string): Record<string, unknown> {
  try {
    const value = JSON.parse(raw) as unknown;
    return typeof value === 'object' && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  } catch {
    return {};
  }
}

/** 资源读取返回的同步入口：从当前 head 之后继续。 */
export function headCursor(scope: string, head: number): string {
  return encodeCursor({ v: 1, scope: `sync:${scope}`, dir: 'after', seq: head, snapshot: head });
}

export async function sync(
  db: D1Database,
  principal: Principal,
  input: SyncInput,
): Promise<SyncPage> {
  const viewer = actorId(principal);
  const info = await resolveScope(db, viewer, input.scope);
  const cursorScope = `sync:${input.scope}`;
  const limit = clampPage(input.limit, LIMITS.syncPageDefault, LIMITS.syncPageMax);
  const cursor = input.cursor ? decodeCursor(input.cursor, cursorScope) : null;

  if (cursor && cursor.seq < info.min) {
    return {
      scope: input.scope,
      items: [],
      next_cursor: headCursor(input.scope, info.head),
      has_more: false,
      head_seq: String(info.head),
      min_seq: String(info.min),
      reset_required: true,
    };
  }

  const start = cursor?.seq ?? info.min;
  const rows = await db
    .prepare(
      `SELECT * FROM events
        WHERE scope_kind = ? AND scope_id = ? AND event_seq > ?
        ORDER BY event_seq ASC LIMIT ?`,
    )
    .bind(info.kind, info.id, start, limit + 1)
    .all<EventRow>();

  const page = rows.results.slice(0, limit);
  const hasMore = rows.results.length > limit;
  const last = page.at(-1);
  const nextSeq = last?.event_seq ?? start;

  return {
    scope: input.scope,
    items: page.map((row) => ({
      seq: String(row.event_seq),
      scope: `${row.scope_kind}:${row.scope_id}`,
      type: row.event_type,
      ref_id: row.ref_id,
      metadata: parseMetadata(row.metadata),
      created_at: String(row.created_at),
    })),
    next_cursor: headCursor(input.scope, nextSeq),
    has_more: hasMore,
    head_seq: String(info.head),
    min_seq: String(info.min),
    reset_required: false,
  };
}
