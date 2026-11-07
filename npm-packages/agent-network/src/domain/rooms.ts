import { BusinessError } from '../contracts/errors';
import { decodeCursor, encodeCursor } from '../contracts/cursor';
import { clampPage, LIMITS } from '../contracts/limits';
import type {
  RoomCreateInput,
  RoomLeaveInput,
  RoomMembersUpdateInput,
  RoomsListInput,
} from '../contracts/schemas';
import {
  newId,
  nowMs,
  sha256Hex,
  sortedPairKey,
  stableStringify,
} from '../storage/db';
import {
  eventStatement,
  eventStatementIfAbsent,
  inboxHeadStatement,
  inboxTopic,
  outboxStatement,
  outboxStatementIfAbsent,
  roomHeadStatement,
  roomMemberSeqStatement,
  roomTopic,
} from '../storage/events';
import { guardSql } from '../storage/guards';
import { getActor, getMember, getMessage, getRoom, isActiveMember } from '../storage/queries';
import type { ActorRow, MessageRow, RoomMemberRow, RoomRow } from '../storage/rows';
import { operationsStatement, runIdempotent } from '../storage/transactions/runner';
import { areContacts } from './contacts';
import { actorId, type Principal } from './principal';
import { messagePreviewOf, type MessagePreviewView } from './views';

export type { MessagePreviewView } from './views';

export interface RoomView {
  room_id: string;
  kind: 'direct' | 'group';
  owner_id: string | null;
  title: string;
  head_seq: string;
  member_seq: string;
  member_count: number;
  my_role: 'owner' | 'member';
  my_last_read_seq: string;
  last_message: MessagePreviewView | null;
  created_at: string;
  updated_at: string;
}

export interface MemberView {
  actor_id: string;
  kind: 'human' | 'agent';
  name: string;
  agent_session_id: string | null;
  role: 'owner' | 'member';
  joined_seq: string;
  joined_at: string;
  last_read_seq: string;
  /**
   * Agent 的在线快照；`null` 表示未知。
   *
   * 域层不访问 Durable Object，默认 `null`；`network_room_get` 出口再按需
   * 补充。人类成员恒为 `null`，页面不展示在线状态。
   */
  online: boolean | null;
}

export interface RoomDetail {
  room: RoomView;
  members: MemberView[];
}

export interface RoomPage {
  items: RoomView[];
  next_cursor: string | null;
  has_more: boolean;
}

function messagePreview(row: MessageRow | null): MessagePreviewView | null {
  return messagePreviewOf(row);
}

function roomView(
  room: RoomRow,
  member: Pick<RoomMemberRow, 'role' | 'last_read_seq'>,
  memberCount: number,
  lastMessage: MessageRow | null,
): RoomView {
  return {
    room_id: room.room_id,
    kind: room.kind,
    owner_id: room.owner_id,
    title: room.title,
    head_seq: String(room.head_seq),
    member_seq: String(room.member_seq),
    member_count: memberCount,
    my_role: member.role,
    my_last_read_seq: String(member.last_read_seq),
    last_message: messagePreview(lastMessage),
    created_at: String(room.created_at),
    updated_at: String(room.updated_at),
  };
}

async function memberCount(db: D1Database, roomId: string): Promise<number> {
  const row = await db
    .prepare("SELECT COUNT(*) AS n FROM room_members WHERE room_id = ? AND status = 'active'")
    .bind(roomId)
    .first<{ n: number }>();
  return row?.n ?? 0;
}

async function loadRoomView(
  db: D1Database,
  room: RoomRow,
  member: RoomMemberRow,
): Promise<RoomView> {
  const [count, last] = await Promise.all([
    memberCount(db, room.room_id),
    room.last_message_id ? getMessage(db, room.last_message_id) : Promise.resolve(null),
  ]);
  return roomView(room, member, count, last);
}

async function loadMembers(db: D1Database, roomId: string): Promise<MemberView[]> {
  const rows = await db
    .prepare(
      `SELECT m.*, a.kind, a.name, a.agent_session_id
         FROM room_members m
         JOIN actors a ON a.actor_id = m.actor_id
        WHERE m.room_id = ? AND m.status = 'active'
        ORDER BY CASE m.role WHEN 'owner' THEN 0 ELSE 1 END, m.joined_at ASC, m.actor_id ASC`,
    )
    .bind(roomId)
    .all<RoomMemberRow & Pick<ActorRow, 'kind' | 'name' | 'agent_session_id'>>();
  return rows.results.map((row) => ({
    actor_id: row.actor_id,
    kind: row.kind,
    name: row.name,
    agent_session_id: row.agent_session_id,
    role: row.role,
    joined_seq: String(row.joined_seq),
    joined_at: String(row.joined_at),
    last_read_seq: String(row.last_read_seq),
    online: null,
  }));
}

interface RoomListRow extends RoomRow {
  my_role: 'owner' | 'member';
  my_last_read_seq: number;
  member_count: number;
  lm_message_id: string | null;
  lm_sender_id: string | null;
  lm_preview: string | null;
  lm_body_bytes: number | null;
  lm_created_seq: number | null;
  lm_created_at: number | null;
}

export async function roomsList(
  db: D1Database,
  principal: Principal,
  input: RoomsListInput,
): Promise<RoomPage> {
  const id = actorId(principal);
  const scope = `rooms:${id}`;
  const limit = clampPage(input.limit, LIMITS.historyPageDefault, LIMITS.historyPageMax);
  const cursor = input.cursor ? decodeCursor(input.cursor, scope) : null;
  const keyset = cursor
    ? 'AND (r.updated_at < ? OR (r.updated_at = ? AND r.room_id < ?))'
    : '';
  const binds: unknown[] = cursor ? [id, cursor.seq, cursor.seq, cursor.key ?? ''] : [id];

  const rows = await db
    .prepare(
      `SELECT r.*, m.role AS my_role, m.last_read_seq AS my_last_read_seq,
              (SELECT COUNT(*) FROM room_members mm WHERE mm.room_id = r.room_id AND mm.status = 'active') AS member_count,
              lm.message_id AS lm_message_id, lm.sender_id AS lm_sender_id, lm.preview AS lm_preview,
              lm.body_bytes AS lm_body_bytes, lm.created_seq AS lm_created_seq, lm.created_at AS lm_created_at
         FROM room_members m
         JOIN rooms r ON r.room_id = m.room_id
         LEFT JOIN messages lm ON lm.message_id = r.last_message_id
        WHERE m.actor_id = ? AND m.status = 'active' ${keyset}
        ORDER BY r.updated_at DESC, r.room_id DESC
        LIMIT ?`,
    )
    .bind(...binds, limit + 1)
    .all<RoomListRow>();

  const page = rows.results.slice(0, limit);
  const hasMore = rows.results.length > limit;
  const last = page.at(-1);
  const items = page.map((row) =>
    roomView(
      row,
      { role: row.my_role, last_read_seq: row.my_last_read_seq },
      row.member_count,
      row.lm_message_id
        ? {
            message_id: row.lm_message_id,
            room_id: row.room_id,
            sender_id: row.lm_sender_id ?? '',
            client_message_id: '',
            created_seq: row.lm_created_seq ?? 0,
            body: '',
            body_bytes: row.lm_body_bytes ?? 0,
            preview: row.lm_preview ?? '',
            reply_to: null,
            root_id: null,
            hop: 0,
            root_invocations_used: 0,
            created_at: row.lm_created_at ?? 0,
          }
        : null,
    ),
  );

  return {
    items,
    next_cursor:
      hasMore && last
        ? encodeCursor({
            v: 1,
            scope,
            dir: 'before',
            seq: last.updated_at,
            key: last.room_id,
            snapshot: 0,
          })
        : null,
    has_more: hasMore,
  };
}

async function ensureActors(db: D1Database, ids: string[]): Promise<ActorRow[]> {
  if (ids.length === 0) return [];
  const placeholders = ids.map(() => '?').join(',');
  const rows = await db
    .prepare(`SELECT * FROM actors WHERE actor_id IN (${placeholders})`)
    .bind(...ids)
    .all<ActorRow>();
  const found = new Map(rows.results.map((row) => [row.actor_id, row]));
  const missing = ids.filter((id) => !found.has(id));
  if (missing.length > 0) {
    throw new BusinessError('INVALID_ARGUMENT', '成员 actor 不存在', { missing });
  }
  return ids.map((id) => found.get(id) as ActorRow);
}

function uniqueOthers(self: string, members: string[]): string[] {
  return [...new Set(members)].filter((id) => id !== self);
}

async function requireFriends(
  db: D1Database,
  owner: string,
  targets: string[],
): Promise<void> {
  for (const target of targets) {
    if (!(await areContacts(db, owner, target))) {
      throw new BusinessError('FRIEND_REQUIRED', '只能把好友加入群聊', { actor_id: target });
    }
  }
}

/** 房间创建时所有参与者的 inbox 事件（含分享入口）。 */
function inboxChangeStatements(
  db: D1Database,
  actorIds: string[],
  eventKeyFor: (actor: string) => string,
  eventType: string,
  at: number,
  refId: string,
): D1PreparedStatement[] {
  const statements: D1PreparedStatement[] = [];
  for (const actor of actorIds) {
    const eventKey = eventKeyFor(actor);
    statements.push(
      eventStatementIfAbsent(
        db,
        { eventKey, scopeKind: 'inbox', scopeId: actor, eventType, refId },
        at,
      ),
      outboxStatementIfAbsent(db, { topic: inboxTopic(actor), eventKey }, at),
      inboxHeadStatement(db, actor, eventKey, at, 0),
    );
  }
  return statements;
}

export async function roomCreate(
  db: D1Database,
  principal: Principal,
  input: RoomCreateInput,
): Promise<RoomDetail> {
  const owner = actorId(principal);
  const others = uniqueOthers(owner, input.members);
  if (others.length === 0) {
    throw new BusinessError('INVALID_ARGUMENT', '房间至少需要另一个成员');
  }
  await ensureActors(db, others);

  if (input.kind === 'direct') {
    if (others.length !== 1) {
      throw new BusinessError('INVALID_ARGUMENT', '单聊只能指定一个对方');
    }
    return createDirect(db, principal, others[0] as string, input.request_id);
  }

  await requireFriends(db, owner, others);
  if (others.length + 1 > LIMITS.groupMembersMax) {
    throw new BusinessError('ROOM_FULL', '群成员数量超过上限');
  }

  const at = nowMs();
  const roomId = newId('rm');
  const payloadHash = await sha256Hex(stableStringify(input));
  const roomRef = `room_created:${roomId}`;
  const participants = [owner, ...others];

  await runIdempotent(db, {
    actorId: owner,
    requestId: input.request_id,
    payloadHash,
    result: { room_id: roomId },
    build: (resultJson) => [
      operationsStatement(db, owner, input.request_id, payloadHash, resultJson, at),
      db
        .prepare(
          guardSql(
            'roomLimit',
            '(SELECT COUNT(*) FROM room_members WHERE actor_id = ? AND status = \'active\') >= ?',
          ),
        )
        .bind(owner, LIMITS.activeRoomsPerActor),
      db
        .prepare(
          `INSERT INTO rooms
             (room_id, kind, owner_id, title, direct_pair_key, head_seq, events_min_seq, member_seq, last_message_id, created_at, updated_at)
           VALUES (?, 'group', ?, ?, NULL, 0, 0, 0, NULL, ?, ?)`,
        )
        .bind(roomId, owner, input.title ?? '', at, at),
      eventStatement(
        db,
        {
          eventKey: roomRef,
          scopeKind: 'room',
          scopeId: roomId,
          eventType: 'room_created',
          refId: roomId,
          metadata: { owner_id: owner, members: others },
        },
        at,
      ),
      outboxStatement(db, { topic: roomTopic(roomId), eventKey: roomRef }, at),
      roomHeadStatement(db, roomId, roomRef, at),
      ...participants.map((participant) =>
        db
          .prepare(
            `INSERT INTO room_members (room_id, actor_id, role, status, joined_seq, last_read_seq, joined_at)
             VALUES (?, ?, ?, 'active', (SELECT event_seq FROM events WHERE event_key = ?), 0, ?)`,
          )
          .bind(
            roomId,
            participant,
            participant === owner ? 'owner' : 'member',
            roomRef,
            at,
          ),
      ),
      ...inboxChangeStatements(
        db,
        participants,
        (actor) => `room_created:${roomId}:${actor}`,
        'room_created',
        at,
        roomId,
      ),
    ],
  });

  return roomDetail(db, roomId, owner);
}

async function createDirect(
  db: D1Database,
  principal: Principal,
  target: string,
  requestId: string,
): Promise<RoomDetail> {
  const owner = actorId(principal);
  const at = nowMs();
  const pairKey = sortedPairKey(owner, target);
  const roomId = `rm_${(await sha256Hex(`direct:${pairKey}`)).slice(0, 32)}`;
  const roomRef = `room_created:${roomId}`;
  const participants = [owner, target];
  const payloadHash = await sha256Hex(
    stableStringify({ kind: 'direct', members: [target], request_id: requestId }),
  );

  await runIdempotent(db, {
    actorId: owner,
    requestId,
    payloadHash,
    result: { room_id: roomId },
    build: (resultJson) => [
      operationsStatement(db, owner, requestId, payloadHash, resultJson, at),
      db
        .prepare(
          guardSql(
            'roomLimit',
            '(SELECT COUNT(*) FROM room_members WHERE actor_id = ? AND status = \'active\') >= ?',
          ),
        )
        .bind(owner, LIMITS.activeRoomsPerActor),
      db
        .prepare(
          `INSERT INTO rooms
             (room_id, kind, owner_id, title, direct_pair_key, head_seq, events_min_seq, member_seq, last_message_id, created_at, updated_at)
           VALUES (?, 'direct', NULL, '', ?, 0, 0, 0, NULL, ?, ?)
           ON CONFLICT(room_id) DO NOTHING`,
        )
        .bind(roomId, pairKey, at, at),
      eventStatementIfAbsent(
        db,
        {
          eventKey: roomRef,
          scopeKind: 'room',
          scopeId: roomId,
          eventType: 'room_created',
          refId: roomId,
          metadata: { pair: participants },
        },
        at,
      ),
      outboxStatementIfAbsent(db, { topic: roomTopic(roomId), eventKey: roomRef }, at),
      roomHeadStatement(db, roomId, roomRef, at),
      ...participants.map((participant) =>
        db
          .prepare(
            `INSERT INTO room_members (room_id, actor_id, role, status, joined_seq, last_read_seq, joined_at)
             VALUES (?, ?, 'member', 'active', (SELECT event_seq FROM events WHERE event_key = ?), 0, ?)
             ON CONFLICT(room_id, actor_id) DO UPDATE SET status = 'active', role = 'member'`,
          )
          .bind(roomId, participant, roomRef, at),
      ),
      ...inboxChangeStatements(
        db,
        participants,
        (actor) => `room_created:${roomId}:${actor}`,
        'room_created',
        at,
        roomId,
      ),
    ],
  });

  return roomDetail(db, roomId, owner);
}

async function roomDetail(db: D1Database, roomId: string, viewer: string): Promise<RoomDetail> {
  const room = await getRoom(db, roomId);
  if (!room) throw new BusinessError('NOT_FOUND', '房间不存在');
  const member = await getMember(db, roomId, viewer);
  if (!member || member.status !== 'active') {
    throw new BusinessError('NOT_MEMBER', '当前身份不是该房间的有效成员');
  }
  const [view, members] = await Promise.all([loadRoomView(db, room, member), loadMembers(db, roomId)]);
  return { room: view, members };
}

export async function roomGet(
  db: D1Database,
  principal: Principal,
  roomId: string,
): Promise<RoomDetail> {
  return roomDetail(db, roomId, actorId(principal));
}

function cancelDeliveriesForMemberStatements(
  db: D1Database,
  roomId: string,
  member: string,
  reason: string,
  at: number,
): D1PreparedStatement[] {
  return [
    // pending 才计入 pending_count；先按当前 pending 数扣减，再取消。
    db
      .prepare(
        `UPDATE actors
            SET pending_count = pending_count - (
                  SELECT COUNT(*) FROM deliveries
                   WHERE room_id = ? AND recipient_id = ? AND state = 'pending'
                ),
                updated_at = ?
          WHERE actor_id = ?`,
      )
      .bind(roomId, member, at, member),
    db
      .prepare(
        `UPDATE deliveries
            SET state = 'cancelled', error = ?, updated_at = ?
          WHERE room_id = ? AND recipient_id = ? AND state IN ('pending', 'claimed')`,
      )
      .bind(reason, at, roomId, member),
  ];
}

export async function roomMembersUpdate(
  db: D1Database,
  principal: Principal,
  input: RoomMembersUpdateInput,
): Promise<RoomDetail> {
  const caller = actorId(principal);
  const room = await getRoom(db, input.room_id);
  if (!room) throw new BusinessError('NOT_FOUND', '房间不存在');
  if (room.kind !== 'group') {
    throw new BusinessError('INVALID_ARGUMENT', '单聊不支持成员变更');
  }
  if (room.owner_id !== caller) {
    throw new BusinessError('NOT_OWNER', '只有群 owner 可以管理成员');
  }

  const add = uniqueOthers(caller, input.add ?? []).filter((id) => id !== room.owner_id);
  const remove = uniqueOthers(caller, input.remove ?? []);
  if (add.length > LIMITS.memberChangeMax || remove.length > LIMITS.memberChangeMax) {
    throw new BusinessError('INVALID_ARGUMENT', '单次成员变更数量超过上限');
  }
  if (add.length === 0 && remove.length === 0 && !input.transfer_owner_to) {
    return roomDetail(db, room.room_id, caller);
  }
  if (add.length > 0) {
    await ensureActors(db, add);
    await requireFriends(db, caller, add);
  }
  if (input.transfer_owner_to) {
    if (input.transfer_owner_to === caller) {
      throw new BusinessError('INVALID_ARGUMENT', 'owner 已经是当前身份');
    }
    if (!(await isActiveMember(db, room.room_id, input.transfer_owner_to))) {
      throw new BusinessError('INVALID_ARGUMENT', 'owner 转移目标必须是当前成员');
    }
    if (remove.includes(input.transfer_owner_to)) {
      throw new BusinessError('INVALID_ARGUMENT', '不能在同一次操作中移除 owner 转移目标');
    }
  }

  const at = nowMs();
  const eventKey = `members:${room.room_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));
  const affected = [...new Set([...add, ...remove, ...(input.transfer_owner_to ? [input.transfer_owner_to] : [])])];

  await runIdempotent(db, {
    actorId: caller,
    requestId: input.request_id,
    payloadHash,
    result: { room_id: room.room_id },
    build: (resultJson) => [
      operationsStatement(db, caller, input.request_id, payloadHash, resultJson, at),
      db
        .prepare(
          guardSql(
            'notOwner',
            'NOT EXISTS (SELECT 1 FROM rooms WHERE room_id = ? AND owner_id = ?)',
          ),
        )
        .bind(room.room_id, caller),
      db
        .prepare(
          guardSql(
            'roomFull',
            '(SELECT COUNT(*) FROM room_members WHERE room_id = ? AND status = \'active\') + ? > ?',
          ),
        )
        .bind(room.room_id, add.length, LIMITS.groupMembersMax),
      eventStatement(
        db,
        {
          eventKey,
          scopeKind: 'room',
          scopeId: room.room_id,
          eventType: 'members_changed',
          refId: room.room_id,
          metadata: { add, remove, transfer_owner_to: input.transfer_owner_to ?? null },
        },
        at,
      ),
      outboxStatement(db, { topic: roomTopic(room.room_id), eventKey }, at),
      ...add.map((member) =>
        db
          .prepare(
            `INSERT INTO room_members (room_id, actor_id, role, status, joined_seq, last_read_seq, joined_at)
             VALUES (?, ?, 'member', 'active', (SELECT event_seq FROM events WHERE event_key = ?), 0, ?)
             ON CONFLICT(room_id, actor_id)
             DO UPDATE SET status = 'active', role = 'member',
                           joined_seq = excluded.joined_seq, joined_at = excluded.joined_at`,
          )
          .bind(room.room_id, member, eventKey, at),
      ),
      ...remove.flatMap((member) => [
        db
          .prepare(
            `UPDATE room_members SET status = 'removed'
              WHERE room_id = ? AND actor_id = ? AND status = 'active'`,
          )
          .bind(room.room_id, member),
        ...cancelDeliveriesForMemberStatements(db, room.room_id, member, 'member_removed', at),
      ]),
      ...(input.transfer_owner_to
        ? [
            db
              .prepare("UPDATE room_members SET role = 'member' WHERE room_id = ? AND actor_id = ?")
              .bind(room.room_id, caller),
            db
              .prepare("UPDATE room_members SET role = 'owner' WHERE room_id = ? AND actor_id = ?")
              .bind(room.room_id, input.transfer_owner_to),
            db
              .prepare('UPDATE rooms SET owner_id = ?, updated_at = ? WHERE room_id = ?')
              .bind(input.transfer_owner_to, at, room.room_id),
          ]
        : []),
      roomMemberSeqStatement(db, room.room_id, eventKey, at),
      ...[...affected, caller]
        .filter((actor, index, list) => list.indexOf(actor) === index)
        .flatMap((actor) => {
          const inboxKey = `members:${room.room_id}:${input.request_id}:${actor}`;
          return [
            eventStatement(
              db,
              {
                eventKey: inboxKey,
                scopeKind: 'inbox',
                scopeId: actor,
                eventType: 'room_members_changed',
                refId: room.room_id,
              },
              at,
            ),
            outboxStatement(db, { topic: inboxTopic(actor), eventKey: inboxKey }, at),
            inboxHeadStatement(db, actor, inboxKey, at, 0),
          ];
        }),
    ],
  });

  return roomDetail(db, room.room_id, caller);
}

export async function roomLeave(
  db: D1Database,
  principal: Principal,
  input: RoomLeaveInput,
): Promise<{ room_id: string; left: true }> {
  const caller = actorId(principal);
  const room = await getRoom(db, input.room_id);
  if (!room) throw new BusinessError('NOT_FOUND', '房间不存在');
  if (room.kind !== 'group') {
    throw new BusinessError('INVALID_ARGUMENT', '单聊不支持离开');
  }
  const member = await getMember(db, room.room_id, caller);
  if (!member || member.status !== 'active') {
    throw new BusinessError('NOT_MEMBER', '当前身份不是该房间的有效成员');
  }
  if (room.owner_id === caller) {
    const others = await db
      .prepare(
        "SELECT COUNT(*) AS n FROM room_members WHERE room_id = ? AND status = 'active' AND actor_id <> ?",
      )
      .bind(room.room_id, caller)
      .first<{ n: number }>();
    if ((others?.n ?? 0) > 0) {
      throw new BusinessError('CONFLICT', '群 owner 离开前必须转移 owner');
    }
  }

  const at = nowMs();
  const eventKey = `leave:${room.room_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));

  await runIdempotent(db, {
    actorId: caller,
    requestId: input.request_id,
    payloadHash,
    result: { room_id: room.room_id, left: true as const },
    build: (resultJson) => [
      operationsStatement(db, caller, input.request_id, payloadHash, resultJson, at),
      db
        .prepare(
          guardSql(
            'notMember',
            "NOT EXISTS (SELECT 1 FROM room_members WHERE room_id = ? AND actor_id = ? AND status = 'active')",
          ),
        )
        .bind(room.room_id, caller),
      eventStatement(
        db,
        {
          eventKey,
          scopeKind: 'room',
          scopeId: room.room_id,
          eventType: 'member_left',
          refId: caller,
        },
        at,
      ),
      outboxStatement(db, { topic: roomTopic(room.room_id), eventKey }, at),
      db
        .prepare(
          "UPDATE room_members SET status = 'left' WHERE room_id = ? AND actor_id = ? AND status = 'active'",
        )
        .bind(room.room_id, caller),
      ...cancelDeliveriesForMemberStatements(db, room.room_id, caller, 'member_left', at),
      ...(room.owner_id === caller
        ? [db.prepare('UPDATE rooms SET owner_id = NULL WHERE room_id = ?').bind(room.room_id)]
        : []),
      roomMemberSeqStatement(db, room.room_id, eventKey, at),
      ...(() => {
        const inboxKey = `leave:${room.room_id}:${input.request_id}`;
        return [
          eventStatement(
            db,
            {
              eventKey: inboxKey,
              scopeKind: 'inbox',
              scopeId: caller,
              eventType: 'room_left',
              refId: room.room_id,
            },
            at,
          ),
          outboxStatement(db, { topic: inboxTopic(caller), eventKey: inboxKey }, at),
          inboxHeadStatement(db, caller, inboxKey, at, 0),
        ];
      })(),
    ],
  });

  return { room_id: room.room_id, left: true };
}
