/**
 * 浏览器构建对 `crypto` / `node:crypto` 的替身（Vite alias，见 vite.config.ts）。
 *
 * SDK `/portable` 入口重导出执行准入与注册表模块，它们的摘要函数引用 `createHash`；
 * peri-cf 不消费准入面（取消走 `session/cancel`，宿主生命周期提供关闭证据）。这里提供
 * **显式失败**而不是静默 polyfill：一旦在浏览器被调用，说明消费面已变化，需要重新裁决，
 * 见 docs/design/peri-cf-browser-agent.md「风险与未决」。
 */
export function createHash(_algorithm: string): never {
  throw new Error(
    "node:crypto createHash is unavailable in the browser build; peri-cf does not consume the SDK admission surface");
}
