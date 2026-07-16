// ========== TerminalTabs 组件 ==========
// 多 tab 终端，xterm + WebSocket，含重连。独立访问时仍可工作。

import html from 'solid-js/html';
import { createSignal, For, Show, onMount, createEffect, onCleanup } from 'solid-js';
import { useTheme } from '/lib/solid-hooks.js';
import { isStandalone } from '/lib/env.js';
import { connectTerminal, nextReconnectDelay, MAX_ATTEMPTS } from '/lib/terminal/ws.js';

const DARK_THEME = {
  background: '#0d1117', foreground: '#e6edf3', cursor: '#e6edf3',
  selectionBackground: 'rgba(88,166,255,0.3)',
  black: '#161b22', red: '#f85149', green: '#3fb950',
  yellow: '#d29922', blue: '#58a6ff', magenta: '#a371f7',
  cyan: '#39c5cf', white: '#8b949e',
};
const LIGHT_THEME = {
  background: '#ffffff', foreground: '#1f2328', cursor: '#1f2328',
  selectionBackground: 'rgba(9,105,218,0.2)',
  black: '#24292f', red: '#cf222e', green: '#1a7f37',
  yellow: '#9a6700', blue: '#0969da', magenta: '#8250df',
  cyan: '#1b7c83', white: '#57606a',
};

export function TerminalTabs() {
  const [theme] = useTheme();
  const [tabs, setTabs] = createSignal([]);   // [{ id, term, fit, ws }]，永不替换对象
  const [statuses, setStatuses] = createSignal({});
  const [activeId, setActiveId] = createSignal(null);
  const [reconnectMsg, setReconnectMsg] = createSignal('');
  const statusOf = (id) => statuses()[id] || 'connecting';

  let containerRefs = {};
  let reconnectTimers = new Map();
  let reconnectAttempts = new Map();

  onMount(() => newTab());

  // theme 同步到所有 xterm
  createEffect(() => {
    const t = theme();
    tabs().forEach(({ term }) => {
      if (term) term.options.theme = t === 'light' ? LIGHT_THEME : DARK_THEME;
    });
  });

  async function newTab() {
    const id = 't' + Date.now() + Math.random().toString(36).slice(2, 6);
    const term = new window.Terminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: '"JetBrains Mono", "Fira Code", Menlo, monospace',
      theme: theme() === 'light' ? LIGHT_THEME : DARK_THEME,
    });
    const fit = new window.FitAddon.FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new window.WebLinksAddon.WebLinksAddon());
    setTabs(t => [...t, { id, term, fit, ws: null }]);
    setStatuses(s => ({ ...s, [id]: 'connecting' }));
    setActiveId(id);
    setTimeout(() => attachTab(id), 0);
  }

  async function attachTab(id) {
    const entry = tabs().find(x => x.id === id);
    if (!entry) return;
    const el = containerRefs[id];
    if (!el) return;
    const { term, fit } = entry;
    if (!term || !fit) return;
    try { term.open(el); } catch {}
    try { fit.fit(); } catch {}

    term.onData(d => {
      const t = tabs().find(x => x.id === id);
      if (t?.ws?.readyState === WebSocket.OPEN) t.ws.send(d);
    });
    term.onResize(({ cols, rows }) => {
      const t = tabs().find(x => x.id === id);
      if (t?.ws?.readyState === WebSocket.OPEN) {
        t.ws.send(JSON.stringify({ type: 'resize', cols, rows }));
      }
    });

    if (entry.ws) { try { entry.ws.close(); } catch {} entry.ws = null; }

    setStatuses(s => ({ ...s, [id]: 'connecting' }));

    const conn = connectTerminal({
      getCols: () => term.cols,
      getRows: () => term.rows,
      onOpen: () => {
        reconnectAttempts.set(id, 0);
        setReconnectMsg('');
        setStatuses(s => ({ ...s, [id]: 'open' }));
      },
      onMessage: (data) => term.write(data),
      onClose: () => {
        if (!tabs().some(x => x.id === id)) return;
        setStatuses(s => ({ ...s, [id]: 'closed' }));
        scheduleReconnect(id);
      },
    });
    entry.ws = conn.ws;
  }

  function scheduleReconnect(id) {
    const attempts = (reconnectAttempts.get(id) || 0) + 1;
    reconnectAttempts.set(id, attempts);
    const { delay, stop } = nextReconnectDelay(attempts);
    if (stop) {
      setReconnectMsg('Tab ' + id + ' gave up reconnecting');
      return;
    }
    setReconnectMsg('Tab ' + id + ' reconnecting... attempt ' + attempts + ' (' + delay + 'ms)');
    const timer = setTimeout(() => attachTab(id), delay);
    reconnectTimers.set(id, timer);
  }

  function closeTab(id) {
    const timer = reconnectTimers.get(id);
    if (timer) { clearTimeout(timer); reconnectTimers.delete(id); }
    reconnectAttempts.delete(id);

    const t = tabs().find(x => x.id === id);
    try { t?.ws?.close(); } catch {}
    try { t?.term?.dispose(); } catch {}
    delete containerRefs[id];
    const remaining = tabs().filter(x => x.id !== id);
    setTabs(remaining);
    setStatuses(s => {
      const next = { ...s };
      delete next[id];
      return next;
    });
    if (activeId() === id) {
      setActiveId(remaining.length > 0 ? remaining[0].id : null);
    }
    if (reconnectMsg() && reconnectMsg().includes(id)) setReconnectMsg('');
  }

  onCleanup(() => {
    tabs().forEach(({ ws, term }) => {
      try { ws?.close(); } catch {}
      try { term?.dispose(); } catch {}
    });
    reconnectTimers.forEach(t => clearTimeout(t));
    reconnectTimers.clear();
  });

  return html`
    <div class="flex flex-col h-full">
      <div class="flex items-center gap-1 px-2 py-1 border-b border-border shrink-0">
        <div class="flex gap-0.5 flex-1 overflow-x-auto">
          <${For} each=${tabs}>${(t) => html`
            <div
              class=${() => 'px-2 py-0.5 text-[11px] cursor-pointer rounded flex items-center gap-1 ' + (activeId() === t.id ? 'bg-bg-active' : 'hover:bg-bg-hover')}
              onClick=${() => setActiveId(t.id)}
            >
              <span>${t.id}${() => statusOf(t.id) === 'closed' ? ' \u26A0' : ''}</span>
              <span
                class="text-text-muted text-[13px] leading-none hover:text-error"
                onClick=${(e) => { e.stopPropagation(); closeTab(t.id); }}
              >\u00D7</span>
            </div>
          `}<//>
        </div>
        <button
          class="bg-transparent border-none text-text-muted cursor-pointer text-base px-2 py-0.5 hover:bg-bg-hover hover:text-text"
          title="New terminal"
          onClick=${newTab}
        >+</button>
      </div>
      <${Show} when=${reconnectMsg}>
        <div class="bg-error text-white text-[11px] px-2 py-0.5 text-center">${reconnectMsg}</div>
      <//>
      <div class="flex-1 min-h-0 relative">
        <${For} each=${tabs}>${(t) => html`
          <div
            class="absolute inset-0 p-1"
            style=${() => activeId() === t.id ? '' : 'display:none'}
            ref=${el => { containerRefs[t.id] = el; }}
          ></div>
        `}<//>
      </div>
    </div>
  `;
}
