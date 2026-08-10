// 对话头部：title + meta。
//
// title 点击 → tooltip 展示当前对话的 ACP 会话列表（agent 磁盘历史，
// 原左栏 SessionList 迁入，§6.3/§8.5）：点击历史会话 = 当前对话内 load
// 切换（不新建对话/进程）；「当前」会话（boundChatId === selectedCid）
// 标徽标且点击无操作。tooltip 交互：点击 title 切换开/关，点击外部或
// Esc 关闭。会话数据来自 store 按对话隔离缓存（currentSessions）。
// 对话操作（新建/新会话/取消/关闭）已收敛到左侧对话列表区。

import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import {
  chatHead,
  chatStatusSignal,
  currentSessions,
  isTerminal,
  openAcpSession,
  refreshSessions,
  selectedCid,
} from '../store';
import { Badge } from './Badge';

function shortId(id: string | null | undefined, n = 8): string {
  if (!id) return '—';
  return id.length > n ? `${id.slice(0, n)}…` : id;
}

// ACP 会话 id 是 UUID v7：前 12 位 = 毫秒时间戳，同一批（秒级内）创建的
// 会话前 8 位几乎必然相同（如 019fe6ee-* 一批）——短 id 必须带**尾部随机
// 段**（v7 区分度在尾部）才有辨识度，否则视觉上像同一会话重复出现。
function sessionShortId(id: string | null | undefined): string {
  if (!id) return '—';
  if (id.length <= 12) return id;
  return `${id.slice(0, 8)}…${id.slice(-4)}`;
}

// RFC3339 → 本地 HH:MM:SS（空串/非法 → '—'）。
function shortTime(s: string | null): string {
  if (!s || s === '—') return '—';
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleTimeString();
}

export function ChatHeader() {
  const ctrl = () => chatHead()?.chat;
  const activeTurn = () => chatHead()?.activeTurn;
  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);

  // ── ACP 会话 tooltip：点击 title 切换开/关；点击 title/tooltip 之外的
  //    任意处或按 Esc 关闭（document 监听 + contains 判定，title 与 tooltip
  //    同挂在 h2 下，rootRef 覆盖两者）。
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  let rootRef: HTMLHeadingElement | undefined;

  const sessions = () => currentSessions();

  const refresh = () => {
    setBusy(true);
    refreshSessions();
    setTimeout(() => setBusy(false), 300);
  };

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

  const title = () => {
    if (!selectedCid()) return '未选择对话';
    const t = ctrl()?.title;
    if (t) return t;
    const id = ctrl()?.chatId || selectedCid()!;
    return `对话 ${shortId(id)}${terminal() ? '（已结束）' : ''}`;
  };

  const meta = () => {
    if (!selectedCid()) return '点击左侧对话列表，或新建对话';
    const c = ctrl();
    if (c) {
      const parts = [c.chatId, c.status || '—'];
      if (activeTurn()?.turnStatus) parts.push(`turn=${activeTurn()!.turnStatus}`);
      return parts.join(' · ');
    }
    return `${selectedCid()}${terminal() ? ` · ${chatStatusSignal()[selectedCid()!] || ''}` : ''}`;
  };

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <div class="min-w-0">
        <h2
          ref={rootRef}
          onClick={(e) => {
            // tooltip 内部点击（列表/刷新）不触发切换，只由 title 本体切换。
            if (e.target !== e.currentTarget) return;
            setOpen((v) => !v);
          }}
          class="relative mt-0 mb-0.5 flex w-fit cursor-pointer items-center gap-2 text-sm font-semibold"
          title="ACP 会话列表（点击切换）"
        >
          {title()}
          <Show when={open()}>
            <div class="absolute top-full left-0 z-20 mt-1.5 w-80 rounded-lg border border-slate-300 bg-white p-2 shadow-lg">
              <div class="mb-1 flex items-center gap-1.5 text-xs font-semibold text-slate-700">
                ACP 会话{' '}
                <span class="font-normal text-slate-500">({sessions().length})</span>
                <Show when={selectedCid() !== null}>
                  <button
                    onClick={refresh}
                    disabled={busy()}
                    class="ml-auto shrink-0 rounded border border-slate-300 bg-white px-1.5 py-0.5 text-[11px] font-normal text-slate-600 hover:bg-slate-100 disabled:opacity-50"
                    title="向 agent 侧重新查询（10s 自动刷新）"
                  >
                    刷新
                  </button>
                </Show>
              </div>
              <Show
                when={selectedCid() !== null}
                fallback={<p class="text-xs text-slate-400">未选择对话（切换对话后按需查询）</p>}
              >
                <Show
                  when={sessions().length > 0}
                  fallback={<p class="text-xs text-slate-400">暂无 ACP 会话（10s 自动刷新）</p>}
                >
                  <ul class="m-0 max-h-72 list-none overflow-y-auto p-0">
                    <For each={sessions()}>
                      {(s) => {
                        // §8.5 会话切换语义：会话是**进程内实体**——列表属于
                        // 当前对话（进程），boundChatId 标注该对话的**当前活跃
                        // 会话**（标「当前」，点击无操作）；其余为历史会话，
                        // 点击 = 在当前对话内 load 切换（不新建对话/进程）。
                        const isCurrent = s.boundChatId === selectedCid();
                        const open = () => {
                          if (isCurrent) return;
                          openAcpSession(s.sessionId, s.title || undefined);
                          setOpen(false);
                        };
                        return (
                          <li
                            onClick={open}
                            class={`mb-1.5 cursor-pointer rounded bg-white px-1.5 py-1 ring-1 ring-slate-200 hover:bg-blue-50 hover:ring-blue-200 ${
                              isCurrent ? 'bg-emerald-50/60 ring-emerald-300' : ''
                            }`}
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
            </div>
          </Show>
        </h2>
        <p class="mt-0.5 mb-0 text-xs text-slate-500">{meta()}</p>
      </div>
    </section>
  );
}
