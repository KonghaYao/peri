import { z } from "zod";
import { chatDetailSchema, chatSchema } from "./chat";

/** App-owned presentation DTOs. Document frames and delivery credit come from the SDK sync wire. */

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
