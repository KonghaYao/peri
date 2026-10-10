import { z } from "zod";
import { chatSchema } from "./chat";
import { wasmResourceSnapshotSchema } from "./resources";

export const instanceObservationFailures = ["host-unreachable", "invalid-observation"] as const;

export const chatInstanceObservationSchema = z.object({
  chat: chatSchema,
  observation: z.object({
    running: z.boolean(),
    executionBlocked: z.boolean(),
    instance: wasmResourceSnapshotSchema.nullable(),
  }).nullable(),
  failure: z.enum(instanceObservationFailures).nullable(),
});
export type ChatInstanceObservation = z.infer<typeof chatInstanceObservationSchema>;

export const instanceListSchema = z.object({
  generatedAt: z.string(),
  instances: z.array(chatInstanceObservationSchema),
});
export type InstanceList = z.infer<typeof instanceListSchema>;
