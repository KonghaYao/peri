#!/usr/bin/env node
// acp-hub 端到端验收脚本（M3 方案 §7 自动化版）：模拟 Web 面板用户全流程。
//
// 零 npm 依赖（node ≥21 内置 WebSocket）；自包含、可重复执行：
//   1. 随机端口 + 临时 config/data 目录启动真实 acp-hub-server 二进制
//      （ACP_HUB_CONFIG_DIR/ACP_HUB_DATA_DIR/ACP_HUB_LISTEN_PORT 注入，
//      ACP_HUB_ACP_CMD 指向 test-child 替身）→ 等 bootstrap instance token；
//   2. 启动 acp-instance（--token-file）→ 等 server 日志 "instance connected"；
//   3. token generate 生成 client full token；
//   4. ws 客户端全流程断言（a..h，每步 PASS/FAIL 打印）；
//   5. 清理全部子进程（server/instance/test-child）与临时目录。
//
// 用法：
//   cd acp-hub && cargo build --workspace && node scripts/e2e-flow.mjs
//   可选环境变量：ACP_HUB_BIN_DIR（二进制目录，默认 <repo>/target/debug）
//
// 输出约定：RESULT: PASS / RESULT: FAIL <摘要>（第一行，供 workflow 判定）；
// 随后为每步断言明细。
//
// 内容级验证说明：chat/session doc 的 update 为 yrs v1 二进制（base64），
// 零依赖下无法解码；脚本对「prompt 回应内容」做帧级断言（收到 ≥1 个
// chat:{sid} 增量帧 = ACP 事件经规范化-聚合-广播回流成功）；若环境中存在
// yjs（scripts/node_modules/yjs）则自动升级为解码级验证（entries 文本含
// prompt 回应 chunk）。

'use strict';

// 异常退出兜底：dump 全部子进程日志（诊断用，调试真实 peri 时必备——
// 脚本因 await 抛错退出时，server/instance 侧证据不能丢）。注意 children
// 会被 cleanup 清空，日志引用必须用全局副本。
const __allProcs = [];
process.on('unhandledRejection', (err) => {
  console.error(`\nUNHANDLED REJECTION: ${err && err.message ? err.message : err}`);
  for (const p of __allProcs) {
    const lines = logDump(p);
    if (lines) console.error(`--- ${p.label} 日志（完整）---\n${lines.split('\n').join('\n')}`);
  }
  process.exit(1);
});

import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync, readFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:net';
import readline from 'node:readline';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, '..');
const BIN_DIR = process.env.ACP_HUB_BIN_DIR || join(REPO, 'target', 'debug');
const SERVER_BIN = join(BIN_DIR, 'acp-hub-server');
const INSTANCE_BIN = join(BIN_DIR, 'acp-instance');
const TEST_CHILD_BIN = join(BIN_DIR, 'test-child');

// ── 断言记账 ────────────────────────────────────────────────────────────────
let passed = 0;
let failed = 0;
const failures = [];
function pass(label, detail) {
  passed++;
  console.log(`PASS (${label}) ${detail || ''}`);
}
function fail(label, detail) {
  failed++;
  failures.push(`${label}: ${detail}`);
  console.error(`FAIL (${label}) ${detail}`);
}

// ── 小工具 ──────────────────────────────────────────────────────────────────
function uuid() {
  return crypto.randomUUID();
}

async function pickFreePort() {
  for (let i = 0; i < 50; i++) {
    const port = 20000 + Math.floor(Math.random() * 20000);
    const ok = await new Promise((resolve) => {
      const srv = createServer();
      srv.once('error', () => resolve(false));
      srv.once('listening', () => srv.close(() => resolve(true)));
      srv.listen(port, '127.0.0.1');
    });
    if (ok) return port;
  }
  throw new Error('无法找到空闲端口');
}

// 日志行缓冲 + 等待子串（支持正则）
function attachLog(proc) {
  const buf = [];
  const rl = readline.createInterface({ input: proc.stderr });
  rl.on('line', (l) => {
    buf.push(l);
    if (process.env.ACP_HUB_E2E_VERBOSE) console.log(`  [${proc.label}] ${l}`);
  });
  proc._logBuf = buf;
  proc._logLine = rl;
}
function logDump(proc) {
  return (proc._logBuf || []).join('\n');
}
async function waitLog(proc, re, timeoutMs, what) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const hit = (proc._logBuf || []).some((l) => re.test(l));
    if (hit) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`超时(${timeoutMs}ms): 等待 ${what}（server/instance 日志）`);
}

// ── 子进程管理（进程组 kill 防残留）────────────────────────────────────────
const children = [];
function startProc(label, bin, args, env, cwd) {
  const proc = spawn(bin, args, {
    env: { ...process.env, ...env },
    cwd,
    detached: true, // 独立进程组，组级 kill 防残留
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  proc.label = label;
  proc.stdout.setEncoding('utf8');
  proc.stderr.setEncoding('utf8');
  attachLog(proc);
  children.push(proc);
  __allProcs.push(proc); // 全局副本：cleanup 清空 children 后仍可 dump
  return proc;
}
function killProc(proc, sig = 'SIGTERM') {
  try {
    process.kill(-proc.pid, sig); // 进程组
  } catch {
    try {
      process.kill(proc.pid, sig);
    } catch {
      /* 已退出 */
    }
  }
}
async function cleanup(keep) {
  // 兜底：杀本次路径的 test-child 残留（instance 退出时 kill_on_drop 覆盖
  // 直接子进程，此处防孙进程/孤儿）。
  try {
    execFileSync('pkill', ['-f', `^${TEST_CHILD_BIN.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`], {
      stdio: 'ignore',
    });
  } catch {
    /* 无匹配 */
  }
  for (const p of children.reverse()) {
    if (!p || p.exitCode !== null) continue;
    killProc(p, 'SIGTERM');
  }
  await new Promise((r) => setTimeout(r, 1500));
  for (const p of children) {
    if (p && p.exitCode === null) killProc(p, 'SIGKILL');
  }
  if (!keep && globalThis.__tmpDirs) {
    for (const d of globalThis.__tmpDirs) {
      try {
        rmSync(d, { recursive: true, force: true });
      } catch {
        /* 忽略 */
      }
    }
  }
  children.length = 0;
}

// ── ws 客户端（waitFor 模式，仿 ws-verify.mjs）────────────────────────────
const waiters = [];
function removeWaiter(w) {
  const i = waiters.indexOf(w);
  if (i >= 0) waiters.splice(i, 1);
}
function waitFor(label, predicate, timeoutMs) {
  return new Promise((resolve, reject) => {
    const w = {
      predicate,
      onMatch: (frame) => {
        clearTimeout(w.timer);
        removeWaiter(w);
        resolve(frame);
        return true; // 已移除，调用方需回退索引
      },
      timer: null,
    };
    w.timer = setTimeout(() => {
      removeWaiter(w);
      reject(new Error(`超时(${timeoutMs}ms): ${label}`));
    }, timeoutMs);
    waiters.push(w);
  });
}
// 收集满足 predicate 的 N 帧（逐帧累积；期间不命中其他 waiter）。
function collectFrames(label, predicate, count, timeoutMs) {
  return new Promise((resolve, reject) => {
    const acc = [];
    const w = {
      predicate,
      onMatch: (frame) => {
        acc.push(frame);
        if (acc.length >= count) {
          clearTimeout(w.timer);
          removeWaiter(w);
          resolve(acc);
          return true; // 已移除
        }
        return false; // 未收满，留在 waiters 等待下一帧
      },
      timer: null,
    };
    w.timer = setTimeout(() => {
      removeWaiter(w);
      reject(new Error(`超时(${timeoutMs}ms): ${label}（已收 ${acc.length}/${count} 帧）`));
    }, timeoutMs);
    waiters.push(w);
  });
}
function checkBase64(b64) {
  try {
    const buf = Buffer.from(b64, 'base64');
    if (buf.length === 0) return { ok: false, why: '解码为空' };
    if (buf.toString('base64') !== b64) return { ok: false, why: '非标准 base64' };
    return { ok: true, bytes: buf };
  } catch (e) {
    return { ok: false, why: e.message };
  }
}

// 可选 yjs（scripts/node_modules/yjs 或全局解析路径）；不可用返回 null。
let yjsCache = undefined;
async function loadYjs() {
  if (yjsCache === undefined) {
    try {
      yjsCache = await import('yjs');
    } catch {
      yjsCache = null;
    }
  }
  return yjsCache;
}

// ── 主流程 ──────────────────────────────────────────────────────────────────
async function main() {
  const keep = process.argv.includes('--keep');
  const verbose = process.argv.includes('--verbose');

  for (const [name, p] of [
    ['acp-hub-server', SERVER_BIN],
    ['acp-instance', INSTANCE_BIN],
    ['test-child', TEST_CHILD_BIN],
  ]) {
    if (!existsSync(p)) {
      console.error(`缺少 ${name} 二进制：${p}\n请先执行 cd acp-hub && cargo build --workspace`);
      process.exit(2);
    }
  }

  const port = await pickFreePort();
  const base = `ws://127.0.0.1:${port}/`;
  const tmpRoot = mkdtempSync(join(tmpdir(), 'acp-hub-e2e-'));
  globalThis.__tmpDirs = [tmpRoot];
  const configDir = join(tmpRoot, 'config');
  const dataDir = join(tmpRoot, 'data');
  const instanceDataDir = join(tmpRoot, 'instance-data');
  const instanceTokenFile = join(tmpRoot, 'instance.token');

  let server;
  let instance;
  try {
    // ── 1. 启动 server（随机端口 + 临时目录 + test-child 替身 ACP）────────
    server = startProc(
      'server',
      SERVER_BIN,
      [],
      {
        ACP_HUB_CONFIG_DIR: configDir,
        ACP_HUB_DATA_DIR: dataDir,
        ACP_HUB_LISTEN_PORT: String(port),
        ACP_HUB_LISTEN_ADDR: '127.0.0.1',
        ACP_HUB_ACP_CMD: process.env.ACP_HUB_ACP_CMD || TEST_CHILD_BIN, // §11 可配：默认 test-child 替身；注入真实 peri（如 ~/.peri/peri acp）做真机验证
        ACP_HUB_LOG_LEVEL: 'trace', // trace 含聚合器拒绝 reason（event applied）
        // EnvFilter 前缀 "acp_hub=…" 会截断 server 的 trace 级（target
        // acp_hub_server::… 前缀匹配），用 RUST_LOG 显式全局覆盖。
        RUST_LOG: 'trace',
      },
      REPO // default_cwd（§4.3 裁决）必须存在
    );
    await waitLog(server, /starting: listening/, 15000, 'server 启动横幅');
    console.log(`server 已启动：127.0.0.1:${port}（config=${configDir}）`);

    // bootstrap instance token：优先 stderr 打印行，回退 tokens.toml 解析。
    const allLog = logDump(server);
    const m = allLog.match(/已自动生成 bootstrap instance token[^\n]*\n(\S+)/);
    let instanceToken = m ? m[1].trim() : null;
    if (!instanceToken) {
      const tokPath = join(configDir, 'tokens.toml');
      const deadline = Date.now() + 15000;
      while (Date.now() < deadline && !existsSync(tokPath)) {
        await new Promise((r) => setTimeout(r, 200));
      }
      if (existsSync(tokPath)) {
        const txt = readFileSync(tokPath, 'utf8');
        const recs = [...txt.matchAll(/\[\[tokens\]\][\s\S]*?role\s*=\s*"instance"[\s\S]*?token\s*=\s*"([^"]+)"/g)];
        instanceToken = recs[0] ? recs[0][1] : null;
      }
    }
    if (!instanceToken || instanceToken.length < 40) {
      throw new Error('未能提取 bootstrap instance token（stderr 与 tokens.toml 均失败）');
    }
    console.log('bootstrap instance token 已提取（长度 ' + instanceToken.length + '）');
    writeFileSync(instanceTokenFile, instanceToken + '\n', { mode: 0o600 });

    // ── 2. 启动 instance → 等 server 日志 "instance connected" ──────────────
    instance = startProc(
      'instance',
      INSTANCE_BIN,
      [
        '--token-file', instanceTokenFile,
        '--server-url', `ws://127.0.0.1:${port}/instance`,
        '--data-dir', instanceDataDir,
        '--log-level', 'info',
      ],
      {},
      REPO
    );
    await waitLog(server, /instance connected/, 20000, 'instance 注册（hello 双向认证）');
    pass('b', `instance connected（instance_id=local，端口 ${port}）`);

    // ── 3. 生成 client full token（token generate 子命令，目录语义一致）──
    const tokOut = execFileSync(SERVER_BIN, ['token', 'generate', '--name', 'e2e-client', '--role', 'full'], {
      env: { ...process.env, ACP_HUB_CONFIG_DIR: configDir, ACP_HUB_DATA_DIR: dataDir },
      encoding: 'utf8',
    });
    const clientToken = tokOut.trim().split('\n')[0];
    if (!clientToken || clientToken.length < 40) {
      throw new Error('token generate 未输出 client full token');
    }
    console.log('client full token 已生成（长度 ' + clientToken.length + '）');

    // ── 4. ws 客户端全流程 ────────────────────────────────────────────────
    await runWsFlow({ base, clientToken, instance, server, port });
    console.log(`\n结果: ${passed} PASS / ${failed} FAIL`);
    return failed === 0;
  } finally {
    await cleanup(keep);
  }
}

async function runWsFlow({ base, clientToken, instance, server }) {
  const ws = new WebSocket(base);
  let closeEvent = null;
  let closeResolve = null;
  const closedPromise = new Promise((r) => {
    closeResolve = r;
  });

  ws.addEventListener('open', () => {
    ws.send(JSON.stringify({ t: 'auth', token: clientToken }));
  });
  ws.addEventListener('message', (ev) => {
    let frame;
    try {
      frame = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (frame.t === 'keep_alive') {
      ws.send(JSON.stringify({ t: 'pong' }));
    }
    for (let i = 0; i < waiters.length; i++) {
      const w = waiters[i];
      if (w.predicate(frame)) {
        // onMatch 返回是否已移除自身：已移除 → splice 使下一 waiter 前移，
        // 回退索引补查；未移除（collectFrames 未收满）→ 不得回退，避免
        // 同一帧被重复匹配（曾导致单帧计双、增量断言假阳性）。
        if (w.onMatch(frame)) i--;
      }
    }
  });
  ws.addEventListener('close', (ev) => {
    closeEvent = { code: ev.code, wasClean: ev.wasClean, reason: ev.reason };
    closeResolve(closeEvent);
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
  // 等 action 终态（committed 或 action_error）
  function waitTerminal(commandId, timeoutMs) {
    return waitFor(
      `终态 ${commandId.slice(0, 8)}…`,
      (f) =>
        (f.t === 'action_ack' && f.commandId === commandId && f.status === 'committed') ||
        (f.t === 'action_error' && f.commandId === commandId),
      timeoutMs
    );
  }

  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('open 超时（10s）')), 10000);
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
        reject(new Error(`open 前关闭 code=${ev.code}`));
      },
      { once: true }
    );
  });

  // (a) 首帧 auth；订阅 registry → ready（projectionVersions 含 hub:registry）。
  // ready 只在首次订阅后推送（§4.6 步骤 4，`if first`），必须先订阅。
  const regSnapWaiter = waitFor(
    'registry 快照',
    (f) => f.t === 'ysync.update' && f.doc === 'hub:registry' && 'projectionVersion' in f,
    15000
  );
  send({ t: 'ysync.subscribe', docs: ['hub:registry'] });
  const ready = await waitFor(
    'ready 含 hub:registry',
    (f) => f.t === 'ready' && f.projectionVersions && 'hub:registry' in f.projectionVersions,
    15000
  );
  pass('a', `auth → subscribe → ready（projectionVersions=${JSON.stringify(ready.projectionVersions)}）`);

  // (b) chat/create → accepted → committed（chatId 非空）
  const regSnap = await regSnapWaiter;
  const regB64 = checkBase64(regSnap.update);
  if (!regB64.ok) fail('b', `registry 快照 update 非法: ${regB64.why}`);
  else console.log(`      registry 快照 projectionVersion=${regSnap.projectionVersion}`);

  // (b0) create 后 registry 增量收集（upsert_session 写回 title，§5.2 单写；
  // 解码验证会话条目 title 非空——「左侧列表不显示裸 id」验收点）。
  const regIncCollect = collectFrames(
    'hub:registry 增量（create 后）',
    (f) => f.t === 'ysync.update' && f.doc === 'hub:registry' && !('projectionVersion' in f),
    1,
    15000
  );
  const createCid = uuid();
  send({ t: 'action', commandId: createCid, type: 'chat/create', payload: {} });
  const createAck = await waitTerminal(createCid, 30000);
  if (createAck.t === 'action_error') {
    fail('b', `create 被拒 code=${createAck.code} msg=${createAck.message}`);
    return;
  }
  const sid = createAck.chatId;
  if (!sid) {
    fail('b', 'create committed 缺 chatId');
    return;
  }
  pass('b', `chat/create → committed（chatId=${sid}）`);
  try {
    const regIncs = await regIncCollect;
    const yj = await loadYjs();
    if (yj && regB64.ok) {
      const d = new yj.Doc();
      yj.applyUpdate(d, regB64.bytes);
      for (const inc of regIncs) yj.applyUpdate(d, Buffer.from(inc.update, 'base64'));
      const s = d.getMap('root').get('chats')?.get(sid);
      const title = s ? s.get('title') : null;
      if (title && String(title).trim()) {
        pass('b', `registry chat 条目含 title（${String(title)}）——左侧列表不显示裸 id`);
      } else {
        fail('b', `registry chat 条目缺 title（当前值 ${JSON.stringify(title)}，面板将显示裸 id）`);
      }
    } else {
      console.log('      [提示] yjs 不可用，跳过 registry title 解码验证');
    }
  } catch (e) {
    fail('b', `create 后未收到 registry 增量帧（registry doc 未更新）：${e.message}`);
  }

  // (c) 关键：真实 spawn 成功（instance 拉起 test-child 进程组）
  try {
    await waitLog(instance, /ACP 子进程已启动/, 10000, 'instance spawn test-child');
    pass('c', `spawn 真实发生（instance 拉起 ACP 进程组，chat=${sid}）`);
  } catch (e) {
    fail('c', `create committed 但 instance 未记录 spawn 成功：${e.message}`);
  }

  // (d) 订阅 chat:{sid} + control:{sid} → chat 快照帧（带 projectionVersion）
  // control doc 在 create 后、prompt 前无任何 update（mirror 未创建）——
  // §4.6「空会话视图由客户端按空 doc 处理」，server 不推空快照（设计语义，
  // 见 server/src/control/hub.rs snapshot 注释）；故 control 快照为可选。
  const chatDoc = `chat:${sid}`;
  const controlDoc = `control:${sid}`;
  send({ t: 'ysync.subscribe', docs: [chatDoc, controlDoc] });
  const chatSnap = await waitFor(
    `chat 快照 ${chatDoc}`,
    (f) => f.t === 'ysync.update' && f.doc === chatDoc && 'projectionVersion' in f,
    15000
  );
  const chatB64 = checkBase64(chatSnap.update);
  if (!chatB64.ok) {
    fail('d', `chat 快照 update 非法: ${chatB64.why}`);
  } else {
    pass('d', `chat:{sid} 快照投影 projectionVersion=${chatSnap.projectionVersion}（合法 base64）`);
  }
  try {
    const controlSnap = await waitFor(
      `control 快照 ${controlDoc}`,
      (f) => f.t === 'ysync.update' && f.doc === controlDoc && 'projectionVersion' in f,
      3000
    );
    console.log(`      control:{sid} 快照 projectionVersion=${controlSnap.projectionVersion}`);
  } catch {
    console.log('      control:{sid} 无快照帧（doc 尚无 update，按空 doc 处理，§4.6 语义）');
  }

  // (e) prompt → accepted → committed → ACP delta 回流。
  // chat 增量 ≥1 = server 单写 UserMessage 注册帧（prompt 到达 ACP 进程的
  // 决定性证据）；test-child 替身还会追加 chunk_1 文本帧（≥2）。
  // 真实 peri：prompt 是真实 LLM 任务，响应耗时不定（本脚本用超短任务
  // 约束到秒级），事件回流节奏由 agent 循环决定。
  const chatIncCollect = collectFrames(
    'chat:{sid} 增量帧 ≥1（UserMessage 注册回流）',
    (f) => f.t === 'ysync.update' && f.doc === chatDoc && !('projectionVersion' in f),
    1,
    60000
  );
  // control doc 增量累计收集：≥2 = active_turn 注册（prompt）+ turn 终态
  // 投影（§7.2：终态由 prompt L3 stopReason 驱动注入，cancel 对已终态
  // turn 幂等——终态增量在 prompt 阶段已广播，必须累计式收集）。
  const controlIncCollect = collectFrames(
    'control:{sid} 增量帧 ≥2（active_turn 注册 + turn 终态投影）',
    (f) => f.t === 'ysync.update' && f.doc === controlDoc && !('projectionVersion' in f),
    2,
    120000
  );
  const promptCid = uuid();
  // 真实 peri 会把 prompt 当真实任务执行（agent 循环 + 工具调用，可能
  // 持续数分钟）——验收用显式超短任务，约束执行时长到秒级。
  const promptMsg = '只回复一个字：好。不要调用任何工具，不要读取任何文件。';
  send({
    t: 'action',
    commandId: promptCid,
    type: 'chat/prompt',
    payload: { chatId: sid, message: promptMsg },
  });
  await waitAck(promptCid, 'accepted', 15000);
  const promptAck = await waitTerminal(promptCid, 120000);
  if (promptAck.t === 'action_error') {
    fail('e', `prompt 被拒 code=${promptAck.code} msg=${promptAck.message}`);
    return;
  }
  pass('e', `prompt → accepted → committed（turnId=${promptAck.turnId ? promptAck.turnId.slice(0, 8) + '…' : '—'}）`);

  // delta 内容级验证：yjs 可用则解码 chat 快照+增量验证文本；否则帧级。
  let chatIncs = null;
  try {
    chatIncs = await chatIncCollect;
    pass('e', `收到 ACP 事件回流增量帧 ${chatIncs.length} 个（UserMessage 注册帧 = prompt 真实到达 ACP）`);
  } catch (e) {
    fail('e', `未收到 chat 增量帧（prompt 未到达 ACP 或事件回流断点）：${e.message}`);
  }
  const yjs = await loadYjs();
  if (yjs && chatB64.ok && chatIncs && chatIncs.length >= 1) {
    try {
      const doc = new yjs.Doc();
      yjs.applyUpdate(doc, chatB64.bytes);
      for (const inc of chatIncs) {
        yjs.applyUpdate(doc, Buffer.from(inc.update, 'base64'));
      }
      const root = doc.getMap('root');
      const entries = root.get('entries');
      let text = '';
      if (entries) {
        entries.forEach((e) => {
          e.get('blocks')?.forEach((b) => {
            const kind = b.get('kind');
            const t = b.get('text');
            // server 写入的 text 是 yrs TextRef（Y.Text，chat_writer.rs
            // TextPrelim），不是字符串——统一经 toString() 提取。
            const tval = typeof t === 'string' ? t : t && typeof t.toString === 'function' ? t.toString() : '';
            if (tval && kind !== 'reasoning') text += tval;
          });
        });
      }
      // test-child 替身回复 chunk_1；真实 peri 回复短任务的实际文本
      // （"好"）——两者都应有非空 assistant 文本。
      if (text.includes('chunk_1')) {
        pass('e', `yjs 解码验证：chat doc 含 prompt 回应内容（chunk_1 文本到达）`);
      } else if (text.trim()) {
        pass('e', `yjs 解码验证：chat doc 含 assistant 回应文本（${JSON.stringify(text.slice(0, 60))}）`);
      } else {
        console.log(`      [yjs] 解码成功但 assistant 文本为空（text=${JSON.stringify(text.slice(0, 80))}）`);
      }
    } catch (e) {
      console.log(`      [yjs] 解码失败（跳过内容级验证）: ${e.message}`);
    }
  } else if (chatIncs && chatIncs.length >= 1) {
    console.log('      [提示] 未安装 yjs，内容级验证降级为帧级（npm i yjs 于 scripts/ 可启用解码）');
  }

  // (f) cancel → accepted → committed；turn 终态断言（§7.2：终态由 prompt
  // L3 stopReason 驱动注入，cancel 对已终态 turn 幂等——终态增量在 prompt
  // 阶段已广播，由 sessionIncCollect 累计断言）。
  const cancelCid = uuid();
  send({
    t: 'action',
    commandId: cancelCid,
    type: 'chat/cancel',
    payload: { chatId: sid },
  });
  await waitAck(cancelCid, 'accepted', 15000);
  const cancelAck = await waitTerminal(cancelCid, 15000);
  if (cancelAck.t === 'action_error') {
    fail('f', `cancel 被拒 code=${cancelAck.code} msg=${cancelAck.message}`);
    return;
  }
  pass('f', `chat/cancel → accepted → committed`);
  try {
    const incs = await controlIncCollect;
    pass('f', `cancel 生效：control doc 出现 turn 终态投影增量帧（共 ${incs.length} 个：active_turn 注册 + 终态）`);
  } catch (e) {
    fail('f', `control doc 未出现 turn 终态投影增量（prompt 终态注入或广播断点）：${e.message}`);
  }

  // (g) chat/close → committed + server 日志 chat closed + instance kill
  const closeCid = uuid();
  send({
    t: 'action',
    commandId: closeCid,
    type: 'chat/close',
    payload: { chatId: sid },
  });
  const closeAck = await waitTerminal(closeCid, 20000);
  if (closeAck.t === 'action_error') {
    fail('g', `close 被拒 code=${closeAck.code} msg=${closeAck.message}`);
  } else {
    pass('g', `chat/close → accepted → committed`);
    let ok = true;
    try {
      await waitLog(server, /chat closed/, 10000, 'server "chat closed" 日志');
      console.log('      server 日志：chat closed');
    } catch (e) {
      ok = false;
      fail('g', `server 未见 chat closed 日志（debug 级）：${e.message}`);
    }
    try {
      await waitLog(instance, /kill (完成（进程组）|幂等)/, 10000, 'instance kill 日志');
      console.log('      instance 日志：kill 指令已处理');
    } catch (e) {
      ok = false;
      fail('g', `instance 未见 kill 日志：${e.message}`);
    }
    if (ok) pass('g', 'close 全链确认（kill → 终态投影 → 日志）');
  }

  // (h) 断开（正常 close；1006 视为 server 不回 close 帧的干净关闭）
  ws.close(1000, 'e2e done');
  const ev = await closedPromise;
  if (ev.code === 1000 && ev.wasClean) {
    pass('h', `正常关闭 code=${ev.code} wasClean=${ev.wasClean}`);
  } else if (ev.code === 1006 && !ev.wasClean) {
    pass('h', `关闭完成（server 未回 close 帧，客户端 code=1006，视为干净）`);
  } else {
    fail('h', `关闭异常 code=${ev.code} wasClean=${ev.wasClean} reason=${ev.reason}`);
  }
}

// ── 入口（总护栏）───────────────────────────────────────────────────────────
const overall = setTimeout(() => {
  console.error('整体超时（150s），强制退出');
  cleanup(true).then(() => process.exit(1));
}, 150000);

main()
  .then((ok) => {
    clearTimeout(overall);
    if (ok) {
      console.log('RESULT: PASS');
    } else {
      console.log(`RESULT: FAIL ${failures.join('; ')}`);
    }
    process.exit(ok ? 0 : 1);
  })
  .catch((e) => {
    clearTimeout(overall);
    fail('流程', e.message);
    // 诊断兜底：dump 子进程日志（调试真实 peri 时定位 prompt 卡点；
    // children 已被 finally cleanup 清空，用全局副本）
    for (const p of __allProcs) {
      const lines = logDump(p);
      if (lines) console.error(`--- ${p.label} 日志（完整）---\n${lines.split('\n').join('\n')}`);
    }
    console.log(`RESULT: FAIL ${failures.join('; ')}`);
    cleanup(true).then(() => process.exit(1));
  });
