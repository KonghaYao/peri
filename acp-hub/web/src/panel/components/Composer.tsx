// 发送窗口（Composer）：输入框 + 发送。
//
// 由 ChatView 拆出（右区三区之一）：输入区上方信息行显示模型 / effort /
// 上下文占用（均来自 agent map，server 写入的真实配置，缺失以 — 兜底）；
// 对话操作（新建/新会话/取消/关闭）已收敛到左侧对话列表区。

import { createSignal } from 'solid-js';
import { chatHead, chatStatusSignal, isTerminal, selectedCid, sendMessage } from '../store';

/** tokens 数值 → "12k"/"200k" 缩写（>=1000 取 k；非法值 → null）。 */
function fmtTokens(n: number | null): string | null {
  if (n === null) return null;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

export function Composer() {
  const [msg, setMsg] = createSignal('');

  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);
  const inputDisabled = () => !selectedCid() || terminal();
  const inputPlaceholder = () =>
    !selectedCid()
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
    setMsg('');
    sendMessage(text);
  }

  return (
    <section class="rounded-lg border border-slate-300 bg-white p-3">
      <div class="mb-2 flex items-center gap-3 text-xs text-slate-500">
        <span class="min-w-0 truncate" title={model()}>
          模型：{model()}
        </span>
        <span class="shrink-0" title="推理强度（agent 侧真实配置，只读）">
          effort：{effort()}
        </span>
        <span class="shrink-0" title="上下文占用">
          上下文：{ctxText()}
        </span>
      </div>
      <div class="flex gap-2">
        <input
          type="text"
          value={msg()}
          onInput={(e) => setMsg(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder={inputPlaceholder()}
          disabled={inputDisabled()}
          spellcheck={false}
          class="min-w-0 flex-1 rounded border border-slate-300 px-2 py-1 text-sm disabled:bg-slate-100 disabled:text-slate-400"
        />
        <button
          onClick={submit}
          disabled={inputDisabled()}
          class="rounded bg-blue-600 px-3 py-1 text-sm text-white hover:opacity-90 disabled:opacity-45"
        >
          发送
        </button>
      </div>
    </section>
  );
}
