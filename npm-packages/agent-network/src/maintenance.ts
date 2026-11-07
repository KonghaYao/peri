import { LIMITS } from './contracts/limits';
import { nowMs } from './storage/db';
import type { Env } from './env';

interface ExpiredScope {
  scope_kind: 'room' | 'inbox';
  scope_id: string;
  max_seq: number;
}

/**
 * 分批清理 event / operations / outbox / recovery ticket。
 *
 * 删除 event 前先把对应 scope 的有效游标下界推进到本批最大 event_seq，
 * 使过期 cursor 明确返回 reset_required，而不是假装历史仍完整。
 */
export async function maintain(env: Env): Promise<void> {
  const db = env.DB;
  const now = nowMs();

  const scopes = await db
    .prepare(
      `SELECT scope_kind, scope_id, MAX(event_seq) AS max_seq
         FROM events
        WHERE created_at < ?
        GROUP BY scope_kind, scope_id
        LIMIT 20`,
    )
    .bind(now - LIMITS.eventsRetentionMs)
    .all<ExpiredScope>();

  for (const scope of scopes.results) {
    if (scope.scope_kind === 'room') {
      await db
        .prepare('UPDATE rooms SET events_min_seq = MAX(events_min_seq, ?) WHERE room_id = ?')
        .bind(scope.max_seq, scope.scope_id)
        .run();
    } else {
      await db
        .prepare('UPDATE actors SET inbox_min_seq = MAX(inbox_min_seq, ?) WHERE actor_id = ?')
        .bind(scope.max_seq, scope.scope_id)
        .run();
    }
    await db
      .prepare('DELETE FROM events WHERE scope_kind = ? AND scope_id = ? AND created_at < ?')
      .bind(scope.scope_kind, scope.scope_id, now - LIMITS.eventsRetentionMs)
      .run();
  }

  await db
    .prepare(
      'DELETE FROM operations WHERE rowid IN (SELECT rowid FROM operations WHERE created_at < ? LIMIT 100)',
    )
    .bind(now - LIMITS.operationsRetentionMs)
    .run();

  await db
    .prepare(
      "DELETE FROM outbox WHERE rowid IN (SELECT rowid FROM outbox WHERE status = 'completed' AND completed_at < ? LIMIT 100)",
    )
    .bind(now - LIMITS.outboxRetentionMs)
    .run();

  await db
    .prepare(
      'DELETE FROM recovery_tickets WHERE rowid IN (SELECT rowid FROM recovery_tickets WHERE expires_at < ? LIMIT 100)',
    )
    .bind(now - 24 * 60 * 60_000)
    .run();
}
