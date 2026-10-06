import { authenticatedApp } from "../api/app";
import { publicError } from "../api/http";
import { decodeAuthFrame, encodeSyncFrame, MAX_AUTH_FRAME_CHARS } from "../../shared/sync";
import { HTTPException } from "hono/http-exception";
import type { Env, SessionState } from "../types";
import type { SessionDocSync } from "../sdk";
import type { WebSocketPair as WorkerWebSocketPair } from "@cloudflare/workers-types";

declare const WebSocketPair: typeof WorkerWebSocketPair;

export interface SyncSocket {
  readonly readyState?: number;
  readonly bufferedAmount?: number;
  accept(): void;
  send(message: string): void;
  close(code: number, reason: string): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "close" | "error", listener: () => void): void;
}

export interface SocketUpgrade {
  socket: SyncSocket;
  response: Response;
}

export type UpgradeSocket = () => SocketUpgrade;

export class SocketAdmission {
  private count = 0;

  acquire(): (() => void) | undefined {
    if (this.count >= 16) return undefined;
    this.count++;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.count--;
    };
  }
}

export const upgradeSocket: UpgradeSocket = () => {
  const pair = new WebSocketPair();
  const init = { status: 101, webSocket: pair[0] };
  return { socket: pair[1], response: new Response(null, init) };
};

const authentication = authenticatedApp();
authentication.get("/", () => new Response(null, { status: 204 }));

export function openSync(request: Request, env: Env, state: SessionState, load: () => Promise<SessionDocSync>,
  upgrade: UpgradeSocket, admission: SocketAdmission): Response {
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket")
    return Response.json({ error: "WebSocket upgrade required" }, { status: 426 });
  if (new URL(request.url).searchParams.has("token"))
    return Response.json({ error: "URL credentials are not supported" }, { status: 400 });
  const release = admission.acquire();
  if (!release) return Response.json({ error: "Too many sync connections" }, { status: 429 });
  let upgraded;
  try { upgraded = upgrade(); } catch (error) { release(); throw error; }
  const { socket, response } = upgraded;
  let phase: "auth" | "loading" | "ready" | "closed" = "auth";
  const isClosed = () => phase === "closed";
  let unsubscribe: (() => void) | undefined;
  const cleanup = () => {
    phase = "closed";
    clearTimeout(timer);
    unsubscribe?.();
    release();
  };
  const close = (code: number, reason: string) => {
    cleanup();
    socket.close(code, reason);
  };
  const timer = setTimeout(() => close(4401, "Authentication timed out"), 3_000);
  const send = (message: string) => {
    if (phase === "closed" || (socket.readyState !== undefined && socket.readyState !== 1))
      throw new Error("Sync socket is closed");
    if ((socket.bufferedAmount ?? 0) > 4 * 1024 * 1024) throw new Error("Sync socket is too slow");
    socket.send(message);
  };
  socket.accept();
  socket.addEventListener("close", cleanup);
  socket.addEventListener("error", cleanup);
  socket.addEventListener("message", (event) => {
    if (phase !== "auth") {
      if (phase !== "closed") close(4403, "Client document writes are not allowed");
      return;
    }
    phase = "loading";
    const work = (async () => {
      let frame;
      if (typeof event.data === "string" && event.data.length > MAX_AUTH_FRAME_CHARS) {
        close(1009, "Authentication frame is too large"); return;
      }
      try { frame = decodeAuthFrame(event.data); }
      catch { close(4401, "Invalid authentication frame"); return; }
      let authorized: Response;
      try {
        authorized = await authentication.fetch(new Request("https://sync.invalid/", {
          headers: { Authorization: `Bearer ${frame.token}` },
        }), env);
      } catch { close(4401, "Invalid authentication credentials"); return; }
      if (isClosed()) return;
      if (!authorized.ok) { close(authorized.status === 503 ? 1011 : 4401, "Unauthorized"); return; }
      clearTimeout(timer);
      try {
        const sync = await load();
        if (isClosed()) return;
        const subscribed = sync.subscribe((update) => send(encodeSyncFrame({ type: "update", update })), {
          resume: frame.resume, onError: (error) => {
            console.error("Peri sync subscriber failed", { message: publicError(error, env) });
            close(1011, "Sync subscriber failed");
          },
        });
        unsubscribe = subscribed.unsubscribe;
        send(encodeSyncFrame({ type: "snapshot", snapshot: subscribed.snapshot }));
        phase = "ready";
      } catch (error) {
        console.error("Peri sync failed", { message: publicError(error, env) });
        close(error instanceof HTTPException && error.status === 404 ? 4404 : 1011, "Sync unavailable");
      }
    })();
    state.waitUntil(work);
  });
  return response;
}
