// acp-hub 链路 2 复现客户端：auth → subscribe → ready → session/create
// 用法: node verify-client.mjs <token> <wsUrl> [instanceId]
'use strict';
const token = process.argv[2];
const url = process.argv[3] || 'ws://127.0.0.1:18457/';
const instanceId = process.argv[4] || null;

const ws = new WebSocket(url);
const t0 = Date.now();
let created = false;

function doCreate() {
  if (created) return;
  created = true;
  const payload = instanceId ? { instanceId } : {};
  ws.send(JSON.stringify({
    t: 'action',
    commandId: crypto.randomUUID(),
    type: 'chat/create',
    payload,
  }));
}

ws.addEventListener('open', () => {
  ws.send(JSON.stringify({ t: 'auth', token }));
  // 首帧纪律：认证后首帧必须是 subscribe/action；顺序发送即可
  setTimeout(() => ws.send(JSON.stringify({ t: 'ysync.subscribe', docs: ['hub:registry'] })), 50);
});

ws.addEventListener('message', (ev) => {
  let f;
  try { f = JSON.parse(ev.data); } catch { return; }
  if (f.t === 'keep_alive') { ws.send(JSON.stringify({ t: 'pong' })); return; }
  if (f.t === 'ready') {
    console.log(`ready ${Date.now() - t0}ms`);
    doCreate();
    return;
  }
  if (f.t === 'action_ack' && f.status === 'accepted') { return; }
  if (f.t === 'action_ack' && f.status === 'committed') {
    console.log(`RESULT committed ${Date.now() - t0}ms chatId=${f.chatId}`);
    ws.close();
    process.exit(0);
  }
  if (f.t === 'action_ack') { console.log(`RESULT ack=${f.status} ${Date.now() - t0}ms`); }
  if (f.t === 'action_error') {
    console.log(`RESULT error ${Date.now() - t0}ms code=${f.code} retryable=${f.retryable} msg=${f.message}`);
    ws.close();
    process.exit(0);
  }
});

ws.addEventListener('close', (ev) => {
  if (!created) { console.log(`closed before create code=${ev.code} reason=${ev.reason}`); }
});

setTimeout(() => { console.log('TIMEOUT 20s'); ws.close(); process.exit(2); }, 20000);
