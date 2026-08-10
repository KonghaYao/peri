// acp-hub Web 面板入口（`/`，唯一页面）—— 三区布局 + 状态 rail。
//
// 协议与 TUI client 面一致 · token 仅存 sessionStorage（URL ?token= 一次
// 性注入后即从地址栏清理）。

import { render } from 'solid-js/web';
import '../styles.css';
import { ChatView } from './components/ChatView';
import { ConnectCard } from './components/ConnectCard';
import { InstanceList, ChatList, SessionList, WorkspaceList } from './components/Lists';
import { StatusRail } from './components/StatusRail';
import { Toasts } from './components/Toasts';

function App() {
  return (
    <>
      <header class="flex items-baseline gap-4 px-5 pt-3.5">
        <h1 class="m-0 text-xl font-semibold text-slate-900">acp-hub Web 面板</h1>
        <span class="text-xs text-slate-500">
          协议与 TUI client 面一致 · token 仅存 sessionStorage
        </span>
      </header>

      <div class="grid items-start gap-3 px-5 py-3 pb-5 md:grid-cols-[280px_1fr_280px] max-md:grid-cols-1">
        <aside>
          <ConnectCard />
          <WorkspaceList />
          <InstanceList />
          <ChatList />
          <SessionList />
        </aside>

        <main>
          <ChatView />
        </main>

        <aside>
          <StatusRail />
        </aside>
      </div>

      <Toasts />
    </>
  );
}

render(() => <App />, document.getElementById('app')!);
