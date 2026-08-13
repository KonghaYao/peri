import { createMemo, For, Show } from 'solid-js';
import type { ChatEntry } from '../lib/yjs';
import { messageTime } from '../lib/message-time.mjs';
import { CopyButton } from '../../ui';
import { Markdown } from './Markdown';
import { MessageStatusBadge } from './Badge';
import { ToolCallCard } from './ToolCallCard';

function LoadingDots() {
  return <span class="message-loading" aria-hidden="true">
    <For each={[0, 1, 2]}>{(index) => <span style={{ 'animation-delay': `${index * 0.15}s` }}>●</span>}</For>
  </span>;
}

/** Owns the visual and semantic hierarchy of one server-projected entry. */
export function ConversationMessage(props: { entry: ChatEntry }) {
  const entry = () => props.entry;
  const timestamp = createMemo(() => messageTime(entry().createdAt));
  const role = createMemo(() => entry().role === 'user' ? 'user' : entry().role === 'system' ? 'system' : 'assistant');
  const streaming = () => entry().status === 'streaming';
  const empty = () => !entry().text && !entry().reasoning.length && !entry().toolCalls.length && !entry().error;
  const label = () => role() === 'user' ? '你的消息' : role() === 'system' ? '系统消息' : '助手消息';

  return <article class={`conversation-message conversation-message--${role()}`} aria-label={label()}>
    <div class="conversation-message__surface">
      <Show when={role() !== 'system'}>
        <header class={`conversation-message__meta ${entry().status === 'error' ? 'conversation-message__meta--visible' : ''}`}>
          <Show when={timestamp()}>{(time) => <time dateTime={entry().createdAt} title={time().exact}>{time().label}</time>}</Show>
          <MessageStatusBadge status={entry().status} />
        </header>
      </Show>
      <For each={entry().reasoning}>{(reasoning) => <details class="message-reasoning"><summary>思考过程</summary><pre>{reasoning.text}</pre></details>}</For>
      <Show when={entry().text}>
        <div class="conversation-message__text">
          <Show when={role() === 'assistant' && !streaming()} fallback={<span class="message-plain-text">{entry().text}</span>}><Markdown source={entry().text} /></Show>
          <Show when={streaming()}><LoadingDots /></Show>
        </div>
      </Show>
      <For each={entry().toolCalls}>{(toolCall) => <ToolCallCard toolCall={toolCall} />}</For>
      <For each={entry().resources}>{(resource) => <section class="message-resource" aria-label={resource.name || '关联资源'}>
        <div><strong>{resource.name || resource.resourceId || '资源'}</strong><span>{resource.mediaType || '类型未知'}</span></div>
        <Show when={resource.resourceId}><code title={resource.resourceId || undefined}>{resource.resourceId}</code></Show>
      </section>}</For>
      <Show when={entry().error}>{(error) => <section class="message-error" role="alert" aria-label="消息错误"><code>{error().code || 'UNKNOWN'}{error().message ? `: ${error().message}` : ''}</code></section>}</Show>
      <Show when={role() === 'assistant' && entry().text && !streaming()}><div class="message-actions"><CopyButton text={entry().text} label="复制回答" /></div></Show>
      <Show when={empty()}><LoadingDots /><span class="sr-only">正在生成回答</span></Show>
    </div>
  </article>;
}
