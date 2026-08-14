import { fireEvent, render, screen } from '@solidjs/testing-library';
import { afterEach, describe, expect, it } from 'vitest';
import { setChatEntries, setPermissions, setRuntimeDocsState } from '../store';
import { MessageList } from './MessageList';

function resetStore() {
  setChatEntries([]);
  setPermissions([]);
  setRuntimeDocsState({ chat: false, control: false });
}

afterEach(resetStore);

describe('MessageList hydration', () => {
  it('describes recovery until both authoritative runtime documents arrive', () => {
    setRuntimeDocsState({ chat: true, control: false });
    render(() => <MessageList />);
    expect(screen.getByText('正在载入会话')).toBeInTheDocument();
    expect(screen.queryByText('开始这段对话')).not.toBeInTheDocument();
  });

  it('turns a confirmed empty projection into a meaningful first-message state', () => {
    setRuntimeDocsState({ chat: true, control: true });
    render(() => <MessageList />);
    expect(screen.getByText('开始这段对话')).toBeInTheDocument();
    expect(screen.getByText(/内容会持续保存到当前会话/)).toBeInTheDocument();
    expect(screen.queryByText('正在载入会话')).not.toBeInTheDocument();
  });

  it('exposes every simultaneous permission request in one navigable queue', () => {
    setRuntimeDocsState({ chat: true, control: true });
    setPermissions([
      { permissionId: 'p1', turnId: 't1', toolCallId: 'tool-1', title: '读取文件', description: null, status: 'pending', expiresAt: null, decision: null },
      { permissionId: 'p2', turnId: 't1', toolCallId: 'tool-2', title: '执行命令', description: null, status: 'pending', expiresAt: null, decision: null },
    ]);
    render(() => <MessageList />);

    expect(screen.getByLabelText('待处理权限请求，共 2 项')).toBeInTheDocument();
    expect(screen.getByText('读取文件')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '下一个' }));
    expect(screen.getByText('执行命令')).toBeInTheDocument();
  });
});
