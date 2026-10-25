import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ChatSummary } from '../runtime';
import { ChatHeader } from './ChatHeader';
import { ChatRightPanel } from './ChatRightPanel';
import { Sidebar, type SidebarProps } from './Sidebar';

const summary = (id: string, title: string): ChatSummary => ({
  id, sessionId: id, title, updatedAt: '2026-10-20T00:00:00Z',
  cwd: '/workspace', messageCount: 1, createdAt: '2026-10-20T00:00:00Z',
});

const selected = summary('chat-a', '选中会话');
const other = summary('chat-b', '另一个会话');

function renderSidebar(overrides: Partial<SidebarProps> = {}): string {
  return renderToStaticMarkup(
    <Sidebar
      chats={[selected, other]}
      selectedId={selected.id}
      selectedRunning={false}
      loading={false}
      disabled={false}
      open
      onClose={() => {}}
      onNew={() => {}}
      onRefresh={() => {}}
      onSelect={() => {}}
      {...overrides}
    />,
  );
}

function rowOf(html: string, title: string): string {
  // 每行以 data-slot 标记开始，标题在行内；按标记切分后按标题定位。
  return html.split('data-slot="peri-sidebar-thread-row"').find((row) => row.includes(title)) ?? '';
}

describe('Sidebar running indicator', () => {
  test('only the selected row can show replica running state', () => {
    const html = renderSidebar({ selectedRunning: false });
    expect(rowOf(html, '选中会话')).toContain('data-running="false"');
    expect(rowOf(html, '另一个会话')).toContain('data-running="false"');
  });

  test('the selected row shows the replica running fact', () => {
    const html = renderSidebar({ selectedRunning: true });
    expect(rowOf(html, '选中会话')).toContain('data-running="true"');
    expect(rowOf(html, '另一个会话')).toContain('data-running="false"');
  });
});

describe('Chat header and details panel', () => {
  test('exposes expanded state and controlled panel ids', () => {
    const html = renderToStaticMarkup(
      <ChatHeader
        title="会话"
        detailsOpen
        sidebarOpen={false}
        onOpenSidebar={() => {}}
        onToggleDetails={() => {}}
        onOpenSettings={() => {}}
      />,
    );
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain('aria-controls="peri-right-panel"');
    expect(html).toContain('aria-controls="peri-sidebar"');
    expect(html).toContain('aria-expanded="false"');
  });

  test('renders the details panel as hidden when closed and visible when open', () => {
    const closed = renderToStaticMarkup(<ChatRightPanel open={false} state={null} onClose={() => {}} />);
    expect(closed).toContain('id="peri-right-panel"');
    expect(closed).toContain('aria-hidden="true"');
    expect(closed).toContain('invisible');

    const open = renderToStaticMarkup(<ChatRightPanel open state={null} onClose={() => {}} />);
    expect(open).toContain('aria-hidden="false"');
    expect(open).not.toContain('invisible');
    expect(open).toContain('会话尚未加载。');
  });
});
