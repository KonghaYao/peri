/**
 * 事务条件闸门。
 *
 * 每个 guard 表只接受 x=0；`INSERT ... SELECT 1 WHERE <违反条件>` 命中时
 * 触发 CHECK 失败，使整个 D1 batch 回滚。表名同时作为内部错误分类。
 */
export const GUARD_TABLES = {
  notMember: 'guard_not_member',
  notOwner: 'guard_not_owner',
  queueFull: 'guard_queue_full',
  budgetExceeded: 'guard_budget_exceeded',
  invalidTarget: 'guard_invalid_target',
  claimConflict: 'guard_claim_conflict',
  staleAttempt: 'guard_stale_attempt',
  roomFull: 'guard_room_full',
  roomLimit: 'guard_room_limit',
  actorExists: 'guard_actor_exists',
  recoveryInvalid: 'guard_recovery_invalid',
} as const;

export type GuardName = keyof typeof GUARD_TABLES;

export function guardSql(name: GuardName, violation: string): string {
  return `INSERT INTO ${GUARD_TABLES[name]} (x) SELECT 1 WHERE (${violation})`;
}

export function guardMessageMatch(table: string): GuardName | null {
  for (const [name, guardTable] of Object.entries(GUARD_TABLES)) {
    if (table.includes(guardTable)) return name as GuardName;
  }
  return null;
}
