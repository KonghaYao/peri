import { Show } from 'solid-js';
import { chatHead, connState, projectSessions, selectedSessionId } from '../store';
import { IconButton } from '../../ui/Button';

export type ChatHeaderProps = { onOpenNavigation?: () => void; onOpenStatus?: () => void };

export function ChatHeader(props: ChatHeaderProps) {
  const logical = () => projectSessions().find((s) => s.id === selectedSessionId());
  const title = () => logical()?.title || chatHead()?.chat?.title || '新对话';
  return <header class="chat-header">
    <IconButton label="打开导航" class="mobile-nav-button" onClick={props.onOpenNavigation}>
      <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3 5h14M3 10h14M3 15h14" /></svg>
    </IconButton>
    <div class="chat-title"><strong>{title()}</strong><Show when={logical()}><span>保存在项目中</span></Show></div>
    <div role="status" aria-live="polite" class={`connection-pill connection-pill--${connState().kind || 'idle'}`}><span />{connState().text}</div>
  </header>;
}
