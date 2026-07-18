// ========== AppShell 组件 — macOS 风格桌面 Shell ==========
// 替代 parent.html 的固定分栏布局。
// 通过 window.__store 暴露 store 给所有 AppWindow 子 iframe。

import html from 'solid-js/html';
import { createSignal, createMemo, For, Show } from 'solid-js';
import { AppWindow } from '/lib/components/AppWindow.js';
import { Dock } from '/lib/components/Dock.js';

// ========== App 定义 ==========
const APPS = [
  { id: 'files',    name: '文件',     icon: '📁', src: '/pages/file-tree.html',  defaultW: 280, defaultH: 500, defaultX: 20,  defaultY: 30 },
  { id: 'preview',  name: '预览',     icon: '📄', src: '/pages/preview.html',    defaultW: 600, defaultH: 400, defaultX: 320, defaultY: 30 },
  { id: 'terminal', name: '终端',     icon: '>_', src: '/pages/terminal.html',   defaultW: 700, defaultH: 400, defaultX: 320, defaultY: 450 },
  { id: 'scm',      name: '版本控制', icon: '⎇', src: '/pages/scm.html',          defaultW: 400, defaultH: 500, defaultX: 940, defaultY: 30 },
  { id: 'graph',    name: 'Git 图谱', icon: '◉', src: '/pages/graph.html',        defaultW: 900, defaultH: 550, defaultX: 150, defaultY: 60 },
  { id: 'getman',   name: 'API 测试', icon: '⚡', src: '/pages/getman.html',      defaultW: 700, defaultH: 480, defaultX: 250, defaultY: 80 },
];

const DEFAULT_OPEN = new Set(['files', 'terminal']);

export function AppShell(props) {
  // props.store — 共享 store（通过 window.__store 暴露给 iframe）
  // 注册全局 store（让 AppWindow 可以 expose 给子 iframe）
  window.__store = props.store;

  // ========== 窗口管理状态 ==========
  const [windows, setWindows] = createSignal([]);
  const [zCounter, setZCounter] = createSignal(10);

  // 初始化默认打开的 app
  let initialized = false;
  const ensureInit = () => {
    if (initialized) return;
    initialized = true;
    const initial = APPS
      .filter(a => DEFAULT_OPEN.has(a.id))
      .map((a, i) => ({ ...a, z: i + 1 }));
    setWindows(initial);
    setZCounter(initial.length + 1);
  };
  // 在 render body 中首次调用时初始化
  if (!initialized) ensureInit();

  // ========== 操作 ==========
  const openApp = (appId) => {
    const existing = windows().find(w => w.id === appId);
    if (existing) {
      if (existing.minimized) {
        setWindows(ws => ws.map(w => w.id === appId ? { ...w, minimized: false } : w));
      }
      bringToFront(appId);
      return;
    }
    const def = APPS.find(a => a.id === appId);
    if (!def) return;
    const z = zCounter();
    setZCounter(z + 1);
    setWindows(ws => [...ws, { ...def, z, minimized: false, fullscreen: false }]);
  };

  const closeApp = (appId) => {
    setWindows(ws => ws.filter(w => w.id !== appId));
  };

  const minimizeApp = (appId) => {
    setWindows(ws => ws.map(w => w.id === appId ? { ...w, minimized: true } : w));
  };

  const fullscreenApp = (appId) => {
    setWindows(ws => ws.map(w => {
      if (w.id !== appId) return w;
      if (w.fullscreen) {
        return { ...w, fullscreen: false, x: w.defaultX, y: w.defaultY, w: w.defaultW, h: w.defaultH };
      }
      return { ...w, fullscreen: true, x: 0, y: 0, w: window.innerWidth, h: window.innerHeight - 48 };
    }));
  };

  const bringToFront = (appId) => {
    const z = zCounter();
    setZCounter(z + 1);
    setWindows(ws => ws.map(w => w.id === appId ? { ...w, z } : w));
  };

  const openIds = createMemo(() => windows().map(w => w.id));

  // 暴露给 parent.html 的 store 方法（兼容现有 openGraph/closeGraph 等调用）
  if (props.store) {
    props.store.openGraph = () => openApp('graph');
    props.store.closeGraph = () => closeApp('graph');
    props.store.openGetman = () => openApp('getman');
    props.store.closeGetman = () => closeApp('getman');
    props.store.openScm = () => openApp('scm');
    props.store.closeScm = () => closeApp('scm');
  }

  return html`
    <div class="flex flex-col h-full">
      <!-- 桌面区域 -->
      <div class="flex-1 relative overflow-hidden bg-bg">
        <${For} each=${windows}>${(app) => html`
          <${AppWindow}
            app=${app}
            onClose=${closeApp}
            onFocus=${bringToFront}
            onMinimize=${minimizeApp}
            onFullscreen=${fullscreenApp}
          />
        `}<//>
      </div>
      <!-- Dock -->
      <${Dock}
        apps=${APPS}
        openIds=${openIds}
        onActivate=${openApp}
      />
    </div>
  `;
}
