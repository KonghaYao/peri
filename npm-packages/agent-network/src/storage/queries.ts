import type { ActorRow, DeliveryRow, MessageRow, RoomMemberRow, RoomRow } from './rows';

export interface D1Like {
  prepare(query: string): D1PreparedStatement;
  batch<T = unknown>(statements: D1PreparedStatement[]): Promise<D1Result<T>[]>;
}

export async function getActor(db: D1Like, actorId: string): Promise<ActorRow | null> {
  return db.prepare('SELECT * FROM actors WHERE actor_id = ?').bind(actorId).first<ActorRow>();
}

export async function getActorBySession(db: D1Like, sessionId: string): Promise<ActorRow | null> {
  return db
    .prepare('SELECT * FROM actors WHERE agent_session_id = ?')
    .bind(sessionId)
    .first<ActorRow>();
}

export async function getRoom(db: D1Like, roomId: string): Promise<RoomRow | null> {
  return db.prepare('SELECT * FROM rooms WHERE room_id = ?').bind(roomId).first<RoomRow>();
}

export async function getMember(
  db: D1Like,
  roomId: string,
  actorId: string,
): Promise<RoomMemberRow | null> {
  return db
    .prepare('SELECT * FROM room_members WHERE room_id = ? AND actor_id = ?')
    .bind(roomId, actorId)
    .first<RoomMemberRow>();
}

export async function isActiveMember(db: D1Like, roomId: string, actorId: string): Promise<boolean> {
  const row = await db
    .prepare("SELECT 1 AS ok FROM room_members WHERE room_id = ? AND actor_id = ? AND status = 'active'")
    .bind(roomId, actorId)
    .first<{ ok: number }>();
  return row !== null;
}

export async function getMessage(db: D1Like, messageId: string): Promise<MessageRow | null> {
  return db.prepare('SELECT * FROM messages WHERE message_id = ?').bind(messageId).first<MessageRow>();
}

export async function getDelivery(db: D1Like, deliveryId: string): Promise<DeliveryRow | null> {
  return db
    .prepare('SELECT * FROM deliveries WHERE delivery_id = ?')
    .bind(deliveryId)
    .first<DeliveryRow>();
}

export async function getDeliveryForMessage(
  db: D1Like,
  recipientId: string,
  messageId: string,
): Promise<DeliveryRow | null> {
  return db
    .prepare('SELECT * FROM deliveries WHERE recipient_id = ? AND message_id = ?')
    .bind(recipientId, messageId)
    .first<DeliveryRow>();
}

export async function getClaimedDelivery(
  db: D1Like,
  recipientId: string,
): Promise<DeliveryRow | null> {
  return db
    .prepare("SELECT * FROM deliveries WHERE recipient_id = ? AND state = 'claimed' LIMIT 1")
    .bind(recipientId)
    .first<DeliveryRow>();
}

export async function getOperation(
  db: D1Like,
  actorId: string,
  requestId: string,
): Promise<import('./rows').OperationRow | null> {
  return db
    .prepare('SELECT * FROM operations WHERE actor_id = ? AND request_id = ?')
    .bind(actorId, requestId)
    .first<import('./rows').OperationRow>();
}

export async function getCredentialByHash(
  db: D1Like,
  tokenHash: string,
): Promise<import('./rows').CredentialRow | null> {
  return db
    .prepare('SELECT * FROM credentials WHERE token_hash = ?')
    .bind(tokenHash)
    .first<import('./rows').CredentialRow>();
}

export async function activeRoomCount(db: D1Like, actorId: string): Promise<number> {
  const row = await db
    .prepare(
      "SELECT COUNT(*) AS n FROM room_members WHERE actor_id = ? AND status = 'active'",
    )
    .bind(actorId)
    .first<{ n: number }>();
  return row?.n ?? 0;
}

export async function getEventsMinSeq(
  db: D1Like,
  scopeKind: 'room' | 'inbox' | 'actor',
  scopeId: string,
): Promise<number> {
  if (scopeKind === 'room') {
    const room = await getRoom(db, scopeId);
    return room?.events_min_seq ?? 0;
  }
  if (scopeKind === 'inbox') {
    const actor = await getActor(db, scopeId);
    return actor?.inbox_min_seq ?? 0;
  }
  return 0;
}
