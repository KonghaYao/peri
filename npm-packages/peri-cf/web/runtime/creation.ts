import { DeadlineError, PeriWasmHostStartupError, withDeadline, type WasmAcpTransport } from "@peri-code/sdk/wasm";
import { chatIdSchema } from "../../shared/chat";
import { initializeTransport, WORKSPACE_CWD } from "./bootstrap";
import { defaultDefer } from "./defer";
import { logError } from "./logging";
import type { Defer, StartHost } from "./types";

/**
 * 创建会话：启动一个一次性宿主完成 `session/new` 与 `session/rename`，随后关闭。
 * 创建阶段没有 prompt，也就不会有交互：反向请求保持最小默认拒绝（与执行轮次内的接线不同）。
 */
export async function createPeriSession(title: string, startHost: StartHost,
  defer: Defer = defaultDefer): Promise<string> {
  let transport: WasmAcpTransport | undefined;
  const controller = new AbortController();
  const starting = Promise.resolve().then(() => startHost({ signal: controller.signal }));
  let failure: unknown;
  const cleanupStartup = async (error: unknown): Promise<void> => {
    if (!(error instanceof PeriWasmHostStartupError)) throw error;
    const outcome = await error.cleanup;
    if (!outcome.confirmed)
      throw new AggregateError([error, outcome.error], "Creation startup cleanup is unconfirmed");
  };
  const observeCleanup = (promise: Promise<unknown>): void => {
    defer(promise.catch((error) => {
      logError("Peri session creation cleanup failed", error, { generationId: transport?.generationId });
      throw error;
    }));
  };
  try {
    try {
      transport = await withDeadline(starting, 20_000, "ACP host startup");
    } catch (error) {
      if (error instanceof DeadlineError) {
        controller.abort(error);
        observeCleanup(starting.then((late) => withDeadline(late.close(), 20_000, "Late ACP host close"), cleanupStartup));
      } else if (error instanceof PeriWasmHostStartupError) observeCleanup(cleanupStartup(error));
      throw error;
    }
    transport.setRequestHandler((method) => {
      if (method === "session/request_permission") return { outcome: { outcome: "cancelled" } };
      throw new Error("ACP client capability is not authorized");
    });
    await initializeTransport(transport);
    const created = await withDeadline(transport.request<{ sessionId: string }>("session/new", {
      cwd: WORKSPACE_CWD, mcpServers: [],
    }), 20_000, "ACP session/new");
    if (!chatIdSchema.safeParse(created?.sessionId).success)
      throw new Error("Invalid ACP session/new identity");
    const renamed = await withDeadline(transport.request<{ sessionId: string; title: string }>("session/rename", {
      sessionId: created.sessionId, title,
    }), 20_000, "ACP session/rename");
    if (renamed?.sessionId !== created.sessionId || renamed.title !== title)
      throw new Error("Invalid ACP session/rename response");
    return created.sessionId;
  } catch (error) {
    failure = error;
    logError("Peri session creation failed", error, { generationId: transport?.generationId });
    throw error;
  } finally {
    if (transport) {
      try { await withDeadline(transport.close(), 20_000, "ACP host close after creation"); }
      catch (error) {
        logError("Peri session creation host close failed", error, { generationId: transport.generationId });
        if (failure) throw new AggregateError([failure, error], "Creation failed and host close is unconfirmed");
        throw error;
      }
    }
  }
}
