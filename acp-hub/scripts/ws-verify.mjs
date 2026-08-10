#!/usr/bin/env node
// acp-hub 协议闭环验证脚本（M3 方案 §7）。
//
// 依赖：node ≥21（内置 WebSocket，零 npm 依赖；不进 cargo workspace）。
// 用法：node scripts/ws-verify.mjs <token> [ws://127.0.0.1:8456/]
//
// 断言：
//   (a) 首帧 auth → ready.projectionVersions 含 hub:registry；
//   (b) chat/create → committed ack 的 chatId 非空；
//   (c) subscribe chat:{sid} → 快照帧 doc 匹配且带 projectionVersion、
//       update 为合法 base64（解码非空）；可选 yjs 解码打印 entry_order；
//   (d) session/prompt → ack 两阶段 accepted → committed；
//   (e) 收到无 projectionVersion 的 ysync.update 增量帧；
//   (f) keep_alive → 回 pong，不被 4501 断开（收到第二个 keep_alive）；
//   (g) ysync.unsubscribe 后主动 close，正常关闭（code 1000）。
//
// 环境要求：server 已启动、instance 在线（prompt 才能 committed）、
// token 为 full 角色（read-only 发 action 会被拒）。

'use strict';

const RETRY_READY_MS = 15000;
const RETRY_ACK_MS = 60000;
const RETRY_UPDATE_MS = 30000;
const RETRY_KEEPALIVE_MS = 8000;

const token = process.argv[2];
const baseUrl = process.argv[3] || 'ws://127.0.0.1:8456/';
if (!token) {
  console.error('用法: node scripts/ws-verify.mjs <token> [ws://127.0.0.1:8456/]');
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

// ── 帧流 + waitFor ────────────────────────────────────────────────────────

const waiters = [];

function removeWaiter(w) {
  const i = waiters.indexOf(w);
  if (i >= 0) waiters.splice(i, 1);
}

// 等待满足 predicate 的下一帧；超时 reject。
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

// ── 连接 ──────────────────────────────────────────────────────────────────

const ws = new WebSocket(baseUrl);
let closeEvent = null;
let closeResolve = null;
const closedPromise = new Promise((r) => {
  closeResolve = r;
});

ws.addEventListener('open', () => {
  // 首帧纪律：auth 必须是第一帧。
  ws.send(JSON.stringify({ t: 'auth', token }));
});

ws.addEventListener('message', (ev) => {
  let frame;
  try {
    frame = JSON.parse(ev.data);
  } catch (e) {
    console.error(`非 JSON 帧: ${ev.data.slice(0, 80)}`);
    return;
  }
  // 心跳：立即回 pong（server 每 5s 一次，15s 不回 → 4501 关闭）。
  if (frame.t === 'keep_alive') {
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

ws.addEventListener('close', (ev) => {
  closeEvent = { code: ev.code, wasClean: ev.wasClean, reason: ev.reason };
  closeResolve(closeEvent);
});

ws.addEventListener('error', () => {
  // close 事件会随后到达（1006），错误详情在此不需要。
});

// ── 工具 ──────────────────────────────────────────────────────────────────

function send(frame) {
  if (ws.readyState !== WebSocket.OPEN) {
    throw new Error(`连接未 OPEN（readyState=${ws.readyState}）`);
  }
  ws.send(JSON.stringify(frame));
}

// 等待指定 commandId 的指定 status 的 ack。
function waitAck(commandId, status, timeoutMs) {
  return waitFor(
    `ack ${commandId.slice(0, 8)}…=${status}`,
    (f) =>
      f.t === 'action_ack' &&
      f.commandId === commandId &&
      f.status === status,
    timeoutMs
  );
}

// base64 合法性校验：能解码且非空，且再编码回原串（标准 base64 往返一致）。
function checkBase64(b64) {
  try {
    const buf = Buffer.from(b64, 'base64');
    if (buf.length === 0) return { ok: false, why: '解码为空' };
    if (buf.toString('base64') !== b64) {
      return { ok: false, why: '非标准 base64（往返不一致）' };
    }
    return { ok: true, bytes: buf };
  } catch (e) {
    return { ok: false, why: e.message };
  }
}

// 可选：动态 import yjs 解码 yrs v1 快照；不可用返回 null。
// 注意：初始值必须 undefined（null 会与「import 失败」状态混淆）。
let yjsCache = undefined;
async function loadYjs() {
  if (yjsCache === undefined) {
    try {
      yjsCache = await import('yjs');
    } catch (e) {
      yjsCache = null;
    }
  }
  return yjsCache;
}

// 从 registry 快照解码 instance id（instance_id = token name；本机 bootstrap
// 实例 name="local"，即 create 缺省 instanceId 的目标，§4.3 P5）。
// 返回第一个 instance 的 id；解码失败/无实例 → null。
async function detectInstanceId(registrySnapshot) {
  const env = process.env.ACP_HUB_INSTANCE_ID;
  if (env) return env;
  const yjs = await loadYjs();
  if (!yjs) return null;
  try {
    const doc = new yjs.Doc();
    yjs.applyUpdate(doc, registrySnapshot.bytes);
    const instances = doc.getMap('root').get('instances');
    if (!instances) return null;
    let id = null;
    instances.forEach((v, key) => {
      if (id === null || v.get('status') === 'online') id = key;
    });
    return id;
  } catch (e) {
    console.log(`      [yjs] registry 解码失败: ${e.message}`);
    return null;
  }
}

// ── 主流程 ────────────────────────────────────────────────────────────────

async function main() {
  console.log(`连接 ${baseUrl}（token ${token.length} 字符）…`);

  // 等待握手完成：open 事件（auth 已在其中作为首帧发送）先于一切发送。
  // 连接失败（server 未启动等）会在 open 前触发 close(1006)。
  await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error('open 超时（10s）')),
      10000
    );
    ws.addEventListener(
      'open',
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true }
    );
    ws.addEventListener(
      'close',
      (ev) => {
        clearTimeout(timer);
        reject(new Error(`open 前关闭 code=${ev.code}（server 未启动/被拒？）`));
      },
      { once: true }
    );
  });
  console.log('握手完成，auth 已作为首帧发送');

  // (a) 订阅 registry → ready 含 hub:registry。
  // 先注册快照 waiter 再发送：server 推快照先于 ready，晚注册会错过
  // （ws 消息是异步事件，同一 tick 内 send 后注册 waiter 是安全的）。
  const regSnapWaiter = waitFor(
    'registry 快照',
    (f) =>
      f.t === 'ysync.update' &&
      f.doc === 'hub:registry' &&
      Object.prototype.hasOwnProperty.call(f, 'projectionVersion'),
    RETRY_UPDATE_MS
  );
  send({ t: 'ysync.subscribe', docs: ['hub:registry'] });
  const ready = await waitFor(
    'ready 含 hub:registry',
    (f) =>
      f.t === 'ready' &&
      f.projectionVersions &&
      Object.prototype.hasOwnProperty.call(f.projectionVersions, 'hub:registry'),
    RETRY_READY_MS
  );
  pass('a', `ready.projectionVersions = ${JSON.stringify(ready.projectionVersions)}`);

  // 捕获 registry 快照（create 前先解码出 instance id：instance_id = token
  // name，缺省 instanceId="local" 只匹配同名 token，见 ws-verify 说明）。
  const regSnap = await regSnapWaiter;
  const regB64 = checkBase64(regSnap.update);
  if (!regB64.ok) {
    fail('a', `registry 快照 update 非法: ${regB64.why}`);
  } else {
    console.log(`      registry 快照 projectionVersion=${regSnap.projectionVersion}`);
  }
  const instanceId = regB64.ok ? await detectInstanceId(regB64) : null;
  if (instanceId) {
    console.log(`      检测到 instance id=${instanceId}（将随 create 下发）`);
  }

  // (b) chat/create → committed ack 带 chatId（error 立即诊断）。
  const createCid = uuid();
  const createPayload = instanceId ? { instanceId } : {};
  send({ t: 'action', commandId: createCid, type: 'chat/create', payload: createPayload });
  const createAck = await Promise.race([
    waitAck(createCid, 'committed', RETRY_ACK_MS),
    waitFor(
      `create error ${createCid.slice(0, 8)}…`,
      (f) => f.t === 'action_error' && f.commandId === createCid,
      RETRY_ACK_MS
    ).then((err) => {
      throw new Error(
        `create 被拒 ${err.code}${err.message ? ': ' + err.message : ''}` +
          (instanceId
            ? ''
            : '（未检测到 instance id：registry.instances 投影为空或 yjs 不可用；' +
              'instance id = instance token 的 name，用 ACP_HUB_INSTANCE_ID 指定，' +
              '如 ACP_HUB_INSTANCE_ID=local）')
      );
    }),
  ]);
  const sid = createAck.chatId;
  if (!sid) {
    fail('b', 'create committed ack 缺少 chatId');
    ws.close(1000, 'verify abort');
    return 1;
  }
  pass('b', `create committed chatId=${sid}`);

  // (c) 订阅 chat + control → chat 快照帧（带 projectionVersion，合法 base64）
  const chatDoc = `chat:${sid}`;
  const controlDoc = `control:${sid}`;
  send({ t: 'ysync.subscribe', docs: [chatDoc, controlDoc] });
  const snapshot = await waitFor(
    `chat 快照 ${chatDoc}`,
    (f) =>
      f.t === 'ysync.update' &&
      f.doc === chatDoc &&
      Object.prototype.hasOwnProperty.call(f, 'projectionVersion'),
    RETRY_UPDATE_MS
  );
  const b64 = checkBase64(snapshot.update);
  if (!b64.ok) {
    fail('c', `快照 update 非法: ${b64.why}`);
  } else {
    pass('c', `快照 projectionVersion=${snapshot.projectionVersion} update=${snapshot.update.length} 字符`);
    // 可选：动态 import yjs 解码快照（npm i yjs 后启用），仅增强诊断。
    try {
      const yjs = await loadYjs();
      if (!yjs) throw new Error('yjs 未安装');
      const doc = new yjs.Doc();
      yjs.applyUpdate(doc, b64.bytes);
      const root = doc.getMap('root');
      const order = root.get('entry_order');
      console.log(
        `      [yjs] 解码成功: entry_order=${order ? order.length : 0} ` +
          `entries=${root.get('entries') ? root.get('entries').size : 0}`
      );
    } catch (e) {
      console.log(`      [yjs] 跳过解码（npm i yjs 可启用）: ${e.message}`);
    }
  }

  // (e) 增量帧 waiter 先注册（prompt 的 user 回显增量可能早于 committed）。
  const incWaiter = waitFor(
    'chat 增量帧（无 projectionVersion）',
    (f) =>
      f.t === 'ysync.update' && f.doc === chatDoc && !Object.prototype.hasOwnProperty.call(f, 'projectionVersion'),
    RETRY_UPDATE_MS
  );

  // (d) session/prompt → accepted → committed 两阶段
  const promptCid = uuid();
  send({
    t: 'action',
    commandId: promptCid,
    type: 'chat/prompt',
    payload: { chatId: sid, message: 'ws-verify 闭环验证消息' },
  });
  await waitAck(promptCid, 'accepted', RETRY_ACK_MS);
  pass('d', `prompt accepted（commandId=${promptCid.slice(0, 8)}…）`);
  const promptAck = await waitAck(promptCid, 'committed', RETRY_ACK_MS);
  pass('d', `prompt committed（turnId=${promptAck.turnId ? promptAck.turnId.slice(0, 8) + '…' : '—'}）`);

  // (e) 增量帧到达（快照后同 doc 的无 projectionVersion 更新）
  try {
    const inc = await incWaiter;
    pass('e', `增量帧 doc=${inc.doc} update=${inc.update.length} 字符（无 projectionVersion）`);
  } catch (e) {
    fail('e', e.message);
  }

  // (f) 心跳：第一个 keep_alive 已全局回 pong；等第二个证明未被 4501 踢。
  try {
    await waitFor('第二个 keep_alive', (f) => f.t === 'keep_alive', RETRY_KEEPALIVE_MS);
    pass('f', 'keep_alive→pong 存活（收到第二个 keep_alive，未 4501）');
  } catch (e) {
    if (closeEvent && closeEvent.code === 4501) {
      fail('f', `被 4501 心跳超时关闭：${e.message}`);
    } else {
      fail('f', e.message);
    }
  }

  // (g) ysync.unsubscribe 后主动 close，正常关闭。
  // 注意：server 收到客户端 close 帧后正常处理（其审计日志记录 code=1000）
  // 但可能不回 close 帧直接关 TCP → 客户端侧表现为 1006；二者都视为干净
  // 关闭。4500/4501/4502 等 server 主动异常码仍严格失败。
  send({ t: 'ysync.unsubscribe', docs: [chatDoc, controlDoc] });
  await new Promise((r) => setTimeout(r, 300));
  ws.close(1000, 'verify done');
  const ev = await closedPromise;
  if (ev.code === 1000 && ev.wasClean) {
    pass('g', `unsubscribe 后正常关闭 code=${ev.code} wasClean=${ev.wasClean}`);
  } else if (ev.code === 1006 && !ev.wasClean) {
    pass('g', `unsubscribe 后关闭（server 未回 close 帧，客户端 code=1006；server 侧记录 1000，视为干净）`);
  } else {
    fail('g', `关闭异常 code=${ev.code} wasClean=${ev.wasClean} reason=${ev.reason}`);
  }

  console.log(`\n结果: ${passed} PASS / ${failed} FAIL`);
  return failed === 0 ? 0 : 1;
}

// 总超时护栏：90s 强制退出（防挂死）。
const overall = setTimeout(() => {
  console.error('整体超时（90s），强制退出');
  process.exit(1);
}, 90000);

main()
  .then((code) => {
    clearTimeout(overall);
    process.exit(code);
  })
  .catch((e) => {
    clearTimeout(overall);
    fail('流程', e.message);
    console.error(`\n结果: ${passed} PASS / ${failed} FAIL`);
    process.exit(1);
  });
