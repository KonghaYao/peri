// toast：响应式定位（宽屏右下角、小屏顶部居中，§四.9 避免遮挡
// Composer 与 drawer），2.5s 自动消失（store.toast 负责过期移除，
// 生命周期不变）。进入只做 opacity + translateY(4px)（§3.8 Toast /
// §3.11；离开由 store 直接移除，无离开动画）。
// 容器 pointer-events-none + 单项 pointer-events-auto；z-50 维持既有
// stacking context；reduced-motion 由 motion-reduce 关闭位移。

import { createSignal, For, onMount } from 'solid-js';
import { toasts } from '../store';

function ToastItem(props: { msg: string }) {
  const [shown, setShown] = createSignal(false);
  // rAF 确保初始 opacity-0/translate-y-1 先完成首帧绘制，transition 才生效。
  onMount(() => {
    requestAnimationFrame(() => setShown(true));
  });
  return (
    <div
      class={`pointer-events-auto w-[min(360px,calc(100vw_-_32px))] rounded-xl border border-[var(--border-subtle)] bg-white px-3 py-2 text-sm text-[var(--text-primary)] shadow-[var(--shadow-popover)] transition-all duration-150 motion-reduce:transition-none ${
        shown() ? 'translate-y-0 opacity-100' : 'translate-y-1 opacity-0'
      }`}
    >
      {props.msg}
    </div>
  );
}

export function Toasts() {
  return (
    <div
      aria-live="polite"
      class="pointer-events-none fixed top-4 left-1/2 z-50 flex -translate-x-1/2 flex-col items-center gap-1.5 sm:right-6 sm:bottom-6 sm:top-auto sm:left-auto sm:translate-x-0 sm:items-end"
    >
      <For each={toasts()}>{(t) => <ToastItem msg={t.msg} />}</For>
    </div>
  );
}
