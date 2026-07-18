// ========== 快捷键转发（iframe → parent） ==========
// 快捷键在 iframe 内无法冒泡到父窗口，这里捕获后 postMessage 转发。
// 父 shell 本身也绑定同一套快捷键（见 AppShell.js）。父页面请勿 import 本模块。

if (window.parent === window) {
  throw new Error('[keys] parent shell 禁止使用 keys.js（快捷键已在 AppShell 内绑定）');
}
//
// 快捷键表：
//   Ctrl/Cmd + `   → toggle-terminal
//   Ctrl/Cmd + B   → toggle-sidebar
//   Ctrl/Cmd + J   → toggle-terminal（备用）
//   Escape         → escape（关闭浮层）

const KEY_MAP = (e) => {
  const mod = e.ctrlKey || e.metaKey;
  const k = e.key;
  if (mod && (k === '`' || k === '~')) return 'toggle-terminal';
  if (mod && (k === 'j' || k === 'J')) return 'toggle-terminal';
  if (mod && (k === 'b' || k === 'B')) return 'toggle-sidebar';
  if (k === 'Escape') return 'escape';
  return null;
};

// capture 阶段：优先于 xterm 等组件的 keydown 处理
window.addEventListener('keydown', (e) => {
  const action = KEY_MAP(e);
  if (!action) return;
  if (!window.parent || window.parent === window) return;
  // Escape 不阻止默认行为（页面自身可能也要响应，如关闭下拉）
  if (action !== 'escape') {
    e.preventDefault();
    e.stopPropagation();
  }
  window.parent.postMessage({ type: 'peri:key', action }, '*');
}, true);
