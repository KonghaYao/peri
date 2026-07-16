// ========== 独立运行基础设施 ==========
// 提供：是否独立访问检测、ws URL 计算、父方法 fallback、HTTP URL 计算。
// 让 iframe 子页面也能直接访问（脱离 parent shell）。

import { getParent } from '/lib/comlink-bridge.js';

/**
 * 检测当前页面是否独立访问（不在 iframe 内）。
 * 跨域访问 window.parent 会抛错，此时也视为独立。
 * @returns {boolean}
 */
export function isStandalone() {
  try {
    return window.parent === window || window.top === window;
  } catch {
    return true;
  }
}

/**
 * 计算当前 origin 下的 WebSocket URL。
 * 协议自动跟随 location.protocol（https → wss，其他 → ws）。
 * @param {string} [sessionId] — 可选，附加为 ?session=xxx
 * @returns {string}
 */
export function buildWsUrl(sessionId) {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  let url = `${proto}//${location.host}/ws`;
  if (sessionId) url += (url.includes('?') ? '&' : '?') + 'session=' + encodeURIComponent(sessionId);
  return url;
}

/**
 * 创建一个调用父方法的包装函数，独立访问或父无此方法时走 fallback。
 * @param {string} name — 父 API 上的方法名
 * @param {Function} [fallback] — fallback 函数
 * @returns {Function}
 */
export function parentMethod(name, fallback) {
  return (...args) => {
    if (isStandalone()) return fallback?.(...args);
    return getParent().then(p => {
      if (p && typeof p[name] === 'function') return p[name](...args);
      return fallback?.(...args);
    });
  };
}

/**
 * 将路径补全为绝对 HTTP URL（独立访问时用于 fetch 等）。
 * 已是完整 URL 则原样返回。
 * @param {string} path
 * @returns {string}
 */
export function httpUrl(path) {
  return path.startsWith('http') ? path : location.origin + (path.startsWith('/') ? path : '/' + path);
}

/**
 * 独立访问时打印警告并返回 null；iframe 内则返回父 API。
 * @returns {Promise<object|null>}
 */
export async function requireParent() {
  if (isStandalone()) {
    console.warn('[env] running standalone, parent API unavailable');
    return null;
  }
  return getParent();
}
