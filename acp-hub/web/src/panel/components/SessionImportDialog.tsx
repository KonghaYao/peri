import { createEffect, createMemo, createSignal, For, Show } from 'solid-js';
import type { ProjectInfo, SessionSummaryInfo } from '../lib/yjs';
import { Button, Dialog, Icon, TextField } from '../../ui';
import { importCandidates } from '../lib/session-import.mjs';
import { cleanSessionTitle, formatRelativeTime, shortSessionId } from '../lib/recovery-state.mjs';

export interface SessionImportDialogProps {
  open: boolean;
  project: ProjectInfo | null;
  sessions: SessionSummaryInfo[];
  onClose: () => void;
  onImport: (
    projectId: string,
    sessionId: string,
    onCommitted: () => void,
    onFailed: (kind: 'failed' | 'uncertain') => void,
  ) => boolean;
}

function ChatIcon() {
  return <Icon><path d="M4 4.5h12v9H8l-4 3v-12Z" /></Icon>;
}

export function SessionImportDialog(props: SessionImportDialogProps) {
  const [query, setQuery] = createSignal('');
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [submitting, setSubmitting] = createSignal(false);
  const [problem, setProblem] = createSignal<string | null>(null);

  createEffect(() => {
    if (!props.open) return;
    props.project?.id;
    setQuery('');
    setSelectedId(null);
    setSubmitting(false);
    setProblem(null);
  });

  const candidates = createMemo(() => {
    const all = importCandidates(props.sessions, props.project?.cwd || '');
    const needle = query().trim().toLocaleLowerCase();
    if (!needle) return all;
    return all.filter((candidate) => (
      cleanSessionTitle(candidate.title).toLocaleLowerCase().includes(needle)
      || candidate.sessionId.toLocaleLowerCase().includes(needle)
    ));
  });
  const selected = createMemo(() => candidates().find((candidate) => candidate.sessionId === selectedId()) || null);

  const close = () => {
    if (!submitting()) props.onClose();
  };

  const submit = () => {
    const projectId = props.project?.id;
    const sessionId = selected()?.sessionId;
    if (!projectId || !sessionId || submitting()) return;
    setSubmitting(true);
    setProblem(null);
    const sent = props.onImport(projectId, sessionId, () => {
      setSubmitting(false);
      props.onClose();
    }, (kind) => {
      setSubmitting(false);
      setProblem(kind === 'uncertain'
        ? '导入结果尚未确认。当前选择已保留；请先等待侧边栏同步或使用错误中心的原请求重新确认，不要创建新的重复请求。'
        : '服务器明确拒绝了这次导入。当前选择已保留；请查看错误中心的原因，修复后可重新提交。');
    });
    if (!sent) {
      setSubmitting(false);
      setProblem('导入请求没有发出。当前选择已保留，请检查连接状态后重试。');
    }
  };

  return <Dialog open={props.open} title="导入 ACP 会话" dismissible={!submitting()} onClose={close}>
    <div class="import-dialog">
      <div class="import-dialog__header">
        <span class="dialog-eyebrow">{props.project?.name}</span>
        <h2>导入会话</h2>
        <p>只显示此项目目录中、尚未加入侧边栏的 ACP 会话。导入不会复制或移动原会话。</p>
      </div>
      <TextField label="搜索会话" value={query()} disabled={submitting()} onInput={(event) => setQuery(event.currentTarget.value)} placeholder="按标题或会话 ID 搜索" />
      <div class="import-session-list">
        <For each={candidates()} fallback={<div class="import-empty">没有可导入的会话</div>}>
          {(candidate) => <button
            type="button"
            disabled={submitting()}
            class={`import-session-row ${selectedId() === candidate.sessionId ? 'is-selected' : ''}`}
            aria-pressed={selectedId() === candidate.sessionId}
            aria-controls={selectedId() === candidate.sessionId ? 'import-session-review' : undefined}
            onClick={() => { setSelectedId(candidate.sessionId); setProblem(null); }}
          >
            <ChatIcon />
            <span><strong>{cleanSessionTitle(candidate.title)}</strong><small>{formatRelativeTime(candidate.updatedAt)} · ID …{shortSessionId(candidate.sessionId)}</small></span>
            <Show when={selectedId() === candidate.sessionId}><span class="import-session-check" aria-hidden="true">✓</span></Show>
          </button>}
        </For>
      </div>
      <Show when={selected()}>{(candidate) => <section id="import-session-review" class="import-session-review" role="region" aria-label="待导入会话详情">
        <header><span>导入前复核</span><strong>{cleanSessionTitle(candidate().title)}</strong></header>
        <dl>
          <div><dt>项目目录</dt><dd><code>{props.project?.cwd}</code></dd></div>
          <div><dt>最近活动</dt><dd>{formatRelativeTime(candidate().updatedAt)}</dd></div>
          <div><dt>完整 ACP ID</dt><dd><code>{candidate().sessionId}</code></dd></div>
        </dl>
        <p>ACP 当前没有提供消息内容预览。请依据项目目录、标题、时间和完整 ID 确认；导入只把此会话加入侧边栏，不会复制或移动内容。</p>
      </section>}</Show>
      <Show when={submitting()}><div class="import-session-status" role="status"><span class="ui-spinner" aria-hidden="true" />正在向 server 确认导入结果…</div></Show>
      <Show when={problem()}>{(message) => <div class="import-session-problem" role="alert">{message()}</div>}</Show>
      <div class="form-actions">
        <Button disabled={submitting()} onClick={close}>取消</Button>
        <Button variant="primary" busy={submitting()} disabled={!selected()} onClick={submit}>导入所选会话</Button>
      </div>
    </div>
  </Dialog>;
}
