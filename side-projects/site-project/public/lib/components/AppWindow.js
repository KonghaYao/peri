// ========== AppWindow 组件 — macOS 风格浮动窗口 ==========

import html from 'solid-js/html';
import { createSignal, onMount, Show } from 'solid-js';
import { exposeToChild } from '/lib/comlink-bridge.js';

export function AppWindow(props) {
  const [loaded, setLoaded] = createSignal(false);
  let iframeRef = null;

  // 首次打开时加载 iframe src
  onMount(() => {
    if (iframeRef) iframeRef.src = props.app.src;
  });

  // iframe 加载完成后暴露 store
  const onLoad = () => {
    setLoaded(true);
    // 延迟一帧确保 contentWindow 就绪
    requestAnimationFrame(() => {
      if (window.__store && iframeRef?.contentWindow) {
        exposeToChild(window.__store, iframeRef);
      }
    });
  };

  const a = props.app;
  const w = a.w ?? a.defaultW ?? 600;
  const h = a.h ?? a.defaultH ?? 400;
  const x = a.x ?? a.defaultX ?? 100;
  const y = a.y ?? a.defaultY ?? 100;
  const z = a.z ?? 1;

  return html`
    <div class=${() => 'absolute overflow-hidden rounded-lg shadow-lg border border-border ' + (a.minimized ? 'hidden' : '')}
      style=${() => `left:${x}px;top:${y}px;width:${w}px;height:${h}px;z-index:${z}`}
      onMouseDown=${() => props.onFocus?.(a.id)}>

      <!-- 标题栏：红绿灯 + 标题 -->
      <div class="flex items-center h-7 px-2 bg-bg-secondary border-b border-border select-none">
        <div class="flex items-center gap-1.5 shrink-0">
          <button class="w-3 h-3 rounded-full bg-error cursor-pointer border-none p-0"
            title="关闭" onClick=${(e) => { e.stopPropagation(); props.onClose?.(a.id); }} />
          <button class="w-3 h-3 rounded-full bg-warning cursor-pointer border-none p-0"
            title="最小化" onClick=${(e) => { e.stopPropagation(); props.onMinimize?.(a.id); }} />
          <button class="w-3 h-3 rounded-full bg-success cursor-pointer border-none p-0"
            title="全屏" onClick=${(e) => { e.stopPropagation(); props.onFullscreen?.(a.id); }} />
        </div>
        <span class="text-[11px] text-text-muted flex-1 text-center truncate px-2">${a.name}</span>
        <div class="w-[52px] shrink-0" />
      </div>

      <!-- iframe -->
      <div class="relative bg-bg" style=${() => `height:calc(100% - 28px)`}>
        <${Show} when=${!loaded()}>
          <div class="absolute inset-0 flex items-center justify-center bg-bg z-10">
            <span class="text-text-muted text-xs">加载中...</span>
          </div>
        <//>
        <iframe
          ref=${el => { iframeRef = el; }}
          name=${a.id}
          class="w-full h-full border-none bg-bg"
          sandbox="allow-scripts allow-same-origin"
          onLoad=${onLoad}
        />
      </div>
    </div>
  `;
}
