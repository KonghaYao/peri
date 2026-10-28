import type { ActorRow } from '../storage/rows';
import { renderAppPage } from './chat';
import { escapeHtml, jsonForScript, layout } from './layout';

/**
 * 分享页：把 `agent_session_id` 解析为 Agent 公开资料，并允许浏览器访客一键添加好友、建立单聊。
 *
 * 浏览器身份（token）只保存在 localStorage，首次点击时注册为 human actor；
 * 页面不授予 Agent 身份，也不读取对方的私有数据。
 */
export function renderSharePage(actor: ActorRow): string {
  const name = escapeHtml(actor.name);
  const initial = escapeHtml(Array.from(actor.name.trim())[0] ?? 'A');
  const sessionId = escapeHtml(actor.agent_session_id ?? '');
  const actorId = escapeHtml(actor.actor_id);
  const target = jsonForScript({
    actor_id: actor.actor_id,
    agent_session_id: actor.agent_session_id,
    name: actor.name,
  });
  return layout(
    `添加 ${actor.name} · Agent Network`,
    `<div class="share-head">
  <div class="share-avatar" aria-hidden="true">${initial}</div>
  <h1>${name}</h1>
  <p class="muted">Agent Network</p>
</div>
<p class="share-lead">添加后建立单聊，Agent 可在待办与消息中看到你。</p>
<div class="row share-actions">
  <button class="primary" id="add" type="button">添加为好友并开始单聊</button>
  <button id="copy" type="button">复制 ID</button>
</div>
<p id="status" role="status"></p>
<details>
  <summary>身份与手动添加</summary>
  <p>actor_id：<code id="actor-id">${actorId}</code></p>
  <p>会话：<code>${sessionId}</code></p>
  <pre>network_contact_add {
  "request_id": "&lt;唯一 id&gt;",
  "agent_session_id": "${sessionId}"
}</pre>
</details>
<script>
const target = ${target};
const TOKEN_KEY = 'agent-network-token';
const addButton = document.getElementById('add');
const copyButton = document.getElementById('copy');
const statusElement = document.getElementById('status');

function browserToken() {
  let value = localStorage.getItem(TOKEN_KEY);
  if (value) return value;
  value = crypto.randomUUID();
  localStorage.setItem(TOKEN_KEY, value);
  return value;
}

function requestId(prefix) {
  return prefix + '_' + crypto.randomUUID().replaceAll('-', '');
}

async function rpc(method, params) {
  const headers = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    'mcp-protocol-version': '2026-07-28',
    'mcp-method': method,
    authorization: 'Bearer ' + browserToken()
  };
  if (params && params.name) headers['mcp-name'] = params.name;
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
          'io.modelcontextprotocol/clientInfo': { name: 'agent-network-share', version: '0.1.0' },
          'io.modelcontextprotocol/clientCapabilities': {}
        }
      })
    })
  });
  const payload = await response.json().catch(function () { return null; });
  if (!payload) throw new Error('服务无响应');
  if (payload.error) throw new Error(payload.error.message || '请求失败');
  return payload.result;
}

async function callTool(name, args) {
  const result = await rpc('tools/call', { name: name, arguments: args });
  if (result.structuredContent) return result.structuredContent;
  const item = (result.content || []).find(function (entry) { return entry.type === 'text'; });
  return item ? JSON.parse(item.text) : {};
}

addButton.addEventListener('click', async function () {
  addButton.disabled = true;
  delete statusElement.dataset.error;
  statusElement.textContent = '正在添加…';
  try {
    await callTool('network_register', {
      request_id: requestId('web_reg'),
      kind: 'human',
      name: '网页访客'
    });
    const contact = await callTool('network_contact_add', {
      request_id: requestId('web_contact'),
      agent_session_id: target.agent_session_id
    });
    const room = await callTool('network_room_create', {
      request_id: requestId('web_room'),
      kind: 'direct',
      members: [contact.contact.actor_id]
    });
    statusElement.textContent = '已添加为好友，正在进入单聊…';
    location.href = '/c/' + encodeURIComponent(room.room.room_id);
  } catch (error) {
    statusElement.dataset.error = '1';
    statusElement.textContent = '添加失败：' + (error && error.message ? error.message : String(error));
    addButton.disabled = false;
  }
});

copyButton.addEventListener('click', async function () {
  const value = document.getElementById('actor-id').textContent;
  try {
    await navigator.clipboard.writeText(value);
    delete statusElement.dataset.error;
    statusElement.textContent = 'actor_id 已复制。';
  } catch (error) {
    statusElement.dataset.error = '1';
    statusElement.textContent = '复制失败，请手动选择 actor_id。';
  }
});
</script>`,
  );
}

export function renderInvalidSharePage(): string {
  return layout(
    '分享链接无效 · Agent Network',
    `<div class="share-head">
  <div class="share-avatar" aria-hidden="true">!</div>
  <h1>分享链接无效</h1>
  <p class="muted">没有找到对应的 Agent</p>
</div>
<p class="share-lead">链接可能已失效，或会话尚未注册。请向对方索取新的分享链接。</p>
<p><a href="/">返回 Agent Network</a></p>`,
  );
}

export function renderHomePage(): string {
  return renderAppPage(null);
}
