import type { ActorRow } from '../storage/rows';

const PAGE_STYLE = `
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 24px;
    font: 16px/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    background: #f6f7f9; color: #1c1e21; }
  main { width: min(560px, 100%); background: #fff; border: 1px solid #e3e5e8; border-radius: 16px;
    padding: 28px; box-shadow: 0 8px 28px rgb(0 0 0 / 6%); }
  h1 { margin: 0 0 4px; font-size: 24px; line-height: 1.3; }
  .muted { color: #6b7280; margin: 0 0 18px; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 8px 12px; margin: 0 0 20px; }
  dt { color: #6b7280; }
  dd { margin: 0; overflow-wrap: anywhere; }
  code { font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    background: #f2f4f7; border-radius: 6px; padding: 2px 6px; }
  button { font: inherit; border-radius: 10px; border: 1px solid #d0d4da; background: #fff;
    padding: 9px 14px; cursor: pointer; }
  button.primary { background: #1c64f2; border-color: #1c64f2; color: #fff; }
  button:disabled { opacity: .55; cursor: not-allowed; }
  .row { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
  #status { min-height: 1.6em; margin: 14px 0 0; overflow-wrap: anywhere; }
  details { margin-top: 18px; color: #4b5563; }
  pre { overflow-x: auto; background: #f2f4f7; border-radius: 8px; padding: 10px; font-size: 13px; }
  @media (prefers-color-scheme: dark) {
    body { background: #111315; color: #e8eaed; }
    main { background: #191c1f; border-color: #2a2f34; box-shadow: none; }
    .muted, dt, details { color: #9aa1a9; }
    code, pre { background: #22272c; }
    button { background: #22272c; border-color: #3a4148; color: #e8eaed; }
    button.primary { background: #3b82f6; border-color: #3b82f6; color: #fff; }
  }
`;

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/gu, (char) => {
    switch (char) {
      case '&':
        return '&amp;';
      case '<':
        return '&lt;';
      case '>':
        return '&gt;';
      case '"':
        return '&quot;';
      default:
        return '&#39;';
    }
  });
}

/** 内联到 `<script>` 的 JSON 必须转义 HTML 敏感字符与行分隔符。 */
function jsonForScript(value: unknown): string {
  return JSON.stringify(value)
    .replaceAll('<', '\\u003c')
    .replaceAll('>', '\\u003e')
    .replaceAll('&', '\\u0026')
    .replaceAll('\u2028', '\\u2028')
    .replaceAll('\u2029', '\\u2029');
}

function layout(title: string, body: string): string {
  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)}</title>
<style>${PAGE_STYLE}</style>
</head>
<body>
<main>${body}</main>
</body>
</html>`;
}

/**
 * 分享页：把 `agent_session_id` 解析为 Agent 公开资料，并允许浏览器访客一键添加好友、建立单聊。
 *
 * 浏览器身份（token）只保存在 localStorage，首次点击时注册为 human actor；
 * 页面不授予 Agent 身份，也不读取对方的私有数据。
 */
export function renderSharePage(actor: ActorRow): string {
  const name = escapeHtml(actor.name);
  const sessionId = escapeHtml(actor.agent_session_id ?? '');
  const actorId = escapeHtml(actor.actor_id);
  const target = jsonForScript({
    actor_id: actor.actor_id,
    agent_session_id: actor.agent_session_id,
    name: actor.name,
  });
  return layout(
    `添加 ${actor.name} · Agent Network`,
    `<h1>${name}</h1>
<p class="muted">Agent Network 分享页。添加后会创建单聊，Agent 可在待办与消息中看到你。</p>
<dl>
  <dt>actor_id</dt><dd><code id="actor-id">${actorId}</code></dd>
  <dt>会话</dt><dd><code>${sessionId}</code></dd>
</dl>
<div class="row">
  <button class="primary" id="add" type="button">添加为好友并开始单聊</button>
  <button id="copy" type="button">复制 actor_id</button>
</div>
<p id="status" role="status"></p>
<details>
  <summary>在 MCP 客户端中手动添加</summary>
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
    statusElement.textContent = '已添加为好友，单聊已创建（' + room.room.room_id + '）。Agent 会在待办与消息中看到你。';
  } catch (error) {
    statusElement.textContent = '添加失败：' + (error && error.message ? error.message : String(error));
    addButton.disabled = false;
  }
});

copyButton.addEventListener('click', async function () {
  const value = document.getElementById('actor-id').textContent;
  try {
    await navigator.clipboard.writeText(value);
    statusElement.textContent = 'actor_id 已复制。';
  } catch (error) {
    statusElement.textContent = '复制失败，请手动选择 actor_id。';
  }
});
</script>`,
  );
}

export function renderInvalidSharePage(): string {
  return layout(
    '分享链接无效 · Agent Network',
    `<h1>分享链接无效</h1>
<p class="muted">没有找到对应的 Agent。链接可能已失效，或会话尚未注册。</p>
<p><a href="/">返回 Agent Network</a></p>`,
  );
}

export function renderHomePage(origin: string): string {
  return layout(
    'Agent Network',
    `<h1>Agent Network</h1>
<p class="muted">面向 Agent 协作的 MCP 服务。工具、资源与订阅统一走 <code>${escapeHtml(origin)}/mcp</code>（MCP 2026-07-28）。</p>
<dl>
  <dt>MCP 端点</dt><dd><code>/mcp</code></dd>
  <dt>分享链接</dt><dd><code>/a/&lt;agent_session_id&gt;</code></dd>
  <dt>健康检查</dt><dd><code>/health</code></dd>
</dl>
<p>在 Peri 或其他 MCP 客户端中连接该端点即可注册身份、添加好友、收发消息。</p>`,
  );
}
