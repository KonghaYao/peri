import { DeadlineError, PeriWasmHostStartupError, withDeadline } from "../sdk";
import { HTTPException } from "hono/http-exception";
import { chatIdSchema } from "../../shared/chat";
import { initializeTransport, WORKSPACE_CWD } from "./bootstrap";
import type { AcpTransport, Env, StartTransport } from "../types";
import { WasmResources } from "../wasm/resources";
import { WasmCapacityError } from "../wasm/budget";
import { diagnosticMessage, logError } from "../api/http";

export async function createPeriSession(env: Env, title: string, startTransport: StartTransport,
  waitUntil: (promise: Promise<unknown>) => void): Promise<string> {
  let transport: AcpTransport | undefined;
  const controller = new AbortController();
  const resources = new WasmResources();
  const starting = Promise.resolve().then(() => startTransport(env, resources, controller.signal));
  let failure: unknown;
  const cleanupStartup = async (error: unknown): Promise<void> => {
    if (!(error instanceof PeriWasmHostStartupError)) throw error;
    const outcome = await error.cleanup;
    if (!outcome.confirmed)
      throw new AggregateError([error, outcome.error], "Creation startup cleanup is unconfirmed");
  };
  const observeCleanup = (promise: Promise<unknown>): void => {
    waitUntil(promise.catch((error) => {
      logError("Peri session creation cleanup failed", error, { instanceId: resources.instanceId, generationId: transport?.generationId });
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
      if (error instanceof PeriWasmHostStartupError && error.cause instanceof WasmCapacityError)
        throw new HTTPException(503, {
          message: "WASM isolate capacity is exhausted", cause: error,
          res: Response.json({ error: diagnosticMessage(error) }, {
            status: 503, headers: { "Retry-After": "1", "Cache-Control": "no-store" },
          }),
        });
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
    logError("Peri session creation failed", error, { instanceId: resources.instanceId, generationId: transport?.generationId });
    throw error;
  } finally {
    if (transport) {
      try { await withDeadline(transport.close(), 20_000, "ACP host close after creation"); }
      catch (error) {
        logError("Peri session creation host close failed", error, { instanceId: resources.instanceId, generationId: transport.generationId });
        if (failure) throw new AggregateError([failure, error], "Creation failed and host close is unconfirmed");
        throw error;
      }
    }
  }
}
