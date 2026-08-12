#!/usr/bin/env node
// acp-hub chat/load 恢复历史验证脚本（§8.5 预绑定修复的运行时回归验证）。
//
// 流程：
//   a. auth → ready（hub:registry 快照）；
//   b. chat/create → committed ack（chatId）；
//   c. subscribe chat:{cid} → 快照（记录基线 entries 数）；
//   d. session/list → 拿 ACP 会话列表（选 current=false 的历史会话；
//      无历史会话时以当前会话为目标做「重载」验证）；
//   e. chat/load {chatId, acpSessionId} → committed ack；
//   f. 等 chat:{cid} 增量帧：修复前回放帧被 relay binding_missing 丢弃
//      （无消息增量）；修复后应收到回放增量（yjs entries 增长）。
//
// 用法: node scripts/verify-load.mjs <token> [ws://127.0.0.1:8456/]

'use strict';

// 真实 peri 环境下 create（spawn+initialize+session/new）与 load 的 L3
// 确认可能达分钟级（agent 侧模型调用慢），超时放宽到 2 分钟/1 分钟。
const RETRY_ACK_MS = 120000;
const RETRY_REPLAY_MS = 60000;

const token = process.argv[2];
const baseUrl = process.argv[3] || 'ws://127.0.0.1:8456/';
if (!token) {
  console.error('用法: node scripts/verify-load.mjs <token> [ws://127.0.0.1:8456/]');
  process.exit(2);
}

let passed = 0;
let failed = 0;
function pass(label, detail) {
  passed++;
  console.log(`PASS (${label}) ${detail || ''}`);
}
function fail(label, detail) {
  failed++;
  console.error(`FAIL (${label}) ${detail || ''}`);
}
function uuid() {
  return crypto.randomUUID();
}

// ── yjs（scripts/node_modules/yjs，可选）───────────────────────────────────
async function loadYjs() {
  try {
    const mod = await import('../scripts/node_modules/yjs/dist/yjs.mjs');
    return mod;
  } catch (e) {
    return null;
  }
}
function decodeEntries(yjs, docId, base64) {
  try {
    const ydoc = new yjs.Doc();
    const upd = Uint8Array.from(Buffer.from(base64, 'base64'));
    yjs.applyUpdate(ydoc, upd);
    const root = ydoc.getMap('chat');
    const order = root.get('entry_order') || [];
    const entries = root.get('entries') || new yjs.Map();
    const texts = [];
    for (const id of order) {
      const e = entries.get(id);
      if (e && e.text) texts.push(e.text.slice(0, 60));
    }
    return { order: order.length, texts };
  } catch (e) {
    return { order: -1, texts: [`解码失败: ${e.message}`] };
  }
}

// ── 帧流 ───────────────────────────────────────────────────────────────────
const waiters = [];
function removeWaiter(w) {
  const i = waiters.indexOf(w);
  if (i >= 0) waiters.splice(i, 1);
}
function waitFor(label, predicate, timeoutMs) {
  return new Promise((resolve, reject) => {
    const w = {
      predicate,
      resolve,
      reject,
      timer: null,
    };
    w.timer = setTimeout(() => {
      removeWaiter(w);
      reject(new Error(`超时(${timeoutMs}ms): ${label}`));
    }, timeoutMs);
    waiters.push(w);
  });
}

const ws = new WebSocket(baseUrl);
let sawKeepAlive = false;
ws.addEventListener('open', () => {
  ws.send(JSON.stringify({ t: 'auth', token }));
});
ws.addEventListener('message', (ev) => {
  let frame;
  try {
    frame = JSON.parse(ev.data);
  } catch (e) {
    return;
  }
  if (frame.t === 'keep_alive') {
    sawKeepAlive = true;
    ws.send(JSON.stringify({ t: 'pong' }));
  }
  for (let i = 0; i < waiters.length; i++) {
    const w = waiters[i];
    if (w.predicate(frame)) {
      clearTimeout(w.timer);
      removeWaiter(w);
      w.resolve(frame);
      i--;
    }
  }
});
ws.addEventListener('error', () => {});
function send(frame) {
  if (ws.readyState !== WebSocket.OPEN) throw new Error('连接未 OPEN');
  ws.send(JSON.stringify(frame));
}
function waitAck(commandId, status, timeoutMs) {
  return waitFor(
    `ack ${commandId.slice(0, 8)}…=${status}`,
    (f) => f.t === 'action_ack' && f.commandId === commandId && f.status === status,
    timeoutMs
  );
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 累积 chat doc 增量（base64 原文拼接后整体解码不可行——yrs update 需
// 增量 apply；这里逐帧 apply 累计 entries 数）。
async function trackChatDoc(yjs, docId, base64Snapshot) {
  const ydoc = new yjs.Doc();
  let applied = 0;
  if (base64Snapshot) {
    yjs.applyUpdate(ydoc, Uint8Array.from(Buffer.from(base64Snapshot, 'base64')));
  }
  const counts = () => {
    const root = ydoc.getMap('chat');
    const order = root.get('entry_order') || [];
    return order.length;
  };
  const waitReplayGrowth = waitFor(
    '回放增量帧',
    (f) => {
      if (f.t !== 'ysync.update' || f.doc !== docId) return false;
      if (Object.prototype.hasOwnProperty.call(f, 'projectionVersion')) return false; // 快照不算增量
      if (!f.update) return false;
      try {
        yjs.applyUpdate(ydoc, Uint8Array.from(Buffer.from(f.update, 'base64')));
        applied++;
      } catch (e) {
        /* 跳过坏帧 */
      }
      return counts() > 0;
    },
    RETRY_REPLAY_MS
  );
  await waitReplayGrowth;
  const root = ydoc.getMap('chat');
  const order = root.get('entry_order') || [];
  const entries = root.get('entries') || new yjs.Map();
  const texts = [];
  for (const id of order) {
    const e = entries.get(id);
    if (e && e.text) texts.push(e.text.slice(0, 80));
  }
  return { applied, order: order.length, texts };
}

async function main() {
  const yjs = await loadYjs();
  console.log(`连接 ${baseUrl}（token ${token.length} 字符，yjs ${yjs ? '可用' : '不可用'}）…`);

  // a. auth → subscribe hub:registry → 快照 → ready（§4.6 步骤 3/4：
  //    ready 在订阅快照之后才发，仅 auth 不会触发）。
  const readyP = waitFor('ready', (f) => f.t === 'ready', 15000);
  await sleep(300); // 等 auth 发送
  send({ t: 'ysync.subscribe', docs: ['hub:registry'] });
  const ready = await readyP;
  pass('a', `ready.projectionVersions = ${JSON.stringify(ready.projectionVersions)}`);

  // b. chat/create（VERIFY_LOAD_CHAT 指定时复用已有 chat，跳过 create——
  //    真实面板场景：打开旧 chat 切换历史会话）。
  let chatId = process.env.VERIFY_LOAD_CHAT || '';
  if (!chatId) {
    const createCid = uuid();
    const createAckP = waitAck(createCid, 'committed', RETRY_ACK_MS);
    await sleep(50);
    send({ t: 'action', commandId: createCid, type: 'chat/create', payload: {} });
    const createAck = await createAckP;
    chatId = createAck.chatId;
    if (!chatId) throw new Error('create committed ack 无 chatId');
    pass('b', `create committed chatId=${chatId}`);
  } else {
    pass('b', `复用 VERIFY_LOAD_CHAT=${chatId}（跳过 create）`);
  }

  // c. subscribe chat doc，拿快照基线（快照 = ysync.update 带
  //    projectionVersion 字段，顶层 doc 字段；§4.6 步骤 3）。
  const chatDoc = `chat:${chatId}`;
  const snapP = waitFor('chat 快照', (f) => f.t === 'ysync.update' && f.doc === chatDoc && Object.prototype.hasOwnProperty.call(f, 'projectionVersion'), RETRY_ACK_MS);
  send({ t: 'ysync.subscribe', docs: [chatDoc] });
  const snap = await snapP;
  let baseEntries = 0;
  if (yjs) {
    baseEntries = decodeEntries(yjs, chatId, snap.update).order;
  }
  pass('c', `${chatDoc} 快照到达（update ${snap.update.length} 字符，基线 entries=${baseEntries}）`);

  // d. session/list：结果经独立 `session_list` 帧回投（§6.3 按需查询，
  //    非 action_ack）。
  const listCid = uuid();
  const listP = waitFor(
    'session_list 帧',
    (f) => f.t === 'session_list' && (f.commandId === listCid || f.command_id === listCid),
    RETRY_ACK_MS
  );
  send({ t: 'action', commandId: listCid, type: 'session/list', payload: { chatId } });
  const listFrame = await listP;
  const sessions = listFrame.sessions || [];
  if (!sessions.length) {
    fail('d', 'session_list 未返回会话数组（见上方原始帧）');
  } else {
    const labels = sessions
      .map((s) => `${s.sessionId}${s.boundChatId ? '(当前/已绑定)' : ''}`)
      .join(', ');
    pass('d', `会话列表 ${sessions.length} 项: ${labels}`);
  }

  // e. chat/load：候选优先级——(1) 本 chat 已绑定**非当前**的历史会话
  //    （同 chat 内切换，agent 进程内会话必有历史，真实面板场景）；
  //    (2) 未绑定标注的历史会话（可能已被其他 chat 全局绑定 → 预绑定
  //    冲突，§8.5 每会话单 chat 语义，冲突时换下一个候选）；
  //    (3) 本 chat 当前会话（重载兜底）。
  const cur = sessions.find((s) => s.sessionId && s.boundChatId === chatId);
  const others = sessions.filter((s) => s.sessionId && s.boundChatId === chatId && s.sessionId !== (cur && cur.sessionId));
  const candidates = [
    ...others,
    ...sessions.filter((s) => s.sessionId && !s.boundChatId),
    ...(cur ? [cur] : []),
  ];
  let chosen = null;
  let loadAck = null;
  // 先注册 chat doc 增量监听（回放帧可能早于 load 响应到达——
  // replay before response；catch 兜底防 unhandledRejection）。
  const tracker = trackChatDoc(yjs, `chat:${chatId}`, null).catch((e) => ({
    applied: 0,
    order: 0,
    texts: [`tracker 异常: ${e.message}`],
  }));
  for (const s of candidates) {
    const cid = uuid();
    // 注意:load 与 create 一样是两阶段 ack(accepted → committed);
    // accepted 只代表入队,不算终态,必须等 committed 或 action_error,
    // 否则会把成功入队的 load 误判为「被拒」并连发下一个候选。
    const outcomeP = waitFor(
      `load 终态 ${cid.slice(0, 8)}…`,
      (f) =>
        (f.t === 'action_ack' && f.commandId === cid && f.status === 'committed') ||
        (f.t === 'action_error' && (f.commandId === cid || f.command_id === cid)),
      RETRY_ACK_MS
    );
    await sleep(50);
    send({ t: 'action', commandId: cid, type: 'chat/load', payload: { chatId, acpSessionId: s.sessionId } });
    const res = await outcomeP;
    if (res.t === 'action_ack' && res.status === 'committed') {
      chosen = s;
      loadAck = res;
      pass('e', `chat/load committed acpSessionId=${s.sessionId}（chatId=${res.chatId}）`);
      break;
    }
    console.log(`  [候选 ${s.sessionId.slice(0, 8)}… load 被拒: ${res.message || res.status}（换下一个）]`);
  }
  if (!chosen) {
    fail('e', '所有候选会话 load 均被拒（绑定冲突或 agent 拒绝）');
    process.exit(failed ? 1 : 0);
  }
  const targetId = chosen.sessionId;

  // f. 回放增量验证
  try {
    const rep = await tracker;
    const detail = yjs
      ? `收到 ${rep.applied} 个增量帧，entry_order=${rep.order}，消息片段: ${JSON.stringify(rep.texts.slice(0, 3))}`
      : `收到 ${rep.applied} 个增量帧（yjs 不可用，帧级验证）`;
    if (rep.order > baseEntries) {
      pass('f', `回放生效（消息恢复）: ${detail}`);
    } else if (rep.applied > 0) {
      fail('f', `收到增量帧但 entries 未增长: ${detail}`);
    } else {
      fail('f', `load committed 后 15s 内无回放增量帧（预绑定未生效或 agent 未回放）`);
    }
  } catch (e) {
    fail('f', `回放等待超时: ${e.message}`);
  }

  ws.close();
  await sleep(200);
  console.log(`\n结果: ${passed} PASS / ${failed} FAIL`);
  process.exit(failed ? 1 : 0);
}

main().catch((e) => {
  console.error(`\nFATAL: ${e.message}`);
  ws.close();
  process.exit(1);
});
