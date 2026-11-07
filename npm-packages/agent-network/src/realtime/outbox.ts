import { LIMITS } from '../contracts/limits';
import { backoffMs, nowMs } from '../storage/db';
import type { Env } from '../env';
import { publishHint } from './bus';

interface OutboxDueRow {
  outbox_id: string;
  topic: string;
  event_seq: number;
  attempts: number;
  created_at: number;
  event_type: string;
}

export interface DrainResult {
  claimed: number;
  published: number;
  failed: number;
  oldestPendingAgeMs: number | null;
}

const MEMBERSHIP_EVENTS = new Set(['members_changed', 'member_left']);

function chunks<T>(items: T[], size: number): T[][] {
  const result: T[][] = [];
  for (let index = 0; index < items.length; index += size) {
    result.push(items.slice(index, index + size));
  }
  return result;
}

/**
 * 发布 D1 outbox 中的变更提示。
 *
 * - 30 秒租约竞争待发布行；同一 topic 合并为最高 event_seq（提示只需要版本）。
 * - 成员变化必须单独以 `membership` 提示发送，避免被合并丢失。
 * - 失败按指数退避加 jitter 重排，记录保留，不静默丢弃。
 */
export async function drainOutbox(env: Env, limit = LIMITS.outboxBatchMax): Promise<DrainResult> {
  const db = env.DB;
  const now = nowMs();

  await db
    .prepare(
      "UPDATE outbox SET status = 'pending', lease_token = NULL, lease_until = NULL WHERE status = 'processing' AND lease_until IS NOT NULL AND lease_until < ?",
    )
    .bind(now)
    .run();

  const due = await db
    .prepare(
      `SELECT o.outbox_id, o.topic, o.event_seq, o.attempts, o.created_at, e.event_type
         FROM outbox o JOIN events e ON e.event_seq = o.event_seq
        WHERE o.status = 'pending' AND o.next_attempt_at <= ?
        ORDER BY o.next_attempt_at ASC, o.outbox_id ASC
        LIMIT ?`,
    )
    .bind(now, limit)
    .all<OutboxDueRow>();

  const leaseToken = crypto.randomUUID();
  const leaseUntil = now + LIMITS.outboxLeaseMs;
  const claimed: OutboxDueRow[] = [];
  for (const row of due.results) {
    const result = await db
      .prepare(
        "UPDATE outbox SET status = 'processing', lease_token = ?, lease_until = ? WHERE outbox_id = ? AND status = 'pending'",
      )
      .bind(leaseToken, leaseUntil, row.outbox_id)
      .run();
    if ((result.meta.changes ?? 0) > 0) claimed.push(row);
  }

  const byTopic = new Map<string, OutboxDueRow[]>();
  for (const row of claimed) {
    const list = byTopic.get(row.topic) ?? [];
    list.push(row);
    byTopic.set(row.topic, list);
  }

  let published = 0;
  let failed = 0;
  for (const [topic, rows] of byTopic) {
    const maxSeq = Math.max(...rows.map((row) => row.event_seq));
    const membership = rows.some((row) => MEMBERSHIP_EVENTS.has(row.event_type));
    try {
      await publishHint(env, topic, maxSeq, membership ? 'membership' : undefined);
      for (const batch of chunks(rows, 50)) {
        const placeholders = batch.map(() => '?').join(',');
        await db
          .prepare(
            `UPDATE outbox
                SET status = 'completed', completed_at = ?, attempts = attempts + 1,
                    lease_token = NULL, lease_until = NULL
              WHERE outbox_id IN (${placeholders}) AND lease_token = ?`,
          )
          .bind(now, ...batch.map((row) => row.outbox_id), leaseToken)
          .run();
      }
      published += rows.length;
    } catch (error) {
      console.error('agent-network outbox publish failed', topic, error);
      failed += rows.length;
      for (const row of rows) {
        await db
          .prepare(
            `UPDATE outbox
                SET status = 'pending', attempts = attempts + 1, next_attempt_at = ?,
                    lease_token = NULL, lease_until = NULL
              WHERE outbox_id = ? AND lease_token = ?`,
          )
          .bind(now + backoffMs(row.attempts), row.outbox_id, leaseToken)
          .run();
      }
    }
  }

  const oldest = await db
    .prepare("SELECT MIN(created_at) AS oldest FROM outbox WHERE status IN ('pending', 'processing')")
    .first<{ oldest: number | null }>();

  return {
    claimed: claimed.length,
    published,
    failed,
    oldestPendingAgeMs:
      oldest?.oldest === null || oldest?.oldest === undefined ? null : now - oldest.oldest,
  };
}
