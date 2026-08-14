import { createEffect, createMemo, createSignal, Show } from 'solid-js';
import type { PendingPermission } from '../lib/control-view';
import type { PermissionDecisionState } from '../lib/permission-delivery';
import { Button } from '../../ui';
import { PermissionRequestCard } from './PermissionRequestCard';

export interface PermissionQueueProps {
  permissions: PendingPermission[];
  decisions: Map<string, PermissionDecisionState>;
  readOnly: boolean;
  onResolve: (permissionId: string, decision: 'allow' | 'deny') => void;
  onRetry?: (commandId: string) => void;
}

/** Keeps one security decision in focus while making every server-projected
 * request discoverable. Selection follows permission identity, not array
 * position, so unrelated Yjs updates cannot silently switch the question. */
export function PermissionQueue(props: PermissionQueueProps) {
  const [activeId, setActiveId] = createSignal<string | null>(null);
  const [lastIndex, setLastIndex] = createSignal(0);
  const activeIndex = createMemo(() => {
    const items = props.permissions;
    if (!items.length) return -1;
    const id = activeId();
    const matched = id ? items.findIndex((item) => item.permissionId === id) : -1;
    return matched >= 0 ? matched : Math.min(lastIndex(), items.length - 1);
  });
  const active = createMemo(() => props.permissions[activeIndex()]);

  createEffect(() => {
    const index = activeIndex();
    const item = active();
    if (!item || index < 0) {
      setActiveId(null);
      setLastIndex(0);
      return;
    }
    setLastIndex(index);
    setActiveId(item.permissionId);
  });

  const select = (index: number) => {
    const item = props.permissions[index];
    if (!item) return;
    setLastIndex(index);
    setActiveId(item.permissionId);
  };

  return <Show when={active()}>{(permission) => {
    const permissionId = () => permission().permissionId;
    return <aside class="permission-queue" aria-label={`待处理权限请求，共 ${props.permissions.length} 项`}>
      <Show when={props.permissions.length > 1}>
        <div class="permission-queue__navigation">
          <span aria-live="polite">{activeIndex() + 1} / {props.permissions.length} 待处理</span>
          <div>
            <Button size="compact" disabled={activeIndex() <= 0} onClick={() => select(activeIndex() - 1)}>上一个</Button>
            <Button size="compact" disabled={activeIndex() >= props.permissions.length - 1} onClick={() => select(activeIndex() + 1)}>下一个</Button>
          </div>
        </div>
      </Show>
      <PermissionRequestCard
        permission={permission()}
        decision={permissionId() ? props.decisions.get(permissionId()!) : undefined}
        readOnly={props.readOnly}
        onResolve={(decision) => {
          const id = permissionId();
          if (id) props.onResolve(id, decision);
        }}
        onRetry={props.onRetry}
      />
    </aside>;
  }}</Show>;
}
