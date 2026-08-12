// 消息区：权限条 + 消息列表（自动吸底滚动）。
//
// 由 ChatView 拆出（中间区三块之一）；气泡内 reasoning 在前、正文在后，
// loading 状态统一由 LoadingDots 组件承担。
//
// F4（ui.md §四.6 / §3.8）：滚动区为 flex-1 + min-h-0 独立滚动，内部为
// 居中正文列（max-w 820px，pt-6 / pb-6，底部 156px 留白随 F7 Composer
// 悬浮再调）；PermissionBar 位于正文列顶部 sticky（top 12px）；消息按
// role/状态呈现八类视觉。消息模型、顺序、Yjs 读取、自动吸底算法与
// permission decision 值（allow/deny、按钮顺序）均不变。

import { createEffect, createSignal, For, Show } from 'solid-js';
import type { ChatEntry } from '../lib/yjs';
import { chatEntries, permissions, resolvePermission } from '../store';
import { Badge, badgeKind } from './Badge';

function shortId(id: string | null | undefined, n = 8): string {
  if (!id) return '—';
  return id.length > n ? `${id.slice(0, n)}…` : id;
}

// 工具/资源状态文字着色（§3.8：只给状态文字着色，不给整块强色背景）。
// badgeKind 与 Badge 同源，语义映射保持一致；色值统一走 §3.2 token
// （--success/--warning/--danger），与 Badge 的 KIND_CLASS 同一事实源。
const STATUS_TEXT: Record<string, string> = {
  ok: 'text-[var(--success)]',
  warn: 'text-[var(--warning)]',
  err: 'text-[var(--danger)]',
  neutral: 'text-[var(--text-muted)]',
};

// ── 权限条 ──────────────────────────────────────────────────────────────

// 位于正文列顶部 sticky（§3.8）：warning-soft 背景、1px 淡化 warning 边框、
// 16px 圆角；「允许」深灰实心、「拒绝」outline/ghost，避免把安全决策设计
// 成绿色诱导操作；不加阴影（§3.5 阴影只用于真正浮起的层级）。
// permissions()[0] 逐条展示、resolvePermission(id, 'allow'|'deny') 调用
// 与按钮顺序（允许在前/拒绝在后）保持现状不变。
function PermissionBar() {
  const p = () => permissions()[0]; // M3 雏形：逐条展示第一条，处理完移除

  return (
    <Show when={p()}>
      {(perm) => (
        <section class="sticky top-3 z-10 mb-4 rounded-2xl border border-[var(--warning)]/25 bg-[var(--warning-soft)] p-4">
          <strong class="block text-sm font-semibold text-[var(--warning)]">
            {perm().title || '权限请求'}
          </strong>
          <p class="mt-0.5 text-[13px] leading-[19px] text-[var(--warning)] opacity-90">
            {perm().description || ''}
            {perm().toolCallId ? (
              <span class="font-mono text-xs text-[var(--text-muted)]">
                {` · tool=${shortId(perm().toolCallId)}`}
              </span>
            ) : null}
          </p>
          <div class="mt-2.5 flex gap-2">
            <button
              onClick={() => resolvePermission(perm().permissionId || '', 'allow')}
              class="min-h-10 rounded-lg bg-[var(--btn-primary)] px-4 py-2 text-sm font-medium text-white transition-colors duration-150 hover:bg-[var(--btn-primary-hover)] sm:min-h-9"
            >
              允许
            </button>
            <button
              onClick={() => resolvePermission(perm().permissionId || '', 'deny')}
              class="min-h-10 rounded-lg border border-[var(--border-strong)] bg-white px-4 py-2 text-sm text-[var(--text-secondary)] transition-colors duration-150 hover:bg-[var(--hover)] sm:min-h-9"
            >
              拒绝
            </button>
          </div>
        </section>
      )}
    </Show>
  );
}

// ── loading 组件 ────────────────────────────────────────────────────────

// 三点脉冲动画：streaming 光标与空体骨架共用；通过 className 调整尺寸/颜色
function LoadingDots(props: { className?: string }) {
  return (
    <span
      class={`inline-flex items-center ${props.className ?? ''}`}
      aria-label="加载中"
    >
      <For each={[0, 1, 2]}>
        {(i) => (
          <span
            class="animate-pulse leading-none"
            style={{ 'animation-delay': `${i * 0.15}s` }}
          >
            ●
          </span>
        )}
      </For>
    </span>
  );
}

// ── 消息气泡 ────────────────────────────────────────────────────────────

function MessageBubble(props: { entry: ChatEntry }) {
  const e = () => props.entry;
  const align = () =>
    e().role === 'user' ? 'justify-end' : e().role === 'system' ? 'justify-center' : 'justify-start';
  // §3.8：user 右对齐浅灰圆角气泡（16px 圆角、12px 16px padding、深色文字，
  // 不再使用蓝底白字）；system 居中 pill（12px caption）；assistant 不使用
  // 气泡容器，正文直接排在白色面上。
  const contentCls = () =>
    e().role === 'user'
      ? 'max-w-[72%] rounded-2xl bg-[var(--surface-muted)] px-4 py-3'
      : e().role === 'system'
        ? 'max-w-[70%] rounded-full bg-[var(--surface-muted)] px-3 py-1 text-xs text-[var(--text-secondary)]'
        : 'min-w-0';
  // meta 行（时间 + 状态 Badge，无 role 前缀）：默认隐藏避免视觉噪声，
  // 消息 hover/focus 时显示（opacity 切换不产生布局跳动）；error 恒显。
  // system 不展示 meta。
  const metaCls = () =>
    `flex items-center gap-1.5 text-xs transition-opacity duration-150 ${
      e().status === 'error'
        ? 'opacity-100'
        : 'opacity-0 group-hover:opacity-100 group-focus-within:opacity-100'
    }`;
  const textCls = () =>
    e().role === 'system'
      ? 'whitespace-pre-wrap break-words'
      : 'whitespace-pre-wrap break-words text-[15px] leading-[25px] text-[var(--text-primary)]';

  return (
    <div class={`group mb-3 flex ${align()}`}>
      <div class={contentCls()}>
        {/* 内容块纵向间距统一 10px（§3.8 正文与代码/工具块间距），首块不缩进 */}
        <div class="[&>*+*]:mt-2.5">
          <Show when={e().role !== 'system'}>
            <div class={metaCls()}>
              <span class="text-[var(--text-muted)]">{e().createdAt}</span>
              <Badge status={e().status} />
            </div>
          </Show>

          <For each={e().reasoning}>
            {(r) => (
              <details class="text-[13px] text-[var(--text-secondary)]">
                <summary class="cursor-pointer select-none">思考过程</summary>
                <div class="mt-1 ml-3 border-l-2 border-[var(--divider)] pl-3">
                  <pre class="m-0 whitespace-pre-wrap break-words font-mono text-xs leading-5 text-[var(--text-secondary)]">
                    {r.text}
                  </pre>
                </div>
              </details>
            )}
          </For>

          <Show when={e().text}>
            <div class={textCls()}>
              {e().text}
              {e().status === 'streaming' && <LoadingDots className="ml-0.5" />}
            </div>
          </Show>

          <For each={e().toolCalls}>
            {(tc) => (
              <div class="rounded-[10px] bg-[var(--surface-muted)] px-3 py-2.5">
                <div class="flex items-baseline gap-2">
                  <span class="text-[13px] font-medium text-[var(--text-primary)]">
                    {tc.name || tc.toolCallId || '工具调用'}
                  </span>
                  <span class={`text-xs ${STATUS_TEXT[badgeKind(tc.status)]}`}>
                    {tc.status || '—'}
                  </span>
                </div>
                <div class="mt-0.5 font-mono text-xs text-[var(--text-muted)]">{tc.toolCallId}</div>
              </div>
            )}
          </For>

          <For each={e().resources}>
            {(r) => (
              <div class="rounded-[10px] bg-[var(--surface-muted)] px-3 py-2.5">
                <div class="flex items-baseline gap-2">
                  <span class="text-[13px] font-medium text-[var(--text-primary)]">
                    {r.name || r.resourceId || '资源'}
                  </span>
                  <span class="text-xs text-[var(--text-muted)]">{r.mediaType || '—'}</span>
                </div>
                <div class="mt-0.5 font-mono text-xs text-[var(--text-muted)]">{r.resourceId}</div>
              </div>
            )}
          </For>

          <Show when={e().error}>
            <div class="select-text rounded-xl border-l-[3px] border-[var(--danger)] bg-[var(--danger-soft)] px-3 py-2.5">
              <div class="whitespace-pre-wrap break-words font-mono text-xs text-[var(--danger)]">
                {e().error!.code}
                {e().error!.message ? `: ${e().error!.message}` : ''}
              </div>
            </div>
          </Show>

          {/* 空体（pending/streaming 骨架）→ 占位 loading */}
          <Show when={!e().text && !e().reasoning.length && !e().toolCalls.length && !e().error}>
            <LoadingDots className="text-[var(--text-muted)]" />
          </Show>
        </div>
      </div>
    </div>
  );
}

// ── 消息滚动区 ──────────────────────────────────────────────────────────

export function MessageList() {
  const [stick, setStick] = createSignal(true);
  let areaRef: HTMLDivElement | undefined;

  // 自动吸底（用户上滚时暂停）——算法与阈值（40px）保持不变
  createEffect(() => {
    const list = chatEntries();
    if (stick() && areaRef && list.length) {
      areaRef.scrollTop = areaRef.scrollHeight;
    }
  });

  return (
    <section
      ref={areaRef}
      onScroll={(e) => {
        const el = e.currentTarget;
        setStick(el.scrollHeight - el.scrollTop - el.clientHeight < 40);
      }}
      class="ui-scrollbar min-h-0 flex-1 overflow-y-auto"
    >
      {/* 居中正文列：宽屏 max-w 820px 居中留白（§3.13 验收 8），窄屏 16px
          padding；上 24px 按 §3.4，底部 156px 留白等 F7 Composer 悬浮 */}
      <div class="mx-auto w-full max-w-[820px] px-4 pt-6 pb-6">
        <PermissionBar />
        <For each={chatEntries()}>
          {(e) => <MessageBubble entry={e} />}
        </For>
      </div>
    </section>
  );
}
