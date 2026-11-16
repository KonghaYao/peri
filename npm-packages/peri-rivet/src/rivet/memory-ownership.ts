import type { ChatOwnership } from './chat-ports.js';

/**
 * 进程内 ownership，只用于 quickstart、测试与单实例宿主。
 *
 * 记录随进程消失，也不跨实例共享；多实例部署必须换成共享 KV（Rivet KV、Redis 等），
 * 否则同一 Session 会被多个实例同时认领。读写都做 `structuredClone`，避免调用方
 * 改到内部对象后绕过下一轮的归属判断。
 */
export function createMemoryChatOwnership(): ChatOwnership {
  const records = new Map<string, unknown>();

  return {
    async getItem(key) {
      const value = records.get(key);
      return value === undefined ? null : structuredClone(value);
    },
    async setItem(key, value) {
      records.set(key, structuredClone(value));
    },
    async removeItem(key) {
      records.delete(key);
    },
  };
}
