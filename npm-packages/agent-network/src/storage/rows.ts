export interface ActorRow {
  actor_id: string;
  kind: 'human' | 'agent';
  agent_session_id: string | null;
  name: string;
  metadata: string;
  inbox_head_seq: number;
  inbox_min_seq: number;
  pending_count: number;
  created_at: number;
  updated_at: number;
}

export interface CredentialRow {
  credential_id: string;
  actor_id: string;
  device_id: string;
  token_hash: string;
  revoked_at: number | null;
  created_at: number;
}

export interface ContactRow {
  owner_actor_id: string;
  target_actor_id: string;
  created_at: number;
}

export interface RoomRow {
  room_id: string;
  kind: 'direct' | 'group';
  owner_id: string | null;
  title: string;
  direct_pair_key: string | null;
  head_seq: number;
  events_min_seq: number;
  member_seq: number;
  last_message_id: string | null;
  created_at: number;
  updated_at: number;
}

export interface RoomMemberRow {
  room_id: string;
  actor_id: string;
  role: 'owner' | 'member';
  status: 'active' | 'left' | 'removed';
  joined_seq: number;
  last_read_seq: number;
  joined_at: number;
}

export interface MessageRow {
  message_id: string;
  room_id: string;
  sender_id: string;
  client_message_id: string;
  created_seq: number;
  body: string;
  body_bytes: number;
  preview: string;
  reply_to: string | null;
  root_id: string | null;
  hop: number;
  root_invocations_used: number;
  created_at: number;
}

export interface DeliveryRow {
  delivery_id: string;
  recipient_id: string;
  message_id: string;
  room_id: string;
  state: 'pending' | 'claimed' | 'completed' | 'failed' | 'cancelled';
  attempt: number;
  claim_id: string | null;
  claimed_at: number | null;
  progress_at: number | null;
  result_message_id: string | null;
  error: string | null;
  created_seq: number;
  created_at: number;
  updated_at: number;
}

export interface EventRow {
  event_seq: number;
  scope_kind: 'room' | 'inbox' | 'actor';
  scope_id: string;
  event_type: string;
  ref_id: string | null;
  metadata: string;
  event_key: string;
  created_at: number;
}

export interface OutboxRow {
  outbox_id: string;
  topic: string;
  event_seq: number;
  status: 'pending' | 'processing' | 'completed';
  attempts: number;
  next_attempt_at: number;
  lease_token: string | null;
  lease_until: number | null;
  completed_at: number | null;
  created_at: number;
}

export interface OperationRow {
  actor_id: string;
  request_id: string;
  payload_hash: string;
  write_nonce: string;
  result_json: string;
  created_at: number;
}

export interface RecoveryTicketRow {
  ticket_hash: string;
  actor_id: string;
  expires_at: number;
  consumed_at: number | null;
  redeemed_credential_id: string | null;
  redeem_request_id: string | null;
  created_at: number;
}
