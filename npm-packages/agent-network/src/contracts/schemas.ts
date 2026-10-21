import { z } from 'zod';

import { LIMITS } from './limits';

const id = (max = 128) => z.string().min(1).max(max);
const requestId = z.string().min(1).max(128);
const cursor = z.string().min(1).max(2048).optional();
const seq = z.union([z.number().int().nonnegative(), z.string().regex(/^\d{1,18}$/u)]);

export const registerSchema = z.object({
  request_id: requestId,
  kind: z.enum(['human', 'agent']),
  actor_id: id().optional(),
  agent_session_id: id(256).optional(),
  device_id: id(256).optional(),
  name: z.string().min(1).max(96),
  metadata: z.record(z.string(), z.unknown()).optional(),
});

export const meSchema = z.object({});

export const profileUpdateSchema = z.object({
  request_id: requestId,
  name: z.string().min(1).max(96).optional(),
  metadata: z.record(z.string(), z.unknown()).optional(),
});

export const recoveryCreateSchema = z.object({ request_id: requestId });

export const recoveryRedeemSchema = z.object({
  request_id: requestId,
  ticket: z.string().min(16).max(512),
  device_id: id(256),
});

export const contactsListSchema = z.object({ cursor, limit: z.number().int().min(1).max(100).optional() });

export const contactAddSchema = z.object({
  request_id: requestId,
  actor_id: id().optional(),
  agent_session_id: id(256).optional(),
});

export const contactRemoveSchema = z.object({ request_id: requestId, actor_id: id() });

export const roomCreateSchema = z.object({
  request_id: requestId,
  kind: z.enum(['direct', 'group']),
  members: z.array(id()).min(1).max(LIMITS.memberChangeMax),
  title: z.string().max(160).optional(),
});

export const roomsListSchema = z.object({ cursor, limit: z.number().int().min(1).max(100).optional() });

export const roomGetSchema = z.object({ room_id: id() });

export const roomMembersUpdateSchema = z.object({
  request_id: requestId,
  room_id: id(),
  add: z.array(id()).max(LIMITS.memberChangeMax).optional(),
  remove: z.array(id()).max(LIMITS.memberChangeMax).optional(),
  transfer_owner_to: id().optional(),
});

export const roomLeaveSchema = z.object({ request_id: requestId, room_id: id() });

export const messageSendSchema = z.object({
  request_id: requestId,
  client_message_id: id(128),
  room_id: id(),
  body: z.string().min(1).max(LIMITS.messageBodyBytes),
  invoke: z.array(id()).max(LIMITS.invokeMax).optional(),
  reply_to: id().optional(),
  claim: z
    .object({ delivery_id: id(), attempt: z.number().int().nonnegative(), claim_id: id() })
    .optional(),
});

export const historyListSchema = z.object({
  room_id: id(),
  direction: z.enum(['before', 'after']).optional(),
  cursor,
  limit: z.number().int().min(1).max(LIMITS.historyPageMax).optional(),
});

export const messageReadSchema = z.object({
  message_id: id(),
  chunk_cursor: z.string().max(64).optional(),
});

export const roomMarkReadSchema = z.object({ room_id: id(), through_seq: seq });

export const syncSchema = z.object({
  scope: z.string().min(1).max(256),
  cursor,
  limit: z.number().int().min(1).max(LIMITS.syncPageMax).optional(),
});

export const inboxListSchema = z.object({
  state: z.enum(['pending', 'claimed', 'completed', 'failed', 'cancelled', 'all']).optional(),
  cursor,
  limit: z.number().int().min(1).max(LIMITS.inboxPageMax).optional(),
});

export const deliveryClaimSchema = z.object({ request_id: requestId, delivery_id: id() });

export const deliveryReportSchema = z.object({
  request_id: requestId,
  delivery_id: id(),
  attempt: z.number().int().nonnegative(),
  claim_id: id(),
  status: z.string().min(1).max(280),
});

export const deliveryCompleteSchema = z.object({
  request_id: requestId,
  delivery_id: id(),
  attempt: z.number().int().nonnegative(),
  claim_id: id(),
  final_body: z.string().min(1).max(LIMITS.messageBodyBytes).optional(),
  client_message_id: id(128).optional(),
});

export const deliveryFailSchema = z.object({
  request_id: requestId,
  delivery_id: id(),
  attempt: z.number().int().nonnegative(),
  claim_id: id(),
  error: z.string().min(1).max(1024),
});

export const deliveryCancelSchema = z.object({
  request_id: requestId,
  delivery_id: id(),
  reason: z.string().min(1).max(512),
});

export const deliveryRetrySchema = z.object({
  request_id: requestId,
  delivery_id: id(),
  expected_attempt: z.number().int().nonnegative(),
});

export type RegisterInput = z.infer<typeof registerSchema>;
export type ProfileUpdateInput = z.infer<typeof profileUpdateSchema>;
export type RecoveryRedeemInput = z.infer<typeof recoveryRedeemSchema>;
export type ContactsListInput = z.infer<typeof contactsListSchema>;
export type ContactAddInput = z.infer<typeof contactAddSchema>;
export type ContactRemoveInput = z.infer<typeof contactRemoveSchema>;
export type RoomCreateInput = z.infer<typeof roomCreateSchema>;
export type RoomsListInput = z.infer<typeof roomsListSchema>;
export type RoomMembersUpdateInput = z.infer<typeof roomMembersUpdateSchema>;
export type RoomLeaveInput = z.infer<typeof roomLeaveSchema>;
export type MessageSendInput = z.infer<typeof messageSendSchema>;
export type HistoryListInput = z.infer<typeof historyListSchema>;
export type MessageReadInput = z.infer<typeof messageReadSchema>;
export type RoomMarkReadInput = z.infer<typeof roomMarkReadSchema>;
export type SyncInput = z.infer<typeof syncSchema>;
export type InboxListInput = z.infer<typeof inboxListSchema>;
export type DeliveryClaimInput = z.infer<typeof deliveryClaimSchema>;
export type DeliveryReportInput = z.infer<typeof deliveryReportSchema>;
export type DeliveryCompleteInput = z.infer<typeof deliveryCompleteSchema>;
export type DeliveryFailInput = z.infer<typeof deliveryFailSchema>;
export type DeliveryCancelInput = z.infer<typeof deliveryCancelSchema>;
export type DeliveryRetryInput = z.infer<typeof deliveryRetrySchema>;
