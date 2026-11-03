import { useState } from 'react';
import { ChatShell } from '../chat';
import { MonitorDialog } from '../settings';

/**
 * 前端只负责展示与命令转发：Agent 在服务端执行，配置与凭据由服务端环境变量管理。
 * 监控面板只展示服务端已去敏的只读快照。
 */
export default function App() {
  const [monitorOpen, setMonitorOpen] = useState(false);
  return <>
    {monitorOpen && <MonitorDialog onClose={() => setMonitorOpen(false)} />}
    <ChatShell onOpenSettings={() => setMonitorOpen(true)} />
  </>;
}
