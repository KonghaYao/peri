import { chatPane, emptyPane, groupDialog, sidebarPane } from './chat-panels';
import { chatClientScript } from './chat-script';
import { APP_STYLE } from './chat-styles';
import {
  DEVICE_HANDOFF_STYLE,
  HANDOFF_ADOPT_SCRIPT,
  deviceHandoffDialog,
} from './handoff';
import { layout } from './layout';

/**
 * 微信式双栏应用页：左侧会话列表，右侧聊天区（或空状态）。
 *
 * `roomId` 为 `null` 时渲染首页；有值时渲染同一套布局并载入该房间。
 * 服务端不在 HTML 中校验成员资格或输出房间数据；页面用浏览器 token
 * 调用工具层，权限判断始终留在 MCP 工具中。
 *
 * 移动端（`< 760px`）由 `<body>` 的 `page-home` / `page-room` 切为单栏：
 * 首页只显示列表，聊天页只显示聊天区与返回箭头。
 */
export function renderAppPage(roomId: string | null): string {
  const isHome = roomId === null;
  return layout(
    isHome ? 'Agent Network' : '聊天 · Agent Network',
    `<script>${HANDOFF_ADOPT_SCRIPT}</script>
${sidebarPane()}
${isHome ? emptyPane() : chatPane()}
${groupDialog()}
${deviceHandoffDialog()}
${chatClientScript(roomId)}`,
    APP_STYLE + DEVICE_HANDOFF_STYLE,
    { wide: true, bodyClass: isHome ? 'page-home' : 'page-room' },
  );
}

export function renderChatPage(roomId: string): string {
  return renderAppPage(roomId);
}
