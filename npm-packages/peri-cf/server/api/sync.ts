import {
  ChatViewReplayOverflowError,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatViewRegistry,
  type ChatYjsChannel,
} from "@peri-code/sdk";
import type { MiddlewareHandler } from "hono";
import { createMiddleware } from "hono/factory";
import type { WSEvents, WSContext } from "hono/ws";
import { logError } from "../logging";
import { chatNotFound } from "./errors";

/**
 * 只读 Yjs 同步的 WebSocket 入口。
 *
 * 每条连接一个 `ChatYjsChannel`，`ChatView.sync` 为它创建独立的 ChatSyncConnection：
 * HELLO 触发 SNAPSHOT/UPDATE，ACK 驱动投递信用；慢客户端由 SDK 显式关闭。
 * 客户端只能发 HELLO/ACK；任何 Yjs 更新或其它帧都由 SDK 拒绝。
 */

/** 与 SDK HELLO 上限一致；客户端帧只允许握手与 ACK。 */
export const MAX_CLIENT_SYNC_FRAME_BYTES = 192 * 1024;
const NORMAL_CLOSE = 1000;
const UNSUPPORTED_DATA = 1003;
const TOO_LARGE = 1009;
const INTERNAL_ERROR = 1011;

/** 精确匹配升级路径；`app.use` 会连带匹配子路径，改变未匹配路径的状态码。 */
export const SYNC_PATH = "/api/chats/:chatId/sync";

export interface SyncConnection {
  readonly chatId: string;
  readonly chat: ChatServiceContract;
  readonly channel: WebSocketSyncChannel;
  readonly view: ReturnType<ChatViewRegistry["attach"]>;
}

export type SyncEnv = { Variables: { syncConnection: SyncConnection } };

/** WS 升级所需的注册表能力；不依赖父层具体装配类。 */
type SyncRegistry = {
  readonly chats: Pick<ChatManagerContract, "getChat" | "findChatBySession">;
  readonly views: Pick<ChatViewRegistry, "attach">;
};

/** 适配器提供的单连接发送/关闭能力（`@hono/bun` 的 `WSContext`）。 */
interface SyncSocket {
  send(frame: Uint8Array): void;
  close(code?: number, reason?: string): void;
}

/**
 * 适配器 socket → SDK 逻辑通道。
 *
 * `peerClosed` 由 socket close 事件调用；`close` 是 SDK 主动终止。两者幂等，
 * 都只通知一次 close listener。
 */
export class WebSocketSyncChannel implements ChatYjsChannel {
  closed = false;
  private socket: SyncSocket | undefined;
  private readonly messageListeners = new Set<(frame: Uint8Array) => void>();
  private readonly closeListeners = new Set<(error?: Error) => void>();

  bind(socket: SyncSocket): void {
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
    socket.send(frame);
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
    if (socket) {
      // `WSContext.readyState` 是事件创建时的快照，不反映当前连接状态；
      // `closed` 已保证幂等，且 Bun 对已关闭的 socket 调用 close 不会抛错。
      socket.close(
        reason ? INTERNAL_ERROR : NORMAL_CLOSE,
        reason?.message
          ? reason.message.length > 120
            ? `${reason.message.slice(0, 119)}…`
            : reason.message
          : undefined,
      );
    }
  }
}

/**
 * `/api/chats/:chatId/sync` 的升级前置条件；顺序与既有协议一致：
 * 方法 → Upgrade 头 → Chat 查找 → View attach。
 *
 * 未登记的 Chat 在升级前以 410 拒绝，不创建空句柄、不自动绑定历史。
 * 成功时把逐连接状态写入 `syncConnection`，供升级工厂闭包读取。
 */
export function syncUpgradeGuard(registry: SyncRegistry): MiddlewareHandler<SyncEnv> {
  return createMiddleware<SyncEnv, typeof SYNC_PATH>(async (c, next) => {
    if (c.req.method !== "GET") {
      return c.text("Method not allowed", 405, { Allow: "GET" });
    }
    if (c.req.header("upgrade")?.toLowerCase() !== "websocket") {
      return c.text("WebSocket upgrade required", 426, { Upgrade: "websocket" });
    }

    const chatId = c.req.param("chatId");
    const chat = registry.chats.getChat(chatId)
      ?? registry.chats.findChatBySession(chatId);
    if (!chat) {
      const mapped = chatNotFound(chatId);
      return c.json(mapped.body, 410);
    }

    let view: SyncConnection["view"];
    try {
      view = registry.views.attach(chat);
    } catch (error) {
      logError("[peri-cf] Peri chat view attach failed", error, { chatId: chat.id });
      return c.json({
        ok: false,
        code: error instanceof ChatViewReplayOverflowError
          ? "view_replay_overflow"
          : "view_attach_failed",
        message: error instanceof ChatViewReplayOverflowError
          ? "Chat view fact log overflowed; rebind before connecting"
          : "Chat view could not be attached",
        chatId: chat.id,
      }, 500);
    }

    c.set("syncConnection", {
      chatId: chat.id,
      chat,
      channel: new WebSocketSyncChannel(),
      view,
    });
    await next();
  });
}

/** Hono WebSocket 生命周期 → 每条连接独立的 SDK 同步连接。 */
export function syncEvents(connection: SyncConnection): WSEvents {
  return {
    onOpen(_event, ws) {
      connection.channel.bind({
        send: (frame) => ws.send(frame as Parameters<WSContext["send"]>[0]),
        close: (code, reason) => ws.close(code, reason),
      });
      try {
        connection.view.sync(connection.channel);
      } catch (error) {
        connection.channel.close(
          error instanceof Error ? error : new Error("Chat view sync failed"),
        );
      }
    },

    onMessage(event, ws) {
      if (typeof event.data === "string" || event.data instanceof Blob) {
        ws.close(UNSUPPORTED_DATA, "Binary frames required");
        return;
      }
      const frame = new Uint8Array(event.data);
      if (frame.byteLength > MAX_CLIENT_SYNC_FRAME_BYTES) {
        ws.close(TOO_LARGE, "Chat sync frame exceeds client budget");
        return;
      }
      try {
        connection.channel.receive(frame);
      } catch (error) {
        connection.channel.close(
          error instanceof Error ? error : new Error("Chat sync frame rejected"),
        );
      }
    },

    onClose(event) {
      const detail = event.reason ? `: ${event.reason}` : "";
      connection.channel.peerClosed(
        new Error(`Chat sync socket closed (${String(event.code)}${detail})`),
      );
    },
  };
}
