import type { Chat } from "./contracts";

/**
 * 句柄表：登记本客户端创建的 Action/WS 句柄，或在 409 时复用服务端既有控制者。
 *
 * 只是 `sessionId` 到句柄记录的表；连接与投影生命周期仍由控制器持有。
 */

export interface ChatHandleRecord {
  /** Action/WS 地址：新句柄用 handle id；409 复用已有控制者时用 sessionId。 */
  address: string;
  /** 本客户端创建的句柄；为 null 表示复用服务端既有控制者。 */
  handleId: string | null;
  chat: Chat;
}

export interface RebindHandleInput {
  /** 换绑前的展示身份。 */
  previousChatId: string;
  address: string;
  handleId: string | null;
  /** 换绑后的展示身份。 */
  chat: Chat;
}

export class ChatHandleTable {
  private readonly records = new Map<string, ChatHandleRecord>();

  get(sessionId: string): ChatHandleRecord | undefined {
    return this.records.get(sessionId);
  }

  set(sessionId: string, record: ChatHandleRecord): void {
    this.records.set(sessionId, record);
  }

  delete(sessionId: string): void {
    this.records.delete(sessionId);
  }

  clear(): void {
    this.records.clear();
  }

  /**
   * 副本换绑后让句柄记录跟随绑定身份。
   * 旧 sessionId 的记录只在仍指向同一地址时移除，避免误删别的绑定。
   */
  followRebind(input: RebindHandleInput): void {
    const previous = this.records.get(input.previousChatId);
    if (previous?.address === input.address) this.records.delete(input.previousChatId);
    this.records.set(input.chat.id, { address: input.address, handleId: input.handleId, chat: input.chat });
  }
}
