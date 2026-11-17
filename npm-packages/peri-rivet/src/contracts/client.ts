import type { JsonValue } from '../json.js';
import type { ConfigRevision, SessionEpoch, SessionId } from './identity.js';

/**
 * 客户端契约只出现公开身份，不出现服务端控制面字段。
 *
 * actor key、activationId、leaseEpoch、runtimeId 属于服务端控制面，公开后会把内部路由与租约
 * 语义固化成外部依赖，因此这里不提供任何对应字段。
 */
export interface SessionHandle {
  readonly sessionId: SessionId;
  /** 只用于拒绝过期句柄，不参与所有权 CAS；Rust 未发送时为 null。 */
  readonly sessionEpoch: SessionEpoch | null;
  readonly configRevision: ConfigRevision;
}

export interface CreateSessionInput {
  readonly agentConfigId: string;
}

/**
 * 客户端只做“读 + 显式 create”，不提供 getOrCreate 猜测语义；失败通过 PeriError 暴露。
 */
export interface PeriSessions {
  create(input: CreateSessionInput): Promise<SessionHandle>;
  open(sessionId: SessionId): Promise<SessionHandle>;
  close(sessionId: SessionId): Promise<void>;
  destroy(sessionId: SessionId): Promise<void>;
}

export interface EnsureSandboxInput {
  readonly sessionId: SessionId;
}

/**
 * 工具环境句柄。sandboxId 仅服务端可见，workspace/MCP descriptor 与租约同理：
 * 这些是跨 actor 的定位/凭据面，公开后会固化为客户端路由依赖。
 */
export interface SandboxHandle {
  readonly ready: boolean;
  readonly observedAt: string;
}

export interface PeriSandboxes {
  ensure(input: EnsureSandboxInput): Promise<SandboxHandle>;
}

/** 定义类资源；catalog 在客户端视角就是只读的 ResourceStore。 */
export type ResourceKind = 'agent_config' | 'provider_list' | 'mcp_plugin_config' | 'model_settings';

export interface ResourceGetInput {
  readonly kind: ResourceKind;
  readonly id?: string;
}

export interface ResourceSnapshot {
  readonly kind: ResourceKind;
  /** 不透明版本，只用于判断是否变化。 */
  readonly revision: ConfigRevision;
  /** 服务端已掩码的 JSON；密钥不回传。 */
  readonly value: JsonValue;
  readonly observedAt: string;
}

export interface PeriResources {
  get(input: ResourceGetInput): Promise<ResourceSnapshot>;
}

/**
 * M0 只有接口，没有工厂或实现：公开入口不得提供可调用的“未实现客户端”。
 */
export interface PeriClient {
  readonly sessions: PeriSessions;
  readonly sandboxes: PeriSandboxes;
  readonly resources: PeriResources;
}
