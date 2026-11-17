import type { ChatFact } from '@peri-code/sdk';
import type { ActivitySnapshot } from './activity.js';
import type { SessionEpoch, SessionId, TurnId } from './identity.js';

/**
 * 事实身份。`attemptId` 在 M0 固定为 null：事件转发不携带 attempt，`eventIdentity` 落地前
 * 任何非 null 值都只能是伪造。`turnId` 只在事实属于某一轮时存在，缺失记为 null 而不是自造。
 */
export interface FactIdentity {
  readonly sessionId: SessionId;
  readonly sessionEpoch: SessionEpoch | null;
  readonly turnId: TurnId | null;
  readonly attemptId: null;
}

/**
 * 一条已投影的会话事实。
 *
 * 事实按当前绑定代次整体回放，契约不提供跨连接的续读位置。
 */
export interface FactEnvelope {
  readonly identity: FactIdentity;
  readonly fact: ChatFact;
  /** ISO 8601；服务端注入时间，用于诊断，不参与排序。 */
  readonly emittedAt: string;
}

/** reset 是投影换代，不是补发；客户端收到后必须清空本地投影再重建。 */
export type FactResetReason =
  | 'lease_epoch_changed'
  | 'session_epoch_changed'
  | 'takeover'
  | 'recover'
  | 'resync';

/** 首帧的全量替代：facts 是新投影的起点。 */
export interface FactSnapshotFrame {
  readonly type: 'snapshot';
  readonly identity: FactIdentity;
  readonly facts: readonly FactEnvelope[];
  /** 与 facts 同一时刻采集的活动快照；数据源缺失时为 null，客户端不得据此推断可休眠。 */
  readonly activity: ActivitySnapshot | null;
  readonly emittedAt: string;
}

export interface FactResetFrame {
  readonly type: 'reset';
  readonly identity: FactIdentity;
  readonly reason: FactResetReason;
  readonly emittedAt: string;
}

/** 服务端重放缓冲突或超出有界缓冲时发出；客户端只能重新 snapshot。 */
export interface FactOverflowFrame {
  readonly type: 'overflow';
  readonly identity: FactIdentity;
  readonly reason: 'overflow';
  readonly emittedAt: string;
}

/**
 * 流帧联合：FactEnvelope 没有 `type`，用 `'type' in frame` 与带标签的帧区分。
 */
export type FactStreamFrame = FactEnvelope | FactSnapshotFrame | FactResetFrame | FactOverflowFrame;
