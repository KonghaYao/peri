import { Show } from 'solid-js';
import type { MessageSubmission } from '../lib/message-delivery';
import { Button, CopyButton } from '../../ui';

const titleFor = (phase: MessageSubmission['phase']) => phase === 'uncertain'
  ? '结果尚未确认'
  : phase === 'delivery_unknown'
    ? '投递结果未知，请勿重发'
  : phase === 'failed'
    ? '消息未发送'
    : phase === 'committed'
      ? '正在同步消息'
      : phase === 'accepted'
        ? '服务器已接收'
        : '正在发送';

/** Ephemeral prompt state. It disappears only after the exact durable entry arrives. */
export function MessageOutbox(props: {
  submission: MessageSubmission;
  onRetry: () => void;
  onEdit: () => void;
}) {
  const actionable = () => ['uncertain', 'delivery_unknown', 'failed'].includes(props.submission.phase);

  return <article
    class={`conversation-message conversation-message--user message-outbox message-outbox--${props.submission.phase}`}
    aria-label="你的待确认消息"
    role={actionable() ? 'alert' : 'status'}
  >
    <div class="conversation-message__surface">
      <div class="conversation-message__text"><span class="message-plain-text">{props.submission.text}</span></div>
      <footer class="message-outbox__status">
        <span class="message-outbox__indicator" aria-hidden="true" />
        <span><strong>{titleFor(props.submission.phase)}</strong><Show when={props.submission.detail}> · {props.submission.detail}</Show></span>
      </footer>
      <Show when={actionable()}>
        <div class="message-outbox__actions">
          <CopyButton text={props.submission.text} label="复制原文" size="compact" />
          <Show when={props.submission.retryable}>
            <Button variant="primary" size="compact" onClick={props.onRetry}>使用同一请求重新确认</Button>
          </Show>
          <Show when={props.submission.phase === 'failed'}>
            <Button size="compact" onClick={props.onEdit}>返回编辑</Button>
          </Show>
        </div>
      </Show>
    </div>
  </article>;
}
