import { BusinessError, type BusinessErrorCode } from '../../contracts/errors';
import { guardMessageMatch } from '../guards';
import type { OperationRow } from '../rows';
import { getOperation } from '../queries';

function errorText(error: unknown): string {
  if (error instanceof Error) return `${error.name}: ${error.message}`;
  return String(error);
}

const GUARD_CODES: Record<string, BusinessErrorCode> = {
  notMember: 'NOT_MEMBER',
  notOwner: 'NOT_OWNER',
  queueFull: 'QUEUE_FULL',
  budgetExceeded: 'CONFLICT',
  invalidTarget: 'INVALID_ARGUMENT',
  claimConflict: 'CLAIM_CONFLICT',
  staleAttempt: 'STALE_ATTEMPT',
  roomFull: 'ROOM_FULL',
  roomLimit: 'CONFLICT',
  actorExists: 'ACTOR_EXISTS',
  recoveryInvalid: 'RECOVERY_INVALID',
};

const GUARD_MESSAGES: Record<string, string> = {
  notMember: '当前身份不是该房间的有效成员',
  notOwner: '只有群 owner 可以执行该操作',
  queueFull: '目标 Agent 的待办队列已满',
  budgetExceeded: '协作深度或 root 累计派发预算已用尽',
  invalidTarget: '目标不是当前房间中的有效 Agent 成员',
  claimConflict: '该工作已被领取或状态已变化',
  staleAttempt: 'attempt 或 claim_id 已过期',
  roomFull: '群成员数量达到上限',
  roomLimit: '活跃房间数量达到上限',
  actorExists: '该 actor_id 已被其他凭据占用',
  recoveryInvalid: '恢复 ticket 无效、已过期或已被使用',
};

/** 把 D1 batch 抛出的约束错误映射成有类型的业务错误。 */
export function mapBatchError(error: unknown): BusinessError | null {
  const text = errorText(error);
  if (isOneClaimedConflict(error)) {
    return new BusinessError('CLAIM_CONFLICT', '当前已有领取中的工作，不能并行领取');
  }
  const guard = guardMessageMatch(text);
  if (guard) {
    return new BusinessError(GUARD_CODES[guard] ?? 'CONFLICT', GUARD_MESSAGES[guard] ?? '事务条件不满足');
  }
  return null;
}

export function isOperationConflict(error: unknown): boolean {
  return errorText(error).includes('UNIQUE constraint failed: operations.actor_id, operations.request_id');
}

export function isMessageConflict(error: unknown): boolean {
  return errorText(error).includes(
    'UNIQUE constraint failed: messages.sender_id, messages.client_message_id',
  );
}

export function isDeliveryConflict(error: unknown): boolean {
  return errorText(error).includes('UNIQUE constraint failed: deliveries.recipient_id, deliveries.message_id');
}

export function isOneClaimedConflict(error: unknown): boolean {
  const text = errorText(error);
  if (text.includes('idx_deliveries_one_claimed')) return true;
  // SQLite 只报告冲突列；单列 recipient_id 冲突只可能来自“每 Agent 至多一条 claimed”的部分唯一索引。
  return (
    text.includes('UNIQUE constraint failed: deliveries.recipient_id') &&
    !text.includes('deliveries.message_id')
  );
}

export function resolveOperation(row: OperationRow, payloadHash: string): unknown {
  if (row.payload_hash !== payloadHash) {
    throw new BusinessError('IDEMPOTENCY_CONFLICT', '同一 request_id 携带了不同的参数', {
      request_id: row.request_id,
    });
  }
  try {
    return JSON.parse(row.result_json) as unknown;
  } catch {
    throw new BusinessError('CONFLICT', '已保存的幂等结果不可读', { request_id: row.request_id });
  }
}

export interface IdempotentOptions {
  actorId: string;
  requestId: string;
  payloadHash: string;
  result: unknown;
  /** 返回完整 batch；第一条必须是 operations 插入。 */
  build: (resultJson: string) => D1PreparedStatement[];
  /** 非 operations 的唯一键冲突（例如长期消息唯一键）的恢复入口。 */
  onUniqueConflict?: (error: unknown) => Promise<unknown | undefined>;
}

/**
 * 统一幂等写入：
 * 1. 先读 operations 快速返回；
 * 2. batch 内以 operations 唯一键作为本次操作所有权；
 * 3. 约束冲突时读取已提交结果或明确冲突。
 */
export async function runIdempotent(
  db: D1Database,
  options: IdempotentOptions,
): Promise<unknown> {
  const existing = await getOperation(db, options.actorId, options.requestId);
  if (existing) return resolveOperation(existing, options.payloadHash);

  const resultJson = JSON.stringify(options.result);
  const statements = options.build(resultJson);
  try {
    await db.batch(statements);
    return options.result;
  } catch (error) {
    const business = mapBatchError(error);
    if (isOperationConflict(error)) {
      const committed = await getOperation(db, options.actorId, options.requestId);
      if (committed) return resolveOperation(committed, options.payloadHash);
      // 并发的同 request_id 操作失败并回滚：本请求重试一次。
      await db.batch(statements);
      return options.result;
    }
    if (options.onUniqueConflict) {
      const recovered = await options.onUniqueConflict(error);
      if (recovered !== undefined) return recovered;
    }
    if (business) throw business;
    throw error;
  }
}

export function operationsStatement(
  db: D1Database,
  actorId: string,
  requestId: string,
  payloadHash: string,
  resultJson: string,
  at: number,
): D1PreparedStatement {
  return db
    .prepare(
      `INSERT INTO operations (actor_id, request_id, payload_hash, write_nonce, result_json, created_at)
       VALUES (?, ?, ?, ?, ?, ?)`,
    )
    .bind(actorId, requestId, payloadHash, crypto.randomUUID(), resultJson, at);
}
