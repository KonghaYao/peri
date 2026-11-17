import { type Brand, parseIdentityText } from './identity.js';

/**
 * 服务端身份只用于 actor 路由与诊断，不通过 `/contracts` 或 `/client` 导出：
 * 公开后会把内部寻址固化成外部依赖。
 */

/** sandboxId 由服务端派生并用于定位工具环境，SandboxHandle 只回传 ready/observedAt。 */
export type SandboxId = Brand<string, 'SandboxId'>;

export function parseSandboxId(value: string): SandboxId {
  return parseIdentityText('sandboxId', value);
}
