// acp-hub Web 面板入口（`/`，唯一页面）—— Cookie AuthGate + 应用壳。

import { render } from 'solid-js/web';
import '../styles.css';
import { AppShell } from './components/AppShell';
import { Toasts } from './components/Toasts';
import { AuthGate } from './components/AuthGate';

render(
  () => (
    <>
      <AuthGate><AppShell /></AuthGate>
      <Toasts />
    </>
  ),
  document.getElementById('app')!,
);
