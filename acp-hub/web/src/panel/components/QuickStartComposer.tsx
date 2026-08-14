import { createEffect, createSignal, For, Show } from 'solid-js';
import { Button, SelectField, Textarea } from '../../ui';
import { createSessionWithFirstMessage, creatingSessionProjectId, retryQuickStart } from '../store';
import { readOnly } from '../lib/auth-state';
import { dismissFailedQuickStart, quickStartSubmission } from '../lib/quick-start-delivery';

export function QuickStartComposer(props: { projects: Array<{ id: string; name: string }>; initialProjectId?: string }) {
  const [draft, setDraft] = createSignal('');
  const [projectId, setProjectId] = createSignal(props.initialProjectId || props.projects[0]?.id || '');
  const pending = () => quickStartSubmission();
  const locked = () => (!!pending() && pending()!.phase !== 'failed') || !!creatingSessionProjectId();
  const project = () => props.projects.find((item) => item.id === projectId());
  createEffect(() => {
    if (!pending() && !project()) setProjectId(props.projects[0]?.id || '');
  });
  const submit = () => {
    const text = draft().trim();
    if (!text || pending() || !projectId()) return;
    createSessionWithFirstMessage(projectId(), text);
  };

  return <section class="quick-start" aria-label="开始新会话">
    <Show when={props.projects.length > 1}><SelectField label="保存到项目" value={projectId()} disabled={locked()} onChange={(event) => setProjectId(event.currentTarget.value)}>
      <For each={props.projects}>{(item) => <option value={item.id}>{item.name}</option>}</For>
    </SelectField></Show>
    <div class="quick-start__surface">
      <Textarea
        autoResize
        maxHeight={180}
        value={draft()}
        disabled={readOnly() || locked()}
        onInput={(event) => setDraft(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.isComposing) return;
          if (event.key === 'Enter' && !event.shiftKey) { event.preventDefault(); submit(); }
        }}
        placeholder={project() ? `向 ${project()!.name} 提问…` : '输入消息…'}
        aria-label="第一条消息"
      />
      <div class="quick-start__footer"><span>Enter 发送 · Shift+Enter 换行</span><Button variant="primary" busy={pending()?.phase === 'creating' || pending()?.phase === 'accepted'} disabled={readOnly() || locked() || !!pending() || !draft().trim()} onClick={submit}>开始</Button></div>
    </div>
    <Show when={pending()}>{(submission) => <div class={`quick-start__state quick-start__state--${submission().phase}`} role={submission().phase === 'failed' || submission().phase === 'uncertain' ? 'alert' : 'status'}>
      <span>{submission().phase === 'uncertain' ? '创建结果尚未确认' : submission().phase === 'failed' ? '无法创建会话' : '正在创建并连接会话…'}</span>
      <Show when={submission().detail}><small>{submission().detail}</small></Show>
      <Show when={(submission().phase === 'failed' || submission().phase === 'uncertain') && submission().retryable}><Button size="compact" variant="secondary" onClick={retryQuickStart}>使用同一请求重新确认</Button></Show>
      <Show when={submission().phase === 'failed'}><Button size="compact" onClick={dismissFailedQuickStart}>返回编辑</Button></Show>
    </div>}</Show>
  </section>;
}
