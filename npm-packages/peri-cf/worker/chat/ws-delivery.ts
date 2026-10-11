import { z } from "zod";

/** Durable socket attachment owned by the chat Durable Object. Delivery credit rules live in the SDK. */

/** Attachment schema revision; a bump invalidates sockets restored from an older wire protocol. */
export const SYNC_ATTACHMENT_VERSION = 2;
export const MAX_OUTSTANDING_BYTES = 4 * 1024 * 1024;
export const MAX_OUTSTANDING_FRAMES = 16;
export const MAX_TOTAL_OUTSTANDING_BYTES = 8 * 1024 * 1024;
export const DELIVERY_TIMEOUT_MS = 10_000;
export const AUTH_TIMEOUT_MS = 3_000;

const safeCounter = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
export const attachmentSchema = z.object({
  version: z.literal(SYNC_ATTACHMENT_VERSION), chatId: z.string().uuid(),
  phase: z.enum(["auth", "ready", "closing"]), credential: z.string().optional(),
  nextDelivery: safeCounter.min(1), acknowledged: safeCounter,
  authDeadline: safeCounter,
  pending: z.array(z.object({ delivery: safeCounter.min(1), bytes: safeCounter.min(1), deadline: safeCounter })).max(MAX_OUTSTANDING_FRAMES),
}).strict().superRefine((value, context) => {
  let expected = value.acknowledged + 1;
  let bytes = 0;
  for (const pending of value.pending) {
    if (pending.delivery !== expected++) context.addIssue({ code: "custom", message: "Invalid delivery order" });
    bytes += pending.bytes;
  }
  if (expected !== value.nextDelivery || bytes > MAX_OUTSTANDING_BYTES ||
      (value.phase === "ready" && !/^[a-f0-9]{64}$/.test(value.credential ?? "")))
    context.addIssue({ code: "custom", message: "Invalid socket attachment" });
});

export type SocketAttachment = z.infer<typeof attachmentSchema>;
