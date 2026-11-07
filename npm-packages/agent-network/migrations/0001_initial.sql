-- Agent Network v1 schema（M1/M2/M3）
-- 所有序号使用 D1/SQLite INTEGER；对外以十进制字符串返回。
-- 本文件是 schema 的单一事实源，事务实现见 src/storage/transactions。

CREATE TABLE actors (
  actor_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
  agent_session_id TEXT UNIQUE,
  name TEXT NOT NULL,
  metadata TEXT NOT NULL DEFAULT '{}',
  inbox_head_seq INTEGER NOT NULL DEFAULT 0,
  inbox_min_seq INTEGER NOT NULL DEFAULT 0,
  pending_count INTEGER NOT NULL DEFAULT 0 CHECK (pending_count >= 0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE credentials (
  credential_id TEXT PRIMARY KEY,
  actor_id TEXT NOT NULL REFERENCES actors(actor_id),
  device_id TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  revoked_at INTEGER,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_credentials_actor ON credentials(actor_id);

CREATE TABLE contacts (
  owner_actor_id TEXT NOT NULL,
  target_actor_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (owner_actor_id, target_actor_id)
);
CREATE INDEX idx_contacts_owner_created ON contacts(owner_actor_id, created_at DESC, target_actor_id);

CREATE TABLE rooms (
  room_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('direct', 'group')),
  owner_id TEXT,
  title TEXT NOT NULL DEFAULT '',
  direct_pair_key TEXT UNIQUE,
  head_seq INTEGER NOT NULL DEFAULT 0,
  events_min_seq INTEGER NOT NULL DEFAULT 0,
  member_seq INTEGER NOT NULL DEFAULT 0,
  last_message_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_rooms_updated ON rooms(updated_at DESC, room_id);

CREATE TABLE room_members (
  room_id TEXT NOT NULL,
  actor_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
  status TEXT NOT NULL CHECK (status IN ('active', 'left', 'removed')),
  joined_seq INTEGER NOT NULL DEFAULT 0,
  last_read_seq INTEGER NOT NULL DEFAULT 0,
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (room_id, actor_id)
);
CREATE INDEX idx_room_members_actor ON room_members(actor_id, status, room_id);

CREATE TABLE messages (
  message_id TEXT PRIMARY KEY,
  room_id TEXT NOT NULL,
  sender_id TEXT NOT NULL,
  client_message_id TEXT NOT NULL,
  created_seq INTEGER NOT NULL,
  body TEXT NOT NULL,
  body_bytes INTEGER NOT NULL,
  preview TEXT NOT NULL,
  reply_to TEXT,
  root_id TEXT,
  hop INTEGER NOT NULL DEFAULT 0,
  root_invocations_used INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  UNIQUE (sender_id, client_message_id)
);
CREATE INDEX idx_messages_room_seq ON messages(room_id, created_seq DESC);
CREATE INDEX idx_messages_root ON messages(root_id, created_seq);

CREATE TABLE deliveries (
  delivery_id TEXT PRIMARY KEY,
  recipient_id TEXT NOT NULL,
  message_id TEXT NOT NULL,
  room_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending', 'claimed', 'completed', 'failed', 'cancelled')),
  attempt INTEGER NOT NULL DEFAULT 0,
  claim_id TEXT,
  claimed_at INTEGER,
  progress_at INTEGER,
  result_message_id TEXT,
  error TEXT,
  created_seq INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (recipient_id, message_id)
);
CREATE INDEX idx_deliveries_recipient_state ON deliveries(recipient_id, state, created_seq);
CREATE INDEX idx_deliveries_message ON deliveries(message_id);
-- 每个 Agent 同时最多一条 claimed（§11.1）。
CREATE UNIQUE INDEX idx_deliveries_one_claimed ON deliveries(recipient_id) WHERE state = 'claimed';

CREATE TABLE events (
  event_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('room', 'inbox', 'actor')),
  scope_id TEXT NOT NULL,
  event_type TEXT NOT NULL,
  ref_id TEXT,
  metadata TEXT NOT NULL DEFAULT '{}',
  event_key TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_events_scope_seq ON events(scope_kind, scope_id, event_seq);

CREATE TABLE outbox (
  outbox_id TEXT PRIMARY KEY,
  topic TEXT NOT NULL,
  event_seq INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending', 'processing', 'completed')),
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL,
  lease_token TEXT,
  lease_until INTEGER,
  completed_at INTEGER,
  created_at INTEGER NOT NULL,
  UNIQUE (topic, event_seq)
);
CREATE INDEX idx_outbox_due ON outbox(status, next_attempt_at);

CREATE TABLE operations (
  actor_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  write_nonce TEXT NOT NULL,
  result_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (actor_id, request_id)
);
CREATE INDEX idx_operations_created ON operations(created_at);

CREATE TABLE recovery_tickets (
  ticket_hash TEXT PRIMARY KEY,
  actor_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  consumed_at INTEGER,
  redeemed_credential_id TEXT,
  redeem_request_id TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_recovery_expires ON recovery_tickets(expires_at);

-- 事务条件失败的显式闸门。
-- 每张表只接受 x=0；写入 1 会触发 CHECK 失败并回滚整个 D1 batch。
-- 表名同时是内部错误分类，不能合并成一张通用 guard 表。
CREATE TABLE guard_not_member (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_not_owner (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_queue_full (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_budget_exceeded (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_invalid_target (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_claim_conflict (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_stale_attempt (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_room_full (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_room_limit (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_actor_exists (x INTEGER NOT NULL CHECK (x = 0));
CREATE TABLE guard_recovery_invalid (x INTEGER NOT NULL CHECK (x = 0));
