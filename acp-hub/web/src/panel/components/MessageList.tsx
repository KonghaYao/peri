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

import { createEffect, createMemo, createSignal, For, Show } from 'solid-js';
import type { ChatEntry } from '../lib/yjs';
import { chatEntries, pendingPermissionDecisions, permissions, readOnly, resolvePermission } from '../store';
import { MessageStatusBadge } from './Badge';
import { Markdown } from './Markdown';
import { ToolCallCard } from './ToolCallCard';
import { messageActivity, nextFollowState } from '../lib/message-follow.mjs';
import { messageTime } from '../lib/message-time.mjs';
import { Button, CopyButton } from '../../ui';

function shortId(id: string | null | undefined, n = 8): string {
  if (!id) return '—';
  return id.length > n ? `${id.slice(0, n)}…` : id;
}


// ── 权限条 ──────────────────────────────────────────────────────────────

// 位于正文列顶部 sticky（§3.8）：warning-soft 背景、1px 淡化 warning 边框、
// 16px 圆角；「允许」深灰实心、「拒绝」outline/ghost，避免把安全决策设计
// 成绿色诱导操作；不加阴影（§3.5 阴影只用于真正浮起的层级）。
// permissions()[0] 逐条展示、resolvePermission(id, 'allow'|'deny') 调用
// 与按钮顺序（允许在前/拒绝在后）保持现状不变。
function PermissionBar() {
  const p = () => permissions()[0]; // M3 雏形：逐条展示第一条，处理完移除
  const pendingDecision = () => p()?.permissionId ? pendingPermissionDecisions().get(p()!.permissionId!) : undefined;
  const pending = () => !!pendingDecision();

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
          <div class="permission-actions mt-2.5 flex gap-2">
            <Button
              variant="primary"
              disabled={readOnly() || pending()}
              busy={pending()}
              onClick={() => resolvePermission(perm().permissionId || '', 'allow')}
            >
              {pendingDecision() === 'allow' ? '正在允许…' : '允许'}
            </Button>
            <Button
              variant="secondary"
              disabled={readOnly() || pending()}
              onClick={() => resolvePermission(perm().permissionId || '', 'deny')}
            >
              {pendingDecision() === 'deny' ? '正在拒绝…' : '拒绝'}
            </Button>
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
    <span class={props.className ?? ''}>
      <span class="inline-flex items-center" aria-hidden="true">
        <For each={[0, 1, 2]}>
          {(i) => (
            <span class="animate-pulse leading-none" style={{ 'animation-delay': `${i * 0.15}s` }}>●</span>
          )}
        </For>
      </span>
      <span class="sr-only">加载中</span>
    </span>
  );
}

// ── 消息气泡 ────────────────────────────────────────────────────────────

function MessageBubble(props: { entry: ChatEntry }) {
  const e = () => props.entry;
  const timestamp = createMemo(() => messageTime(e().createdAt));
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
              <Show when={timestamp()}>{(time) => <time class="text-[var(--text-muted)]" dateTime={e().createdAt} title={time().exact}>{time().label}</time>}</Show>
              <MessageStatusBadge status={e().status} />
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
              <Show when={e().role === 'assistant' && e().status !== 'streaming'} fallback={e().text}><Markdown source={e().text} /></Show>
              {e().status === 'streaming' && <LoadingDots className="ml-0.5" />}
            </div>
          </Show>

          <For each={e().toolCalls}>
            {(tc) => <ToolCallCard toolCall={tc} />}
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

          <Show when={e().role === 'assistant' && e().text && e().status !== 'streaming'}>
            <div class="message-actions"><CopyButton text={e().text} label="复制回答" /></div>
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
  const [hasNewContent, setHasNewContent] = createSignal(false);
  let areaRef: HTMLDivElement | undefined;
  let previousActivity = '';

  // 自动吸底（用户上滚时暂停）——算法与阈值（40px）保持不变
  createEffect(() => {
    const list = chatEntries();
    const activity = messageActivity(list);
    const follow = nextFollowState({ stick: stick(), hasNewContent: hasNewContent(), previousActivity, activity });
    if (follow.stick && areaRef && list.length) {
      areaRef.scrollTop = areaRef.scrollHeight;
    }
    setHasNewContent(follow.hasNewContent);
    previousActivity = follow.activity;
  });

  const jumpToLatest = () => {
    if (!areaRef) return;
    areaRef.scrollTo({ top: areaRef.scrollHeight, behavior: window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth' });
    setStick(true);
    setHasNewContent(false);
  };

  const completionAnnouncement = () => {
    const entry = [...chatEntries()].reverse().find((item) => item.role === 'assistant' && ['completed', 'failed', 'cancelled', 'interrupted'].includes(item.status || ''));
    if (!entry) return '';
    const status = entry.status === 'completed' ? '完成' : entry.status === 'failed' ? '失败' : entry.status === 'cancelled' ? '取消' : '中断';
    const timestamp = messageTime(entry.createdAt);
    return `助手回答已${status}${timestamp ? `，${timestamp.label}` : ''}`;
  };

  return (
    <div class="message-list-shell">
    <section aria-label="对话消息"
      ref={areaRef}
      onScroll={(e) => {
        const el = e.currentTarget;
        setStick(el.scrollHeight - el.scrollTop - el.clientHeight < 40);
      }}
      class="ui-scrollbar min-h-0 flex-1 overflow-y-auto"
    >
      <div class="sr-only" role="status" aria-live="polite" aria-atomic="true">{completionAnnouncement()}</div>
      {/* 居中正文列：宽屏 max-w 820px 居中留白（§3.13 验收 8），窄屏 16px
          padding；上 24px 按 §3.4，底部 156px 留白等 F7 Composer 悬浮 */}
      <div class="mx-auto w-full max-w-[820px] px-4 pt-6 pb-6">
        <PermissionBar />
        <For each={chatEntries()}>
          {(e) => <MessageBubble entry={e} />}
        </For>
      </div>
    </section>
    <Show when={!stick() || hasNewContent()}><Button type="button" size="compact" class="jump-latest" onClick={jumpToLatest}>{hasNewContent() ? '↓ 有新内容' : '↓ 回到底部'}</Button></Show>
    </div>
  );
}
