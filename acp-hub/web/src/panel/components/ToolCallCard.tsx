import { createMemo, Show } from 'solid-js';
import type { ToolCallInfo } from '../lib/chat-view';
import { CopyButton } from '../../ui';

const STATUS: Record<string, { label: string; tone: string }> = {
  pending: { label: '等待执行', tone: 'pending' },
  awaitingpermission: { label: '等待你的许可', tone: 'permission' },
  running: { label: '执行中', tone: 'running' },
  in_progress: { label: '执行中', tone: 'running' },
  completed: { label: '已完成', tone: 'success' },
  complete: { label: '已完成', tone: 'success' },
  error: { label: '执行失败', tone: 'error' },
  failed: { label: '执行失败', tone: 'error' },
  cancelled: { label: '已取消', tone: 'neutral' },
};

export function readableToolValue(value: unknown): string {
  if (value === undefined || value === null) return '';
  if (typeof value === 'string') return value;
  try { return JSON.stringify(value, null, 2); } catch { return String(value); }
}

export function observedDuration(startedAt: string | null, completedAt: string | null): string | null {
  if (!startedAt || !completedAt) return null;
  const elapsed = Date.parse(completedAt) - Date.parse(startedAt);
  if (!Number.isFinite(elapsed) || elapsed < 0) return null;
  if (elapsed < 1000) return `${elapsed} ms`;
  if (elapsed < 10_000) return `${(elapsed / 1000).toFixed(1)} s`;
  if (elapsed < 60_000) return `${Math.round(elapsed / 1000)} s`;
  const minutes = Math.floor(elapsed / 60_000);
  const seconds = Math.round((elapsed % 60_000) / 1000);
  return `${minutes} min ${seconds} s`;
}

export function readableBytes(value: number | null): string | null {
  if (value === null || !Number.isFinite(value) || value < 0) return null;
  if (value < 1024) return `${Math.round(value)} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(value < 10 * 1024 ? 1 : 0)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function DataSection(props: { label: string; value: unknown; tone?: 'error' }) {
  const text = createMemo(() => readableToolValue(props.value));
  const lines = createMemo(() => text() ? text().split('\n').length : 0);
  return (
    <section class={`tool-data ${props.tone === 'error' ? 'tool-data--error' : ''}`}>
      <header><strong>{props.label}</strong><span>{lines()} 行</span><CopyButton text={text()} label={`复制${props.label}`} size="compact" /></header>
      <pre><code>{text()}</code></pre>
    </section>
  );
}

export function ToolCallCard(props: { toolCall: ToolCallInfo }) {
  const tool = () => props.toolCall;
  const state = createMemo(() => STATUS[(tool().status || '').toLowerCase()] || { label: tool().status || '状态未知', tone: 'neutral' });
  const duration = createMemo(() => observedDuration(tool().startedAt, tool().completedAt));
  const errorText = createMemo(() => [tool().publicError?.code, tool().publicError?.message].filter(Boolean).join(': '));
  const omittedSize = createMemo(() => readableBytes(tool().resultBytes));
  return (
    <details class={`tool-card tool-card--${state().tone}`} open={state().tone === 'error'}>
      <summary class="tool-card__summary">
        <span class="tool-card__mark" aria-hidden="true" />
        <span class="tool-card__identity"><strong>{tool().name || '工具调用'}</strong><code>{tool().toolCallId || '无调用 ID'}</code></span>
        <span class="tool-card__status">{state().label}</span>
        <Show when={duration()}>{(value) => <span class="tool-card__duration" title="Hub 接收到开始与完成事件之间的时间">Hub 观测 {value()}</span>}</Show>
        <span class="tool-card__chevron" aria-hidden="true">›</span>
      </summary>
      <div class="tool-card__body">
        <Show when={tool().arguments !== undefined && tool().arguments !== null}><DataSection label="输入" value={tool().arguments} /></Show>
        <Show when={tool().result !== undefined && tool().result !== null}><DataSection label="输出" value={tool().result} /></Show>
        <Show when={errorText()}><DataSection label="公开错误" value={errorText()} tone="error" /></Show>
        <Show when={tool().resultOmitted}>
          <aside class="tool-card__omitted" role="note"><strong>输出未载入</strong><span>Hub 观测到的结果{omittedSize() ? `约 ${omittedSize()}` : ''}，超过页面投影上限。内容没有写入此会话视图。</span></aside>
        </Show>
        <Show when={tool().resultOmitted === false && (tool().result === undefined || tool().result === null)}><Show when={!errorText() && !['running', 'pending', 'permission'].includes(state().tone)}><p class="tool-card__empty">工具没有返回可展示的输出。</p></Show></Show>
        <Show when={tool().resultOmitted === null && (tool().result === undefined || tool().result === null)}><Show when={!errorText() && !['running', 'pending', 'permission'].includes(state().tone)}><p class="tool-card__legacy">此历史记录没有输出内容；旧版投影未记录它是空结果还是因大小限制省略。</p></Show></Show>
      </div>
    </details>
  );
}
