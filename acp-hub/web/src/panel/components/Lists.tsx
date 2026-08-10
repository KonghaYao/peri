// 左栏列表：工作区 + 实例 + 对话 + ACP 会话（原 panel.html 左栏 + ui.js renderInstances/renderChats）。

import { createSignal, For, Show } from 'solid-js';
import {
  chats,
  createWorkspace,
  currentSessions,
  instances,
  openAcpSession,
  refreshSessions,
  removeWorkspace,
  selectChat,
  selectedCid,
  selectedWsId,
  setSelectedWsId,
  workspaces,
} from '../store';
import { Badge } from './Badge';

function shortId(id: string | null | undefined): string {
  if (!id) return '—';
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

// ACP 会话 id 是 UUID v7：前 12 位 = 毫秒时间戳，同一批（秒级内）创建的
// 会话前 8 位几乎必然相同（如 019fe6ee-* 一批）——短 id 必须带**尾部随机
// 段**（v7 区分度在尾部）才有辨识度，否则视觉上像同一会话重复出现。
function sessionShortId(id: string | null | undefined): string {
  if (!id) return '—';
  if (id.length <= 12) return id;
  return `${id.slice(0, 8)}…${id.slice(-4)}`;
}

// ── 工作区（独立于 chat 的上层概念，§6.3）──────────────────────────────

// 选中工作区后：对话列表按 workspace_id 过滤、ACP 会话按 cwd 过滤；
// 新建对话/进入历史会话继承该工作区 cwd。「全部」= 不过滤（兼容未归
// workspace 的旧数据）。
export function WorkspaceList() {
  const [name, setName] = createSignal('');
  const [cwd, setCwd] = createSignal('');
  const [expanded, setExpanded] = createSignal(false);

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    if (!cwd().trim()) return;
    createWorkspace(name().trim(), cwd().trim());
    setName('');
    setCwd('');
    setExpanded(false);
  };

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 text-sm font-semibold">
        工作区 <span class="font-normal text-slate-500">({workspaces().length})</span>
      </h2>
      <ul class="m-0 list-none p-0">
        <li
          onClick={() => setSelectedWsId(null)}
          class={`mb-1.5 cursor-pointer rounded px-1.5 py-1 text-sm hover:bg-slate-100 ${
            selectedWsId() === null ? 'bg-blue-50 ring-1 ring-blue-200' : ''
          }`}
        >
          全部（不过滤）
        </li>
        <For each={workspaces()}>
          {(w) => (
            <li
              onClick={() => setSelectedWsId(w.id)}
              class={`mb-1.5 cursor-pointer rounded px-1.5 py-1 hover:bg-slate-100 ${
                selectedWsId() === w.id ? 'bg-blue-50 ring-1 ring-blue-200' : ''
              }`}
            >
              <div class="flex items-center gap-1.5 text-sm">
                <span class="min-w-0 truncate">{w.name || shortId(w.id)}</span>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    removeWorkspace(w.id);
                  }}
                  class="ml-auto shrink-0 rounded px-1 text-xs text-slate-400 hover:bg-red-50 hover:text-red-600"
                  title="删除工作区定义（不影响已建对话）"
                >
                  删
                </button>
              </div>
              <div class="truncate text-xs text-slate-500" title={w.cwd}>
                {w.cwd}
              </div>
            </li>
          )}
        </For>
      </ul>
      <Show
        when={expanded()}
        fallback={
          <button
            onClick={() => setExpanded(true)}
            class="mt-1 w-full rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-100"
          >
            + 新建工作区
          </button>
        }
      >
        <form onSubmit={submit} class="mt-2 flex flex-col gap-1.5">
          <input
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
            placeholder="名称（留空 = 目录名）"
            class="w-full rounded border border-slate-300 px-1.5 py-1 text-xs"
          />
          <input
            value={cwd()}
            onInput={(e) => setCwd(e.currentTarget.value)}
            placeholder="本地目录（绝对路径，须已存在）"
            class="w-full rounded border border-slate-300 px-1.5 py-1 text-xs"
          />
          <div class="flex gap-1.5">
            <button
              type="submit"
              class="flex-1 rounded bg-blue-600 px-2 py-1 text-xs text-white hover:bg-blue-700 disabled:opacity-50"
              disabled={!cwd().trim()}
            >
              创建
            </button>
            <button
              type="button"
              onClick={() => setExpanded(false)}
              class="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-100"
            >
              取消
            </button>
          </div>
        </form>
      </Show>
    </section>
  );
}

export function InstanceList() {  const list = () =>
    instances().slice().sort((a, b) => String(a.hostname).localeCompare(String(b.hostname)));

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 text-sm font-semibold">
        实例 <span class="font-normal text-slate-500">({instances().length})</span>
      </h2>
      <ul class="m-0 list-none p-0">
        <For each={list()}>
          {(m) => (
            <li class="mb-1.5 rounded px-1.5 py-1 hover:bg-slate-100">
              <div class="flex items-center gap-1.5 text-sm">
                <span class="min-w-0 truncate">{m.hostname || m.id || '?'}</span>
                <Badge status={m.status} />
              </div>
              <div class="text-xs text-slate-500">
                对话 {m.chatCount === null || m.chatCount === undefined ? '—' : String(m.chatCount)} ·{' '}
                {shortId(m.id)}
              </div>
            </li>
          )}
        </For>
      </ul>
    </section>
  );
}

export function ChatList() {
  // 只显示非终态对话（accepting/active）；ended/closed/crashed 的历史对话
  // 不再展示（终态 chat 的 ACP 进程已退出，无操作价值）。
  // 按选中工作区过滤（§6.3）：workspace_id 匹配；「全部」= 不过滤。
  const active = () => {
    const wsId = selectedWsId();
    return chats().filter(
      (s) =>
        !['ended', 'closed', 'crashed'].includes(s.status || '') &&
        (wsId === null || s.workspaceId === wsId),
    );
  };
  const list = () =>
    active()
      .slice()
      .sort((a, b) => String(b.updatedAt || '').localeCompare(String(a.updatedAt || '')));

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 text-sm font-semibold">
        对话 <span class="font-normal text-slate-500">({active().length})</span>
      </h2>
      <ul class="m-0 list-none p-0">
        <For each={list()}>
          {(s) => {
            const isSel = () => selectedCid() === s.id;
            return (
              <li
                onClick={() => selectChat(s.id)}
                class={`mb-1.5 cursor-pointer rounded px-1.5 py-1 hover:bg-slate-100 ${
                  isSel() ? 'bg-blue-50 ring-1 ring-blue-200' : ''
                }`}
              >
                <div class="flex items-center gap-1.5 text-sm">
                  <span class="min-w-0 truncate">{s.title || shortId(s.id)}</span>
                  <Badge status={s.status} />
                </div>
                <div class="text-xs text-slate-500">{shortTime(s.updatedAt)}</div>
              </li>
            );
          }}
        </For>
      </ul>
    </section>
  );
}

// RFC3339 → 本地 HH:MM:SS（空串/非法 → '—'）。
function shortTime(s: string | null): string {
  if (!s || s === '—') return '—';
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleTimeString();
}

// ── ACP 会话（agent 磁盘历史）────────────────────────────────────────────

// ACP 会话 ≠ hub 对话：前者是 ACP agent 的磁盘历史会话（agent 侧真实
// 数据源，§6.3），后者是 hub 控制面的对话（Registry）。会话是**进程内
// 实体**：一个对话（= 一个 ACP 进程）可先后持有多个会话，load 即切换
// （§8.5）——点击历史会话 = 在当前对话内 load，**不新建对话/进程**。
//
// §6.3 按需查询：切换对话时向 agent 侧发 session/list（携带该对话 cwd），
// 结果**按对话隔离缓存**（sessionsByChat）——SessionList 跟随当前选中
// 对话显示；10s 定时刷新 + 手动刷新按钮。不再按工作区过滤投影数据。
export function SessionList() {
  const [busy, setBusy] = createSignal(false);
  const refresh = () => {
    setBusy(true);
    refreshSessions();
    setTimeout(() => setBusy(false), 300);
  };
  const sessions = () => currentSessions();

  return (
    <section class="mb-3 rounded-lg border border-dashed border-slate-300 bg-slate-50 p-3">
      <h2 class="mb-1 flex items-center text-sm font-semibold">
        ACP 会话{' '}
        <span class="font-normal text-slate-500">
          ({sessions().length})
          {selectedCid() !== null ? ' · 当前对话' : ''}
        </span>
        <Show when={selectedCid() !== null}>
          <button
            onClick={refresh}
            disabled={busy()}
            class="ml-auto shrink-0 rounded border border-slate-300 bg-white px-1.5 py-0.5 text-[11px] text-slate-600 hover:bg-slate-100 disabled:opacity-50"
            title="向 agent 侧重新查询（10s 自动刷新）"
          >
            刷新
          </button>
        </Show>
      </h2>
      <p class="mb-2 text-[11px] leading-4 text-slate-500">
        agent 磁盘历史（真实数据源 · 切换对话时按需查询）· 点击历史会话 = 当前对话内切换
      </p>
      <Show
        when={selectedCid() !== null}
        fallback={<p class="text-xs text-slate-400">未选择对话（切换对话后按需查询）</p>}
      >
        <Show
          when={sessions().length > 0}
          fallback={<p class="text-xs text-slate-400">暂无 ACP 会话（10s 自动刷新）</p>}
        >
          <ul class="m-0 list-none p-0">
            <For each={sessions()}>
              {(s) => {
                // §8.5 会话切换语义：会话是**进程内实体**——列表属于当前
                // 对话（进程），boundChatId 标注该对话的**当前活跃会话**
                // （标「当前」，点击无操作）；其余为历史会话，点击 = 在
                // 当前对话内 load 切换（不新建对话/进程）。
                const isCurrent = s.boundChatId === selectedCid();
                const open = () => {
                  if (isCurrent) return;
                  openAcpSession(s.sessionId, s.title || undefined);
                };
                return (
                <li
                  onClick={open}
                  class={`mb-1.5 cursor-pointer rounded bg-white px-1.5 py-1 ring-1 ring-slate-200 hover:bg-blue-50 hover:ring-blue-200 ${isCurrent ? 'ring-emerald-300 bg-emerald-50/60' : ''}`}
                  title={isCurrent ? '当前会话' : '在当前对话内切换到此会话（不新建对话）'}
                >
                  <div class="flex items-center gap-1.5 text-sm">
                    <Show
                      when={s.title}
                      fallback={<span class="italic text-slate-400">（无标题）</span>}
                    >
                      <span class="min-w-0 truncate" title={s.title || ''}>
                        {s.title}
                      </span>
                    </Show>
                    <Show when={s.status}>
                      <Badge status={s.status} />
                    </Show>
                    <Show when={isCurrent}>
                      <span class="ml-auto shrink-0 rounded bg-emerald-100 px-1 py-0.5 text-[10px] text-emerald-700">
                        当前
                      </span>
                    </Show>
                  </div>
                  <div class="text-xs text-slate-500">
                    {shortTime(s.updatedAt)} · {sessionShortId(s.sessionId)}
                    {s.cwd ? ` · ${s.cwd}` : ''}
                  </div>
                </li>
                );
              }}
            </For>
          </ul>
        </Show>
      </Show>
    </section>
  );
}