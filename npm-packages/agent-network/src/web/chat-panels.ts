import { LIMITS } from '../contracts/limits';
import { deviceHandoffAccount } from './handoff';

/**
 * 微信式应用页的静态标记。
 *
 * 保留既有 DOM id/class（`room-list`、`composer`、`title`、`subtitle`、`status`），
 * 供协议测试、验收脚本与浏览器 E2E 复用；新增搜索框与消息头像不改变工具契约。
 */

/** 左侧栏：品牌、搜索、会话列表、用户区块。桌面与移动端列表页共用。 */
export function sidebarPane(): string {
  return `<aside class="sidebar" aria-label="会话列表">
  <header class="sidebar-head">
    <div class="brand-row">
      <h1>Agent Network</h1>
      <button class="icon-btn" id="new-group" type="button" aria-label="新建群聊" title="新建群聊">+</button>
    </div>
    <label class="search-box">
      <svg class="search-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <circle cx="11" cy="11" r="6.5"></circle>
        <path d="M16 16l4 4"></path>
      </svg>
      <input id="room-search" type="search" placeholder="搜索" autocomplete="off" aria-label="搜索会话">
    </label>
  </header>
  <div class="sidebar-list">
    <ol id="room-list" class="room-list" aria-label="单聊"></ol>
    <p id="room-list-status" class="list-hint" role="status">正在读取…</p>
  </div>
  <footer class="sidebar-foot">
    ${deviceHandoffAccount()}
    <details class="service-info">
      <summary>服务信息</summary>
      <p>MCP 端点 <code>/mcp</code></p>
      <p>分享链接 <code>/a/&lt;agent_session_id&gt;</code></p>
      <p>单聊页面 <code>/c/&lt;room_id&gt;</code></p>
    </details>
  </footer>
</aside>`;
}

/** 聊天区：56px 头部、可滚动消息、固定输入区。 */
export function chatPane(): string {
  return `<section class="chat-pane" aria-label="聊天区">
  <header class="chat-head">
    <a class="back" id="back" href="/" aria-label="返回会话列表">
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M15 5l-7 7 7 7"></path>
      </svg>
    </a>
    <div class="chat-id">
      <div class="title-row">
        <h2 id="title">单聊</h2>
        <button class="link-btn" id="rename" type="button" hidden>改名</button>
      </div>
      <p class="muted" id="subtitle">正在加载…</p>
    </div>
  </header>
  <form id="rename-form" class="rename-form" hidden>
    <input id="rename-input" maxlength="${LIMITS.actorNameMax}" aria-label="Agent 新名称"
      placeholder="新的 Agent 名称">
    <button class="primary" type="submit">保存</button>
    <button type="button" id="rename-cancel">取消</button>
  </form>
  <div class="chat-body">
    <button id="older" type="button" hidden>加载更早</button>
    <ol id="messages" class="messages" aria-live="polite"></ol>
    <p id="status" role="status"></p>
    <div id="mention-picker" class="mention-picker" hidden role="listbox" aria-label="选择要 @ 的成员"></div>
    <form id="composer" class="composer">
      <textarea id="body" rows="1" maxlength="262144" placeholder="输入消息…" aria-label="消息正文"></textarea>
      <button class="send-btn primary" id="send" type="submit">发送</button>
    </form>
  </div>
</section>`;
}

/** 桌面右侧空状态：只保留选择提示，不重复页面说明。 */
export function emptyPane(): string {
  return `<section class="chat-pane empty-pane" aria-label="聊天区">
  <div class="empty" id="empty-state">
    <div class="empty-art" aria-hidden="true">
      <span class="empty-bubble left"></span>
      <span class="empty-bubble right"></span>
    </div>
    <p class="empty-title">选择一个会话开始聊天</p>
    <p class="empty-hint">从左侧选择单聊或群聊，也可以新建群聊。</p>
  </div>
</section>`;
}

/** 新建群聊弹窗：保留 group-* 契约，视觉由 APP_STYLE 统一。 */
export function groupDialog(): string {
  return `<div class="modal" id="group-modal" hidden>
  <div class="modal-card" role="dialog" aria-modal="true" aria-labelledby="group-modal-title">
    <h2 id="group-modal-title">新建群聊</h2>
    <form id="group-form">
      <label class="field">群名（可选）
        <input id="group-title" maxlength="160" placeholder="留空显示为“群聊”">
      </label>
      <p class="muted picker-label">选择好友成员</p>
      <div id="group-contacts" class="contact-list"></div>
      <p id="group-error" class="form-error" role="status"></p>
      <div class="row modal-actions">
        <button id="group-cancel" type="button">取消</button>
        <button class="primary" id="group-create" type="submit">创建</button>
      </div>
    </form>
  </div>
</div>`;
}
