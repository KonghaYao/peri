import { agentResourcesSchema } from "../../shared/resources";
import { chatInstanceObservationSchema, type ChatInstanceObservation } from "../../shared/instances";
import { logError } from "../api/http";
import type { Chat } from "../../shared/chat";

export type InstanceObservationReader = (chatId: string) => Promise<unknown>;

// 每次刷新都会唤醒被观测的聊天 DO；并发上限保证一次刷新不会同时唤醒全部会话。
export const INSTANCE_OBSERVATION_CONCURRENCY = 6;

async function observeChat(chat: Chat, read: InstanceObservationReader): Promise<ChatInstanceObservation> {
  let payload: unknown;
  try {
    payload = await read(chat.id);
  } catch (error) {
    logError("Peri instance observation request failed", error, { sessionId: chat.id });
    return { chat, observation: null, failure: "host-unreachable" };
  }
  const parsed = agentResourcesSchema.safeParse(payload);
  if (!parsed.success) {
    logError("Peri instance observation response rejected", parsed.error, { sessionId: chat.id });
    return { chat, observation: null, failure: "invalid-observation" };
  }
  if (parsed.data.sessionId !== chat.id) {
    logError("Peri instance observation response rejected",
      new Error("Observed session does not match the requested chat"), { sessionId: chat.id });
    return { chat, observation: null, failure: "invalid-observation" };
  }
  return {
    chat,
    observation: {
      running: parsed.data.running,
      executionBlocked: parsed.data.executionBlocked,
      instance: parsed.data.instance,
    },
    failure: null,
  };
}

export async function collectChatInstances(chats: Chat[], read: InstanceObservationReader,
  concurrency = INSTANCE_OBSERVATION_CONCURRENCY): Promise<ChatInstanceObservation[]> {
  const observations = new Array<ChatInstanceObservation>(chats.length);
  let cursor = 0;
  const drain = async (): Promise<void> => {
    while (cursor < chats.length) {
      const index = cursor++;
      observations[index] = await observeChat(chats[index], read);
    }
  };
  const width = Math.max(1, Math.min(concurrency, chats.length));
  await Promise.all(Array.from({ length: width }, () => drain()));
  return observations;
}
