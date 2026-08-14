import { For, Show } from 'solid-js';
import type { PromptRecoveryView } from '../store';

export interface PromptRecoveryNoticeProps { recovery: PromptRecoveryView | null }

const publicItems = (recovery: PromptRecoveryView | null) => recovery?.prompts.filter((prompt) => prompt.status !== 'completed') ?? [];

function label(status: string): string {
  if (status === 'failed') return '处理失败';
  if (status === 'projected') return '消息已保存，终态仍在核对';
  return '投递结果尚未确认';
}

/** Historical evidence only. Never fabricates/restores a prompt body or old runtime. */
export function PromptRecoveryNotice(props: PromptRecoveryNoticeProps) {
  return <Show when={props.recovery?.loading || props.recovery?.error || props.recovery?.evidenceIncomplete || publicItems(props.recovery).length > 0}>
    <aside class="prompt-recovery" aria-label="历史消息恢复状态">
      <Show when={props.recovery?.loading} fallback={<>
        <div class="prompt-recovery__heading"><strong>发现需要核对的历史消息</strong><span>旧运行实例没有被恢复</span></div>
        <Show when={props.recovery?.error}>{(message) => <p role="alert">{message()}</p>}</Show>
        <Show when={props.recovery?.evidenceIncomplete}><p role="status">部分历史证据已不可用，系统无法证明更早消息的最终状态。</p></Show>
        <ul><For each={publicItems(props.recovery)}>{(prompt) => <li><span>{label(prompt.status)}</span><time dateTime={prompt.updatedAt}>{new Date(prompt.updatedAt).toLocaleString()}</time><code title={prompt.commandId}>{prompt.commandId.slice(0, 8)}…</code></li>}</For></ul>
        <Show when={props.recovery?.truncated}><p>较早的已完成记录已折叠；所有未决状态优先保留。</p></Show>
      </>}>
        <div class="prompt-recovery__loading" role="status"><span class="ui-spinner" aria-hidden="true" />正在核对历史消息状态…</div>
      </Show>
    </aside>
  </Show>;
}
