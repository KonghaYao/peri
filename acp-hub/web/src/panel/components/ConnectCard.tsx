// 左栏「连接」卡片：token 输入 + 连接/断开（原 panel.html 连接区 + ui.js）。

import { busy, connect, disconnect, tokenInput, setTokenInput } from '../store';

export function ConnectCard() {
  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 text-sm font-semibold">连接</h2>
      <div class="flex gap-2">
        <input
          type="password"
          value={tokenInput()}
          onInput={(e) => setTokenInput(e.currentTarget.value)}
          placeholder="粘贴 full 角色 token"
          spellcheck={false}
          class="min-w-0 flex-1 rounded border border-slate-300 px-2 py-1 text-sm"
        />
        <button
          onClick={() => connect(tokenInput())}
          disabled={busy()}
          class="whitespace-nowrap rounded bg-blue-600 px-3 py-1 text-sm text-white hover:opacity-90 disabled:opacity-45"
        >
          连接
        </button>
        <button
          onClick={disconnect}
          disabled={busy()}
          class="whitespace-nowrap rounded bg-slate-500 px-3 py-1 text-sm text-white hover:opacity-90 disabled:opacity-45"
        >
          断开
        </button>
      </div>
      <p class="mt-1.5 text-xs text-slate-500">
        签发：<code>acp-hub-server token generate --name web-panel --role full</code>
        （read-only 档发 action 会被拒，需 full 档）
      </p>
    </section>
  );
}
