import { fromBase64, toBase64 } from "lib0/buffer";
import { z } from "zod";
import type { DocSnapshot, DocStateVector, DocUpdate } from "../../@peri-sdk/src/sync";
import { chatDetailSchema, chatSchema } from "./chat";

export const MAX_SYNC_FRAME_CHARS = 16 * 1024 * 1024;
export const MAX_AUTH_FRAME_CHARS = 192 * 1024;
const MAX_DOCUMENT_BYTES = 4 * 1024 * 1024;
const MAX_VECTOR_BYTES = 64 * 1024;

function encodedBytes(maximum: number) {
  return z.string().max(4 * Math.ceil(maximum / 3))
    .regex(/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/)
    .transform((encoded, context) => {
      const bytes = fromBase64(encoded);
      if (bytes.byteLength > maximum || toBase64(bytes) !== encoded) {
        context.addIssue({ code: "custom", message: "Invalid document bytes" });
        return z.NEVER;
      }
      return bytes;
    });
}

const identity = { protocol: z.literal(2), generation: z.string().min(1).max(128) };
const sequence = z.number().int().min(0).max(Number.MAX_SAFE_INTEGER);
const documentBytes = encodedBytes(MAX_DOCUMENT_BYTES);
const vectorBytes = encodedBytes(MAX_VECTOR_BYTES);
const snapshotSchema = z.object({
  ...identity, sequence, mode: z.enum(["snapshot", "delta"]),
  chat: documentBytes, session: documentBytes,
});
const updateSchema = z.object({
  ...identity, sequence, chat: documentBytes.optional(), session: documentBytes.optional(),
}).refine((update) => update.chat !== undefined || update.session !== undefined);
const resumeSchema = z.object({ ...identity, chat: vectorBytes, session: vectorBytes });
const authSchema = z.object({
  type: z.literal("auth"), token: z.string().min(1).max(4096), resume: resumeSchema.optional(),
}).strict();
const syncSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("snapshot"), snapshot: snapshotSchema }).strict(),
  z.object({ type: z.literal("update"), update: updateSchema }).strict(),
]);

export const syncStateSchema = chatDetailSchema.extend({
  running: z.boolean(), executionBlocked: z.boolean(), error: z.string().max(512).optional(),
});
export const syncMetadataSchema = z.object({
  chat: chatSchema, running: z.boolean(), executionBlocked: z.boolean(), error: z.string().max(512).optional(),
  entryMetadata: z.record(z.string(), z.object({
    id: z.string(), createdAt: z.string(), error: z.string().max(512).optional(),
  })),
});
export type SyncMetadata = z.infer<typeof syncMetadataSchema>;
export type SyncState = z.infer<typeof syncStateSchema>;
export type SyncFrame = { type: "snapshot"; snapshot: DocSnapshot } | { type: "update"; update: DocUpdate };
export type AuthFrame = { type: "auth"; token: string; resume?: DocStateVector };

function parseFrame(raw: unknown, maximum: number): unknown {
  if (typeof raw !== "string" || raw.length > maximum) throw new Error("Invalid document sync frame");
  try { return JSON.parse(raw); }
  catch { throw new Error("Invalid document sync frame"); }
}

export function decodeSyncFrame(raw: unknown): SyncFrame {
  const parsed = syncSchema.safeParse(parseFrame(raw, MAX_SYNC_FRAME_CHARS));
  if (!parsed.success) throw new Error("Invalid document sync frame");
  return parsed.data;
}

export function decodeAuthFrame(raw: unknown): AuthFrame {
  const parsed = authSchema.safeParse(parseFrame(raw, MAX_AUTH_FRAME_CHARS));
  if (!parsed.success) throw new Error("Invalid document sync authentication");
  return parsed.data;
}

export function encodeSyncFrame(frame: SyncFrame): string {
  const source = frame.type === "snapshot" ? frame.snapshot : frame.update;
  const encoded = { ...source,
    ...(source.chat !== undefined ? { chat: toBase64(source.chat) } : {}),
    ...(source.session !== undefined ? { session: toBase64(source.session) } : {}),
  };
  const raw = JSON.stringify(frame.type === "snapshot"
    ? { type: "snapshot", snapshot: encoded } : { type: "update", update: encoded });
  decodeSyncFrame(raw);
  return raw;
}

export function encodeAuthFrame(frame: AuthFrame): string {
  const raw = JSON.stringify({ ...frame, ...(frame.resume ? {
    resume: { ...frame.resume, chat: toBase64(frame.resume.chat), session: toBase64(frame.resume.session) },
  } : {}) });
  decodeAuthFrame(raw);
  return raw;
}
