import type { Server, ServerWebSocket, WebSocketHandler } from "bun";
import {
  ChatViewReplayOverflowError,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatViewRegistry,
  type ChatYjsChannel,
} from "@peri-code/sdk";
import { logError } from "@core/agent";
import { chatNotFound } from "./errors";

/**
 * 只读 Yjs 同步的 WebSocket 入口。
 *
 * 每条连接一个 `ChatYjsChannel`，`ChatView.sync` 为它创建独立的 ChatSyncConnection：
 * HELLO 触发 SNAPSHOT/UPDATE，ACK 驱动投递信用；慢客户端由 SDK 显式关闭。
 * 客户端只能发 HELLO/ACK；任何 Yjs 更新或其它帧都由 SDK 拒绝。
 */

export const SYNC_PATH_PATTERN = /^\/api\/chats\/([^/]+)\/sync$/;
/** 与 SDK HELLO 上限一致；客户端帧只允许握手与 ACK。 */
export const MAX_CLIENT_SYNC_FRAME_BYTES = 192 * 1024;
const NORMAL_CLOSE = 1000;
const UNSUPPORTED_DATA = 1003;
const TOO_LARGE = 1009;
const INTERNAL_ERROR = 1011;

export interface SyncUpgradeDeps {
  manager: Pick<ChatManagerContract, "getChat" | "findChatBySession">;
  views: ChatViewRegistry;
}

export interface SyncSocketData {
  readonly chatId: string;
  readonly chat: ChatServiceContract;
  readonly channel: SyncSocketChannel;
  readonly view: ReturnType<ChatViewRegistry["attach"]>;
}

function closeReason(reason: string | undefined): string | undefined {
  if (!reason) return undefined;
  return reason.length > 120 ? `${reason.slice(0, 119)}…` : reason;
}

/**
 * Bun ServerWebSocket → SDK 逻辑通道。
 *
 * `peerClosed` 由 socket close 事件调用；`close` 是 SDK 主动终止。两者幂等，
 * 都只通知一次 close listener。
 */
export class SyncSocketChannel implements ChatYjsChannel {
  closed = false;
  private socket: ServerWebSocket<SyncSocketData> | undefined;
  private readonly messageListeners = new Set<(frame: Uint8Array) => void>();
  private readonly closeListeners = new Set<(error?: Error) => void>();

  bind(socket: ServerWebSocket<SyncSocketData>): void {
    if (this.closed) throw new Error("Chat sync channel is already closed");
    this.socket = socket;
  }

  /** 服务端收到一帧；帧内容由 SDK 协议解释。 */
  receive(frame: Uint8Array): void {
    if (this.closed) return;
    for (const listener of [...this.messageListeners]) listener(frame);
  }

  /** 物理连接已断开；不再次调用 socket.close。 */
  peerClosed(error?: Error): void {
    if (this.closed) return;
    this.closed = true;
    this.socket = undefined;
    for (const listener of [...this.closeListeners]) listener(error);
    this.closeListeners.clear();
    this.messageListeners.clear();
  }

  send(frame: Uint8Array): void {
    if (this.closed) throw new Error("Chat sync channel is closed");
    const socket = this.socket;
    if (!socket) throw new Error("Chat sync channel has no socket");
    socket.sendBinary(frame);
  }

  onMessage(listener: (frame: Uint8Array) => void): () => void {
    if (this.closed) return () => {};
    this.messageListeners.add(listener);
    return () => this.messageListeners.delete(listener);
  }

  onClose(listener: (error?: Error) => void): () => void {
    if (this.closed) {
      listener(new Error("Chat sync channel is already closed"));
      return () => {};
    }
    this.closeListeners.add(listener);
    return () => this.closeListeners.delete(listener);
  }

  close(reason?: Error): void {
    if (this.closed) return;
    this.closed = true;
    const socket = this.socket;
    this.socket = undefined;
    for (const listener of [...this.closeListeners]) listener(reason);
    this.closeListeners.clear();
    this.messageListeners.clear();
    if (socket && socket.readyState === WebSocket.OPEN) {
      socket.close(
        reason ? INTERNAL_ERROR : NORMAL_CLOSE,
        closeReason(reason?.message),
      );
    }
  }
}

/**
 * 处理 `/api/chats/:chatId/sync` 的 WS 升级。
 *
 * 返回 `undefined` 表示已成功升级；其它路径返回 `undefined` 由调用方继续路由。
 * 未登记的 Chat 在升级前以 410 拒绝，不创建空句柄、不自动绑定历史。
 */
export async function handleSyncUpgrade(
  request: Request,
  server: Server<SyncSocketData>,
  deps: SyncUpgradeDeps,
): Promise<Response | undefined> {
  const pathname = new URL(request.url).pathname;
  const match = SYNC_PATH_PATTERN.exec(pathname);
  if (!match) return undefined;

  if (request.method !== "GET") {
    return new Response("Method not allowed", {
      status: 405,
      headers: { Allow: "GET" },
    });
  }
  if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    return new Response("WebSocket upgrade required", {
      status: 426,
      headers: { Upgrade: "websocket" },
    });
  }

  let chatId: string;
  try {
    chatId = decodeURIComponent(match[1]!);
  } catch {
    return new Response("Invalid chat id", { status: 400 });
  }
  const chat = deps.manager.getChat(chatId)
    ?? deps.manager.findChatBySession(chatId);
  if (!chat) {
    const mapped = chatNotFound(chatId);
    return Response.json(mapped.body, { status: mapped.status });
  }

  let view: SyncSocketData["view"];
  try {
    view = deps.views.attach(chat);
  } catch (error) {
    logError("[peri-cf] Peri chat view attach failed", error, { chatId: chat.id });
    return Response.json({
      ok: false,
      code: error instanceof ChatViewReplayOverflowError
        ? "view_replay_overflow"
        : "view_attach_failed",
      message: error instanceof ChatViewReplayOverflowError
        ? "Chat view fact log overflowed; rebind before connecting"
        : "Chat view could not be attached",
      chatId: chat.id,
    }, { status: 500 });
  }

  const data: SyncSocketData = {
    chatId: chat.id,
    chat,
    channel: new SyncSocketChannel(),
    view,
  };
  if (!server.upgrade(request, { data })) {
    return new Response("WebSocket upgrade rejected", { status: 400 });
  }
  return undefined;
}

/** Bun WebSocket 生命周期 → 每条连接独立的 SDK 同步连接。 */
export const syncWebSocketHandlers: WebSocketHandler<SyncSocketData> = {
  open(ws) {
    ws.data.channel.bind(ws);
    try {
      ws.data.view.sync(ws.data.channel);
    } catch (error) {
      ws.data.channel.close(error instanceof Error ? error : new Error("Chat view sync failed"));
    }
  },

  message(ws, message) {
    if (typeof message === "string") {
      ws.close(UNSUPPORTED_DATA, "Binary frames required");
      return;
    }
    const frame = new Uint8Array(message);
    if (frame.byteLength > MAX_CLIENT_SYNC_FRAME_BYTES) {
      ws.close(TOO_LARGE, "Chat sync frame exceeds client budget");
      return;
    }
    try {
      ws.data.channel.receive(frame);
    } catch (error) {
      ws.data.channel.close(error instanceof Error ? error : new Error("Chat sync frame rejected"));
    }
  },

  close(ws, code, reason) {
    const detail = reason ? `: ${reason}` : "";
    ws.data.channel.peerClosed(
      new Error(`Chat sync socket closed (${String(code)}${detail})`),
    );
  },
};
