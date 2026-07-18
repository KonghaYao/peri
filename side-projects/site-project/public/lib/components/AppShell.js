// ========== AppShell 组件 — Perihelion 工作台 Shell ==========
// 布局：顶栏 / [活动栏 | 侧边面板 | 主区(预览) + 底部终端] / 状态栏
// Graph / Getman 以模态浮层打开（backdrop + 弹出动画，Esc 关闭）。

import html from 'solid-js/html';
import { createSignal, onMount, onCleanup, For, Show } from 'solid-js';
import { HostedIframe } from '/lib/components/HostedIframe.js';
import { Icon } from '/lib/ui/icons.js';

// ========== 面板定义 ==========
const SIDE_VIEWS = [
  { id: 'files', name: '文件', icon: 'folder', src: '/pages/file-tree.html' },
  { id: 'scm', name: '源代码管理', icon: 'git-branch', src: '/pages/scm.html' },
];

const OVERLAYS = {
  graph: { id: 'graph', name: 'Git 图谱', src: '/pages/graph.html', w: 'min(1160px, 94vw)', h: 'min(760px, 88vh)' },
  getman: { id: 'getman', name: 'API 测试', src: '/pages/getman.html', w: 'min(860px, 92vw)', h: 'min(680px, 88vh)' },
};

const clamp = (v, min, max) => Math.min(max, Math.max(min, v));

export function AppShell(props) {
  const store = props.store;
  // 注册全局 store（让 HostedIframe / 子 iframe 可 expose）
  window.__store = store;

  // ========== 布局状态（hydrate from store） ==========
  const [theme, setTheme] = createSignal(store.get('theme') || 'dark');
  const [sidebarOpen, setSidebarOpen] = createSignal(store.get('sidebarOpen') !== false);
  const [sideView, setSideView] = createSignal(store.get('activeSideView') || 'files');
  const [terminalOpen, setTerminalOpen] = createSignal(store.get('terminalOpen') !== false);
  const [sidebarWidth, setSidebarWidth] = createSignal(store.get('sidebarWidth') || 272);
  const [panelHeight, setPanelHeight] = createSignal(store.get('panelHeight') || 260);
  const [currentFile, setCurrentFile] = createSignal(store.get('currentFile'));
  const [statusText, setStatusText] = createSignal('');
  const [overlay, setOverlay] = createSignal(null);   // 'graph' | 'getman' | null
  const [dragging, setDragging] = createSignal(null); // 'side' | 'term' | null

  // overlay iframe host API（lazy 加载）
  const overlayHosts = {};

  // ========== store 订阅 ==========
  store.subscribe('currentFile', (f) => setCurrentFile(f));
  store.subscribe('theme', (t) => { if (t) setTheme(t); });

  // ========== 操作 ==========
  const toggleTheme = () => store.set('theme', theme() === 'dark' ? 'light' : 'dark');

  const toggleSidebar = () => {
    const next = !sidebarOpen();
    setSidebarOpen(next);
    store.set('sidebarOpen', next);
  };

  const toggleTerminal = () => {
    const next = !terminalOpen();
    setTerminalOpen(next);
    store.set('terminalOpen', next);
  };

  const switchSideView = (id) => {
    if (sideView() === id && sidebarOpen()) {
      toggleSidebar();   // 再点当前视图图标 → 折叠面板
      return;
    }
    setSideView(id);
    store.set('activeSideView', id);
    if (!sidebarOpen()) { setSidebarOpen(true); store.set('sidebarOpen', true); }
  };

  const openOverlay = (id) => {
    setOverlay(id);
    const host = overlayHosts[id];
    if (host) {
      host.open();   // lazy 首次设置 src
      // graph 每次打开刷新数据
      if (id === 'graph' && host.loaded()) host.call('refresh');
    }
  };
  const closeOverlay = () => setOverlay(null);

  // ========== 暴露给子 iframe 的父方法（保持现有契约） ==========
  store.openGraph = () => openOverlay('graph');
  store.closeGraph = () => closeOverlay();
  store.openGetman = () => openOverlay('getman');
  store.closeGetman = () => closeOverlay();
  store.openScm = () => switchSideView('scm');
  store.setStatusText = (text) => setStatusText(text || '');
  store.toggleTerminal = toggleTerminal;
  store.toggleSidebar = toggleSidebar;
  store.closeOverlays = closeOverlay;

  // ========== 拖拽调整尺寸 ==========
  const startDrag = (e, type) => {
    e.preventDefault();
    setDragging(type);
    const startX = e.clientX;
    const startY = e.clientY;
    const startW = sidebarWidth();
    const startH = panelHeight();
    const move = (ev) => {
      if (type === 'side') {
        setSidebarWidth(clamp(startW + ev.clientX - startX, 200, 480));
      } else {
        setPanelHeight(clamp(startH - (ev.clientY - startY), 140, window.innerHeight - 240));
      }
    };
    const up = () => {
      setDragging(null);
      store.set(type === 'side' ? 'sidebarWidth' : 'panelHeight',
        type === 'side' ? sidebarWidth() : panelHeight());
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  // ========== 键盘快捷键 ==========
  const onKeyDown = (e) => {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && (e.key === '`' || e.key === '~' || e.key === 'j' || e.key === 'J')) {
      e.preventDefault();
      toggleTerminal();
    } else if (mod && (e.key === 'b' || e.key === 'B')) {
      e.preventDefault();
      toggleSidebar();
    } else if (e.key === 'Escape') {
      closeOverlay();
    }
  };
  onMount(() => {
    window.addEventListener('keydown', onKeyDown);
    onCleanup(() => window.removeEventListener('keydown', onKeyDown));
  });

  // ========== 面包屑 ==========
  const crumbs = () => {
    const f = currentFile();
    if (!f?.path) return [];
    return f.path.split('/');
  };

  // ========== 活动栏按钮 ==========
  const ActivityButton = (p) => html`
    <button
      title=${p.title}
      class=${() => [
        'relative w-9 h-9 mx-auto flex items-center justify-center rounded-lg border-none cursor-pointer transition-colors duration-150',
        p.active ? 'text-accent bg-bg-active' : 'text-text-muted hover:text-text hover:bg-bg-hover bg-transparent',
      ].join(' ')}
      onClick=${p.onClick}
    >
      <span class=${'absolute left-[-5px] top-1/2 -translate-y-1/2 w-[2.5px] h-4 rounded-full transition-colors duration-150 ' + (p.active ? 'bg-accent' : 'bg-transparent')} />
      <${Icon} name=${p.icon} class="w-[18px] h-[18px]" />
    </button>
  `;

  return html`
    <div class=${'flex flex-col h-full ' + (dragging() ? 'shell-dragging' : '')}>

      <!-- ===== 顶栏 ===== -->
      <header class="flex items-center h-10 px-3 gap-2.5 border-b border-border bg-bg shrink-0 select-none">
        <div class="flex items-center gap-2 text-accent">
          <${Icon} name="orbit" class="w-5 h-5" />
          <span class="text-[13px] font-semibold tracking-wide text-text">Perihelion</span>
        </div>

        <div class="w-px h-4 bg-border mx-1" />

        <!-- 面包屑 -->
        <div class="flex items-center gap-1 min-w-0 flex-1 text-[12px]">
          <${Show} when=${() => crumbs().length > 0}
            fallback=${() => html`<span class="text-text-muted">工作台</span>`}>
            <${For} each=${crumbs}>${(c, i) => html`
              <span class="flex items-center gap-1 min-w-0">
                <${Show} when=${() => i() > 0}>
                  <${Icon} name="chevron-right" class="w-3 h-3 text-text-muted shrink-0" />
                <//>
                <span class=${'truncate ' + (i() === crumbs().length - 1 ? 'text-text font-medium' : 'text-text-muted')}>${c}</span>
              </span>
            `}<//>
          <//>
        </div>

        <!-- 顶栏操作 -->
        <div class="flex items-center gap-0.5">
          <button title=${() => sidebarOpen() ? '隐藏侧边栏 (Ctrl+B)' : '显示侧边栏 (Ctrl+B)'}
            class=${() => 'w-7 h-7 flex items-center justify-center rounded-md border-none cursor-pointer transition-colors duration-150 ' + (sidebarOpen() ? 'text-accent bg-bg-active' : 'text-text-muted hover:text-text hover:bg-bg-hover bg-transparent')}
            onClick=${toggleSidebar}>
            <${Icon} name="panel-left" class="w-4 h-4" />
          </button>
          <button title=${() => terminalOpen() ? '隐藏终端 (Ctrl+`)' : '显示终端 (Ctrl+`)'}
            class=${() => 'w-7 h-7 flex items-center justify-center rounded-md border-none cursor-pointer transition-colors duration-150 ' + (terminalOpen() ? 'text-accent bg-bg-active' : 'text-text-muted hover:text-text hover:bg-bg-hover bg-transparent')}
            onClick=${toggleTerminal}>
            <${Icon} name="panel-bottom" class="w-4 h-4" />
          </button>
          <div class="w-px h-4 bg-border mx-1.5" />
          <button title=${() => theme() === 'dark' ? '切换到亮色主题' : '切换到暗色主题'}
            class="w-7 h-7 flex items-center justify-center rounded-md border-none cursor-pointer text-text-muted hover:text-text hover:bg-bg-hover bg-transparent transition-colors duration-150"
            onClick=${toggleTheme}>
            <${Icon} name=${() => theme() === 'dark' ? 'sun' : 'moon'} class="w-4 h-4" />
          </button>
        </div>
      </header>

      <!-- ===== 主行 ===== -->
      <div class="flex flex-1 min-h-0">

        <!-- 活动栏 -->
        <nav class="flex flex-col items-center w-11 py-2 gap-1 bg-bg-secondary border-r border-border shrink-0 select-none">
          <${For} each=${SIDE_VIEWS}>${(v) => html`
            <${ActivityButton}
              title=${v.name}
              icon=${v.icon}
              active=${() => sideView() === v.id && sidebarOpen()}
              onClick=${() => switchSideView(v.id)}
            />
          `}<//>
          <${ActivityButton}
            title="终端 (Ctrl+`)"
            icon="terminal"
            active=${() => terminalOpen()}
            onClick=${toggleTerminal}
          />
          <div class="flex-1" />
          <${ActivityButton} title="Git 图谱" icon="graph" active=${() => overlay() === 'graph'} onClick=${() => openOverlay('graph')} />
          <${ActivityButton} title="API 测试" icon="zap" active=${() => overlay() === 'getman'} onClick=${() => openOverlay('getman')} />
        </nav>

        <!-- 侧边面板 -->
        <aside
          class=${'bg-bg-secondary border-r border-border shrink-0 overflow-hidden flex flex-col ' + (dragging() ? '' : 'transition-[width] duration-200 ease-out')}
          style=${() => `width:${sidebarOpen() ? sidebarWidth() : 0}px;border-right-width:${sidebarOpen() ? 1 : 0}px`}
        >
          <div class="flex-1 min-h-0 relative" style=${() => `width:${sidebarWidth()}px`}>
            <${For} each=${SIDE_VIEWS}>${(v) => html`
              <div class="absolute inset-0" style=${() => sideView() === v.id ? '' : 'display:none'}>
                <${HostedIframe} src=${v.src} name=${'side-' + v.id} store=${store} class="w-full h-full border-none" />
              </div>
            `}<//>
          </div>
        </aside>

        <!-- 侧边拖拽手柄 -->
        <${Show} when=${sidebarOpen}>
          <div
            class=${() => 'w-[5px] shrink-0 cursor-col-resize transition-colors duration-150 hover:bg-accent/60 ' + (dragging() === 'side' ? 'bg-accent' : 'bg-transparent')}
            onPointerDown=${(e) => startDrag(e, 'side')}
          />
        <//>

        <!-- 中央列：预览 + 终端 -->
        <main class="flex-1 flex flex-col min-w-0 min-h-0">
          <div class="flex-1 min-h-0 relative bg-bg">
            <${HostedIframe} src="/pages/preview.html" name="preview" store=${store} class="w-full h-full border-none" />
          </div>

          <!-- 终端面板 -->
          <div
            class=${'shrink-0 overflow-hidden border-t border-border flex flex-col ' + (dragging() ? '' : 'transition-[height] duration-200 ease-out')}
            style=${() => `height:${terminalOpen() ? panelHeight() : 0}px;border-top-width:${terminalOpen() ? 1 : 0}px`}
          >
            <!-- 终端拖拽手柄 -->
            <div
              class=${() => 'h-[5px] shrink-0 cursor-row-resize transition-colors duration-150 hover:bg-accent/60 ' + (dragging() === 'term' ? 'bg-accent' : 'bg-transparent')}
              onPointerDown=${(e) => startDrag(e, 'term')}
            />
            <div class="flex-1 min-h-0">
              <${HostedIframe} src="/pages/terminal.html" name="terminal" store=${store} class="w-full h-full border-none" />
            </div>
          </div>
        </main>
      </div>

      <!-- ===== 状态栏 ===== -->
      <footer class="flex items-center justify-between h-[26px] px-3 bg-bg-secondary border-t border-border text-[11px] text-text-muted shrink-0 select-none">
        <div class="flex items-center gap-1.5 min-w-0">
          <${Icon} name="git-branch" class="w-3 h-3 shrink-0" />
          <span class="truncate">${() => statusText() || '就绪'}</span>
        </div>
        <div class="flex items-center gap-3 shrink-0">
          <span class="font-mono truncate max-w-[360px]">${() => currentFile()?.path || ''}</span>
        </div>
      </footer>

      <!-- ===== 模态浮层（Graph / Getman） ===== -->
      <${For} each=${Object.values(OVERLAYS)}>${(o) => html`
        <div
          class="fixed inset-0 z-50 flex items-center justify-center p-4"
          style=${() => overlay() === o.id ? '' : 'display:none'}
        >
          <div class="absolute inset-0 bg-overlay overlay-fade" onClick=${closeOverlay} />
          <div
            class="relative overlay-pop bg-bg border border-border-strong rounded-xl shadow-2xl flex flex-col overflow-hidden"
            style=${`width:${o.w};height:${o.h}`}
          >
            <${HostedIframe}
              src=${o.src}
              name=${o.id}
              store=${store}
              lazy=${true}
              class="w-full h-full border-none"
              ref=${(api) => { overlayHosts[o.id] = api; }}
            />
          </div>
        </div>
      `}<//>
    </div>
  `;
}
