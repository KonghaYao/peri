import { fireEvent, render, screen } from '@solidjs/testing-library';
import { createSignal } from 'solid-js';
import { describe, expect, it, vi } from 'vitest';
import type { PendingPermission } from '../lib/control-view';
import type { PermissionDecisionState } from '../lib/permission-delivery';
import { PermissionQueue } from './PermissionQueue';

function permission(id: string, title: string): PendingPermission {
  return { permissionId: id, turnId: 'turn-1', toolCallId: `tool-${id}`, title, description: null, status: 'pending', expiresAt: null, decision: null };
}

describe('PermissionQueue', () => {
  it('makes every pending request discoverable without submitting on navigation', () => {
    const resolve = vi.fn();
    render(() => <PermissionQueue permissions={[permission('p1', '读取文件'), permission('p2', '执行命令')]} decisions={new Map()} readOnly={false} onResolve={resolve} />);

    expect(screen.getByLabelText('待处理权限请求，共 2 项')).toHaveTextContent('1 / 2 待处理');
    expect(screen.getByText('读取文件')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '下一个' }));
    expect(screen.getByText('执行命令')).toBeInTheDocument();
    expect(screen.getByLabelText('待处理权限请求，共 2 项')).toHaveTextContent('2 / 2 待处理');
    expect(resolve).not.toHaveBeenCalled();
  });

  it('keeps the selected permission identity across unrelated projection updates', () => {
    const [items, setItems] = createSignal([permission('p1', '第一项'), permission('p2', '第二项')]);
    render(() => <PermissionQueue permissions={items()} decisions={new Map()} readOnly={false} onResolve={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: '下一个' }));
    setItems([permission('p0', '新增前置项'), ...items()]);

    expect(screen.getByText('第二项')).toBeInTheDocument();
    expect(screen.getByLabelText('待处理权限请求，共 3 项')).toHaveTextContent('3 / 3 待处理');
  });

  it('advances predictably when the selected request disappears', () => {
    const [items, setItems] = createSignal([permission('p1', '第一项'), permission('p2', '第二项'), permission('p3', '第三项')]);
    render(() => <PermissionQueue permissions={items()} decisions={new Map()} readOnly={false} onResolve={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: '下一个' }));
    setItems([permission('p1', '第一项'), permission('p3', '第三项')]);

    expect(screen.getByText('第三项')).toBeInTheDocument();
    expect(screen.getByLabelText('待处理权限请求，共 2 项')).toHaveTextContent('2 / 2 待处理');
  });

  it('applies a decision lock only to its matching permission id', () => {
    const decisions = new Map<string, PermissionDecisionState>([['p1', { commandId: 'cmd-1', permissionId: 'p1', decision: 'allow', phase: 'pending', retryable: false }]]);
    const resolve = vi.fn();
    render(() => <PermissionQueue permissions={[permission('p1', '第一项'), permission('p2', '第二项')]} decisions={decisions} readOnly={false} onResolve={resolve} />);
    expect(screen.getByRole('button', { name: /正在允许/ })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: '下一个' }));
    expect(screen.getByRole('button', { name: '允许' })).toBeEnabled();
    fireEvent.click(screen.getByRole('button', { name: '拒绝' }));
    expect(resolve).toHaveBeenCalledExactlyOnceWith('p2', 'deny');
  });
});
