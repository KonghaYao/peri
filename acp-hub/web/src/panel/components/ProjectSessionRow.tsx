import { createEffect, createSignal, Show } from 'solid-js';
import type { ProjectSessionInfo } from '../lib/registry-view';
import { Button, Icon, IconButton, Menu, MenuItem, Popover, Spinner, TextField } from '../../ui';
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
  menuOpen: boolean;
  runtimeActive: boolean;
  replacementBusy: boolean;
  onNavigate: () => void;
  onOpen: (sessionId: string, onCommitted: () => void) => void;
  onSelectRuntime: (sessionId: string, chatId: string) => void;
  onRenameOpenChange: (open: boolean) => void;
  onMenuOpenChange: (open: boolean) => void;
  onRename: (
    sessionId: string,
    name: string,
    onCommitted: () => void,
    onFailed: () => void,
  ) => boolean;
  onCreateReplacement: (title: string) => void;
  onArchiveRequest: (sessionId: string) => void;
}

function ChatIcon() {
  return <Icon><path d="M4 4.5h12v9H8l-4 3v-12Z" /></Icon>;
}

function MoreIcon() { return <Icon><circle cx="4" cy="10" r="1" /><circle cx="10" cy="10" r="1" /><circle cx="16" cy="10" r="1" /></Icon>; }

export function ProjectSessionRow(props: ProjectSessionRowProps) {
  const [draft, setDraft] = createSignal(props.session.title);
  const [submitting, setSubmitting] = createSignal(false);
  let menuTrigger: HTMLButtonElement | undefined;
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
      class="session-menu"
      ref={menuTrigger}
      disabled={props.readOnly || submitting()}
      label={`会话操作：${displayTitle()}`}
      aria-haspopup="menu"
      aria-expanded={props.menuOpen}
      aria-controls={props.menuOpen ? `${renameId()}-menu` : undefined}
      onClick={() => props.onMenuOpenChange(!props.menuOpen)}
    >
      <MoreIcon />
    </IconButton>
    <Menu open={props.menuOpen} id={`${renameId()}-menu`} label={`会话操作：${displayTitle()}`} trigger={() => menuTrigger} onClose={() => props.onMenuOpenChange(false)}>
      <MenuItem onClick={() => { props.onMenuOpenChange(false); props.onRenameOpenChange(true); }}>重命名会话</MenuItem>
      <MenuItem tone="danger" disabled={props.runtimeActive} title={props.runtimeActive ? '请先关闭此会话的运行实例' : undefined} onClick={() => {
        props.onMenuOpenChange(false);
        props.onArchiveRequest(props.session.id);
      }}>归档会话</MenuItem>
    </Menu>
    <Popover
      open={props.renameOpen}
      id={renameId()}
      label={`重命名 ${displayTitle()}`}
      trigger={() => menuTrigger}
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
