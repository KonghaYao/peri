import { fireEvent, render, screen } from '@solidjs/testing-library';
import { describe, expect, it, vi } from 'vitest';
import { PermissionRequestCard } from './PermissionRequestCard';

const permission = {
  permissionId: 'permission-123456789', turnId: 'turn-1', toolCallId: 'tool-123456789',
  title: '执行 shell 命令', description: '读取当前项目的 Git 状态', status: 'pending',
  expiresAt: '2026-08-13T12:00:00Z', decision: null,
};

describe('PermissionRequestCard', () => {
  it('submits only the first security decision and exposes known request facts', () => {
    const resolve = vi.fn();
    const view = render(() => <PermissionRequestCard permission={permission} readOnly={false} onResolve={resolve} />);
    expect(screen.getByText('执行 shell 命令')).toBeInTheDocument();
    expect(screen.getByText('读取当前项目的 Git 状态')).toBeInTheDocument();
    expect(screen.getByText('tool tool-123…')).toHaveAttribute('title', 'tool-123456789');
    fireEvent.click(screen.getByRole('button', { name: '允许' }));
    expect(resolve).toHaveBeenCalledExactlyOnceWith('allow');
    view.unmount();
  });

  it('locks both opposing decisions while delivery is pending', () => {
    render(() => <PermissionRequestCard permission={permission} decision={{ commandId: 'cmd-1', permissionId: permission.permissionId, decision: 'allow', phase: 'pending', retryable: false }} readOnly={false} onResolve={vi.fn()} />);
    expect(screen.getByRole('button', { name: /正在允许/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: '拒绝' })).toBeDisabled();
    expect(screen.getByLabelText('执行 shell 命令')).toHaveAttribute('aria-busy', 'true');
    expect(screen.getByRole('status')).toHaveTextContent('正在允许');
  });

  it('keeps the lock and explains uncertainty instead of offering a contrary decision', () => {
    render(() => <PermissionRequestCard permission={permission} decision={{ commandId: 'cmd-1', permissionId: permission.permissionId, decision: 'deny', phase: 'uncertain', retryable: false }} readOnly={false} onResolve={vi.fn()} />);
    expect(screen.getByRole('alert')).toHaveTextContent('拒绝的结果尚未确认');
    expect(screen.getByRole('button', { name: '允许' })).toBeDisabled();
    expect(screen.getByRole('button', { name: /正在拒绝/ })).toBeDisabled();
    expect(screen.getByLabelText('执行 shell 命令')).not.toHaveAttribute('aria-busy');
    expect(screen.queryByRole('button', { name: '使用原请求重试' })).not.toBeInTheDocument();
  });

  it('offers the original command only after a definitely retryable delivery failure', () => {
    const retry = vi.fn();
    render(() => <PermissionRequestCard permission={permission} decision={{ commandId: 'cmd-1', permissionId: permission.permissionId, decision: 'allow', phase: 'uncertain', retryable: true }} readOnly={false} onResolve={vi.fn()} onRetry={retry} />);
    expect(screen.getByRole('alert')).toHaveTextContent('尚未送达');
    fireEvent.click(screen.getByRole('button', { name: '使用原请求重试' }));
    expect(retry).toHaveBeenCalledExactlyOnceWith('cmd-1');
  });

  it('closes mutation affordances for a read-only principal', () => {
    render(() => <PermissionRequestCard permission={permission} readOnly onResolve={vi.fn()} />);
    expect(screen.getByText('只读模式无法处理该请求。')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '允许' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '拒绝' })).toBeDisabled();
  });

  it('fails closed when a malformed projection has no permission identity', () => {
    const resolve = vi.fn();
    render(() => <PermissionRequestCard permission={{ ...permission, permissionId: null }} readOnly={false} onResolve={resolve} />);
    expect(screen.getByText(/请求缺少权限标识/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '允许' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '拒绝' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: '允许' }));
    expect(resolve).not.toHaveBeenCalled();
  });
});
