import { createEffect, createSignal, Show } from 'solid-js';
import type { ProjectSessionInfo } from '../lib/yjs';
import { Button, Icon, IconButton, Popover, Spinner, TextField } from '../../ui';
import { formatRelativeTime, sessionDisplayTitle } from '../lib/recovery-state.mjs';
import { runConfirmedMutation } from '../lib/form-mutation';

export interface SessionRowState {
  label: string;
  tone: string;
}

export interface ProjectSessionRowProps {
  session: ProjectSessionInfo;
  state: SessionRowState;
  selected: boolean;
  opening: boolean;
  navigationBusy: boolean;
  readOnly: boolean;
  renameOpen: boolean;
  replacementBusy: boolean;
  onNavigate: () => void;
  onOpen: (sessionId: string, onCommitted: () => void) => void;
  onSelectRuntime: (sessionId: string, chatId: string) => void;
  onRenameOpenChange: (open: boolean) => void;
  onRename: (
    sessionId: string,
    name: string,
    onCommitted: () => void,
    onFailed: () => void,
  ) => boolean;
  onCreateReplacement: (title: string) => void;
}

function ChatIcon() {
  return <Icon><path d="M4 4.5h12v9H8l-4 3v-12Z" /></Icon>;
}

function EditIcon() {
  return <Icon><path d="m4 14.8.7-3.4 7.8-7.8a1.5 1.5 0 0 1 2.1 0l1.8 1.8a1.5 1.5 0 0 1 0 2.1l-7.8 7.8-3.4.7Z" /><path d="m11.3 4.8 3.9 3.9" /></Icon>;
}

export function ProjectSessionRow(props: ProjectSessionRowProps) {
  const [draft, setDraft] = createSignal(props.session.title);
  const [submitting, setSubmitting] = createSignal(false);
  let renameTrigger: HTMLButtonElement | undefined;
  const renameId = () => `rename-session-${props.session.id}`;
  const displayTitle = () => sessionDisplayTitle(
    props.session.title,
    props.session.acpSessionId || props.session.id,
  );
  const renameValid = () => !!draft().trim();

  createEffect(() => {
    if (props.renameOpen) setDraft(props.session.title);
  });

  const open = () => {
    if (props.readOnly && props.session.activeChatId) {
      props.onSelectRuntime(props.session.id, props.session.activeChatId);
      props.onNavigate();
      return;
    }
    props.onOpen(props.session.id, props.onNavigate);
  };

  const submitRename = (event: SubmitEvent) => {
    event.preventDefault();
    if (!renameValid() || submitting()) return;
    runConfirmedMutation(
      () => { setSubmitting(true); },
      () => { setSubmitting(false); },
      (committed, failed) => props.onRename(
        props.session.id,
        draft().trim(),
        committed,
        failed,
      ),
      () => props.onRenameOpenChange(false),
    );
  };

  return <div class={`session-row ${props.selected ? 'is-selected' : ''}`}>
    <button
      type="button"
      class="session-main"
      aria-current={props.selected ? 'page' : undefined}
      title={props.readOnly && !props.session.activeChatId ? '需要 full 权限才能启动此会话' : undefined}
      onClick={open}
      disabled={(props.readOnly && !props.session.activeChatId) || props.session.lifecycle !== 'ready' || props.navigationBusy}
    >
      <ChatIcon />
      <span class="session-copy">
        <strong>{displayTitle()}</strong>
        <small class={`session-state session-state--${props.state.tone}`}>{props.state.label} · {formatRelativeTime(props.session.lastOpenedAt || props.session.updatedAt)}</small>
      </span>
      <Show when={props.opening || ['activating', 'pending'].includes(props.session.lifecycle)}><Spinner label="正在打开" /></Show>
    </button>
    <IconButton
      tooltipPlacement="end"
      ref={renameTrigger}
      class="session-menu"
      disabled={props.readOnly || submitting()}
      label={`重命名 ${displayTitle()}`}
      aria-haspopup="dialog"
      aria-expanded={props.renameOpen}
      aria-controls={props.renameOpen ? renameId() : undefined}
      onClick={() => props.onRenameOpenChange(true)}
    >
      <EditIcon />
    </IconButton>
    <Popover
      open={props.renameOpen}
      id={renameId()}
      label={`重命名 ${displayTitle()}`}
      trigger={() => renameTrigger}
      dismissible={!submitting()}
      onClose={() => props.onRenameOpenChange(false)}
    >
      <form class="rename-popover" onSubmit={submitRename}>
        <TextField aria-label="会话名称" value={draft()} error={!renameValid() ? '名称不能为空' : undefined} onInput={(event) => setDraft(event.currentTarget.value)} autofocus />
        <div class="form-actions">
          <Button disabled={submitting()} onClick={() => props.onRenameOpenChange(false)}>取消</Button>
          <Button variant="primary" type="submit" busy={submitting()} disabled={!renameValid()}>保存</Button>
        </div>
      </form>
    </Popover>
    <Show when={props.session.lifecycle === 'failed'}>
      <div class="session-problem">打开失败 · <Button size="compact" busy={props.replacementBusy} disabled={props.readOnly || props.replacementBusy} onClick={() => props.onCreateReplacement(props.session.title)}>新建替代会话</Button></div>
    </Show>
    <Show when={props.session.lifecycle === 'reconciliation_required'}>
      <div class="session-problem session-problem--warn">需要服务端人工对账，暂不可重试</div>
    </Show>
  </div>;
}
