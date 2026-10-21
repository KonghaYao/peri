import { getServerByName } from 'partyserver';

import type { Env } from '../env';

export interface HintMessage {
  topic: string;
  seq: number;
  kind?: string;
}

/**
 * 一条 MCP `subscriptions/listen` 流到 PartyServer topic 的桥。
 *
 * - `subscribe()` 由 SDK 的 listen 路由调用；这里按 topic 建立内部 WebSocket。
 * - DO 只广播小型提示；资源正文由客户端回读。
 * - 连接断开时调用 `onBroken`，让 Gateway 结束本次 MCP 订阅流，客户端重建。
 */
export class TopicEventBus implements ServerEventBus {
  readonly #env: Env;
  readonly #topicUris: Map<string, Set<string>>;
  readonly #onBroken: () => void;
  readonly #listeners = new Set<(event: ServerEvent) => void>();
  readonly #sockets: WebSocket[] = [];
  readonly #buffer: ServerEvent[] = [];
  readonly #ready: Promise<void>;
  #closed = false;

  constructor(env: Env, topicUris: Map<string, Set<string>>, onBroken: () => void) {
    this.#env = env;
    this.#topicUris = topicUris;
    this.#onBroken = onBroken;
    this.#ready = Promise.all([...topicUris.keys()].map((topic) => this.#connect(topic))).then(
      () => undefined,
    );
  }

  /** 等所有 topic 的内部 WebSocket 就绪后再建立订阅流，避免漏掉间隙里的提示。 */
  ready(): Promise<void> {
    return this.#ready;
  }

  async #connect(topic: string): Promise<void> {
    try {
      const stub = await getServerByName(this.#env.NOTIFICATION_CHANNELS, topic);
      const response = await stub.fetch(
        new Request(`https://notification-channel.internal/${encodeURIComponent(topic)}`, {
          headers: { Upgrade: 'websocket' },
        }),
      );
      const socket = response.webSocket;
      if (!socket) throw new Error(`topic ${topic} did not return a websocket`);
      if (this.#closed) {
        socket.accept();
        socket.close(1000, 'closed');
        return;
      }
      socket.accept();
      this.#sockets.push(socket);
      socket.addEventListener('message', (event) => {
        this.#onChannelMessage(topic, event.data);
      });
      socket.addEventListener('close', () => this.#break());
      socket.addEventListener('error', () => this.#break());
    } catch (error) {
      console.error('notification channel connect failed', topic, error);
      this.#break();
      throw error;
    }
  }

  #onChannelMessage(topic: string, data: unknown): void {
    let seq = 0;
    let kind: string | undefined;
    if (typeof data === 'string') {
      try {
        const parsed = JSON.parse(data) as Partial<HintMessage>;
        seq = typeof parsed.seq === 'number' ? parsed.seq : 0;
        kind = typeof parsed.kind === 'string' ? parsed.kind : undefined;
      } catch {
        // 非 JSON 提示按无版本处理。
      }
    }
    if (kind === 'membership') {
      // 成员变化后必须关闭流并重新鉴权，避免已被移除的身份继续收到房间提示。
      this.#break();
      return;
    }
    const uris = this.#topicUris.get(topic);
    if (!uris) return;
    for (const uri of uris) {
      const event: ServerEvent = { kind: 'resource_updated', uri };
      void seq;
      void kind;
      this.#dispatch(event);
    }
  }

  #dispatch(event: ServerEvent): void {
    if (this.#listeners.size === 0) {
      if (this.#buffer.length >= 32) this.#buffer.shift();
      this.#buffer.push(event);
      return;
    }
    for (const listener of this.#listeners) listener(event);
  }

  #break(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const socket of this.#sockets) {
      try {
        socket.close(1001, 'upstream closed');
      } catch {
        // 已关闭。
      }
    }
    this.#sockets.length = 0;
    this.#onBroken();
  }

  publish(): void {
    // 服务端的变更发布走 D1 outbox + DO，不经过单请求 bus。
  }

  subscribe(listener: (event: ServerEvent) => void): () => void {
    this.#listeners.add(listener);
    if (this.#buffer.length > 0) {
      const buffered = this.#buffer.splice(0, this.#buffer.length);
      for (const event of buffered) listener(event);
    }
    return () => {
      this.#listeners.delete(listener);
    };
  }

  close(): void {
    this.#closed = true;
    for (const socket of this.#sockets) {
      try {
        socket.close(1000, 'listen closed');
      } catch {
        // 已关闭。
      }
    }
    this.#sockets.length = 0;
  }
}

// 仅类型导入，避免把 SDK 的运行时实现带进 Worker 的额外路径。
import type { ServerEvent, ServerEventBus } from '@modelcontextprotocol/server';

export async function publishHint(env: Env, topic: string, seq: number, kind?: string): Promise<void> {
  const stub = await getServerByName(env.NOTIFICATION_CHANNELS, topic);
  await stub.publish(JSON.stringify({ topic, seq, kind } satisfies HintMessage));
}
