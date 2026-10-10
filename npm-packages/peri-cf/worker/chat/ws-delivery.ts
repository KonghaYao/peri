/**
 * 会话同步连接的投递信用与超时预算。连接状态只在 isolate 内存中（无 DO、无 attachment、无持久化）：
 * 未确认帧的上限由下面的常量决定，投递编号与确认规则由 SDK 的 delivery ledger 提供。
 */

export const MAX_OUTSTANDING_BYTES = 4 * 1024 * 1024;
export const MAX_OUTSTANDING_FRAMES = 16;
export const MAX_TOTAL_OUTSTANDING_BYTES = 8 * 1024 * 1024;
export const DELIVERY_TIMEOUT_MS = 10_000;
export const AUTH_TIMEOUT_MS = 3_000;

/** 单个会话在同一 isolate 内的并发同步连接上限。 */
export const MAX_SYNC_CONNECTIONS = 16;
