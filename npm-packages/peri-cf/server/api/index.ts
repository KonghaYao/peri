import { Hono } from "hono";
import { streamSSE } from "hono/streaming";
import { ChatCommandError, ChatNotFoundError } from "../../core/agent";
import { chatCommandSchema, chatIdSchema, createChatBodySchema } from "../../core/shared";
import type { publicServerInfo } from "../config";
import type { ChatManagerPort, ChatSubscription } from "../agent";

export interface ApiDeps {
  /** API 只依赖组合根端口，不接触 Store、宿主或实例。 */
  manager: ChatManagerPort;
  info: ReturnType<typeof publicServerInfo>;
}

const NOT_FOUND = "Chat not found";
const HEARTBEAT_MS = 15_000;
const SNAPSHOT_INTERVAL_MS = 50;

function isNotFound(error: unknown): boolean {
  return error instanceof ChatNotFoundError;
}

/**
 * 服务端 HTTP 面：会话清单/创建/读取、命令提交与投影 SSE。
 * 路由不接触 WASM 宿主；会话生命周期由 `ChatService` 的驻留实例管理。
 */
export function createApi(deps: ApiDeps): Hono {
  const app = new Hono();

  app.get("/api/info", (c) => c.json({ ok: true, ...deps.info }));

  app.get("/api/chats", async (c) => c.json({ chats: await deps.manager.list() }));

  app.post("/api/chats", async (c) => {
    const body = await c.req.json().catch(() => null);
    const parsed = createChatBodySchema.safeParse(body ?? {});
    if (!parsed.success) return c.json({ error: "Invalid chat title" }, 400);
    return c.json({ chat: await deps.manager.create(parsed.data.title) }, 201);
  });

  app.get("/api/chats/:id", async (c) => {
    const id = chatIdSchema.safeParse(c.req.param("id"));
    if (!id.success) return c.json({ error: "Invalid chat id" }, 400);
    try {
      return c.json({ state: await deps.manager.read(id.data) });
    } catch (error) {
      if (isNotFound(error)) return c.json({ error: NOT_FOUND }, 404);
      throw error;
    }
  });

  app.post("/api/chats/:id/commands", async (c) => {
    const id = chatIdSchema.safeParse(c.req.param("id"));
    if (!id.success) return c.json({ error: "Invalid chat id" }, 400);
    const body = await c.req.json().catch(() => null);
    const command = chatCommandSchema.safeParse(body);
    if (!command.success)
      return c.json({ ok: false, code: "invalid_command", message: "命令格式无效", disposition: "rejected" }, 400);
    try {
      const payload = await deps.manager.run(id.data, command.data);
      return c.json({ ok: true, ...(payload ? { payload } : {}) });
    } catch (error) {
      if (error instanceof ChatCommandError)
        return c.json({
          ok: false, code: error.code, message: error.message, disposition: error.disposition,
        }, error.code === "not_found" ? 404 : 409);
      if (isNotFound(error))
        return c.json({ ok: false, code: "not_found", message: NOT_FOUND, disposition: "rejected" }, 404);
      console.error("Peri chat command failed", { chatId: id.data, op: command.data.op }, error);
      return c.json({
        ok: false, code: "internal_error", message: "命令执行失败；诊断已记录", disposition: "unknown",
      }, 500);
    }
  });

  app.get("/api/chats/:id/events", async (c) => {
    const id = chatIdSchema.safeParse(c.req.param("id"));
    if (!id.success) return c.json({ error: "Invalid chat id" }, 400);
    // 先验证会话可复原，避免把错误塞进已经开始的 SSE 流。
    try {
      await deps.manager.read(id.data);
    } catch (error) {
      if (isNotFound(error)) return c.json({ error: NOT_FOUND }, 404);
      throw error;
    }

    return streamSSE(c, async (stream) => {
      let closed = false;
      let initialized = false;
      let latest: Awaited<ReturnType<ChatManagerPort["read"]>> | undefined;
      let timer: ReturnType<typeof setTimeout> | undefined;
      let heartbeat: ReturnType<typeof setInterval> | undefined;
      let subscription: ChatSubscription | undefined;
      let writing = Promise.resolve();
      let settle!: () => void;
      const done = new Promise<void>((resolve) => { settle = resolve; });

      // 幂等清理：取消订阅后再结算等待，避免断开后继续持有 watcher。
      const cleanup = (): void => {
        if (closed) return;
        closed = true;
        if (timer !== undefined) clearTimeout(timer);
        if (heartbeat !== undefined) clearInterval(heartbeat);
        subscription?.close();
        settle();
      };
      stream.onAbort(cleanup);
      // Hono 只对注册之后的取消调用 onAbort；请求信号已中止时也要立即清理。
      if (stream.aborted || c.req.raw.signal.aborted) cleanup();
      else c.req.raw.signal.addEventListener("abort", cleanup, { once: true });

      const write = (state: NonNullable<typeof latest>): Promise<void> => {
        writing = writing.then(async () => {
          if (closed) return;
          await stream.writeSSE({ event: "state", data: JSON.stringify(state) });
        }).catch(() => { closed = true; });
        return writing;
      };

      const schedule = (): void => {
        if (closed || !initialized || timer !== undefined || !latest) return;
        timer = setTimeout(() => {
          timer = undefined;
          if (closed || !latest) return;
          void write(latest);
        }, SNAPSHOT_INTERVAL_MS);
      };

      subscription = await deps.manager.watch(id.data, (state) => {
        latest = state;
        schedule();
      });
      if (closed) {
        subscription.close();
        return;
      }
      latest = subscription.initial;

      await stream.writeSSE({ event: "state", data: JSON.stringify(subscription.initial) });
      if (closed) return;
      initialized = true;
      if (latest !== subscription.initial) schedule();

      heartbeat = setInterval(() => {
        if (closed) return;
        writing = writing.then(async () => {
          if (closed) return;
          await stream.writeSSE({ event: "ping", data: "" });
        }).catch(() => { closed = true; });
      }, HEARTBEAT_MS);

      await done;
    });
  });

  app.onError((error, c) => {
    console.error("Peri server request failed", error);
    return c.json({ error: "Internal server error" }, 500);
  });

  return app;
}
