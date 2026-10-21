import { LIMITS } from '../contracts/limits';
import { CHAT_LIST_SCRIPT } from './chat-list-script';
import { CHAT_RENDER_SCRIPT } from './chat-render-script';
import { HANDOFF_CONTROLLER_SCRIPT } from './handoff';
import { jsonForScript } from './layout';
import { QR_ENCODER_SCRIPT } from './qr';

/**
 * 应用页内联客户端脚本：会话列表、聊天、建群、@、回执与手机扫码。
 *
 * `roomId` 为 `null` 时只初始化首页列表；有值时同一脚本加载房间。
 * 列表与消息渲染片段由独立模块提供，但拼进同一个 `<script>`，共享作用域。
 * 阈值仍由 `LIMITS` 内联，页面不复制字面量。
 */
export function chatClientScript(roomId: string | null): string {
  const roomJson = roomId === null ? 'null' : jsonForScript(roomId);
  return `<script>
const roomId = ${roomJson};
const TOKEN_KEY = 'agent-network-token';
const MAX_CHUNKS = 8;
// 服务端 LIMITS.invokeMax 的内联值：阈值只在 limits.ts 维护，页面不复制字面量。
const INVOKE_MAX = ${LIMITS.invokeMax};
const ACTOR_NAME_MAX = ${LIMITS.actorNameMax};
// claimed 超过该时长未更新时显示提示；只做本地重绘，不轮询服务端。
const CLAIM_STALE_MS = ${LIMITS.claimedStaleMs};
const roomListElement = document.getElementById('room-list');
const listStatusElement = document.getElementById('room-list-status');
const searchInput = document.getElementById('room-search');
const accountNameElement = document.getElementById('account-name');
const accountAvatarElement = document.getElementById('account-avatar');
const titleElement = document.getElementById('title');
const subtitleElement = document.getElementById('subtitle');
const messagesElement = document.getElementById('messages');
const olderButton = document.getElementById('older');
const composer = document.getElementById('composer');
const bodyInput = document.getElementById('body');
const sendButton = document.getElementById('send');
const statusElement = document.getElementById('status');
const renameButton = document.getElementById('rename');
const renameForm = document.getElementById('rename-form');
const renameInput = document.getElementById('rename-input');
const renameCancel = document.getElementById('rename-cancel');
const mentionPicker = document.getElementById('mention-picker');
const newGroupButton = document.getElementById('new-group');
const groupModal = document.getElementById('group-modal');
const groupForm = document.getElementById('group-form');
const groupTitleInput = document.getElementById('group-title');
const groupContactsElement = document.getElementById('group-contacts');
const groupErrorElement = document.getElementById('group-error');
const groupCreateButton = document.getElementById('group-create');
const groupCancelButton = document.getElementById('group-cancel');

let identity = null;
let members = new Map();
let room = null;
let nextOlder = null;
let refreshChain = Promise.resolve();
let refreshQueued = false;
let reloadTimer = null;
let roomEntries = [];
let mentionTargets = new Map();
let activeMention = null;
let composing = false;
let ignoreEnterUntil = 0;
// 发送失败/投递缺失/截断提示必须跨过 SSE 触发的刷新，不能在 300ms 后被清空。
let stickyStatus = '';

${CHAT_LIST_SCRIPT}
${CHAT_RENDER_SCRIPT}

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
  if (result.isError) {
    const item = (result.content || []).find(function (entry) { return entry.type === 'text'; });
    let detail = null;
    if (item) {
      try { detail = JSON.parse(item.text); } catch (error) { detail = { message: item.text }; }
    }
    const failure = new Error((detail && detail.message) || '请求失败');
    if (detail && detail.code) failure.code = detail.code;
    throw failure;
  }
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
 * direct 自动唤醒对方 Agent；group 只唤醒文本中仍然存在的 @Agent 记录。
 * 只使用 network_room_get 已缓存的 members，不额外发请求；人类成员与
 * 发送者自己不进入 invoke。超过上限时返回错误，由调用方明确提示，
 * 不静默截断。
 */
function agentInvokeTargets() {
  if (!identity || identity.kind !== 'human') return { targets: [], error: null };
  if (!room) return { targets: [], error: '房间成员尚未加载，请稍后重试' };
  if (room.kind === 'direct') {
    for (const member of members.values()) {
      if (member.actor_id !== identity.actor_id && member.kind === 'agent') {
        return { targets: [member.actor_id], error: null };
      }
    }
    return { targets: [], error: null };
  }
  const targets = [];
  const text = bodyInput ? bodyInput.value : '';
  for (const actorId of mentionTargets.keys()) {
    if (actorId === identity.actor_id) continue;
    const member = members.get(actorId);
    if (member && member.kind === 'agent' && text.includes('@' + member.name)) {
      targets.push(actorId);
    }
  }
  if (targets.length > INVOKE_MAX) {
    return {
      targets: [],
      error: '单条消息最多 @ ' + INVOKE_MAX + ' 个 Agent，请减少 @ 后重试。'
    };
  }
  return { targets: targets, error: null };
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

function directPeer() {
  if (!room || room.kind !== 'direct' || !identity) return null;
  for (const member of members.values()) {
    if (member.actor_id !== identity.actor_id) return member;
  }
  return null;
}

function updateRenameAffordance() {
  if (!renameButton) return;
  const peer = directPeer();
  const canRename = Boolean(room && room.kind === 'direct' && peer && peer.kind === 'agent');
  renameButton.hidden = !canRename;
  if (!canRename && renameForm) renameForm.hidden = true;
}

function openRenameForm() {
  const peer = directPeer();
  if (!peer || peer.kind !== 'agent' || !renameForm || !renameInput) return;
  renameInput.value = peer.name;
  renameForm.hidden = false;
  renameButton.hidden = true;
  renameInput.focus();
  renameInput.select();
}

function closeRenameForm() {
  if (renameForm) renameForm.hidden = true;
  updateRenameAffordance();
}

function applyAgentName(actorId, name) {
  const member = members.get(actorId);
  if (member) member.name = name;
  const entry = roomEntries.find(function (item) { return item.room_id === roomId; });
  if (entry) entry.name = name;
  if (room && room.kind === 'direct' && titleElement) titleElement.textContent = name;
  renderRooms();
}

function hideMentionPicker() {
  if (mentionPicker) mentionPicker.hidden = true;
  activeMention = null;
}

/** 文本中已不存在的 @名字 不再作为 invoke 目标。 */
function pruneMentions() {
  const value = bodyInput ? bodyInput.value : '';
  for (const entry of Array.from(mentionTargets.entries())) {
    if (!value.includes('@' + entry[1])) mentionTargets.delete(entry[0]);
  }
}

function updateMentionPicker() {
  if (!mentionPicker || !bodyInput || !room || room.kind !== 'group' || !identity) {
    hideMentionPicker();
    return;
  }
  const value = bodyInput.value;
  const caret = bodyInput.selectionStart === null ? value.length : bodyInput.selectionStart;
  const match = value.slice(0, caret).match(/@([^\\s@]*)$/);
  if (!match) {
    hideMentionPicker();
    return;
  }
  const query = match[1];
  activeMention = { start: caret - query.length - 1, end: caret };
  const lower = query.toLowerCase();
  const candidates = [];
  for (const member of members.values()) {
    if (member.actor_id === identity.actor_id) continue;
    if (lower && member.name.toLowerCase().indexOf(lower) < 0) continue;
    candidates.push(member);
  }
  mentionPicker.replaceChildren();
  if (candidates.length === 0) {
    const empty = document.createElement('p');
    empty.className = 'list-hint';
    empty.textContent = query ? '没有匹配成员' : '房间还没有其他成员';
    mentionPicker.append(empty);
  } else {
    for (const member of candidates.slice(0, 20)) {
      const item = document.createElement('button');
      item.type = 'button';
      item.className = 'mention-item';
      item.setAttribute('role', 'option');
      const name = document.createElement('span');
      name.textContent = member.name;
      const kind = document.createElement('span');
      kind.className = 'mention-kind';
      kind.textContent = member.kind === 'agent' ? 'Agent' : '人';
      item.append(name, kind);
      item.addEventListener('click', function () { insertMention(member); });
      mentionPicker.append(item);
    }
  }
  mentionPicker.hidden = false;
}

function insertMention(member) {
  if (!activeMention || !bodyInput) return;
  const value = bodyInput.value;
  const insert = '@' + member.name + ' ';
  bodyInput.value = value.slice(0, activeMention.start) + insert + value.slice(activeMention.end);
  const position = activeMention.start + insert.length;
  bodyInput.setSelectionRange(position, position);
  mentionTargets.set(member.actor_id, member.name);
  hideMentionPicker();
  resizeComposer();
  bodyInput.focus();
}

async function loadGroupContacts() {
  const contacts = [];
  let cursor = null;
  for (let page = 0; page < 5; page += 1) {
    const args = { limit: 100 };
    if (cursor) args.cursor = cursor;
    const result = await callTool('network_contacts_list', args);
    contacts.push.apply(contacts, result.items || []);
    if (!result.has_more || !result.next_cursor) break;
    cursor = result.next_cursor;
  }
  return contacts;
}

function renderGroupContacts(contacts) {
  if (!groupContactsElement || !identity) return;
  groupContactsElement.replaceChildren();
  const others = contacts.filter(function (contact) { return contact.actor_id !== identity.actor_id; });
  if (others.length === 0) {
    groupContactsElement.textContent = '还没有好友。先打开 Agent 分享链接添加好友。';
    return;
  }
  for (const contact of others) {
    const label = document.createElement('label');
    label.className = 'contact-choice';
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.value = contact.actor_id;
    const name = document.createElement('span');
    name.textContent = contact.name;
    const kind = document.createElement('span');
    kind.className = 'contact-kind';
    kind.textContent = contact.kind === 'agent' ? 'Agent' : '人';
    label.append(input, name, kind);
    groupContactsElement.append(label);
  }
}

function closeGroupModal() {
  if (groupModal) groupModal.hidden = true;
}

async function openGroupModal() {
  if (!groupModal || !groupContactsElement) return;
  groupModal.hidden = false;
  if (groupTitleInput) groupTitleInput.value = '';
  if (groupErrorElement) groupErrorElement.textContent = '';
  groupContactsElement.textContent = '正在读取好友…';
  try {
    await ensureIdentity();
    renderGroupContacts(await loadGroupContacts());
  } catch (error) {
    groupContactsElement.replaceChildren();
    if (groupErrorElement) {
      groupErrorElement.textContent = '读取好友失败：' + (error && error.message ? error.message : error);
    }
  }
}

/** 输入框 1–4 行自适应，超过 4 行后内部滚动。 */
function resizeComposer() {
  if (!bodyInput) return;
  const max = 96;
  bodyInput.style.height = 'auto';
  const height = Math.min(bodyInput.scrollHeight, max);
  bodyInput.style.height = height + 'px';
  bodyInput.style.overflowY = bodyInput.scrollHeight > max ? 'auto' : 'hidden';
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
  appendHistory(messagesElement, page.items.slice().reverse());
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
    appendHistory(fragment, page.items.slice().reverse());
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
  if (room.kind === 'direct') {
    const peer = directPeer();
    titleElement.textContent = others.join('、') || '单聊';
    if (peer && peer.kind === 'agent') {
      subtitleElement.textContent = presenceLabel(peer.online);
      subtitleElement.hidden = false;
    } else {
      subtitleElement.textContent = '';
      subtitleElement.hidden = true;
    }
    const entry = roomEntries.find(function (item) { return item.room_id === roomId; });
    if (entry) {
      entry.peer_kind = peer ? peer.kind : null;
      entry.peer_online = peer && peer.kind === 'agent' ? peer.online : null;
      renderRooms();
    }
  } else {
    titleElement.textContent = room.title || '群聊';
    subtitleElement.textContent = '群聊 · ' + detail.members.length + ' 人';
    subtitleElement.hidden = false;
  }
  updateRenameAffordance();
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
    resizeComposer();
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
    statusElement.textContent = '';
    const invoke = agentInvokeTargets();
    if (invoke.error) {
      stickyStatus = invoke.error;
      statusElement.textContent = stickyStatus;
      return;
    }
    sendButton.disabled = true;
    try {
      const args = {
        request_id: requestId('web_msg'),
        client_message_id: crypto.randomUUID(),
        room_id: roomId,
        body: body
      };
      // direct 自动唤醒对方 Agent；group 只唤醒被 @ 的 Agent。
      if (invoke.targets.length > 0) args.invoke = invoke.targets;
      const result = await callTool('network_message_send', args);
      const deliveryIds = Array.isArray(result.delivery_ids) ? result.delivery_ids : [];
      bodyInput.value = '';
      mentionTargets.clear();
      hideMentionPicker();
      resizeComposer();
      await refresh(true);
      if (invoke.targets.length > 0 && deliveryIds.length !== invoke.targets.length) {
        stickyStatus =
          '消息已发送，但 Agent 投递未建立（' + deliveryIds.length + '/' +
          invoke.targets.length + '），请检查房间成员后重试。';
        statusElement.textContent = stickyStatus;
      }
    } catch (error) {
      stickyStatus = '发送失败：' + (error && error.message ? error.message : error);
      statusElement.textContent = stickyStatus;
    } finally {
      sendButton.disabled = false;
      bodyInput.focus();
    }
  });

  bodyInput.addEventListener('input', function () {
    resizeComposer();
    pruneMentions();
    updateMentionPicker();
  });
  bodyInput.addEventListener('compositionstart', function () {
    composing = true;
  });
  bodyInput.addEventListener('compositionend', function () {
    composing = false;
    // Safari/Chrome 可能在确认候选后紧接着再派发一次 Enter，短暂忽略。
    ignoreEnterUntil = Date.now() + 80;
  });
  bodyInput.addEventListener('keydown', function (event) {
    if (event.key === 'Escape') {
      hideMentionPicker();
      return;
    }
    if (event.key !== 'Enter' || event.shiftKey) return;
    if (event.isComposing || composing || event.keyCode === 229 || Date.now() < ignoreEnterUntil) {
      return;
    }
    event.preventDefault();
    composer.requestSubmit();
  });
  olderButton.addEventListener('click', loadOlder);

  if (renameButton) renameButton.addEventListener('click', openRenameForm);
  if (renameCancel) renameCancel.addEventListener('click', closeRenameForm);
  if (renameForm) {
    renameForm.addEventListener('submit', async function (event) {
      event.preventDefault();
      const peer = directPeer();
      if (!peer || peer.kind !== 'agent') return;
      const name = renameInput.value.trim();
      if (!name) {
        statusElement.textContent = '名称不能为空';
        return;
      }
      if (name.length > ACTOR_NAME_MAX) {
        statusElement.textContent = '名称最多 ' + ACTOR_NAME_MAX + ' 个字符';
        return;
      }
      const submitButton = renameForm.querySelector('button[type=submit]');
      submitButton.disabled = true;
      try {
        const result = await callTool('network_agent_rename', {
          request_id: requestId('rename'),
          actor_id: peer.actor_id,
          name: name
        });
        applyAgentName(peer.actor_id, result.name);
        stickyStatus = '';
        statusElement.textContent = '';
        closeRenameForm();
      } catch (error) {
        statusElement.textContent = '改名失败：' + (error && error.message ? error.message : error);
      } finally {
        submitButton.disabled = false;
      }
    });
  }
}

document.addEventListener('click', function (event) {
  if (!mentionPicker || mentionPicker.hidden) return;
  if (mentionPicker.contains(event.target) || event.target === bodyInput) return;
  hideMentionPicker();
});

if (newGroupButton) newGroupButton.addEventListener('click', openGroupModal);
if (groupCancelButton) groupCancelButton.addEventListener('click', closeGroupModal);
if (groupModal) {
  groupModal.addEventListener('click', function (event) {
    if (event.target === groupModal) closeGroupModal();
  });
}
if (groupForm) {
  groupForm.addEventListener('submit', async function (event) {
    event.preventDefault();
    const selected = [];
    for (const input of groupContactsElement.querySelectorAll('input[type=checkbox]:checked')) {
      selected.push(input.value);
    }
    if (selected.length === 0) {
      groupErrorElement.textContent = '请至少选择一个成员';
      return;
    }
    const title = groupTitleInput.value.trim();
    groupCreateButton.disabled = true;
    groupErrorElement.textContent = '正在创建…';
    try {
      const args = {
        request_id: requestId('group'),
        kind: 'group',
        members: selected
      };
      if (title) args.title = title;
      const result = await callTool('network_room_create', args);
      location.href = '/c/' + encodeURIComponent(result.room.room_id);
    } catch (error) {
      groupErrorElement.textContent =
        '创建失败：' + (error && error.message ? error.message : error);
      groupCreateButton.disabled = false;
    }
  });
}

(async function init() {
  await loadRooms();
  if (roomId) await initChat();
})();
</script>
<script>
${QR_ENCODER_SCRIPT}
${HANDOFF_CONTROLLER_SCRIPT}
</script>`;
}
