import { DeadlineError, withDeadline } from "./deadline";
import { chatIdSchema } from "../../shared/chat";
import { initializeTransport, WORKSPACE_CWD } from "./bootstrap";
import type { AcpTransport, Env, StartTransport } from "../types";

export async function createPeriSession(env: Env, title: string, startTransport: StartTransport,
  waitUntil: (promise: Promise<unknown>) => void): Promise<string> {
  let transport: AcpTransport | undefined;
  const starting = startTransport(env);
  try {
    try {
      transport = await withDeadline(starting, 20_000, "ACP host startup");
    } catch (error) {
      if (error instanceof DeadlineError)
        waitUntil(starting.then((late) => withDeadline(late.close(), 20_000, "Late ACP host close")).catch(() => {}));
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
  } finally {
    if (transport) await withDeadline(transport.close(), 20_000, "ACP host close after creation");
  }
}
