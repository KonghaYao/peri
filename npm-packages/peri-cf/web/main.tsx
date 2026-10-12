import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import { createVfs } from './vfs';
import './styles.css';

const root = document.getElementById('root');
if (!root) throw new Error('页面缺少应用挂载节点。');
// 同一个库名只能有一个 VFS 实例（第二个要等前一个空闲 500ms 释放跨标签页锁），
// 所以在这里建一次、由 App 显式持有，不放进会重复求值的组件状态里。
createRoot(root).render(<StrictMode><App vfs={createVfs()} /></StrictMode>);
