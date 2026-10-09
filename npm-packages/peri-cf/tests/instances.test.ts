import { describe, expect, test } from "bun:test";
import { fetchApi } from "../worker/api/router";
import { collectChatInstances } from "../worker/instances/collect";
import { instanceListSchema } from "../shared/instances";
import { WasmResources } from "../worker/wasm/resources";
import { memoryInstance } from "./helpers/wasm";
import type { Chat, ChatRepository, Env } from "../worker/types";

const token = "trusted-app-token";
const chats: Chat[] = [
  { id: "00000000-0000-4000-8000-000000000001", title: "运行中的会话", updatedAt: "2026-10-09T10:00:00.000Z" },
  { id: "00000000-0000-4000-8000-000000000002", title: "读取失败的会话", updatedAt: "2026-10-09T09:00:00.000Z" },
  { id: "00000000-0000-4000-8000-000000000003", title: "响应不可信的会话", updatedAt: "2026-10-09T08:00:00.000Z" },
];

function liveResources() {
  const resources = new WasmResources();
  resources.attach(memoryInstance());
  resources.markStartup("module-ready", 3.4);
  resources.markStartup("native-ready", 71.6);
  resources.ready("generation-1");
  return resources;
}

function observationEnv(onFetch?: (chatId: string, request: Request) => void): Env {
  return {
    APP_AUTH_TOKEN: token,
    PERI_STORAGE_URL: "libsql://fixture.invalid",
    PERI_STORAGE_TOKEN: "private-storage-token",
    CHAT_SESSIONS: {
      idFromName(name) { return String(name); },
      get(identity) {
        const chatId = String(identity);
        return {
          async fetch(request: Request): Promise<Response> {
            onFetch?.(chatId, request);
            if (chatId.endsWith("02")) return new Response("unavailable", { status: 503 });
            if (chatId.endsWith("03"))
              return Response.json({ sessionId: chats[0].id, running: false, executionBlocked: false, instance: null });
            const resources = liveResources();
            return Response.json({
              sessionId: chatId, running: true, executionBlocked: false, instance: resources.snapshot(),
            });
          },
        };
      },
    },
  };
}

function repository(): ChatRepository {
  return {
    async list() { return structuredClone(chats); },
    async get(id) { return structuredClone(chats.find((chat) => chat.id === id) ?? null); },
    async create() { throw new Error("Unexpected create"); },
  };
}

describe("per-chat WASM instance observation", () => {
  test("aggregates chat order, keeps live observations and degrades single chats without failing the page", async () => {
    const requests: { chatId: string; path: string; authorization: string | null }[] = [];
    const env = observationEnv((chatId, request) => {
      requests.push({ chatId, path: new URL(request.url).pathname, authorization: request.headers.get("Authorization") });
    });
    const response = await fetchApi(new Request("https://app.invalid/api/instances", {
      headers: { Authorization: `Bearer ${token}` },
    }), env, repository());
    expect(response.status).toBe(200);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    const payload = instanceListSchema.parse(await response.json());
    expect(payload.instances.map((entry) => entry.chat.title))
      .toEqual(["运行中的会话", "读取失败的会话", "响应不可信的会话"]);
    const [live, unreachable, untrusted] = payload.instances;
    expect(live.failure).toBeNull();
    expect(live.observation?.running).toBe(true);
    expect(live.observation?.instance).toMatchObject({
      phase: "ready", generationId: "generation-1", endedAt: null,
      startup: { moduleReadyMs: 3, nativeReadyMs: 72, acpReadyMs: null },
      memory: { allocatedBytes: 65_536, pages: 1, peakObservedBytes: 65_536, observation: "live" },
      cpu: { supported: false },
    });
    expect(unreachable).toMatchObject({ observation: null, failure: "host-unreachable" });
    expect(untrusted).toMatchObject({ observation: null, failure: "invalid-observation" });
    expect(requests.map((entry) => [entry.chatId, entry.path, entry.authorization])).toEqual([
      [chats[0].id, `/api/chats/${chats[0].id}/resources`, `Bearer ${token}`],
      [chats[1].id, `/api/chats/${chats[1].id}/resources`, `Bearer ${token}`],
      [chats[2].id, `/api/chats/${chats[2].id}/resources`, `Bearer ${token}`],
    ]);
  });

  test("requires the shared bearer token before observing any DO", async () => {
    let lookups = 0;
    const env = observationEnv(() => { lookups++; });
    env.CHAT_SESSIONS = {
      idFromName(name) { lookups++; return String(name); },
      get() { throw new Error("Unexpected DO access"); },
    };
    const response = await fetchApi(new Request("https://app.invalid/api/instances"), env, repository());
    expect(response.status).toBe(401);
    expect(lookups).toBe(0);
  });

  test("rejects non-GET methods on the instance collection", async () => {
    const response = await fetchApi(new Request("https://app.invalid/api/instances", {
      method: "POST", headers: { Authorization: `Bearer ${token}` },
    }), observationEnv(), repository());
    expect(response.status).toBe(405);
  });

  test("bounds concurrent observation requests and returns empty for no chats", async () => {
    let inFlight = 0;
    let peak = 0;
    const total = 12;
    const many = Array.from({ length: total }, (_value, index) => ({
      id: `00000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
      title: `chat-${index}`, updatedAt: "2026-10-09T10:00:00.000Z",
    }));
    const observations = await collectChatInstances(many, async (chatId) => {
      inFlight++; peak = Math.max(peak, inFlight);
      await Promise.resolve();
      inFlight--;
      return { sessionId: chatId, running: false, executionBlocked: false, instance: null };
    });
    expect(observations).toHaveLength(total);
    expect(peak).toBeGreaterThan(1);
    expect(peak).toBeLessThanOrEqual(6);
    expect(observations.every((entry) => entry.failure === null)).toBe(true);
    expect(await collectChatInstances([], async () => { throw new Error("Unexpected read"); })).toEqual([]);
  });
});
