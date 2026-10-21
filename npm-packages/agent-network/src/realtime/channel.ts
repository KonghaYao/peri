import { Server, type Connection } from 'partyserver';

import type { Env } from '../env';

const PRESENCE_KEY = 'presence';
const ACTOR_KIND_KEY = 'actor-kind';
const INBOX_PREFIX = 'inbox:';

interface PresenceState {
  online: boolean;
  since: number;
  /** 上线时的 inbox_head_seq；重连补发提示只需要版本号。 */
  seq: number;
  /** login 时授权的连接；新连接不会自动继承上线状态。 */
  connections: string[];
}

export interface LoginPresence {
  online: true;
  since: number;
  connections: number;
}

function loginHint(topic: string, seq: number): string {
  return JSON.stringify({ topic, seq, kind: 'login' });
}

/**
 * 一个 topic 一个 Durable Object 实例（房间或收件箱）。
 *
 * 只负责把小型变更提示广播给当前连接；不保存业务历史、不读业务表
 * （仅缓存 inbox 的 actor kind 以区分人/Agent）。开启 hibernation：
 * 没有广播时 DO 可以休眠，连接与上线状态由平台和 DO storage 保存。
 *
 * 上线规则：
 * - Agent 必须显式调用 `login` 后才会收到 inbox 提示；订阅建立本身不算上线。
 * - 上线状态绑定 login 时的连接代次：这些连接全部断开即离线，期间新建立的
 *   订阅不会自动继承上线状态；重连后需要再次 `login`。
 * - 人类 actor 没有上线概念，其订阅提示始终广播。
 */
export class NotificationChannel extends Server<Env> {
  static override options = { hibernate: true };

  #actorKind: 'human' | 'agent' | null | undefined;

  /** Gateway 发布的变更提示（不超过 1 KiB）。 */
  async publish(message: string): Promise<void> {
    if (await this.#gated()) {
      const presence = await this.#presence();
      if (!presence?.online) return;
      for (const connection of this.getConnections()) {
        if (!presence.connections.includes(connection.id)) continue;
        try {
          connection.send(message);
        } catch {
          // 连接已进入关闭流程；离线状态由 onClose 收敛。
        }
      }
      return;
    }
    this.broadcast(message);
  }

  /**
   * Agent 显式上线。
   *
   * 允许先上线再建立订阅：没有连接时记录空代次，首个连接会被收养并补发提示。
   * 重复调用幂等，刷新上线时间、当前连接代次与 inbox 版本。
   */
  async login(seq: number): Promise<LoginPresence> {
    const current = await this.#presence();
    const since = Date.now();
    const connections = [...this.getConnections()].map((connection) => connection.id);
    const next: PresenceState = {
      online: true,
      since,
      seq: Math.max(seq, current?.seq ?? 0),
      connections,
    };
    await this.ctx.storage.put(PRESENCE_KEY, next);
    this.#sendTo(connections, loginHint(this.name, next.seq));
    return { online: true, since, connections: connections.length };
  }

  override onConnect(connection: Connection): void {
    void this.#adopt(connection);
  }

  override async onClose(connection: Connection): Promise<void> {
    const presence = await this.#presence();
    if (!presence?.online || !presence.connections.includes(connection.id)) return;
    const connections = presence.connections.filter((id) => id !== connection.id);
    await this.ctx.storage.put(PRESENCE_KEY, {
      ...presence,
      online: connections.length > 0,
      connections,
    });
  }

  override async onMessage(connection: Connection, message: import('partyserver').WSMessage) {
    // 该通道只允许服务端内部发布；客户端上行消息直接丢弃。
    void connection;
    void message;
  }

  /** login 先于订阅时，把首个连接纳入代次并补发提示。 */
  async #adopt(connection: Connection): Promise<void> {
    const presence = await this.#presence();
    if (!presence?.online || presence.connections.length > 0) return;
    const next: PresenceState = { ...presence, connections: [connection.id] };
    await this.ctx.storage.put(PRESENCE_KEY, next);
    connection.send(loginHint(this.name, next.seq));
  }

  #sendTo(connectionIds: string[], message: string): void {
    if (connectionIds.length === 0) return;
    for (const connection of this.getConnections()) {
      if (!connectionIds.includes(connection.id)) continue;
      try {
        connection.send(message);
      } catch {
        // 连接已进入关闭流程。
      }
    }
  }

  async #presence(): Promise<PresenceState | null> {
    return (await this.ctx.storage.get<PresenceState>(PRESENCE_KEY)) ?? null;
  }

  /** 只有 Agent 的 inbox 需要上线；房间与人类 inbox 不受影响。 */
  async #gated(): Promise<boolean> {
    if (!this.name.startsWith(INBOX_PREFIX)) return false;
    return (await this.#resolveActorKind()) === 'agent';
  }

  async #resolveActorKind(): Promise<'human' | 'agent' | null> {
    if (this.#actorKind !== undefined) return this.#actorKind;
    const cached = await this.ctx.storage.get<'human' | 'agent'>(ACTOR_KIND_KEY);
    if (cached) {
      this.#actorKind = cached;
      return cached;
    }
    const actor = this.name.slice(INBOX_PREFIX.length);
    const row = await this.env.DB.prepare('SELECT kind FROM actors WHERE actor_id = ?')
      .bind(actor)
      .first<{ kind: 'human' | 'agent' }>();
    const kind = row?.kind ?? null;
    if (kind) await this.ctx.storage.put(ACTOR_KIND_KEY, kind);
    this.#actorKind = kind;
    return kind;
  }
}
