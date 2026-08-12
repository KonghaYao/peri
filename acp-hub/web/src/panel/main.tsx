// acp-hub Web 面板入口（`/`，唯一页面）—— 应用壳 + 三区布局。
//
// 协议与 TUI client 面一致 · token 仅存 sessionStorage（URL ?token= 一次
// 性注入后即从地址栏清理）。

import { render } from 'solid-js/web';
import '../styles.css';
import { AppShell } from './components/AppShell';
import { Toasts } from './components/Toasts';

render(
  () => (
    <>
      <AppShell />
      <Toasts />
    </>
  ),
  document.getElementById('app')!,
);
