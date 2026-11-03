/** `node:crypto` 的浏览器替身（Vite alias）：peri-cf 不消费 SDK 准入面，误用时应显式失败而不是静默 polyfill。 */
export function createHash(_algorithm: string): never {
  throw new Error(
    "node:crypto createHash is unavailable in the browser build; peri-cf does not consume the SDK admission surface");
}
