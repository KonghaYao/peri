import { Server } from 'partyserver';

import type { Env } from '../env';

/**
 * 一个 topic 一个 Durable Object 实例（房间或收件箱）。
 *
 * 只负责把小型变更提示广播给当前连接；不保存业务历史、不读 D1、不跑定时器。
 * 开启 hibernation：没有广播时 DO 可以休眠，连接元数据由平台保存。
 */
export class NotificationChannel extends Server<Env> {
  static override options = { hibernate: true };

  /** Gateway 发布的变更提示（不超过 1 KiB）。 */
  publish(message: string): void {
    this.broadcast(message);
  }

  override onConnect(): void {
    // 连接本身不需要额外状态：订阅过滤在 Gateway 侧完成。
  }

  override async onMessage(connection: import('partyserver').Connection, message: import('partyserver').WSMessage) {
    // 该通道只允许服务端内部发布；客户端上行消息直接丢弃。
    void connection;
    void message;
  }
}
