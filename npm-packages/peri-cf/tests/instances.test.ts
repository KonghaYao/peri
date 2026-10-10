import { describe, expect, test } from "bun:test";
import { fetchApi } from "../worker/api/router";
import { ChatRegistry } from "../worker/chat/registry";
import { collectChatInstances } from "../worker/instances/collect";
import { instanceListSchema } from "../shared/instances";
import { runtimeFixture } from "./helpers/runtime";
import type { ChatRepository } from "../worker/types";

/**
 * 无 DO 的实例观测：`/api/instances` 只 peek 本 isolate 的运行期，绝不唤醒会话（不触发重放）。
 * 未驻留本 isolate 的会话如实报告为不可达，不伪造成「空闲实例」。
 */

const token = "trusted-app-token";

function repository(): ChatRepository {
  return {
    async list() { return []; },
    async get() { return null; },
    async create() { throw new Error("Unexpected create"); },
  };
}

describe("per-chat WASM instance observation", () => {
  test("只报告本 isolate 已驻留的实例，未驻留的会话按不可达降级（不唤醒、不报错整页）", async () => {
    const app = runtimeFixture({ resources: true, chunks: [], blocked: true });
    const resident = await app.create("驻留中的会话");
    const unreached = await app.create("未驻留的会话");
    // 让第一个会话在本 isolate 内跑起来（有活跃执行 + 在连 peer）。
    const { socket } = await app.attached(resident.id);
    expect((await app.send(socket, { type: "command", id: "p1", op: "prompt", content: "run" })).ok).toBe(true);
    await (await app.runTransport()).promptStarted;

    const response = await fetchApi(new Request("https://app.invalid/api/instances", {
      headers: { Authorization: `Bearer ${app.env.APP_AUTH_TOKEN}` },
    }), app.env, app.repository, { runtimes: app.registry, socketHost: () => app.host,
      startTransport: async () => { throw new Error("观测不得启动宿主"); } });
    expect(response.status).toBe(200);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    const payload = instanceListSchema.parse(await response.json());
    expect(payload.instances.map((entry) => entry.chat.title)).toEqual(["驻留中的会话", "未驻留的会话"]);
    const [live, absent] = payload.instances;
    expect(live.failure).toBeNull();
    expect(live.observation?.running).toBe(true);
    expect(live.observation?.executionBlocked).toBe(false);
    expect(live.observation?.instance).toMatchObject({
      phase: "ready", generationId: null,
      memory: { allocatedBytes: 65_536, pages: 1, peakObservedBytes: 65_536, observation: "live" },
      cpu: { supported: false },
    });
    expect(absent).toMatchObject({ observation: null, failure: "host-unreachable" });
    // 观测不得为第二个会话启动宿主（两个宿主分别来自复原与执行轮次）。
    expect(app.registry.peek(unreached.id)).toBeUndefined();
    expect(app.transports).toHaveLength(2);
    await app.send(socket, { type: "command", id: "c1", op: "cancel" });
    await app.drain();
  });

  test("requires the shared bearer token before observing any runtime", async () => {
    const registry = new ChatRegistry();
    let peeks = 0;
    const spy = { runtimeFor: registry.runtimeFor.bind(registry), peek(chatId: string) { peeks++; return registry.peek(chatId); } };
    const response = await fetchApi(new Request("https://app.invalid/api/instances"), { APP_AUTH_TOKEN: token },
      repository(), { runtimes: spy, startTransport: async () => { throw new Error("Unexpected host startup"); } });
    expect(response.status).toBe(401);
    expect(peeks).toBe(0);
  });

  test("rejects non-GET methods on the instance collection", async () => {
    const response = await fetchApi(new Request("https://app.invalid/api/instances", {
      method: "POST", headers: { Authorization: `Bearer ${token}` },
    }), { APP_AUTH_TOKEN: token }, repository(), { startTransport: async () => { throw new Error("Unexpected"); } });
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
