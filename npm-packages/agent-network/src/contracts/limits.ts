/** 第一版默认限制（§12.2）。所有阈值只有这一份实现。 */
export const LIMITS = {
  /** 单次 HTTP JSON body 上限。 */
  httpBodyBytes: 512 * 1024,
  /** 单条消息正文 UTF-8 上限。 */
  messageBodyBytes: 256 * 1024,
  /** actor 展示名称长度上限（按去空格后的字符数）。 */
  actorNameMax: 96,
  /** actor metadata 上限。 */
  actorMetadataBytes: 4 * 1024,
  /** 消息预览上限。 */
  previewBytes: 512,
  /** 历史页默认 / 最大条数。 */
  historyPageDefault: 50,
  historyPageMax: 100,
  /** 正文分块上限。 */
  bodyChunkBytes: 64 * 1024,
  /** 普通工具响应上限。 */
  toolResponseBytes: 256 * 1024,
  /** inbox 页默认 / 最大条数。 */
  inboxPageDefault: 20,
  inboxPageMax: 50,
  /** sync 页默认 / 最大条数。 */
  syncPageDefault: 100,
  syncPageMax: 200,
  /** 每 Agent pending 上限。 */
  pendingPerActor: 500,
  /** 单次成员变更上限。 */
  memberChangeMax: 20,
  /** 群成员上限。 */
  groupMembersMax: 100,
  /** 单条消息 invoke 上限。 */
  invokeMax: 8,
  /** 协作深度与每个 root 的累计 delivery 上限。 */
  cooperationDepthMax: 4,
  rootDeliveryMax: 32,
  /** 每个 actor 的活跃房间上限。 */
  activeRoomsPerActor: 200,
  /** 每条订阅请求的 resource URI 上限。 */
  subscriptionUrisMax: 4,
  /** 内部提示上限。 */
  hintBytes: 1024,
  /** 单条监听待发送队列。 */
  listenQueueMax: 32,
  /** SSE 保活间隔（毫秒）；同时决定客户端断开后服务端发现离线的上限。 */
  keepaliveMs: 5_000,
  /** 主动轮换间隔（毫秒）。 */
  rotationMs: 15 * 60_000,
  /** 同 URI 通知合并窗口（毫秒）。 */
  notifyMergeMs: 250,
  /** 恢复 ticket 有效期（毫秒）。 */
  recoveryTicketMs: 3 * 60_000,
  /** 幂等窗口（毫秒）。 */
  operationsRetentionMs: 30 * 24 * 60 * 60_000,
  /** events 留存（毫秒）。 */
  eventsRetentionMs: 30 * 24 * 60 * 60_000,
  /** 已完成 outbox 留存（毫秒）。 */
  outboxRetentionMs: 7 * 24 * 60 * 60_000,
  /** outbox 单轮处理上限。 */
  outboxBatchMax: 100,
  /** outbox 租约时长（毫秒）。 */
  outboxLeaseMs: 30_000,
  /** 重连退避。 */
  reconnectBaseMs: 1_000,
  reconnectMaxMs: 30_000,
} as const;

export function clampPage(value: number | undefined, fallback: number, max: number): number {
  if (value === undefined) return fallback;
  if (!Number.isInteger(value) || value < 1) return fallback;
  return Math.min(value, max);
}
