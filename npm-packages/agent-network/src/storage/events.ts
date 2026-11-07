import { newId } from './db';

export interface EventSpec {
  eventKey: string;
  scopeKind: 'room' | 'inbox' | 'actor';
  scopeId: string;
  eventType: string;
  refId?: string | null;
  metadata?: Record<string, unknown>;
}

export interface OutboxSpec {
  topic: string;
  eventKey: string;
}

/** 插入 events；event_key 是事务内唯一的事件身份。 */
export function eventStatement(db: D1Database, spec: EventSpec, at: number): D1PreparedStatement {
  return db
    .prepare(
      `INSERT INTO events (scope_kind, scope_id, event_type, ref_id, metadata, event_key, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(
      spec.scopeKind,
      spec.scopeId,
      spec.eventType,
      spec.refId ?? null,
      JSON.stringify(spec.metadata ?? {}),
      spec.eventKey,
      at,
    );
}

/**
 * 幂等插入事件：同一 event_key 已存在时保持原行。
 *
 * 仅用于 event_key 由业务身份（而非本次请求）决定、且并发创建必须收敛到同一行的场景，
 * 例如单聊房间的 `room_created`。普通业务事件使用 {@link eventStatement}，
 * 让重复插入显式失败。
 */
export function eventStatementIfAbsent(
  db: D1Database,
  spec: EventSpec,
  at: number,
): D1PreparedStatement {
  return db
    .prepare(
      `INSERT INTO events (scope_kind, scope_id, event_type, ref_id, metadata, event_key, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(event_key) DO NOTHING`,
    )
    .bind(
      spec.scopeKind,
      spec.scopeId,
      spec.eventType,
      spec.refId ?? null,
      JSON.stringify(spec.metadata ?? {}),
      spec.eventKey,
      at,
    );
}

/** 插入 outbox；event_seq 由同一事务内已写入的 event_key 解析。 */
export function outboxStatement(
  db: D1Database,
  spec: OutboxSpec,
  at: number,
): D1PreparedStatement {
  return db
    .prepare(
      `INSERT INTO outbox (outbox_id, topic, event_seq, status, attempts, next_attempt_at, created_at)
       VALUES (?, ?, (SELECT event_seq FROM events WHERE event_key = ?), 'pending', 0, ?, ?)`,
    )
    .bind(newId('obx'), spec.topic, spec.eventKey, at, at);
}

/** 幂等 outbox：同一 (topic, event_seq) 只保留一行。 */
export function outboxStatementIfAbsent(
  db: D1Database,
  spec: OutboxSpec,
  at: number,
): D1PreparedStatement {
  return db
    .prepare(
      `INSERT INTO outbox (outbox_id, topic, event_seq, status, attempts, next_attempt_at, created_at)
       VALUES (?, ?, (SELECT event_seq FROM events WHERE event_key = ?), 'pending', 0, ?, ?)
       ON CONFLICT(topic, event_seq) DO NOTHING`,
    )
    .bind(newId('obx'), spec.topic, spec.eventKey, at, at);
}

export function roomTopic(roomId: string): string {
  return `room:${roomId}`;
}

export function inboxTopic(actorId: string): string {
  return `inbox:${actorId}`;
}

/** 更新房间 head 与 member 版本（成员事件使用同一 head 序号）。 */
export function roomHeadStatement(
  db: D1Database,
  roomId: string,
  eventKey: string,
  at: number,
  lastMessageId?: string,
): D1PreparedStatement {
  return db
    .prepare(
      `UPDATE rooms
         SET head_seq = MAX(head_seq, (SELECT event_seq FROM events WHERE event_key = ?)),
             last_message_id = COALESCE(?, last_message_id),
             updated_at = ?
       WHERE room_id = ?`,
    )
    .bind(eventKey, lastMessageId ?? null, at, roomId);
}

export function roomMemberSeqStatement(
  db: D1Database,
  roomId: string,
  eventKey: string,
  at: number,
): D1PreparedStatement {
  return db
    .prepare(
      `UPDATE rooms
         SET head_seq = MAX(head_seq, (SELECT event_seq FROM events WHERE event_key = ?)),
             member_seq = MAX(member_seq, (SELECT event_seq FROM events WHERE event_key = ?)),
             updated_at = ?
       WHERE room_id = ?`,
    )
    .bind(eventKey, eventKey, at, roomId);
}

export function inboxHeadStatement(
  db: D1Database,
  actorId: string,
  eventKey: string,
  at: number,
  pendingDelta: number,
): D1PreparedStatement {
  return db
    .prepare(
      `UPDATE actors
         SET inbox_head_seq = MAX(inbox_head_seq, (SELECT event_seq FROM events WHERE event_key = ?)),
             pending_count = pending_count + ?,
             updated_at = ?
       WHERE actor_id = ?`,
    )
    .bind(eventKey, pendingDelta, at, actorId);
}
