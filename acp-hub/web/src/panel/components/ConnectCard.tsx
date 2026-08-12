// 左栏底部连接条 + 设置 popover（ui.md 步骤 3）：默认只显示连接状态与
// 齿轮设置按钮；点击展开 token 输入（password）与连接/断开动作。token
// 仍只进 sessionStorage、不明文展示；connect / disconnect / tokenInput
// 行为零改动。CLI 签发说明移入展开区域。
//
// popover 交互与 ChatHeader ACP 会话 tooltip 同模式：点击外部或按
// Escape 关闭（本组件自持 signal，不进 store）；向上弹出浮于列表之上，
// 不锁 ChatList 滚动。

import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { busy, connState, connect, disconnect, setTokenInput, tokenInput } from '../store';
import { Icon } from './Lists';

// 状态文字颜色（§3.2 语义色；状态文字本身即数据，不只靠颜色表达）。
function stateTextCls(kind: string): string {
  if (kind === 'ok') return 'text-[var(--success)]';
  if (kind === 'warn') return 'text-[var(--warning)]';
  if (kind === 'err') return 'text-[var(--danger)]';
  return 'text-[var(--text-secondary)]';
}

export function ConnectCard() {
  const [open, setOpen] = createSignal(false);
  let rootRef: HTMLDivElement | undefined;

  // 点击外部 / Escape 关闭（document 监听 + contains 判定）。
  onMount(() => {
    const onDocClick = (e: MouseEvent) => {
      if (!rootRef?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('click', onDocClick);
    document.addEventListener('keydown', onKey);
    onCleanup(() => {
      document.removeEventListener('click', onDocClick);
      document.removeEventListener('keydown', onKey);
    });
  });

  const state = () => connState();

  return (
    <div ref={rootRef} class="relative shrink-0 border-t border-[var(--divider)] py-4">
      <div class="flex items-center gap-2">
        <div class="flex min-w-0 flex-1 items-center gap-2">
          <Show
            when={busy()}
            fallback={
              <span
                class={`h-2 w-2 shrink-0 rounded-full ${
                  state().kind === 'ok' ? 'bg-[var(--success)]' : 'bg-[var(--text-faint)]'
                }`}
                aria-hidden="true"
              />
            }
          >
            {/* 连接中：12px spinner（§3.9），按钮保持原宽度防止跳动 */}
            <span
              class="h-3 w-3 shrink-0 animate-spin rounded-full border-2 border-[var(--text-faint)] border-t-[var(--text-secondary)]"
              aria-hidden="true"
            />
          </Show>
          <span class={`truncate text-sm ${stateTextCls(state().kind)}`}>{state().text}</span>
        </div>
        <button
          onClick={() => setOpen((v) => !v)}
          aria-label="连接设置"
          aria-expanded={open()}
          class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg text-[var(--text-secondary)] transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)] md:h-8 md:w-8"
        >
          <Icon name="gear" class="h-5 w-5" />
        </button>
      </div>
      <Show when={open()}>
        {/* 向上弹出：140ms opacity + 4px 上移（§3.11） */}
        <div class="absolute right-0 bottom-full left-0 z-20 mb-2 animate-[ui-popover-in_140ms_ease-out] rounded-xl border border-[var(--border-subtle)] bg-[var(--surface)] p-3 shadow-[var(--shadow-popover)]">
          <input
            type="password"
            value={tokenInput()}
            onInput={(e) => setTokenInput(e.currentTarget.value)}
            placeholder="粘贴 full 角色 token"
            spellcheck={false}
            class="h-9 w-full rounded-lg border border-[var(--border-strong)] bg-[var(--surface)] px-2.5 text-sm text-[var(--text-primary)] placeholder:text-[var(--text-faint)] focus:border-[var(--accent)] focus:outline-2 focus:outline-[var(--focus-ring)]"
          />
          <div class="mt-2 flex gap-2">
            <button
              onClick={() => connect(tokenInput())}
              disabled={busy()}
              class="h-9 flex-1 rounded-lg bg-[var(--btn-primary)] px-2.5 text-sm text-white transition-colors duration-[120ms] ease-out hover:bg-[var(--btn-primary-hover)] disabled:cursor-not-allowed disabled:opacity-45"
            >
              连接
            </button>
            <button
              onClick={disconnect}
              disabled={busy()}
              class="h-9 rounded-lg border border-[var(--border-strong)] px-2.5 text-sm text-[var(--text-secondary)] transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)] disabled:cursor-not-allowed disabled:opacity-45"
            >
              断开
            </button>
          </div>
          <p class="mt-2 text-xs leading-[17px] text-[var(--text-muted)]">
            签发：<code>acp-hub-server token generate --name web-panel --role full</code>
            （read-only 档发 action 会被拒，需 full 档）
          </p>
        </div>
      </Show>
    </div>
  );
}
