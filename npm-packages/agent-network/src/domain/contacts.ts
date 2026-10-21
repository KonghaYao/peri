import { BusinessError } from '../contracts/errors';
import { decodeCursor, encodeCursor } from '../contracts/cursor';
import { clampPage, LIMITS } from '../contracts/limits';
import type {
  ContactAddInput,
  ContactRemoveInput,
  ContactsListInput,
} from '../contracts/schemas';
import { nowMs, sha256Hex, stableStringify } from '../storage/db';
import { getActor, getActorBySession } from '../storage/queries';
import type { ActorRow, ContactRow } from '../storage/rows';
import { operationsStatement, runIdempotent } from '../storage/transactions/runner';
import { actorId, type Principal } from './principal';

export interface ContactView {
  actor_id: string;
  kind: 'human' | 'agent';
  name: string;
  agent_session_id: string | null;
  created_at: string;
}

export interface ContactPage {
  items: ContactView[];
  next_cursor: string | null;
  has_more: boolean;
}

function contactView(actor: ActorRow, createdAt: number): ContactView {
  return {
    actor_id: actor.actor_id,
    kind: actor.kind,
    name: actor.name,
    agent_session_id: actor.agent_session_id,
    created_at: String(createdAt),
  };
}

interface ContactJoinRow extends ActorRow {
  contact_created_at: number;
}

/** 好友分页：按 (created_at DESC, target_actor_id ASC) 的复合 keyset。 */
export async function contactsList(
  db: D1Database,
  principal: Principal,
  input: ContactsListInput,
): Promise<ContactPage> {
  const owner = actorId(principal);
  const scope = `contacts:${owner}`;
  const limit = clampPage(input.limit, LIMITS.historyPageDefault, LIMITS.historyPageMax);
  const cursor = input.cursor ? decodeCursor(input.cursor, scope) : null;

  const where = cursor
    ? 'c.owner_actor_id = ? AND (c.created_at < ? OR (c.created_at = ? AND c.target_actor_id > ?))'
    : 'c.owner_actor_id = ?';
  const binds: unknown[] = cursor
    ? [owner, cursor.seq, cursor.seq, cursor.key ?? '']
    : [owner];
  const rows = await db
    .prepare(
      `SELECT a.*, c.created_at AS contact_created_at
         FROM contacts c
         JOIN actors a ON a.actor_id = c.target_actor_id
        WHERE ${where}
        ORDER BY c.created_at DESC, c.target_actor_id ASC
        LIMIT ?`,
    )
    .bind(...binds, limit + 1)
    .all<ContactJoinRow>();

  const page = rows.results.slice(0, limit);
  const hasMore = rows.results.length > limit;
  const last = page.at(-1);
  return {
    items: page.map((row) => contactView(row, row.contact_created_at)),
    next_cursor:
      hasMore && last
        ? encodeCursor({
            v: 1,
            scope,
            dir: 'before',
            seq: last.contact_created_at,
            key: last.actor_id,
            snapshot: 0,
          })
        : null,
    has_more: hasMore,
  };
}

async function resolveTarget(
  db: D1Database,
  input: ContactAddInput,
): Promise<ActorRow> {
  const hasId = typeof input.actor_id === 'string';
  const hasSession = typeof input.agent_session_id === 'string';
  if (hasId === hasSession) {
    throw new BusinessError('INVALID_ARGUMENT', 'actor_id 与 agent_session_id 必须且只能提供一个');
  }
  const actor = hasId
    ? await getActor(db, input.actor_id as string)
    : await getActorBySession(db, input.agent_session_id as string);
  if (!actor) throw new BusinessError('NOT_FOUND', '目标 actor 不存在');
  return actor;
}

/** 幂等建立单向好友关系。 */
export async function contactAdd(
  db: D1Database,
  principal: Principal,
  input: ContactAddInput,
): Promise<{ contact: ContactView }> {
  const owner = actorId(principal);
  const target = await resolveTarget(db, input);
  if (target.actor_id === owner) {
    throw new BusinessError('INVALID_ARGUMENT', '不能把自己添加为好友');
  }

  const at = nowMs();
  const payloadHash = await sha256Hex(stableStringify(input));
  const result = { contact: contactView(target, at) };
  await runIdempotent(db, {
    actorId: owner,
    requestId: input.request_id,
    payloadHash,
    result,
    build: (resultJson) => [
      operationsStatement(db, owner, input.request_id, payloadHash, resultJson, at),
      db
        .prepare(
          `INSERT INTO contacts (owner_actor_id, target_actor_id, created_at)
           VALUES (?, ?, ?)
           ON CONFLICT(owner_actor_id, target_actor_id) DO NOTHING`,
        )
        .bind(owner, target.actor_id, at),
    ],
  });

  const existing = await db
    .prepare('SELECT * FROM contacts WHERE owner_actor_id = ? AND target_actor_id = ?')
    .bind(owner, target.actor_id)
    .first<ContactRow>();
  return { contact: contactView(target, existing?.created_at ?? at) };
}

/** 删除自己的好友关系；关系不存在时同样成功。 */
export async function contactRemove(
  db: D1Database,
  principal: Principal,
  input: ContactRemoveInput,
): Promise<{ actor_id: string; removed: true }> {
  const owner = actorId(principal);
  const at = nowMs();
  const payloadHash = await sha256Hex(stableStringify(input));
  const result = { actor_id: input.actor_id, removed: true as const };
  await runIdempotent(db, {
    actorId: owner,
    requestId: input.request_id,
    payloadHash,
    result,
    build: (resultJson) => [
      operationsStatement(db, owner, input.request_id, payloadHash, resultJson, at),
      db
        .prepare('DELETE FROM contacts WHERE owner_actor_id = ? AND target_actor_id = ?')
        .bind(owner, input.actor_id),
    ],
  });
  return result;
}

/** 事务内校验好友关系。 */
export async function areContacts(
  db: D1Database,
  owner: string,
  target: string,
): Promise<boolean> {
  const row = await db
    .prepare('SELECT 1 AS ok FROM contacts WHERE owner_actor_id = ? AND target_actor_id = ?')
    .bind(owner, target)
    .first<{ ok: number }>();
  return row !== null;
}
