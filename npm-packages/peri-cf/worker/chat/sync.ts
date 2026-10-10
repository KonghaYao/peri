import { authenticatedApp } from "../api/app";
import { logError } from "../api/http";
import { decodeAckFrame, decodeAuthFrame, frameBytes, MAX_AUTH_FRAME_BYTES, type SyncFramePayload,
  acknowledgeDelivery, credentialFingerprint, reserveDeliveryFrame } from "../sdk";
import { HTTPException } from "hono/http-exception";
import type { Defer, Env } from "../types";
import type { SessionDocSync, DocStateVector, DeliveryLedger } from "../sdk";
import type { WebSocketPair as WorkerWebSocketPair } from "@cloudflare/workers-types";
import {
  AUTH_TIMEOUT_MS, DELIVERY_TIMEOUT_MS, MAX_OUTSTANDING_BYTES, MAX_OUTSTANDING_FRAMES,
  MAX_SYNC_CONNECTIONS, MAX_TOTAL_OUTSTANDING_BYTES,
} from "./ws-delivery";
import { encodeControlFrame, parseControlFrame, type ChatCommand, type ChatCommandResult } from "../../shared/sync";

declare const WebSocketPair: typeof WorkerWebSocketPair;

/**
 * 无 DO 的会话同步连接：连接状态只在 isolate 内存中，随 isolate 回收一起消失；客户端重连后由
 * `session/load` 重放重建展示态（`ChatRuntime.hydrate`），不依赖任何持久连接状态。
 *
 * **帧分流是硬约束**：文本帧只承载应用控制帧，二进制帧只承载 SDK 文档同步帧。
 * 服务端在这条连接上**只发送一种文本帧：`command_result`**（`encodeControlFrame`，唯一发送点见
 * `control()`）；客户端按控制帧严格解析文本帧，出现任何其它文本帧即判协议错误并终止同步。
 * 后续新增文本帧类型必须同步修改前端解析契约（`shared/sync.ts`）。
 */

/**
 * 入站二进制帧的平台差异：CF 文档投递 `ArrayBuffer`，代理/兼容路径（如 Miniflare 的 Node 桥）
 * 投递 `Blob`。两者都必须按二进制处理，否则鉴权帧会被误判成非法帧（4401）。
 */
async function inboundFrame(data: unknown): Promise<unknown> {
  if (typeof Blob !== "undefined" && data instanceof Blob) return new Uint8Array(await data.arrayBuffer());
  return data;
}

export interface SyncSocket {
  readonly readyState?: number;
  accept(): void;
  /** 二进制帧 = SDK 文档同步；文本帧 = 应用控制帧。 */
  send(message: Uint8Array | string): void;
  close(code: number, reason: string): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
  addEventListener(type: "close" | "error", listener: () => void): void;
}

/**
 * 平台 seam：升级、保活与时钟都不由业务决定。
 * - `upgrade()`：CF 用 `new WebSocketPair()`；测试注入内存 socket。
 * - `keepAlive()`：CF 用**升级请求**的 `ExecutionContext.waitUntil`；测试记录 promise。
 * - `now()`：投递超时判定用，测试可注入虚拟时钟。
 */
export interface SocketHost {
  upgrade(): { socket: SyncSocket; response: Response };
  keepAlive(promise: Promise<unknown>): void;
  now(): number;
}

/** 会话侧能力：连接层只做连接与帧编解码，命令与复原都归 runtime。 */
export interface SocketRuntimeBridge {
  /** 鉴权成功后按需复原展示态（重放）；失败必须抛出，由连接层按 404/1011 处理。 */
  hydrate(defer?: Defer): Promise<void>;
  /** 复原后的文档同步通道（attach 时按信用推送 snapshot/delta）。 */
  sync(): SessionDocSync;
  /** 控制帧命令；失败以 `command_result` 的错误码返回，不抛给调用方。 */
  run(command: ChatCommand, defer?: Defer): Promise<ChatCommandResult>;
  /** 当前在途执行 promise；用于把后台执行登记到平台保活。 */
  inFlight(): Promise<unknown> | undefined;
}

const cfUpgrade = (): { socket: SyncSocket; response: Response } => {
  const pair = new WebSocketPair();
  const init = { status: 101, webSocket: pair[0] };
  return { socket: pair[1] as unknown as SyncSocket, response: new Response(null, init) };
};

/** CF 平台实现：升级请求的 ExecutionContext 负责保活。 */
export function cfSocketHost(execution: { waitUntil(promise: Promise<unknown>): void }): SocketHost {
  return { upgrade: cfUpgrade, keepAlive: (promise) => execution.waitUntil(promise), now: () => Date.now() };
}

/** 兜底 host（测试/无平台上下文时）：保活只记录拒绝，不阻塞业务。 */
export const detachedSocketHost: SocketHost = {
  upgrade: cfUpgrade,
  keepAlive: (promise) => { void promise.catch(() => {}); },
  now: () => Date.now(),
};

interface Peer {
  socket: SyncSocket;
  host: SocketHost;
  phase: "auth" | "ready";
  credential?: string;
  ledger: DeliveryLedger;
  /**
   * 本连接自己的待发送队列。workerd 只允许在**创建该 socket 的那个请求上下文**里写它
   * （"Cannot perform I/O on behalf of a different request."），因此任何上下文产生的帧都只入队，
   * 由本连接自己的 pump（在它自己的上下文里排定的定时器）按节奏真正写出。
   */
  queue: (Uint8Array | string)[];
  /** 待执行的关闭；只在 pump（本上下文）里落 socket.close。 */
  closing?: { code: number; reason: string };
  authDeadline: number;
  pumpTimer?: ReturnType<typeof setTimeout>;
  loading?: Promise<void>;
  closed: boolean;
  unsubscribe?: () => void;
}

/**
 * 连接自己的泵节奏：有排队帧时快，空闲时慢（同上下文定时器，成本与连接数同阶）。
 * 空闲拍决定「别的上下文产生的更新」的最长展示延迟，是平台 I/O 约束换来的代价。
 */
export const PUMP_ACTIVE_MS = 5;
export const PUMP_IDLE_MS = 50;

/** 一个连接可排队但尚未写出的字节上限（与投递信用同量级，防止跨上下文堆积无界）。 */
export const MAX_QUEUED_BYTES = MAX_OUTSTANDING_BYTES;

const encoder = new TextEncoder();
const queuedBytes = (frame: Uint8Array | string): number =>
  typeof frame === "string" ? encoder.encode(frame).byteLength : frame.byteLength;

const authentication = authenticatedApp();
authentication.get("/", () => new Response(null, { status: 204 }));

export class ChatSockets {
  private readonly peers = new Map<SyncSocket, Peer>();

  constructor(private readonly chatId: string, private readonly env: Env,
    private readonly bridge: SocketRuntimeBridge) {}

  get size(): number { return this.peers.size; }

  /** 会话有在连 peer 时不得被 isolate 内注册表回收。 */
  get connected(): boolean { return this.peers.size > 0; }

  open(request: Request, host: SocketHost = detachedSocketHost): Response {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket")
      return Response.json({ error: "WebSocket upgrade required" }, { status: 426 });
    if (new URL(request.url).searchParams.has("token"))
      return Response.json({ error: "URL credentials are not supported" }, { status: 400 });
    if (this.peers.size >= MAX_SYNC_CONNECTIONS)
      return Response.json({ error: "Too many sync connections" }, { status: 429 });
    const { socket, response } = host.upgrade();
    const peer: Peer = { socket, host, phase: "auth", closed: false, queue: [],
      authDeadline: host.now() + AUTH_TIMEOUT_MS, ledger: { nextDelivery: 1, acknowledged: 0, pending: [] } };
    this.peers.set(socket, peer);
    try {
      socket.accept();
      socket.addEventListener("message", (event) => this.defer(peer, this.message(peer, event.data)));
      socket.addEventListener("close", () => this.closed(peer));
      socket.addEventListener("error", () => this.error(peer, new Error("WebSocket transport error")));
      this.start(peer);
    } catch (error) { this.closed(peer); logError("Peri sync socket admission failed", error); throw error; }
    return response;
  }

  async message(peer: Peer, raw: unknown): Promise<void> {
    if (peer.closed) return;
    const data = await inboundFrame(raw);
    if (peer.closed) return;
    if (peer.phase === "ready") {
      if (typeof data === "string") { await this.control(peer, data); return; }
      try {
        const frame = decodeAckFrame(data);
        acknowledgeDelivery(peer.ledger, frame.delivery);
      } catch (error) {
        logError("Peri sync acknowledgement rejected", error);
        this.close(peer, 4403, "Invalid delivery acknowledgement; document writes are not allowed");
        return;
      }
      return;
    }
    if (peer.loading) { this.close(peer, 4403, "Authentication already in progress"); return; }
    peer.loading = this.authenticate(peer, data);
    await peer.loading;
    peer.loading = undefined;
  }

  closed(peer: Peer): void {
    if (!this.peers.has(peer.socket)) return;
    peer.closed = true;
    clearTimeout(peer.pumpTimer);
    peer.pumpTimer = undefined;
    peer.queue = [];
    peer.unsubscribe?.();
    this.peers.delete(peer.socket);
  }

  error(peer: Peer, error: unknown): void {
    logError("Peri sync socket failed", error);
    this.close(peer, 1011, "Sync transport failed");
  }

  /** 关闭全部连接（isolate 内回收该会话运行期时使用）。 */
  closeAll(code: number, reason: string): void {
    for (const peer of [...this.peers.values()]) this.close(peer, code, reason);
  }

  private async control(peer: Peer, data: string): Promise<void> {
    let command: ChatCommand | null;
    try { command = parseControlFrame(data); }
    catch (error) {
      // 控制帧不合法时无法回执（没有可信 id）：按协议违规关闭连接。
      logError("Peri sync control frame rejected", error);
      this.close(peer, 4403, "Invalid control frame");
      return;
    }
    if (!command) { this.close(peer, 4403, "Invalid control frame"); return; }
    const pending = this.bridge.run(command, (promise) => peer.host.keepAlive(promise));
    // 无 DO 后没有 per-message waitUntil：长命令用升级请求的执行上下文保活。
    // 长时执行是否被平台回收只能在部署端验证（残余风险，本仓库无法证明）。
    peer.host.keepAlive(pending.catch(() => {}));
    const result = await pending;
    if (peer.closed) return;
    // prompt 被接受后真正长时的是后台执行；同一个保活入口负责它的存续。
    const inFlight = this.bridge.inFlight();
    if (inFlight) peer.host.keepAlive(inFlight.catch(() => {}));
    this.enqueue(peer, encodeControlFrame(result));
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
      peer.credential = await credentialFingerprint(frame.token);
      if (peer.closed) return;
      peer.phase = "ready";
      await this.attach(peer, frame.resume);
    } catch (error) {
      logError("Peri sync authentication failed", error);
      this.close(peer, error instanceof HTTPException && error.status === 404 ? 4404 : 1011, "Sync unavailable");
    }
  }

  private async attach(peer: Peer, resume?: DocStateVector): Promise<void> {
    await this.bridge.hydrate((promise) => peer.host.keepAlive(promise));
    if (peer.closed) return;
    const sync = this.bridge.sync();
    const subscription = sync.subscribe((update) => {
      this.send(peer, { type: "update", update });
    }, { resume, onError: (error) => { logError("Peri sync subscriber failed", error); this.close(peer, 1011, "Sync subscriber failed"); } });
    peer.unsubscribe = subscription.unsubscribe;
    this.send(peer, { type: "snapshot", snapshot: subscription.snapshot });
  }

  /** 文档同步帧入队并预留投递信用；真正的 socket 写入只发生在 pump（本连接上下文）。 */
  private send(peer: Peer, frame: SyncFramePayload): void {
    if (peer.closed || peer.closing) throw new Error("Sync socket is closed");
    const encoded = reserveDeliveryFrame(peer.ledger, frame, { maxFrames: MAX_OUTSTANDING_FRAMES,
      maxBytes: MAX_OUTSTANDING_BYTES, timeoutMs: DELIVERY_TIMEOUT_MS, now: () => peer.host.now() });
    const total = Array.from(this.peers.values()).reduce((sum, connection) =>
      sum + connection.ledger.pending.reduce((bytes, pending) => bytes + pending.bytes, 0), 0);
    if (total > MAX_TOTAL_OUTSTANDING_BYTES) throw new Error("Session sync delivery budget exhausted");
    this.enqueue(peer, encoded);
  }

  /** 文本帧（控制回执）与二进制帧（文档同步）都只入队，写出统一发生在 pump。 */
  private enqueue(peer: Peer, frame: Uint8Array | string): void {
    if (peer.closed || peer.closing) return;
    const queued = peer.queue.reduce((sum, pending) => sum + queuedBytes(pending), 0);
    if (queued + queuedBytes(frame) > MAX_QUEUED_BYTES) {
      this.requestClose(peer, 1011, "Sync queue budget exhausted");
      return;
    }
    peer.queue.push(frame);
  }

  /** 连接建立时启动属于自己的泵链：定时器与 socket 同属一个请求上下文（workerd 的 I/O 约束）。 */
  private start(peer: Peer): void {
    peer.pumpTimer = setTimeout(() => { peer.pumpTimer = undefined; this.pump(peer); }, 0);
  }

  /** 只在本连接的上下文执行：到期检查、落关闭、写出队列，然后排下一拍。 */
  private pump(peer: Peer): void {
    if (peer.closed) return;
    try {
      if (!peer.closing) this.enforceDeadlines(peer);
      // 自己的上下文里主动推送共享文档的待发布增量：不依赖 SDK 同步通道的内部定时器
      // （那个定时器属于创建运行期的请求上下文，可能早已结束）。
      if (peer.phase === "ready" && !peer.closing) this.bridge.sync().flush();
      if (peer.closing) {
        this.performClose(peer, peer.closing);
        // 平台尚未确认关闭时保持慢拍复查，避免连接名额泄漏。
        if (!peer.closed) peer.pumpTimer = setTimeout(() => { peer.pumpTimer = undefined; this.pump(peer); }, PUMP_IDLE_MS);
        return;
      }
      while (peer.queue.length) {
        if (peer.socket.readyState !== undefined && peer.socket.readyState !== 1) break;
        peer.socket.send(peer.queue.shift()!);
      }
    } catch (error) {
      logError("Peri sync socket pump failed", error);
      peer.closing ??= { code: 1011, reason: "Sync transport failed" };
      try { this.performClose(peer, peer.closing); } catch (closeError) { logError("Peri sync socket close failed", closeError); }
      return;
    }
    if (peer.closed) return;
    peer.pumpTimer = setTimeout(() => { peer.pumpTimer = undefined; this.pump(peer); },
      peer.queue.length ? PUMP_ACTIVE_MS : PUMP_IDLE_MS);
  }

  /** 宽限期由泵按拍检查，不再各自排定时器（避免在别的上下文里触发 socket I/O）。 */
  private enforceDeadlines(peer: Peer): void {
    if (peer.phase === "auth") {
      if (peer.host.now() >= peer.authDeadline) this.requestClose(peer, 4401, "Authentication timed out");
      return;
    }
    const deadline = peer.ledger.pending[0]?.deadline;
    if (deadline !== undefined && peer.host.now() >= deadline)
      this.requestClose(peer, 1013, "Delivery acknowledgement timed out");
  }

  private performClose(peer: Peer, closing: { code: number; reason: string }): void {
    if (peer.closed) return;
    try {
      peer.unsubscribe?.();
      if (peer.socket.readyState !== 3) peer.socket.close(closing.code, closing.reason);
      if (peer.socket.readyState === 3) this.closed(peer);
    } catch (error) { logError("Peri sync socket close failed", error); throw error; }
  }

  private defer(peer: Peer, work: Promise<unknown>): void {
    // 连接内的异步处理必须登记到平台保活，否则 isolate 可能在处理中途结束。
    peer.host.keepAlive(work.catch((error) => logError("Peri sync message handling failed", error)));
  }

  /**
   * 请求关闭：任何上下文都可以调用，但**不在这里写 socket**（可能是别的请求上下文）。
   * 实际关闭由本连接的泵在下一次（≤ 一个空闲拍）执行；关闭未确认前保留连接名额。
   */
  private close(peer: Peer, code: number, reason: string): void {
    this.requestClose(peer, code, reason);
  }

  private requestClose(peer: Peer, code: number, reason: string): void {
    if (peer.closed || peer.closing) return;
    peer.closing = { code, reason };
    peer.unsubscribe?.();
    // 若泵已停（例如排队预算耗尽后立即关闭），补一次排程，确保关闭最终落地。
    if (!peer.pumpTimer) peer.pumpTimer = setTimeout(() => { peer.pumpTimer = undefined; this.pump(peer); }, 0);
  }
}
