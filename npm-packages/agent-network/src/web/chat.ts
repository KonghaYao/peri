import { LIMITS } from '../contracts/limits';
import { jsonForScript, layout } from './layout';

const APP_STYLE = `
  main { display: flex; min-height: 0; }
  .sidebar { flex: 0 0 300px; width: 300px; display: flex; flex-direction: column; min-height: 0;
    background: #f7f7f7; border-right: 1px solid #e2e2e2; }
  .sidebar-head { flex: 0 0 auto; display: flex; align-items: baseline; gap: 8px; padding: 16px 16px 10px; }
  .sidebar-head h1 { margin: 0; font-size: 18px; }
  .me { margin-left: auto; max-width: 120px; font-size: 12px; color: #888; overflow: hidden;
    text-overflow: ellipsis; white-space: nowrap; }
  .sidebar-list { flex: 1 1 auto; min-height: 0; overflow-y: auto; }
  .room-list { list-style: none; margin: 0; padding: 0 8px 8px; }
  .room-list li { margin: 2px 0; }
  .room-item { display: flex; align-items: center; gap: 10px; padding: 10px; border-radius: 8px;
    color: inherit; text-decoration: none; }
  .room-item:hover { background: #eaeaea; }
  .room-item.active { background: #e0e0e0; }
  .avatar { flex: 0 0 auto; width: 40px; height: 40px; border-radius: 50%; background: #5b8def;
    color: #fff; display: grid; place-items: center; font-size: 16px; }
  .room-main { flex: 1 1 auto; min-width: 0; }
  .room-top { display: flex; align-items: baseline; gap: 8px; }
  .room-name { flex: 1 1 auto; min-width: 0; font-size: 15px; overflow: hidden;
    text-overflow: ellipsis; white-space: nowrap; }
  .room-time { flex: 0 0 auto; font-size: 12px; color: #999; }
  .room-preview { margin-top: 2px; font-size: 13px; color: #888; overflow: hidden;
    text-overflow: ellipsis; white-space: nowrap; }
  .unread { flex: 0 0 auto; width: 8px; height: 8px; border-radius: 50%; background: #fa5151; }
  .list-hint { margin: 0; padding: 24px 16px; color: #888; font-size: 14px; line-height: 1.6; }
  .sidebar-foot { flex: 0 0 auto; padding: 6px 16px 14px; font-size: 12px; color: #888; }
  .sidebar-foot details { margin: 0; }
  .sidebar-foot p { margin: 6px 0 0; }
  .sidebar-foot code { font-size: 12px; }
  .chat-pane { flex: 1 1 auto; min-width: 0; min-height: 0; display: flex; flex-direction: column;
    background: #f5f5f5; }
  .chat-pane .empty { flex: 1 1 auto; display: grid; place-items: center; color: #999; font-size: 15px; }
  .chat-head { flex: 0 0 auto; display: flex; align-items: center; gap: 8px; min-height: 56px;
    padding: 8px 16px; background: #f7f7f7; border-bottom: 1px solid #e2e2e2; }
  .back { display: none; flex: 0 0 auto; align-items: center; justify-content: center; width: 28px;
    height: 32px; color: inherit; text-decoration: none; font-size: 26px; line-height: 1; }
  .chat-id { min-width: 0; }
  .chat-id h2 { margin: 0; font-size: 16px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chat-id .muted { margin: 0; font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chat-body { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; }
  #older { align-self: center; flex: 0 0 auto; margin: 10px 0 0; padding: 4px 12px; font-size: 12px; }
  .messages { flex: 1 1 auto; min-height: 0; overflow-y: auto; list-style: none; margin: 0; padding: 16px; }
  .msg { display: flex; flex-direction: column; align-items: flex-start; margin-bottom: 14px; }
  .msg.mine { align-items: flex-end; }
  .msg-meta { font-size: 11px; color: #999; margin-bottom: 4px; }
  .msg .bubble { max-width: min(560px, 78%); padding: 9px 12px; border-radius: 10px; background: #fff;
    overflow-wrap: anywhere; white-space: pre-wrap; }
  .msg.mine .bubble { background: #1c64f2; color: #fff; }
  .msg .expand { margin-top: 4px; border: 0; background: none; padding: 0; color: #1c64f2; font-size: 12px; }
  .composer { flex: 0 0 auto; display: flex; gap: 8px; align-items: flex-end; padding: 10px 14px;
    background: #f7f7f7; border-top: 1px solid #e2e2e2; }
  .composer textarea { flex: 1 1 auto; min-height: 42px; max-height: 132px; padding: 10px 12px;
    border: 1px solid #dcdcdc; border-radius: 8px; resize: none; font: inherit; color: inherit; background: #fff; }
  #send { flex: 0 0 auto; }
  #status { min-height: 0; margin: 0; font-size: 12px; color: #c0392b; }
  #status:empty { display: none; }
  #status:not(:empty) { padding: 6px 16px; }
  @media (max-width: 759px) {
    .sidebar { flex-basis: 100%; width: 100%; border-right: 0; }
    .chat-pane { width: 100%; }
    body.page-home .chat-pane { display: none; }
    body.page-room .sidebar { display: none; }
    body.page-room .back { display: inline-flex; }
    .msg .bubble { max-width: 84%; }
  }
  @media (prefers-color-scheme: dark) {
    .sidebar, .chat-head, .composer { background: #191c1f; border-color: #2a2f34; }
    .room-item:hover { background: #22272c; }
    .room-item.active { background: #2a2f34; }
    .chat-pane { background: #111315; }
    .msg .bubble { background: #22272c; color: #e8eaed; }
    .msg.mine .bubble { background: #2563eb; color: #fff; }
    .composer textarea { background: #22272c; border-color: #3a4148; }
  }
`;

function chatPane(): string {
  return `<section class="chat-pane" aria-label="聊天区">
  <header class="chat-head">
    <a class="back" id="back" href="/" aria-label="返回会话列表">‹</a>
    <div class="chat-id">
      <h2 id="title">单聊</h2>
      <p class="muted" id="subtitle">正在加载…</p>
    </div>
  </header>
  <div class="chat-body">
    <button id="older" type="button" hidden>加载更早</button>
    <ol id="messages" class="messages" aria-live="polite"></ol>
    <p id="status" role="status"></p>
    <form id="composer" class="composer">
      <textarea id="body" rows="2" maxlength="262144" placeholder="输入消息…" aria-label="消息正文"></textarea>
      <button class="primary" id="send" type="submit">发送</button>
    </form>
  </div>
</section>`;
}

function emptyPane(): string {
  return `<section class="chat-pane empty-pane" aria-label="聊天区">
  <div class="empty" id="empty-state">选择一个单聊开始聊天</div>
</section>`;
}

/**
 * 微信式双栏应用页：左侧会话列表，右侧聊天区（或空状态）。
 *
 * `roomId` 为 `null` 时渲染首页；有值时渲染同一套布局并载入该房间。
 * 服务端不在 HTML 中校验成员资格或输出房间数据；页面用浏览器 token
 * 调用工具层，权限判断始终留在 MCP 工具中。
 */
export function renderAppPage(roomId: string | null): string {
  const roomJson = roomId === null ? 'null' : jsonForScript(roomId);
  const isHome = roomId === null;
  return layout(
    isHome ? 'Agent Network' : '单聊 · Agent Network',
    `<aside class="sidebar" aria-label="会话列表">
  <header class="sidebar-head">
    <h1>Agent Network</h1>
    <span class="me" id="me"></span>
  </header>
  <div class="sidebar-list">
    <ol id="room-list" class="room-list" aria-label="单聊"></ol>
    <p id="room-list-status" class="list-hint">正在读取…</p>
  </div>
  <footer class="sidebar-foot">
    <details>
      <summary>服务信息</summary>
      <p>MCP 端点 <code>/mcp</code></p>
      <p>分享链接 <code>/a/&lt;agent_session_id&gt;</code></p>
      <p>单聊页面 <code>/c/&lt;room_id&gt;</code></p>
    </details>
  </footer>
</aside>
${isHome ? emptyPane() : chatPane()}
<script>
const roomId = ${roomJson};
const TOKEN_KEY = 'agent-network-token';
const MAX_CHUNKS = 8;
// 服务端 LIMITS.invokeMax 的内联值：阈值只在 limits.ts 维护，页面不复制字面量。
const INVOKE_MAX = ${LIMITS.invokeMax};
const roomListElement = document.getElementById('room-list');
const listStatusElement = document.getElementById('room-list-status');
const meElement = document.getElementById('me');
const titleElement = document.getElementById('title');
const subtitleElement = document.getElementById('subtitle');
const messagesElement = document.getElementById('messages');
const olderButton = document.getElementById('older');
const composer = document.getElementById('composer');
const bodyInput = document.getElementById('body');
const sendButton = document.getElementById('send');
const statusElement = document.getElementById('status');

let identity = null;
let members = new Map();
let room = null;
let nextOlder = null;
let refreshChain = Promise.resolve();
let refreshQueued = false;
let reloadTimer = null;
let roomEntries = [];
// 发送失败/投递缺失/截断提示必须跨过 SSE 触发的刷新，不能在 300ms 后被清空。
let stickyStatus = '';

function browserToken() {
  return localStorage.getItem(TOKEN_KEY);
}

function requestId(prefix) {
  return prefix + '_' + crypto.randomUUID().replaceAll('-', '');
}

async function api(method, params, toolName) {
  const token = browserToken();
  if (!token) throw new Error('没有浏览器身份，请通过邀请链接进入');
  const headers = {
    'content-type': 'application/json',
    accept: method === 'subscriptions/listen' ? 'text/event-stream' : 'application/json, text/event-stream',
    'mcp-protocol-version': '2026-07-28',
    'mcp-method': method,
    authorization: 'Bearer ' + token
  };
  if (toolName) headers['mcp-name'] = toolName;
  const response = await fetch('/mcp', {
    method: 'POST',
    headers: headers,
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: crypto.randomUUID(),
      method: method,
      params: Object.assign({}, params, {
        _meta: {
          'io.modelcontextprotocol/protocolVersion': '2026-07-28',
          'io.modelcontextprotocol/clientInfo': { name: 'agent-network-web', version: '0.1.0' },
          'io.modelcontextprotocol/clientCapabilities': {}
        }
      })
    })
  });
  return response;
}

async function rpc(method, params, toolName) {
  const response = await api(method, params, toolName);
  const payload = await response.json().catch(function () { return null; });
  if (!payload) throw new Error('服务无响应');
  if (payload.error) throw new Error(payload.error.message || '请求失败');
  return payload.result;
}

async function callTool(name, args) {
  const result = await rpc('tools/call', { name: name, arguments: args }, name);
  if (result.structuredContent) return result.structuredContent;
  const item = (result.content || []).find(function (entry) { return entry.type === 'text'; });
  return item ? JSON.parse(item.text) : {};
}

async function ensureIdentity() {
  if (!identity) identity = await callTool('network_me', {});
  return identity;
}

/**
 * 收集网页人类发送消息时要唤醒的 Agent 成员。
 *
 * 只使用 network_room_get 已缓存的 members，不额外发请求；人类成员与
 * 发送者自己不进入 invoke。超出服务端上限时截断，由调用方向状态栏提示。
 */
function agentInvokeTargets() {
  if (!identity || identity.kind !== 'human') return { targets: [], skipped: 0 };
  const targets = [];
  for (const member of members.values()) {
    if (member.kind === 'agent' && member.actor_id !== identity.actor_id) {
      targets.push(member.actor_id);
    }
  }
  return {
    targets: targets.slice(0, INVOKE_MAX),
    skipped: Math.max(0, targets.length - INVOKE_MAX)
  };
}

function showError(error) {
  const message = String(error && error.message ? error.message : error);
  if (statusElement) {
    statusElement.textContent = message;
  } else if (listStatusElement) {
    listStatusElement.hidden = false;
    listStatusElement.textContent = message;
  }
  if (sendButton) sendButton.disabled = true;
  if (bodyInput) bodyInput.disabled = true;
}

function firstChar(name) {
  const chars = Array.from(String(name || '单聊').trim());
  return chars.length > 0 ? chars[0] : '聊';
}

function relativeTime(value) {
  const ms = Number(value);
  if (!ms || Number.isNaN(ms)) return '';
  const now = Date.now();
  const diff = now - ms;
  if (diff < 60000) return '刚刚';
  if (diff < 3600000) return Math.floor(diff / 60000) + ' 分钟前';
  const date = new Date(ms);
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return '昨天';
  const month = date.getMonth() + 1;
  const day = date.getDate();
  if (date.getFullYear() === new Date(now).getFullYear()) return month + '/' + day;
  return date.getFullYear() + '/' + month + '/' + day;
}

function messageTime(entry) {
  return entry.last_message ? Number(entry.last_message.created_at) : 0;
}

function sortRooms(entries) {
  return entries.slice().sort(function (a, b) {
    const byMessage = messageTime(b) - messageTime(a);
    if (byMessage !== 0) return byMessage;
    return Number(b.updated_at) - Number(a.updated_at);
  });
}

function renderRooms() {
  roomListElement.replaceChildren();
  for (const entry of roomEntries) {
    const item = document.createElement('li');
    const link = document.createElement('a');
    link.className = 'room-item' + (entry.room_id === roomId ? ' active' : '');
    link.href = '/c/' + encodeURIComponent(entry.room_id);
    if (entry.room_id === roomId) link.setAttribute('aria-current', 'page');
    const avatar = document.createElement('span');
    avatar.className = 'avatar';
    avatar.setAttribute('aria-hidden', 'true');
    avatar.textContent = firstChar(entry.name);
    const main = document.createElement('div');
    main.className = 'room-main';
    const top = document.createElement('div');
    top.className = 'room-top';
    const name = document.createElement('span');
    name.className = 'room-name';
    name.textContent = entry.name;
    const time = document.createElement('span');
    time.className = 'room-time';
    time.textContent = relativeTime(entry.last_message ? entry.last_message.created_at : entry.updated_at);
    top.append(name, time);
    const preview = document.createElement('div');
    preview.className = 'room-preview';
    preview.textContent = entry.last_message ? entry.last_message.preview : '还没有消息';
    main.append(top, preview);
    link.append(avatar, main);
    if (entry.my_last_read_seq !== entry.head_seq) {
      const dot = document.createElement('span');
      dot.className = 'unread';
      dot.setAttribute('aria-label', '未读');
      link.append(dot);
    }
    item.append(link);
    roomListElement.append(item);
  }
}

function updateRoomEntry(roomValue, lastItem, readSucceeded) {
  const entry = roomEntries.find(function (item) { return item.room_id === roomValue.room_id; });
  if (!entry) return;
  entry.head_seq = roomValue.head_seq;
  entry.updated_at = roomValue.updated_at;
  if (readSucceeded) entry.my_last_read_seq = roomValue.head_seq;
  if (lastItem) entry.last_message = lastItem;
  roomEntries = sortRooms(roomEntries);
  renderRooms();
}

async function loadRooms() {
  if (!browserToken()) {
    listStatusElement.hidden = false;
    listStatusElement.textContent = '这个浏览器还没有身份。打开 Agent 分享链接，即可添加好友并开始单聊。';
    return;
  }
  try {
    const me = await ensureIdentity();
    if (meElement) meElement.textContent = me.name || '';
    const page = await callTool('network_rooms_list', { limit: 50 });
    const entries = await Promise.all(page.items.map(async function (entry) {
      let name = entry.kind === 'direct' ? '单聊' : entry.title || '群聊';
      if (entry.kind === 'direct') {
        try {
          const detail = await callTool('network_room_get', { room_id: entry.room_id });
          const others = detail.members
            .filter(function (member) { return member.actor_id !== me.actor_id; })
            .map(function (member) { return member.name; });
          if (others.length > 0) name = others.join('、');
        } catch (error) {
          // 房间不可读时保留占位名称，列表仍可展示。
        }
      }
      return {
        room_id: entry.room_id,
        kind: entry.kind,
        name: name,
        head_seq: entry.head_seq,
        my_last_read_seq: entry.my_last_read_seq,
        last_message: entry.last_message,
        updated_at: entry.updated_at
      };
    }));
    roomEntries = sortRooms(entries);
    renderRooms();
    if (roomEntries.length === 0) {
      listStatusElement.hidden = false;
      listStatusElement.textContent = '还没有单聊。打开 Agent 分享链接即可开始。';
    } else {
      listStatusElement.hidden = true;
    }
  } catch (error) {
    listStatusElement.hidden = false;
    listStatusElement.textContent = '读取单聊失败：' + (error && error.message ? error.message : String(error));
  }
}

function senderName(senderId) {
  if (identity && senderId === identity.actor_id) return '我';
  const member = members.get(senderId);
  return member ? member.name : senderId;
}

function formatTime(value) {
  const date = new Date(Number(value));
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleString();
}

function messageItem(item) {
  const mine = Boolean(identity && item.sender_id === identity.actor_id);
  const element = document.createElement('li');
  element.className = 'msg' + (mine ? ' mine' : '');
  const meta = document.createElement('div');
  meta.className = 'msg-meta';
  meta.textContent = mine
    ? formatTime(item.created_at)
    : senderName(item.sender_id) + ' · ' + formatTime(item.created_at);
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  bubble.textContent = item.preview;
  element.append(meta, bubble);
  if (item.body_bytes > new TextEncoder().encode(item.preview).length) {
    const expand = document.createElement('button');
    expand.type = 'button';
    expand.className = 'expand';
    expand.textContent = '展开全文';
    expand.addEventListener('click', async function () {
      expand.disabled = true;
      try {
        bubble.textContent = await readMessage(item.message_id);
        expand.remove();
      } catch (error) {
        expand.disabled = false;
        statusElement.textContent = '读取正文失败：' + error.message;
      }
    });
    element.append(expand);
  }
  return element;
}

async function readMessage(messageId) {
  let chunk = await callTool('network_message_read', { message_id: messageId });
  let text = chunk.chunk;
  let cursor = chunk.next_cursor;
  for (let index = 0; index < MAX_CHUNKS && cursor && !chunk.eof; index += 1) {
    chunk = await callTool('network_message_read', {
      message_id: messageId,
      chunk_cursor: cursor
    });
    text += chunk.chunk;
    cursor = chunk.next_cursor;
  }
  if (cursor) text += '\\n…（消息过长，已截断）';
  return text;
}

function scrollToBottom(force) {
  const nearBottom =
    messagesElement.scrollHeight - messagesElement.scrollTop - messagesElement.clientHeight < 80;
  if (force || nearBottom) messagesElement.scrollTop = messagesElement.scrollHeight;
}

async function loadLatest(forceScroll) {
  const page = await callTool('network_history_list', {
    room_id: roomId,
    direction: 'before',
    limit: 50
  });
  nextOlder = page.has_more ? page.next_cursor : null;
  olderButton.hidden = !nextOlder;
  messagesElement.replaceChildren();
  const items = page.items.slice().reverse();
  for (const item of items) messagesElement.append(messageItem(item));
  scrollToBottom(forceScroll);
  return page;
}

async function loadOlder() {
  if (!nextOlder) return;
  olderButton.disabled = true;
  try {
    const page = await callTool('network_history_list', {
      room_id: roomId,
      direction: 'before',
      cursor: nextOlder,
      limit: 50
    });
    const previousHeight = messagesElement.scrollHeight;
    const fragment = document.createDocumentFragment();
    for (const item of page.items.slice().reverse()) fragment.append(messageItem(item));
    messagesElement.prepend(fragment);
    messagesElement.scrollTop += messagesElement.scrollHeight - previousHeight;
    nextOlder = page.has_more ? page.next_cursor : null;
    olderButton.hidden = !nextOlder;
  } catch (error) {
    statusElement.textContent = '加载更早消息失败：' + error.message;
  } finally {
    olderButton.disabled = false;
  }
}

/**
 * 串行刷新，且合并等待中的请求。
 *
 * 刷新期间到达的新消息或自己刚发出的消息会再排一次，
 * 不会被“正在刷新”直接丢弃；否则首屏刷新会覆盖刚发送的气泡。
 */
function refresh(forceScroll) {
  if (refreshQueued) return refreshChain;
  refreshQueued = true;
  refreshChain = refreshChain
    .catch(function () {})
    .then(function () {
      refreshQueued = false;
      return doRefresh(forceScroll);
    });
  return refreshChain;
}

async function doRefresh(forceScroll) {
  const detail = await callTool('network_room_get', { room_id: roomId });
  room = detail.room;
  members = new Map(detail.members.map(function (member) { return [member.actor_id, member]; }));
  const others = detail.members
    .filter(function (member) { return member.actor_id !== identity.actor_id; })
    .map(function (member) { return member.name; });
  titleElement.textContent =
    room.kind === 'direct' ? (others.join('、') || '单聊') : (room.title || '群聊');
  const subtitle =
    room.kind === 'direct'
      ? ''
      : detail.members.map(function (member) { return member.name; }).join('、');
  subtitleElement.textContent = subtitle;
  subtitleElement.hidden = subtitle === '';
  const page = await loadLatest(forceScroll);
  statusElement.textContent = stickyStatus;
  let readSucceeded = false;
  try {
    await callTool('network_room_mark_read', {
      room_id: roomId,
      through_seq: room.head_seq
    });
    readSucceeded = true;
  } catch (error) {
    // 已读失败不影响聊天；保留未读标记，下次刷新重试。
  }
  updateRoomEntry(room, page.items[0] || null, readSucceeded);
}

function scheduleReload() {
  if (reloadTimer) return;
  reloadTimer = setTimeout(function () {
    reloadTimer = null;
    refresh(false).catch(showError);
  }, 300);
}

async function listen() {
  const uri = 'agentnet://rooms/' + roomId + '/head';
  while (true) {
    try {
      const response = await api('subscriptions/listen', {
        notifications: { resourceSubscriptions: [uri] }
      });
      if (!response.ok || !response.body) throw new Error('订阅失败');
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      while (true) {
        const step = await reader.read();
        if (step.done) break;
        buffer += decoder.decode(step.value, { stream: true }).replaceAll('\\r\\n', '\\n');
        let boundary = buffer.indexOf('\\n\\n');
        while (boundary >= 0) {
          const frame = buffer.slice(0, boundary);
          buffer = buffer.slice(boundary + 2);
          const data = frame
            .split('\\n')
            .filter(function (line) { return line.startsWith('data:'); })
            .map(function (line) { return line.slice(5).trimStart(); })
            .join('\\n');
          if (data) {
            try {
              const event = JSON.parse(data);
              if (event.method === 'notifications/resources/updated' &&
                  event.params && event.params.uri === uri) {
                scheduleReload();
              }
            } catch (error) {
              // 忽略非 JSON 帧
            }
          }
          boundary = buffer.indexOf('\\n\\n');
        }
      }
    } catch (error) {
      statusElement.textContent = '实时连接中断，正在重连…';
    }
    await new Promise(function (resolve) { return setTimeout(resolve, 2000); });
  }
}

async function initChat() {
  if (!browserToken()) {
    showError('没有浏览器身份，请通过邀请链接进入');
    return;
  }
  try {
    await ensureIdentity();
    await refresh(true);
    listen();
  } catch (error) {
    showError(error);
  }
}

if (roomId) {
  composer.addEventListener('submit', async function (event) {
    event.preventDefault();
    const body = bodyInput.value.trim();
    if (!body) return;
    stickyStatus = '';
    sendButton.disabled = true;
    try {
      if (!room) throw new Error('房间成员尚未加载，请稍后重试');
      const invoke = agentInvokeTargets();
      const args = {
        request_id: requestId('web_msg'),
        client_message_id: crypto.randomUUID(),
        room_id: roomId,
        body: body
      };
      // 网页人类发送时唤醒房间内其他 Agent；Agent 普通回复不经过本页面。
      if (invoke.targets.length > 0) args.invoke = invoke.targets;
      const result = await callTool('network_message_send', args);
      const deliveryIds = Array.isArray(result.delivery_ids) ? result.delivery_ids : [];
      bodyInput.value = '';
      await refresh(true);
      if (invoke.targets.length > 0 && deliveryIds.length !== invoke.targets.length) {
        stickyStatus =
          '消息已发送，但 Agent 投递未建立（' + deliveryIds.length + '/' +
          invoke.targets.length + '），请检查房间成员后重试。';
        statusElement.textContent = stickyStatus;
      } else if (invoke.skipped > 0) {
        stickyStatus =
          '消息已发送；群内 Agent 超过单条消息唤醒上限，本次仅唤醒前 ' +
          invoke.targets.length + ' 个。';
        statusElement.textContent = stickyStatus;
      }
    } catch (error) {
      stickyStatus = '发送失败：' + error.message;
      statusElement.textContent = stickyStatus;
    } finally {
      sendButton.disabled = false;
      bodyInput.focus();
    }
  });
  bodyInput.addEventListener('keydown', function (event) {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      composer.requestSubmit();
    }
  });
  olderButton.addEventListener('click', loadOlder);
}

(async function init() {
  await loadRooms();
  if (roomId) await initChat();
})();
</script>`,
    APP_STYLE,
    { wide: true, bodyClass: isHome ? 'page-home' : 'page-room' },
  );
}

export function renderChatPage(roomId: string): string {
  return renderAppPage(roomId);
}
