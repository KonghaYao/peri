-- 为条件闸门命名约束，使 D1 的 CHECK 错误文本包含闸门身份，便于 mapBatchError 分类。
-- 闸门表只会在整批回滚的失败语句中写入，正常提交后始终为空，因此可安全重建。
DROP TABLE guard_not_member;
CREATE TABLE guard_not_member (x INTEGER NOT NULL, CONSTRAINT guard_not_member CHECK (x = 0));

DROP TABLE guard_not_owner;
CREATE TABLE guard_not_owner (x INTEGER NOT NULL, CONSTRAINT guard_not_owner CHECK (x = 0));

DROP TABLE guard_queue_full;
CREATE TABLE guard_queue_full (x INTEGER NOT NULL, CONSTRAINT guard_queue_full CHECK (x = 0));

DROP TABLE guard_budget_exceeded;
CREATE TABLE guard_budget_exceeded (x INTEGER NOT NULL, CONSTRAINT guard_budget_exceeded CHECK (x = 0));

DROP TABLE guard_invalid_target;
CREATE TABLE guard_invalid_target (x INTEGER NOT NULL, CONSTRAINT guard_invalid_target CHECK (x = 0));

DROP TABLE guard_claim_conflict;
CREATE TABLE guard_claim_conflict (x INTEGER NOT NULL, CONSTRAINT guard_claim_conflict CHECK (x = 0));

DROP TABLE guard_stale_attempt;
CREATE TABLE guard_stale_attempt (x INTEGER NOT NULL, CONSTRAINT guard_stale_attempt CHECK (x = 0));

DROP TABLE guard_room_full;
CREATE TABLE guard_room_full (x INTEGER NOT NULL, CONSTRAINT guard_room_full CHECK (x = 0));

DROP TABLE guard_room_limit;
CREATE TABLE guard_room_limit (x INTEGER NOT NULL, CONSTRAINT guard_room_limit CHECK (x = 0));

DROP TABLE guard_actor_exists;
CREATE TABLE guard_actor_exists (x INTEGER NOT NULL, CONSTRAINT guard_actor_exists CHECK (x = 0));

DROP TABLE guard_recovery_invalid;
CREATE TABLE guard_recovery_invalid (x INTEGER NOT NULL, CONSTRAINT guard_recovery_invalid CHECK (x = 0));
