import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ChatSummary } from '../runtime';
import { ChatHeader } from './ChatHeader';
import { ChatRightPanel } from './ChatRightPanel';
import { Sidebar, type SidebarProps } from './Sidebar';

const summary = (id: string, title: string, running: boolean): ChatSummary => ({
  id, title, updatedAt: '2026-10-20T00:00:00Z',
  messageCount: 1, running, pendingInteractionCount: 0, lastTurnStatus: null, lastCompletedAt: null,
});

const selected = summary('chat-a', '选中会话', true);
const other = summary('chat-b', '另一个会话', true);

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

describe('Sidebar live running authority', () => {
  test('the selected row uses live running state instead of stale list facts', () => {
    const html = renderSidebar({ selectedRunning: false });
    expect(rowOf(html, '选中会话')).toContain('data-running="false"');
    expect(rowOf(html, '另一个会话')).toContain('data-running="true"');
  });

  test('a live running selected row shows the indicator even if the list is stale', () => {
    const stale = { ...selected, running: false };
    const html = renderSidebar({ chats: [stale, other], selectedRunning: true });
    expect(rowOf(html, '选中会话')).toContain('data-running="true"');
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
