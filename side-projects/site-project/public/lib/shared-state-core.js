// ========== 共享状态核心层（框架无关） ==========
// 父端创建 store，子端通过 Comlink 远程访问。

/**
 * @typedef {(keys: string[], state: Record<string, any>) => Promise<void>|void} OnPersist
 */

/**
 * 创建共享状态 store。
 * @param {Record<string, any>} initialState
 * @param {OnPersist} [onPersist] - debounce 500ms 后调用，传入变更 keys 与全量 state
 */
export function createSharedStore(initialState, onPersist) {
  const state = { ...initialState };
  const listeners = new Map();       // key -> Map<id, cb>
  let nextId = 1;
  let persistTimer = null;
  const pendingKeys = new Set();

  function schedulePersist() {
    if (persistTimer) return;
    persistTimer = setTimeout(async () => {
      const keys = [...pendingKeys];
      pendingKeys.clear();
      if (onPersist) {
        try { await onPersist(keys, state); }
        catch (e) { console.error('[shared-state-core] persist failed:', e); }
      }
      persistTimer = null;
    }, 500);
  }

  return {
    get(key) { return state[key]; },

    set(key, val) {
      const next = typeof val === 'function' ? val(state[key]) : val;
      if (Object.is(next, state[key])) return;
      state[key] = next;
      const subs = listeners.get(key);
      if (subs) {
        subs.forEach(cb => {
          try { cb(next); } catch (e) { console.error('[shared-state-core] listener error:', e); }
        });
      }
      if (onPersist) { pendingKeys.add(key); schedulePersist(); }
    },

    /**
     * 订阅 key 变更。返回订阅 ID（可序列化），用 unsubscribe(key, id) 取消。
     */
    subscribe(key, cb) {
      if (!listeners.has(key)) listeners.set(key, new Map());
      const id = nextId++;
      listeners.get(key).set(id, cb);
      return id;
    },

    /** 取消订阅 */
    unsubscribe(key, id) {
      listeners.get(key)?.delete(id);
    },

    getAll() { return { ...state }; },

    hydrate(snapshot) {
      if (snapshot && typeof snapshot === 'object') {
        Object.assign(state, snapshot);
      }
    }
  };
}
