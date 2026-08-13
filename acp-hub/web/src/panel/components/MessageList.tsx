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
import { chatEntries, permissions, resolvePermission, retryPersistentAction, runtimeDocsHydrated } from '../store';
import { readOnly } from '../lib/auth-state';
import { messageActivity, nextFollowState } from '../lib/message-follow.mjs';
import { messageTime } from '../lib/message-time.mjs';
import { Button } from '../../ui';
import { PermissionQueue } from './PermissionQueue';
import { ConversationMessage } from './ConversationMessage';
import { permissionDecisions } from '../lib/permission-delivery';


// ── 权限条 ──────────────────────────────────────────────────────────────

// 位于正文列顶部 sticky（§3.8）：warning-soft 背景、1px 淡化 warning 边框、
// 16px 圆角；「允许」深灰实心、「拒绝」outline/ghost，避免把安全决策设计
// 成绿色诱导操作；不加阴影（§3.5 阴影只用于真正浮起的层级）。
function PermissionBar() {
  return <PermissionQueue
    permissions={permissions()}
    decisions={permissionDecisions()}
    readOnly={readOnly()}
    onResolve={resolvePermission}
    onRetry={retryPersistentAction}
  />;
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
        <Show when={!runtimeDocsHydrated()}>
          <div class="conversation-placeholder" role="status">
            <span class="ui-spinner" aria-hidden="true" />
            <strong>正在载入会话</strong>
            <p>正在从 acp-hub server 恢复消息与运行状态…</p>
          </div>
        </Show>
        <Show when={runtimeDocsHydrated() && chatEntries().length === 0}>
          <div class="conversation-placeholder conversation-placeholder--empty">
            <span class="conversation-placeholder__mark" aria-hidden="true">✦</span>
            <strong>开始这段对话</strong>
            <p>发送第一条消息。内容会持续保存到当前会话，下次回来仍可恢复。</p>
          </div>
        </Show>
        <For each={chatEntries()}>
          {(entry) => <ConversationMessage entry={entry} />}
        </For>
      </div>
    </section>
    <Show when={!stick() || hasNewContent()}><Button type="button" size="compact" class="jump-latest" onClick={jumpToLatest}>{hasNewContent() ? '↓ 有新内容' : '↓ 回到底部'}</Button></Show>
    </div>
  );
}
