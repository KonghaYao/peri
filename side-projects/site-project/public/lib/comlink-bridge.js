// ========== Comlink 端点封装 ==========
// 子端用：获取父暴露的 API、向父暴露自身 API。
// 父端用：向子暴露 store、wrap 子 API。
import { expose, wrap, windowEndpoint } from 'comlink';

/**
 * 子 → 父：获取父端通过 expose 暴露的对象。
 * 顶层 window 调用时返回 null。
 * @returns {Promise<object|null>}
 */
export async function getParent() {
  if (!window.parent || window.parent === window) return null;
  return wrap(windowEndpoint(window.parent, window));
}

/**
 * 子 → 父：向父暴露自身 API（如 close / refresh）。
 * @param {Record<string, any>} api
 */
export function exposeAPI(api) {
  if (!window.parent || window.parent === window) return;
  expose(api, windowEndpoint(window.parent, window));
}

/**
 * 父 → 子：向特定 iframe 暴露对象（通常是 shared store）。
 * 需在 iframe 的 load 事件之后调用，否则 contentWindow 为 null 会静默跳过。
 * @param {any} obj
 * @param {HTMLIFrameElement} frame
 * @returns {boolean} 是否成功暴露（false 表示 iframe 未就绪）
 */
export function exposeToChild(obj, frame) {
  if (!frame.contentWindow) return false;
  expose(obj, windowEndpoint(frame.contentWindow, window));
  return true;
}

/**
 * 父 → 子：wrap 子暴露的 API。
 * 需在 iframe 的 load 事件之后调用，否则 contentWindow 为 null 返回 null。
 * @param {HTMLIFrameElement} frame
 * @returns {object|null}
 */
export function wrapChild(frame) {
  if (!frame.contentWindow) return null;
  return wrap(windowEndpoint(frame.contentWindow, window));
}
