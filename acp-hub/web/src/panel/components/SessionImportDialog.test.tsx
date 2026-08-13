import { fireEvent, render, screen } from '@solidjs/testing-library';
import { describe, expect, it, vi } from 'vitest';
import { SessionImportDialog, type SessionImportDialogProps } from './SessionImportDialog';

function props(overrides: Partial<SessionImportDialogProps> = {}): SessionImportDialogProps {
  return {
    open: true,
    project: { id: 'p1', name: 'Perihelion', cwd: '/repo', instanceId: 'local', createdAt: null, updatedAt: null, archivedAt: null },
    sessions: [
      { sessionId: 'acp-one', title: '架构重构', status: null, updatedAt: '2026-08-13T10:00:00Z', cwd: '/repo' },
      { sessionId: 'acp-other', title: '其他目录', status: null, updatedAt: null, cwd: '/other' },
    ],
    onClose: vi.fn(),
    onImport: vi.fn(() => true),
    ...overrides,
  };
}

describe('SessionImportDialog', () => {
  it('shows only cwd-scoped candidates and filters by stable id', () => {
    render(() => <SessionImportDialog {...props()} />);
    expect(screen.getByRole('button', { name: /架构重构/ })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /其他目录/ })).not.toBeInTheDocument();
    fireEvent.input(screen.getByRole('textbox', { name: '搜索会话' }), { target: { value: 'missing' } });
    expect(screen.getByText('没有可导入的会话')).toBeInTheDocument();
  });

  it('closes only after the import commits', () => {
    const onClose = vi.fn();
    let commit = () => {};
    const onImport = vi.fn((_projectId, _sessionId, onCommitted) => { commit = onCommitted; return true; });
    render(() => <SessionImportDialog {...props({ onClose, onImport })} />);
    fireEvent.click(screen.getByRole('button', { name: /架构重构/ }));
    fireEvent.click(screen.getByRole('button', { name: '导入所选会话' }));
    expect(onImport).toHaveBeenCalledWith('p1', 'acp-one', expect.any(Function), expect.any(Function));
    expect(onClose).not.toHaveBeenCalled();
    commit();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('shows an explicit fact-only review before import', () => {
    render(() => <SessionImportDialog {...props()} />);
    expect(screen.queryByRole('region', { name: '待导入会话详情' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /架构重构/ }));
    const review = screen.getByRole('region', { name: '待导入会话详情' });
    expect(review).toHaveTextContent('架构重构');
    expect(review).toHaveTextContent('/repo');
    expect(review).toHaveTextContent('acp-one');
    expect(review).toHaveTextContent('ACP 当前没有提供消息内容预览');
    expect(screen.getByRole('button', { name: /架构重构/ })).toHaveAttribute('aria-controls', 'import-session-review');
  });

  it('cannot submit a selection hidden by search or catalog refresh', () => {
    const onImport = vi.fn(() => true);
    const view = render(() => <SessionImportDialog {...props({ onImport })} />);
    fireEvent.click(screen.getByRole('button', { name: /架构重构/ }));
    fireEvent.input(screen.getByRole('textbox', { name: '搜索会话' }), { target: { value: 'missing' } });
    expect(screen.getByRole('button', { name: '导入所选会话' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: '导入所选会话' }));
    expect(onImport).not.toHaveBeenCalled();
    view.unmount();
  });

  it('retains selection and dialog context after a definite failure', () => {
    const onClose = vi.fn();
    const onImport = vi.fn((_projectId, _sessionId, _committed, failed) => { failed('failed'); return true; });
    render(() => <SessionImportDialog {...props({ onClose, onImport })} />);
    fireEvent.click(screen.getByRole('button', { name: /架构重构/ }));
    fireEvent.click(screen.getByRole('button', { name: '导入所选会话' }));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: /架构重构/ })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('button', { name: '导入所选会话' })).not.toBeDisabled();
    expect(screen.getByRole('alert')).toHaveTextContent('服务器明确拒绝');
  });

  it('distinguishes an uncertain timeout from a definite rejection', () => {
    const onImport = vi.fn((_projectId, _sessionId, _committed, failed) => { failed('uncertain'); return true; });
    render(() => <SessionImportDialog {...props({ onImport })} />);
    fireEvent.click(screen.getByRole('button', { name: /架构重构/ }));
    fireEvent.click(screen.getByRole('button', { name: '导入所选会话' }));
    expect(screen.getByRole('alert')).toHaveTextContent('导入结果尚未确认');
    expect(screen.getByRole('alert')).toHaveTextContent('不要创建新的重复请求');
  });
});
