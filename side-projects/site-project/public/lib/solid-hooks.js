// ========== Solid 适配层 ==========
// 把 Comlink 远程 store 包装为 Solid 响应式 signal。

import { createSignal, createEffect, onMount, onCleanup } from 'solid-js';
import { proxy } from 'comlink';
import { getParent } from '/lib/comlink-bridge.js';

const parentReady = getParent();   // Promise<Remote<store> | null>

/** 浅比较：对象逐键比较，数组逐项比较，其他 === */
function shallowEqual(a, b) {
  if (a === b) return true;
  if (a == null || b == null) return a === b;
  if (typeof a !== typeof b) return false;
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((v, i) => v === b[i]);
  }
  if (typeof a === 'object') {
    const ka = Object.keys(a), kb = Object.keys(b);
    if (ka.length !== kb.length) return false;
    return ka.every(k => k in b && a[k] === b[k]);
  }
  return false;
}

/**
 * 订阅父端 shared state 的某个 key，返回 [accessor, setter]。
 * @param {string} key
 * @returns {[() => any, (val: any) => Promise<void>]}
 */
export function useParentState(key) {
  const [sig, setSig] = createSignal(undefined, { equals: shallowEqual });

  onMount(async () => {
    try {
      const parent = await Promise.race([
        parentReady,
        new Promise((_, rej) => setTimeout(() => rej(new Error('parent timeout')), 5000)),
      ]);
      if (!parent) return;   // 顶层 window 调用，静默
      setSig(await parent.get(key));

      const cb = proxy((val) => setSig(val));
      const id = await parent.subscribe(key, cb);
      onCleanup(() => { try { parent.unsubscribe(key, id); } catch {} });
    } catch (e) {
      console.error('[solid-hooks] useParentState error:', key, e);
    }
  });

  const setter = (val) => parentReady.then(p => p?.set(key, val));
  return [sig, setter];
}

/** 语义糖 */
export const useCurrentFile = () => useParentState('currentFile');

// ========== 主题同步（postMessage 广播，替代 Comlink 长链路） ==========
// 模块级 signal：一个 iframe 内所有组件共享同一个主题状态
const _initialTheme = document.documentElement.dataset.theme || 'light';
const [_theme, _setTheme] = createSignal(_initialTheme);

// 监听父窗口主题广播
window.addEventListener('message', (e) => {
  if (e.data?.type === 'peri:setTheme' && e.data?.theme) {
    const t = e.data.theme;
    if (t !== _theme()) {
      _setTheme(t);
      document.documentElement.dataset.theme = t;
    }
  }
});

// 启动时向父窗口请求当前主题
function _requestTheme() {
  if (window.parent && window.parent !== window) {
    window.parent.postMessage({ type: 'peri:getTheme' }, '*');
  }
}
setTimeout(_requestTheme, 0);

/**
 * 跨 iframe 主题 hook。
 * 返回 [accessor, setter]，setter 通过 postMessage 通知父窗口切换主题。
 */
export function useTheme() {
  const setTheme = (val) => {
    if (val === _theme()) return;
    _setTheme(val);
    document.documentElement.dataset.theme = val;
    window.parent?.postMessage({ type: 'peri:setTheme', theme: val }, '*');
  };
  return [_theme, setTheme];
}

/** 瞬时事件用（scm commit 后自增） */
export const useScmVersion = () => useParentState('scmVersion');

/**
 * 调用父暴露的方法（如 openGraph / closeGraph）。
 * @param {string} name
 * @returns {(...args: any[]) => Promise<any>}
 */
export function useParentMethod(name) {
  return (...args) => parentReady.then(p => p?.[name]?.(...args));
}
