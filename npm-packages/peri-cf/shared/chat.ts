import { z } from "zod";

export const messageStatuses = ["running", "completed", "cancelled", "error"] as const;
export const chatIdSchema = z.guid().transform((id) => id.toLowerCase());
export const chatSchema = z.object({ id: z.string(), title: z.string(), updatedAt: z.string() });
export const messageSchema = z.object({
  id: z.string(), role: z.enum(["user", "assistant"]), content: z.string(), createdAt: z.string(),
  status: z.enum(messageStatuses).optional(), error: z.string().optional(),
});
export const persistedMessageSchema = messageSchema.extend({ status: z.enum(messageStatuses) });
export const chatListSchema = z.object({ chats: z.array(chatSchema) });
export const chatDetailSchema = z.object({ chat: chatSchema, messages: z.array(messageSchema) });
export const createChatBodySchema = z.object({
  title: z.string().max(200).refine((title) => title.trim().length > 0).transform((title) => title.trim()).default("New chat"),
});
export const sendMessageBodySchema = z.object({
  content: z.string().max(32_768).refine((content) => content.trim().length > 0),
});

export type Chat = z.infer<typeof chatSchema>;
export type Message = z.infer<typeof messageSchema>;
export type PersistedMessage = z.infer<typeof persistedMessageSchema>;
export type ChatDetail = z.infer<typeof chatDetailSchema>;
