// toast：底部浮现，2.5s 自动消失（store.toast 负责过期移除）。

import { For } from 'solid-js';
import { toasts } from '../store';

export function Toasts() {
  return (
    <div class="pointer-events-none fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 flex-col items-center gap-1.5">
      <For each={toasts()}>
        {(t) => (
          <div class="rounded-lg bg-slate-900/90 px-3 py-1.5 text-sm text-white shadow-lg">
            {t.msg}
          </div>
        )}
      </For>
    </div>
  );
}
