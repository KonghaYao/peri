import { createEffect, createSignal, Show } from 'solid-js';
import { chatHead, chatStatusSignal, closeChat, connState, isTerminal, navigateProjectSession, openingSessionId, permissions, projectSessions, runtimeDocsHydrated, selectedCid, selectedSessionId, turnActive } from '../store';
import { readOnly } from '../lib/auth-state';
import { Button, Dialog, Icon, IconButton, Menu, MenuItem, Status } from '../../ui';
import { runtimeState } from '../lib/runtime-state.mjs';
import { sessionDisplayTitle } from '../lib/recovery-state.mjs';
import { runtimeControlFor } from '../lib/runtime-control';

export type ChatHeaderProps = { onOpenNavigation?: () => void; onOpenStatus?: () => void };

export function ChatHeader(props: ChatHeaderProps) {
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [confirmClose, setConfirmClose] = createSignal(false);
  let menuTrigger: HTMLButtonElement | undefined;
  const logical = () => projectSessions().find((s) => s.id === selectedSessionId());
  const title = () => logical()
    ? sessionDisplayTitle(logical()!.title, logical()!.acpSessionId || logical()!.id)
    : chatHead()?.chat?.title || '新对话';
  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);
  const runtimeControl = () => runtimeControlFor(selectedCid());
  const closing = () => runtimeControl()?.kind === 'close' ? runtimeControl() : null;
  const runtimeControlLocked = () => !!runtimeControl() && runtimeControl()?.phase !== 'failed';
  const closeLocked = () => !!closing() && closing()?.phase !== 'failed';
  createEffect(() => {
    if (terminal()) setConfirmClose(false);
  });
  const runtime = () => runtimeState({
    hasSession: !!logical(),
    lifecycle: logical()?.lifecycle,
    isOpening: openingSessionId() === logical()?.id,
    hasRuntime: !!selectedCid(),
    isSelected: true,
    isHydrated: runtimeDocsHydrated(),
    chatStatus: chatStatusSignal()[selectedCid() ?? ''],
    hasPendingPermission: permissions().some((permission) => permission.status === 'pending'),
    turnActive: turnActive(),
  });
  const menuId = 'chat-session-menu';
  return <header class="chat-header">
    <IconButton tooltipPlacement="start" label="打开导航" class="mobile-nav-button" onClick={props.onOpenNavigation}>
      <Icon><path d="M3 5h14M3 10h14M3 15h14" /></Icon>
    </IconButton>
    <div class="chat-title"><strong>{title()}</strong><Show when={runtime().label}><span class={`runtime-status runtime-status--${runtime().tone}`}><i aria-hidden="true" />{runtime().label}</span></Show></div>
    <Status live tone={connState().kind || 'idle'} class="connection-pill">{connState().text}</Status>
    <Show when={logical() && selectedCid()}>
      <div class="chat-actions">
        <IconButton tooltipPlacement="end" ref={menuTrigger} label="会话操作" aria-haspopup="menu" aria-expanded={menuOpen()} aria-controls={menuOpen() ? menuId : undefined} onClick={() => setMenuOpen((value) => !value)}>
          <Icon><circle cx="4" cy="10" r="1" /><circle cx="10" cy="10" r="1" /><circle cx="16" cy="10" r="1" /></Icon>
        </IconButton>
        <Menu open={menuOpen()} id={menuId} label="会话操作" trigger={() => menuTrigger} onClose={() => setMenuOpen(false)}>
          <Show when={terminal()} fallback={
            <MenuItem tone="danger" disabled={readOnly() || runtimeControlLocked()} onClick={() => { setMenuOpen(false); setConfirmClose(true); }}>关闭运行实例</MenuItem>
          }>
            <MenuItem disabled={readOnly() || !!openingSessionId()} onClick={() => { const id = logical()?.id; setMenuOpen(false); if (id) navigateProjectSession(id); }}>重新打开会话</MenuItem>
          </Show>
        </Menu>
      </div>
    </Show>
    <Dialog open={confirmClose()} title="关闭当前运行实例" dismissible={!closeLocked()} onClose={() => setConfirmClose(false)}>
      <div class="runtime-dialog">
        <h2>关闭当前运行实例？</h2>
        <p>左侧会话和历史记录会保留。下次打开时，acp-hub 会启动新的运行实例并加载同一个 ACP 会话。</p>
        <Show when={turnActive()}><p class="runtime-dialog__warning">Agent 仍在工作。关闭实例会终止当前生成和正在执行的工具。</p></Show>
        <div class="form-actions"><Button disabled={closeLocked()} onClick={() => setConfirmClose(false)}>取消</Button><Button variant="danger" disabled={runtimeControlLocked()} busy={closing()?.phase === 'sending' || closing()?.phase === 'accepted'} onClick={() => closeChat(() => setConfirmClose(false))}>关闭实例</Button></div>
      </div>
    </Dialog>
  </header>;
}
