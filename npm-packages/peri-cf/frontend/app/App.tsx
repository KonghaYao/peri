import { useState } from 'react';
import { ChatShell } from '../chat';
import { ServerInfoDialog } from '../settings';

/**
 * 前端只负责展示与命令转发：Agent 在服务端执行，配置与凭据由服务端环境变量管理。
 * 设置页只展示去敏后的服务端连接信息。
 */
export default function App() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  return <>
    {settingsOpen && <ServerInfoDialog onClose={() => setSettingsOpen(false)} />}
    <ChatShell onOpenSettings={() => setSettingsOpen(true)} />
  </>;
}
