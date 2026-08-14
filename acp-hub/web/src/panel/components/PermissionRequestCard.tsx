import { createUniqueId, Show } from 'solid-js';
import type { PendingPermission } from '../lib/control-view';
import type { PermissionDecisionState } from '../lib/permission-delivery';
import { Button } from '../../ui';

function shortId(id: string | null | undefined, length = 8): string {
  if (!id) return '';
  return id.length > length ? `${id.slice(0, length)}…` : id;
}

export interface PermissionRequestCardProps {
  permission: PendingPermission;
  decision?: PermissionDecisionState;
  readOnly: boolean;
  onResolve: (decision: 'allow' | 'deny') => void;
  onRetry?: (commandId: string) => void;
}

/** A security decision surface. The server projection owns its lifetime; the
 * card only renders known facts and prevents a second, conflicting decision. */
export function PermissionRequestCard(props: PermissionRequestCardProps) {
  const domId = createUniqueId();
  const actionable = () => !!props.permission.permissionId;
  const locked = () => !!props.decision;
  const uncertain = () => props.decision?.phase === 'uncertain';
  const retryable = () => uncertain() && props.decision?.retryable === true;
  const action = () => props.decision?.decision === 'allow' ? '允许' : '拒绝';
  const statusId = `permission-status-${domId}`;

  return <section
    class={`permission-request ${uncertain() ? 'permission-request--uncertain' : ''}`}
    aria-labelledby={`${statusId}-title`}
    aria-describedby={statusId}
    aria-busy={locked() && !uncertain() ? 'true' : undefined}
  >
    <div class="permission-request__mark" aria-hidden="true">!</div>
    <div class="permission-request__body">
      <span class="permission-request__eyebrow">需要你的许可</span>
      <strong id={`${statusId}-title`}>{props.permission.title || '权限请求'}</strong>
      <Show when={props.permission.description}><p>{props.permission.description}</p></Show>
      <Show when={props.permission.toolCallId}><code title={props.permission.toolCallId || undefined}>tool {shortId(props.permission.toolCallId)}</code></Show>
      <div id={statusId} class="permission-request__status" role={uncertain() ? 'alert' : 'status'} aria-live="polite">
        <Show when={uncertain()} fallback={locked() ? `正在${action()}…` : !actionable() ? '请求缺少权限标识，已阻止提交。请等待 server 重新同步。' : props.readOnly ? '只读模式无法处理该请求。' : '选择后会立即锁定，避免提交相反决策。'}>
          {retryable()
            ? `${action()}尚未送达。可以使用原请求重新确认，不会创建第二次裁决。`
            : `${action()}的结果尚未确认。为避免执行相反决策，请等待请求消失或明确错误。`}
        </Show>
      </div>
    </div>
    <div class="permission-request__actions">
      <Button variant="primary" disabled={props.readOnly || locked() || !actionable()} busy={props.decision?.phase === 'pending' && props.decision.decision === 'allow'} onClick={() => actionable() && props.onResolve('allow')}>
        {props.decision?.decision === 'allow' ? '正在允许…' : '允许'}
      </Button>
      <Button variant="secondary" disabled={props.readOnly || locked() || !actionable()} busy={props.decision?.phase === 'pending' && props.decision.decision === 'deny'} onClick={() => actionable() && props.onResolve('deny')}>
        {props.decision?.decision === 'deny' ? '正在拒绝…' : '拒绝'}
      </Button>
      <Show when={retryable() && props.decision}>
        {(decision) => <Button variant="primary" disabled={props.readOnly} onClick={() => props.onRetry?.(decision().commandId)}>使用原请求重试</Button>}
      </Show>
    </div>
  </section>;
}
