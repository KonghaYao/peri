import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
// 样式先于 App 求值：组件模块自行 import 的样式（如 chat-welcome.css 的
// `@layer components`）必须在 Tailwind 的层顺序声明之后，否则 components 会
// 成为最先声明的层，被 base/preflight 反向覆盖。Tailwind（分层）在前，
// 既有全局样式（未分层）在后；chat token 最后加载，消息流样式由 token 文件引入。
import '../tailwind.css';
import '../styles.css';
import '../styles/chat-tokens.css';
import App from './App';

const root = document.getElementById('root');
if (!root) throw new Error('页面缺少应用挂载节点。');
createRoot(root).render(<StrictMode><App /></StrictMode>);
