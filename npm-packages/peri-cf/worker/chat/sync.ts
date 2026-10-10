import { authenticatedApp } from "../api/app";
import { logError } from "../api/http";
import { decodeAckFrame, decodeAuthFrame, frameBytes, MAX_AUTH_FRAME_BYTES, type SyncFramePayload,
  acknowledgeDelivery, credentialFingerprint, reserveDeliveryFrame } from "../sdk";
import { HTTPException } from "hono/http-exception";
import type { Env, SessionState } from "../types";
import type { SessionDocSync, DocStateVector } from "../sdk";
import type { WebSocketPair as WorkerWebSocketPair } from "@cloudflare/workers-types";
import {
  attachmentSchema, AUTH_TIMEOUT_MS, DELIVERY_TIMEOUT_MS, MAX_OUTSTANDING_BYTES, MAX_OUTSTANDING_FRAMES,
  MAX_TOTAL_OUTSTANDING_BYTES, SYNC_ATTACHMENT_VERSION, type SocketAttachment,
} from "./ws-delivery";

declare const WebSocketPair: typeof WorkerWebSocketPair;

export interface SyncSocket {
  readonly readyState?: number;
  accept(): void;
  send(message: Uint8Array): void;
  close(code: number, reason: string): void;
  serializeAttachment?(value: unknown): void;
  deserializeAttachment?(): unknown;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "close" | "error", listener: () => void): void;
}

export interface HibernatingSessionState extends SessionState {
  acceptWebSocket?(socket: SyncSocket): void;
  getWebSockets?(): SyncSocket[];
}

export interface SocketUpgrade { socket: SyncSocket; response: Response }
export type UpgradeSocket = () => SocketUpgrade;
export const upgradeSocket: UpgradeSocket = () => {
  const pair = new WebSocketPair();
  const init = { status: 101, webSocket: pair[0] };
  return { socket: pair[1], response: new Response(null, init) };
};

interface Peer {
  socket: SyncSocket;
  attachment: SocketAttachment;
  unsubscribe?: () => void;
  timer?: ReturnType<typeof setTimeout>;
  loading?: Promise<void>;
  closed: boolean;
}

const authentication = authenticatedApp();
authentication.get("/", () => new Response(null, { status: 204 }));

export class ChatSockets {
  private readonly peers = new Map<SyncSocket, Peer>();
  private readonly hibernating: boolean;

  constructor(private readonly env: Env, private readonly state: HibernatingSessionState,
    private readonly load: (chatId: string) => Promise<SessionDocSync>, private readonly upgrade: UpgradeSocket) {
    this.hibernating = typeof state.acceptWebSocket === "function" && typeof state.getWebSockets === "function";
    for (const socket of state.getWebSockets?.() ?? []) {
      const restored = socket.deserializeAttachment?.();
      const attachment = attachmentSchema.safeParse(restored);
      if (!attachment.success) {
        // A version 1 attachment belongs to the removed Base64 wire protocol; reload instead of silently downgrading.
        const version = (restored as { version?: unknown } | undefined)?.version;
        socket.close(version === 1 ? 1008 : 1011, version === 1
          ? "Sync protocol upgraded; reload required" : "Invalid restored connection");
        continue;
      }
      if (this.peers.size >= 16) { socket.close(1011, "Too many sync connections"); continue; }
      const peer: Peer = { socket, attachment: attachment.data, closed: attachment.data.phase === "closing" };
      this.peers.set(socket, peer);
      this.arm(peer);
    }
  }

  open(id: string, request: Request): Response {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket")
      return Response.json({ error: "WebSocket upgrade required" }, { status: 426 });
    if (new URL(request.url).searchParams.has("token"))
      return Response.json({ error: "URL credentials are not supported" }, { status: 400 });
    if (this.peers.size >= 16) return Response.json({ error: "Too many sync connections" }, { status: 429 });
    const { socket, response } = this.upgrade();
    const peer: Peer = { socket, closed: false, attachment: {
      version: SYNC_ATTACHMENT_VERSION, chatId: id, phase: "auth", nextDelivery: 1, acknowledged: 0,
      pending: [], authDeadline: Date.now() + AUTH_TIMEOUT_MS,
    } };
    this.peers.set(socket, peer);
    try {
      if (this.hibernating) {
        if (!socket.serializeAttachment || !socket.deserializeAttachment)
          throw new Error("Hibernating socket attachments are unavailable");
        this.state.acceptWebSocket!(socket);
      } else {
        socket.accept();
        socket.addEventListener("message", (event) => this.state.waitUntil(this.message(socket, event.data)));
        socket.addEventListener("close", () => this.closed(socket));
        socket.addEventListener("error", () => this.error(socket, new Error("WebSocket transport error")));
      }
      this.persist(peer);
      this.arm(peer);
    } catch (error) { this.closed(socket); logError("Peri sync socket admission failed", error); throw error; }
    return response;
  }

  async restoreSubscriptions(): Promise<void> {
    await Promise.all(Array.from(this.peers.values(), (peer) => peer.attachment.phase === "ready" ? this.subscribe(peer) : undefined));
  }

  async message(socket: SyncSocket, data: unknown): Promise<void> {
    const peer = this.peers.get(socket);
    if (!peer || peer.closed) return;
    if (peer.attachment.phase === "ready") {
      try {
        const frame = decodeAckFrame(data);
        acknowledgeDelivery(peer.attachment, frame.delivery);
        this.persist(peer);
        this.arm(peer);
      } catch (error) {
        logError("Peri sync acknowledgement rejected", error);
        this.close(peer, 4403, "Invalid delivery acknowledgement; document writes are not allowed");
        return;
      }
      await this.restoreSubscriptions();
      return;
    }
    if (peer.loading) { this.close(peer, 4403, "Authentication already in progress"); return; }
    peer.loading = this.authenticate(peer, data);
    await peer.loading;
    peer.loading = undefined;
  }

  closed(socket: SyncSocket): void {
    const peer = this.peers.get(socket);
    if (!peer) return;
    peer.closed = true;
    clearTimeout(peer.timer);
    peer.unsubscribe?.();
    this.peers.delete(socket);
  }

  error(socket: SyncSocket, error: unknown): void {
    logError("Peri sync socket failed", error);
    const peer = this.peers.get(socket);
    if (peer) this.close(peer, 1011, "Sync transport failed");
  }

  private async authenticate(peer: Peer, data: unknown): Promise<void> {
    const bytes = frameBytes(data);
    if (!bytes) { this.close(peer, 4401, "Invalid authentication frame"); return; }
    if (bytes.byteLength > MAX_AUTH_FRAME_BYTES) { this.close(peer, 1009, "Authentication frame is too large"); return; }
    let frame;
    try { frame = decodeAuthFrame(bytes); }
    catch (error) { logError("Peri sync authentication rejected", error); this.close(peer, 4401, "Invalid authentication frame"); return; }
    try {
      let authorized: Response;
      try {
        authorized = await authentication.fetch(new Request("https://sync.invalid/", {
          headers: { Authorization: `Bearer ${frame.token}` },
        }), this.env);
      } catch (error) {
        logError("Peri sync credentials rejected", error);
        this.close(peer, 4401, "Invalid authentication credentials"); return;
      }
      if (peer.closed) return;
      if (!authorized.ok) { this.close(peer, authorized.status === 503 ? 1011 : 4401, "Unauthorized"); return; }
      peer.attachment.credential = await credentialFingerprint(frame.token);
      if (peer.closed) return;
      peer.attachment.phase = "ready";
      this.persist(peer);
      this.arm(peer);
      await this.attach(peer, frame.resume);
    } catch (error) {
      logError("Peri sync authentication failed", error);
      this.close(peer, error instanceof HTTPException && error.status === 404 ? 4404 : 1011, "Sync unavailable");
    }
  }

  private async subscribe(peer: Peer): Promise<void> {
    if (peer.closed || peer.unsubscribe) return;
    if (peer.loading) { await peer.loading; return; }
    peer.loading = (async () => {
      try {
        const expected = this.env.APP_AUTH_TOKEN;
        if (!expected || await credentialFingerprint(expected) !== peer.attachment.credential) {
          this.close(peer, 4401, "Authentication configuration changed"); return;
        }
        await this.attach(peer);
      } catch (error) { logError("Peri sync restore failed", error); this.close(peer, 1011, "Sync restore failed"); }
    })();
    await peer.loading;
    peer.loading = undefined;
  }

  private async attach(peer: Peer, resume?: DocStateVector): Promise<void> {
    const sync = await this.load(peer.attachment.chatId);
    if (peer.closed) return;
    const subscription = sync.subscribe((update) => {
      this.send(peer, { type: "update", update });
    }, { resume, onError: (error) => { logError("Peri sync subscriber failed", error); this.close(peer, 1011, "Sync subscriber failed"); } });
    peer.unsubscribe = subscription.unsubscribe;
    this.send(peer, { type: "snapshot", snapshot: subscription.snapshot });
  }

  private send(peer: Peer, frame: SyncFramePayload): void {
    if (peer.closed || (peer.socket.readyState !== undefined && peer.socket.readyState !== 1))
      throw new Error("Sync socket is closed");
    const encoded = reserveDeliveryFrame(peer.attachment, frame, { maxFrames: MAX_OUTSTANDING_FRAMES,
      maxBytes: MAX_OUTSTANDING_BYTES, timeoutMs: DELIVERY_TIMEOUT_MS });
    const total = Array.from(this.peers.values()).reduce((sum, connection) =>
      sum + connection.attachment.pending.reduce((bytes, pending) => bytes + pending.bytes, 0), 0);
    if (total > MAX_TOTAL_OUTSTANDING_BYTES) throw new Error("Session sync delivery budget exhausted");
    this.persist(peer);
    this.arm(peer);
    peer.socket.send(encoded);
  }

  private persist(peer: Peer): void { peer.socket.serializeAttachment?.(peer.attachment); }

  private arm(peer: Peer): void {
    clearTimeout(peer.timer);
    const deadline = peer.attachment.phase === "auth" ? peer.attachment.authDeadline : peer.attachment.pending[0]?.deadline;
    if (deadline === undefined || peer.closed) return;
    peer.timer = setTimeout(() => this.close(peer, peer.attachment.phase === "auth" ? 4401 : 1013,
      peer.attachment.phase === "auth" ? "Authentication timed out" : "Delivery acknowledgement timed out"), Math.max(0, deadline - Date.now()));
  }

  private close(peer: Peer, code: number, reason: string): void {
    if (peer.closed) return;
    peer.closed = true;
    peer.attachment.phase = "closing";
    clearTimeout(peer.timer);
    peer.unsubscribe?.();
    try {
      this.persist(peer);
      if (peer.socket.readyState !== 3) peer.socket.close(code, reason);
      if (peer.socket.readyState === 3) this.closed(peer.socket);
    }
    catch (error) { logError("Peri sync socket close failed", error); throw error; }
  }
}
