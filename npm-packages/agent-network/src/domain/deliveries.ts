import { BusinessError } from '../contracts/errors';
import { decodeCursor, encodeCursor } from '../contracts/cursor';
import { clampPage, LIMITS } from '../contracts/limits';
import type {
  DeliveryCancelInput,
  DeliveryClaimInput,
  DeliveryCompleteInput,
  DeliveryFailInput,
  DeliveryReportInput,
  DeliveryRetryInput,
  InboxListInput,
} from '../contracts/schemas';
import { nowMs, sha256Hex, stableStringify } from '../storage/db';
import {
  eventStatement,
  inboxHeadStatement,
  inboxTopic,
  outboxStatement,
} from '../storage/events';
import { guardSql } from '../storage/guards';
import { getActor, getDelivery, getMessage, getRoom } from '../storage/queries';
import type { DeliveryRow, MessageRow } from '../storage/rows';
import {
  isOneClaimedConflict,
  mapBatchError,
  operationsStatement,
  runIdempotent,
} from '../storage/transactions/runner';
import {
  finalMessagePlan,
  messageCoreStatements,
  messageGuardStatements,
  messageView,
  type MessageView,
} from './messages';
import { actorId, type Principal } from './principal';
import type { MessagePreviewView } from './rooms';
import { messagePreviewOf } from './views';

export interface DeliveryView {
  delivery_id: string;
  recipient_id: string;
  message_id: string;
  room_id: string;
  state: DeliveryRow['state'];
  attempt: number;
  claim_id: string | null;
  claimed_at: string | null;
  progress_at: string | null;
  result_message_id: string | null;
  error: string | null;
  created_seq: string;
  created_at: string;
  updated_at: string;
  message: MessagePreviewView | null;
}

export interface InboxPage {
  items: DeliveryView[];
  next_cursor: string | null;
  has_more: boolean;
  pending_count: number;
  claimed_delivery_id: string | null;
}

const encoder = new TextEncoder();
void encoder;

function previewOf(row: MessageRow | null): MessagePreviewView | null {
  return messagePreviewOf(row);
}

function view(row: DeliveryRow, message: MessageRow | null): DeliveryView {
  return {
    delivery_id: row.delivery_id,
    recipient_id: row.recipient_id,
    message_id: row.message_id,
    room_id: row.room_id,
    state: row.state,
    attempt: row.attempt,
    claim_id: row.claim_id,
    claimed_at: row.claimed_at === null ? null : String(row.claimed_at),
    progress_at: row.progress_at === null ? null : String(row.progress_at),
    result_message_id: row.result_message_id,
    error: row.error,
    created_seq: String(row.created_seq),
    created_at: String(row.created_at),
    updated_at: String(row.updated_at),
    message: previewOf(message),
  };
}

async function deliveryView(db: D1Database, row: DeliveryRow): Promise<DeliveryView> {
  return view(row, await getMessage(db, row.message_id));
}

interface InboxRow extends DeliveryRow {
  m_preview: string;
  m_body_bytes: number;
  m_sender_id: string;
  m_created_at: number;
}

export async function inboxList(
  db: D1Database,
  principal: Principal,
  input: InboxListInput,
): Promise<InboxPage> {
  const actor = actorId(principal);
  const limit = clampPage(input.limit, LIMITS.inboxPageDefault, LIMITS.inboxPageMax);
  const scope = `inbox:${actor}`;
  const cursor = input.cursor ? decodeCursor(input.cursor, scope) : null;
  const state = input.state ?? 'active';
  const states =
    state === 'all' ? null : state === 'active' ? ['pending', 'claimed'] : [state];
  const stateFilter = states ? `AND d.state IN (${states.map(() => '?').join(',')})` : '';
  const binds: unknown[] = states ? [actor, ...states] : [actor];
  if (cursor) binds.push(cursor.seq);
  binds.push(limit + 1);

  const rows = await db
    .prepare(
      `SELECT d.*, m.preview AS m_preview, m.body_bytes AS m_body_bytes,
              m.sender_id AS m_sender_id, m.created_at AS m_created_at
         FROM deliveries d
         JOIN messages m ON m.message_id = d.message_id
        WHERE d.recipient_id = ? ${stateFilter}
          ${cursor ? 'AND d.created_seq > ?' : ''}
        ORDER BY d.created_seq ASC
        LIMIT ?`,
    )
    .bind(...binds)
    .all<InboxRow>();

  const page = rows.results.slice(0, limit);
  const hasMore = rows.results.length > limit;
  const last = page.at(-1);
  const [actorRow, claimed] = await Promise.all([
    getActor(db, actor),
    db
      .prepare("SELECT delivery_id FROM deliveries WHERE recipient_id = ? AND state = 'claimed' LIMIT 1")
      .bind(actor)
      .first<{ delivery_id: string }>(),
  ]);

  return {
    items: page.map((row) =>
      view(row, {
        message_id: row.message_id,
        room_id: row.room_id,
        sender_id: row.m_sender_id,
        client_message_id: '',
        created_seq: row.created_seq,
        body: '',
        body_bytes: row.m_body_bytes,
        preview: row.m_preview,
        reply_to: null,
        root_id: null,
        hop: 0,
        root_invocations_used: 0,
        created_at: row.m_created_at,
      }),
    ),
    next_cursor:
      hasMore && last
        ? encodeCursor({
            v: 1,
            scope,
            dir: 'after',
            seq: last.created_seq,
            snapshot: 0,
          })
        : null,
    has_more: hasMore,
    pending_count: actorRow?.pending_count ?? 0,
    claimed_delivery_id: claimed?.delivery_id ?? null,
  };
}

async function requireOwnedDelivery(
  db: D1Database,
  principal: Principal,
  deliveryId: string,
): Promise<DeliveryRow> {
  const actor = actorId(principal);
  const delivery = await getDelivery(db, deliveryId);
  if (!delivery || delivery.recipient_id !== actor) {
    throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  }
  return delivery;
}

function inboxEventStatements(
  db: D1Database,
  actor: string,
  deliveryId: string,
  eventKey: string,
  eventType: string,
  at: number,
  pendingDelta: number,
  metadata: Record<string, unknown>,
): D1PreparedStatement[] {
  return [
    eventStatement(
      db,
      {
        eventKey,
        scopeKind: 'inbox',
        scopeId: actor,
        eventType,
        refId: deliveryId,
        metadata,
      },
      at,
    ),
    outboxStatement(db, { topic: inboxTopic(actor), eventKey }, at),
    inboxHeadStatement(db, actor, eventKey, at, pendingDelta),
  ];
}

function claimedGuard(
  db: D1Database,
  deliveryId: string,
  attempt: number,
  claimId: string,
): D1PreparedStatement {
  return db
    .prepare(
      guardSql(
        'staleAttempt',
        "NOT EXISTS (SELECT 1 FROM deliveries WHERE delivery_id = ? AND state = 'claimed' AND attempt = ? AND claim_id = ?)",
      ),
    )
    .bind(deliveryId, attempt, claimId);
}

export async function deliveryClaim(
  db: D1Database,
  principal: Principal,
  input: DeliveryClaimInput,
): Promise<DeliveryView> {
  const actor = actorId(principal);
  const delivery = await requireOwnedDelivery(db, principal, input.delivery_id);

  const at = nowMs();
  const claimId = crypto.randomUUID();
  const eventKey = `claim:${delivery.delivery_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));

  try {
    await runIdempotent(db, {
      actorId: actor,
      requestId: input.request_id,
      payloadHash,
      result: { delivery_id: delivery.delivery_id },
      build: (resultJson) => [
        operationsStatement(db, actor, input.request_id, payloadHash, resultJson, at),
        db
          .prepare(
            guardSql(
              'notMember',
              "NOT EXISTS (SELECT 1 FROM room_members WHERE room_id = ? AND actor_id = ? AND status = 'active')",
            ),
          )
          .bind(delivery.room_id, actor),
        db
          .prepare(
            guardSql(
              'claimConflict',
              "NOT EXISTS (SELECT 1 FROM deliveries WHERE delivery_id = ? AND state = 'pending')",
            ),
          )
          .bind(delivery.delivery_id),
        db
          .prepare(
            `UPDATE deliveries
                SET state = 'claimed', attempt = attempt + 1, claim_id = ?,
                    claimed_at = ?, progress_at = ?, updated_at = ?
              WHERE delivery_id = ? AND recipient_id = ? AND state = 'pending'`,
          )
          .bind(claimId, at, at, at, delivery.delivery_id, actor),
        ...inboxEventStatements(
          db,
          actor,
          delivery.delivery_id,
          eventKey,
          'delivery_claimed',
          at,
          -1,
          { message_id: delivery.message_id, room_id: delivery.room_id },
        ),
      ],
    });
  } catch (error) {
    if (isOneClaimedConflict(error)) {
      throw new BusinessError('CLAIM_CONFLICT', '当前已有领取中的工作，不能并行领取');
    }
    throw mapBatchError(error) ?? error;
  }

  const updated = await getDelivery(db, delivery.delivery_id);
  if (!updated) throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  return deliveryView(db, updated);
}

export async function deliveryReport(
  db: D1Database,
  principal: Principal,
  input: DeliveryReportInput,
): Promise<{ delivery_id: string; attempt: number; progress_at: string }> {
  const actor = actorId(principal);
  const delivery = await requireOwnedDelivery(db, principal, input.delivery_id);
  const at = nowMs();
  const eventKey = `progress:${delivery.delivery_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));

  await runIdempotent(db, {
    actorId: actor,
    requestId: input.request_id,
    payloadHash,
    result: { delivery_id: delivery.delivery_id, attempt: input.attempt },
    build: (resultJson) => [
      operationsStatement(db, actor, input.request_id, payloadHash, resultJson, at),
      claimedGuard(db, input.delivery_id, input.attempt, input.claim_id),
      db
        .prepare('UPDATE deliveries SET progress_at = ?, updated_at = ? WHERE delivery_id = ?')
        .bind(at, at, delivery.delivery_id),
      ...inboxEventStatements(
        db,
        actor,
        delivery.delivery_id,
        eventKey,
        'delivery_progress',
        at,
        0,
        { status: input.status, attempt: input.attempt },
      ),
    ],
  });

  return {
    delivery_id: delivery.delivery_id,
    attempt: input.attempt,
    progress_at: String(at),
  };
}

export interface CompleteResult {
  delivery: DeliveryView;
  message: MessageView | null;
}

export async function deliveryComplete(
  db: D1Database,
  principal: Principal,
  input: DeliveryCompleteInput,
): Promise<CompleteResult> {
  const actor = actorId(principal);
  const delivery = await requireOwnedDelivery(db, principal, input.delivery_id);

  const at = nowMs();
  const plan = input.final_body
    ? await finalMessagePlan(
        db,
        actor,
        delivery,
        input.final_body,
        input.client_message_id ?? `complete:${delivery.delivery_id}:${input.attempt}`,
      )
    : null;
  if (plan) plan.at = at;

  const eventKey = `complete:${delivery.delivery_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));
  const result = {
    delivery_id: delivery.delivery_id,
    message_id: plan?.messageId ?? null,
  };

  await runIdempotent(db, {
    actorId: actor,
    requestId: input.request_id,
    payloadHash,
    result,
    build: (resultJson) => [
      operationsStatement(db, actor, input.request_id, payloadHash, resultJson, at),
      claimedGuard(db, delivery.delivery_id, input.attempt, input.claim_id),
      ...(plan ? [...messageGuardStatements(db, plan), ...messageCoreStatements(db, plan)] : []),
      db
        .prepare(
          `UPDATE deliveries
              SET state = 'completed', result_message_id = ?, error = NULL, updated_at = ?
            WHERE delivery_id = ? AND state = 'claimed' AND attempt = ? AND claim_id = ?`,
        )
        .bind(plan?.messageId ?? null, at, delivery.delivery_id, input.attempt, input.claim_id),
      ...inboxEventStatements(
        db,
        actor,
        delivery.delivery_id,
        eventKey,
        'delivery_completed',
        at,
        0,
        { message_id: delivery.message_id, attempt: input.attempt },
      ),
    ],
  });

  const updated = await getDelivery(db, delivery.delivery_id);
  if (!updated) throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  const message =
    updated.result_message_id !== null ? await getMessage(db, updated.result_message_id) : null;
  return {
    delivery: await deliveryView(db, updated),
    message: message ? await messageView(db, message) : null,
  };
}

export async function deliveryFail(
  db: D1Database,
  principal: Principal,
  input: DeliveryFailInput,
): Promise<DeliveryView> {
  const actor = actorId(principal);
  const delivery = await requireOwnedDelivery(db, principal, input.delivery_id);
  const at = nowMs();
  const eventKey = `fail:${delivery.delivery_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));

  await runIdempotent(db, {
    actorId: actor,
    requestId: input.request_id,
    payloadHash,
    result: { delivery_id: delivery.delivery_id },
    build: (resultJson) => [
      operationsStatement(db, actor, input.request_id, payloadHash, resultJson, at),
      claimedGuard(db, delivery.delivery_id, input.attempt, input.claim_id),
      db
        .prepare(
          `UPDATE deliveries SET state = 'failed', error = ?, updated_at = ?
            WHERE delivery_id = ? AND state = 'claimed' AND attempt = ? AND claim_id = ?`,
        )
        .bind(input.error, at, delivery.delivery_id, input.attempt, input.claim_id),
      ...inboxEventStatements(
        db,
        actor,
        delivery.delivery_id,
        eventKey,
        'delivery_failed',
        at,
        0,
        { attempt: input.attempt },
      ),
    ],
  });

  const updated = await getDelivery(db, delivery.delivery_id);
  if (!updated) throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  return deliveryView(db, updated);
}

async function canControlDelivery(
  db: D1Database,
  actor: string,
  delivery: DeliveryRow,
): Promise<boolean> {
  if (delivery.recipient_id === actor) return true;
  const message = await getMessage(db, delivery.message_id);
  if (message?.sender_id === actor) return true;
  const room = await getRoom(db, delivery.room_id);
  return room?.owner_id === actor;
}

export async function deliveryCancel(
  db: D1Database,
  principal: Principal,
  input: DeliveryCancelInput,
): Promise<DeliveryView> {
  const actor = actorId(principal);
  const delivery = await getDelivery(db, input.delivery_id);
  if (!delivery) throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  if (!(await canControlDelivery(db, actor, delivery))) {
    throw new BusinessError('NOT_OWNER', '只有请求者、房间 owner 或接收者可以取消');
  }
  if (delivery.state === 'cancelled') return deliveryView(db, delivery);
  if (delivery.state === 'completed' || delivery.state === 'failed') {
    throw new BusinessError('CONFLICT', '已结束的工作不能取消', { state: delivery.state });
  }

  const at = nowMs();
  const eventKey = `cancel:${delivery.delivery_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));

  await runIdempotent(db, {
    actorId: actor,
    requestId: input.request_id,
    payloadHash,
    result: { delivery_id: delivery.delivery_id },
    build: (resultJson) => [
      operationsStatement(db, actor, input.request_id, payloadHash, resultJson, at),
      db
        .prepare(
          guardSql(
            'claimConflict',
            "NOT EXISTS (SELECT 1 FROM deliveries WHERE delivery_id = ? AND state IN ('pending', 'claimed'))",
          ),
        )
        .bind(delivery.delivery_id),
      // 只有 pending 计入 pending_count；按执行时的真实状态扣减，避免与 claim 竞态。
      db
        .prepare(
          `UPDATE actors
              SET pending_count = pending_count - (
                    SELECT CASE WHEN state = 'pending' THEN 1 ELSE 0 END
                      FROM deliveries WHERE delivery_id = ?
                  ),
                  updated_at = ?
            WHERE actor_id = ?`,
        )
        .bind(delivery.delivery_id, at, delivery.recipient_id),
      db
        .prepare(
          `UPDATE deliveries SET state = 'cancelled', error = ?, updated_at = ?
            WHERE delivery_id = ? AND state IN ('pending', 'claimed')`,
        )
        .bind(input.reason, at, delivery.delivery_id),
      ...inboxEventStatements(
        db,
        delivery.recipient_id,
        delivery.delivery_id,
        eventKey,
        'delivery_cancelled',
        at,
        0,
        { reason: input.reason },
      ),
    ],
  });

  const updated = await getDelivery(db, delivery.delivery_id);
  if (!updated) throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  return deliveryView(db, updated);
}

export async function deliveryRetry(
  db: D1Database,
  principal: Principal,
  input: DeliveryRetryInput,
): Promise<DeliveryView> {
  const actor = actorId(principal);
  const delivery = await getDelivery(db, input.delivery_id);
  if (!delivery) throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  if (!(await canControlDelivery(db, actor, delivery))) {
    throw new BusinessError('NOT_OWNER', '只有请求者或房间 owner 可以重试');
  }

  const at = nowMs();
  const eventKey = `retry:${delivery.delivery_id}:${input.request_id}`;
  const payloadHash = await sha256Hex(stableStringify(input));

  await runIdempotent(db, {
    actorId: actor,
    requestId: input.request_id,
    payloadHash,
    result: { delivery_id: delivery.delivery_id },
    build: (resultJson) => [
      operationsStatement(db, actor, input.request_id, payloadHash, resultJson, at),
      db
        .prepare(
          guardSql(
            'staleAttempt',
            'NOT EXISTS (SELECT 1 FROM deliveries WHERE delivery_id = ? AND attempt = ? AND state IN (\'failed\', \'cancelled\', \'claimed\'))',
          ),
        )
        .bind(delivery.delivery_id, input.expected_attempt),
      db
        .prepare(
          `UPDATE deliveries
              SET state = 'pending', claim_id = NULL, claimed_at = NULL, progress_at = NULL,
                  error = NULL, updated_at = ?
            WHERE delivery_id = ? AND attempt = ? AND state IN ('failed', 'cancelled', 'claimed')`,
        )
        .bind(at, delivery.delivery_id, input.expected_attempt),
      ...inboxEventStatements(
        db,
        delivery.recipient_id,
        delivery.delivery_id,
        eventKey,
        'delivery_retried',
        at,
        1,
        { attempt: input.expected_attempt },
      ),
    ],
  });

  const updated = await getDelivery(db, delivery.delivery_id);
  if (!updated) throw new BusinessError('NOT_FOUND', 'delivery 不存在');
  return deliveryView(db, updated);
}
