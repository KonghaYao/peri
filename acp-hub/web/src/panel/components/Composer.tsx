// 发送窗口（Composer）：输入区 + 底部工具行（ui.md §3.8 / §四.7）。
//
// 由 ChatView 拆出（右区三区之一）：悬浮大圆角卡片，textarea 自动增高
// （max 180px 后内部滚动），Enter 发送 / Shift+Enter 换行（含 IME
// 组合态防护）；底部工具行显示模型 / effort / 上下文占用（均来自 agent
// map，server 写入的真实配置，缺失以 — 兜底），空间不足时依次隐藏
// 上下文、effort 文案，发送按钮始终保留。
// 对话操作（新建/新会话/取消/关闭）已收敛到左侧对话列表区。

import { createSignal } from 'solid-js';
import { chatHead, chatStatusSignal, isTerminal, openingSessionId, readOnly, selectedCid, sendMessage } from '../store';

/** tokens 数值 → "12k"/"200k" 缩写（>=1000 取 k；非法值 → null）。 */
function fmtTokens(n: number | null): string | null {
  if (n === null) return null;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

export function Composer() {
  const [msg, setMsg] = createSignal('');
  let taRef: HTMLTextAreaElement | undefined;

  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);
  const inputDisabled = () => !selectedCid() || terminal() || !!openingSessionId() || readOnly();
  const inputPlaceholder = () =>
    readOnly() ? '只读模式' : openingSessionId() ? '正在打开会话…' : !selectedCid()
      ? '输入消息，Enter 发送（需先选中对话）'
      : terminal()
        ? '对话已结束（历史只读）'
        : '输入消息，Enter 发送（需先选中对话）';

  // 信息行三个真实值（agent map，server 写入；缺失 → —）。
  const model = () => chatHead()?.agent?.model || '—';
  const effort = () => chatHead()?.agent?.effort || '—';

  // 上下文占用（tokens）：12k/200k；任一缺失显示 —。
  const ctxText = () => {
    const used = fmtTokens(chatHead()?.agent?.contextUsed ?? null);
    const cap = fmtTokens(chatHead()?.agent?.contextWindow ?? null);
    if (used === null || cap === null) return '—';
    return `${used}/${cap}`;
  };

  function submit() {
    const text = msg().trim();
    if (!text) return;
    if (taRef) {
      // 先同步清空 DOM 值再测量：value 绑定是延迟 effect，若在
      // setMsg('') 后立即测 scrollHeight 会测到旧多行文本的高度，
      // 发送后 Composer 保持展开高度不收回（min-h 52px 兜底）
      taRef.value = '';
      taRef.style.height = 'auto';
      taRef.style.height = `${taRef.scrollHeight}px`;
    }
    setMsg('');
    sendMessage(text);
  }

  return (
    <div class="composer-wrap">
      <section
        aria-disabled={inputDisabled()}
        class="composer-surface"
      >
        <textarea
          ref={taRef}
          value={msg()}
          onInput={(e) => {
            const el = e.currentTarget;
            setMsg(el.value);
            // 有限自动增高：先复位再取 scrollHeight，max-h-[180px] + 内部滚动兜底
            el.style.height = 'auto';
            el.style.height = `${el.scrollHeight}px`;
          }}
          onKeyDown={(e) => {
            if (e.isComposing) return; // IME 组合确认回车不误发
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder={inputPlaceholder()}
          disabled={inputDisabled()}
          spellcheck={false}
          class="composer-input ui-scrollbar"
        />
        <div class="composer-toolbar">
          <span class="min-w-0 truncate text-xs text-[var(--text-muted)]" title={model()}>
            模型：{model()}
          </span>
          <span class="shrink-0 text-xs text-[var(--text-muted)] max-[480px]:hidden" title="推理强度（agent 侧真实配置，只读）">
            effort：{effort()}
          </span>
          <span class="shrink-0 text-xs text-[var(--text-muted)] max-lg:hidden" title="上下文占用">
            上下文：{ctxText()}
          </span>
          <button
            type="button"
            onClick={submit}
            disabled={inputDisabled()}
            aria-label="发送"
            title="发送"
            class="ml-auto flex size-10 shrink-0 items-center justify-center rounded-full bg-[var(--btn-primary)] text-white transition-colors duration-[120ms] ease-out enabled:hover:bg-[var(--btn-primary-hover)] disabled:cursor-not-allowed disabled:bg-[var(--border-subtle)] disabled:text-[var(--text-faint)]"
          >
            <svg
              viewBox="0 0 20 20"
              fill="none"
              stroke="currentColor"
              stroke-width="1.75"
              stroke-linecap="round"
              stroke-linejoin="round"
              class="h-5 w-5"
              aria-hidden="true"
            >
              <path d="M10 16V4" />
              <path d="M5 9l5-5 5 5" />
            </svg>
          </button>
        </div>
      </section>
    </div>
  );
}
