import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { configuration, loadPlaywright } from './e2e-config.mjs';

let stage = 'configuration';
let browser;
let activePage;
let failureCode = 'operation-failed';
const faults = new Set();
const sockets = [];

function progress(next) {
  stage = next;
  console.log(next);
}

function check(condition, code) {
  if (!condition) {
    failureCode = code;
    throw new Error('E2E assertion failed');
  }
}

async function healthy() {
  check(faults.size === 0, [...faults].sort().join(','));
  if (activePage) {
    check(await activePage.getByRole('alert').count() === 0 &&
      await activePage.locator('.has-error').count() === 0, 'visible-application-error');
  }
}

async function waitFor(predicate, timeout, code) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    await healthy();
    if (await predicate()) return;
    await delay(200);
  }
  check(false, code);
}

function observe(page, token) {
  page.on('pageerror', () => faults.add('pageerror'));
  page.on('response', response => {
    if (response.status() >= 500) faults.add('http-5xx');
    if (response.status() === 401 || response.status() === 403) faults.add('auth-rejected');
  });
  page.on('websocket', socket => {
    const url = new URL(socket.url());
    if (socket.url().includes(token)) faults.add('secret-in-websocket-url');
    if (!/^\/api\/chats\/[^/]+\/sync$/.test(url.pathname)) return;
    if (url.search) faults.add('websocket-query-present');
    const observation = { snapshots: 0, updates: 0 };
    sockets.push(observation);
    socket.on('socketerror', () => faults.add('websocket-error'));
    socket.on('framereceived', ({ payload }) => {
      try {
        const frame = JSON.parse(String(payload));
        if (frame.type === 'snapshot') observation.snapshots++;
        else if (frame.type === 'update') observation.updates++;
        else faults.add('invalid-websocket-frame');
      } catch { faults.add('invalid-websocket-frame'); }
    });
  });
}

function updateCount() {
  return sockets.reduce((total, socket) => total + socket.updates, 0);
}

try {
  progress('configuration');
  const config = await configuration();
  const { chromium } = await loadPlaywright();
  progress('browser-launch');
  browser = await chromium.launch({ headless: !config.headed, slowMo: config.slowMo });
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage();
  activePage = page;
  page.setDefaultTimeout(config.actionMs);
  page.setDefaultNavigationTimeout(config.readyMs);
  observe(page, config.token);
  const input = page.getByRole('textbox', { name: '输入消息', exact: true });
  const send = page.getByRole('button', { name: '发送消息', exact: true });
  const stop = page.getByRole('button', { name: '停止生成', exact: true });
  const assistants = page.getByRole('article', { name: 'Peri 的回答', exact: true });
  const users = page.getByRole('article', { name: '你的消息', exact: true });
  const runId = `peri-e2e-${randomUUID()}`;
  const firstPrompt = `This is an explicit live E2E test (${runId}). Do not use tools. Reply with only: READY ${runId}`;
  const secondPrompt = 'This is turn two of the same E2E test. Do not use tools. Repeat the peri-e2e identifier from my previous message, followed by SECOND.';

  async function idle(timeout = config.readyMs) {
    await waitFor(async () => await input.isEnabled() && await stop.count() === 0 &&
      await page.getByRole('button', { name: '等待服务端停止', exact: true }).count() === 0,
    timeout, 'composer-not-idle');
  }

  async function submit(prompt) {
    await idle();
    await input.fill(prompt);
    await send.click();
    await waitFor(async () => await users.filter({ hasText: prompt }).count() === 1,
      config.readyMs, 'user-message-not-persisted');
  }

  async function reply(index, marker, baseline) {
    await waitFor(async () => await assistants.count() === index + 1 &&
      (await assistants.nth(index).locator('.markdown').allTextContents()).join('').includes(marker) &&
      await assistants.nth(index).getByRole('button', { name: '复制回答', exact: true }).count() === 1 &&
      await stop.count() === 0,
    config.replyMs, 'assistant-reply-not-completed');
    check(updateCount() > baseline, 'missing-live-websocket-update');
    await idle();
  }

  async function history(expected, expectedUsers) {
    await waitFor(async () => await assistants.count() === expected.length && await users.count() === expectedUsers,
      config.readyMs, 'history-count-mismatch');
    for (const [index, content] of expected.entries()) {
      check(await assistants.nth(index).locator('.markdown').innerText() === content, 'history-content-mismatch');
    }
    check(await users.nth(0).innerText() === firstPrompt, 'first-prompt-history-mismatch');
    check(await users.nth(1).innerText() === secondPrompt, 'second-prompt-history-mismatch');
    await idle();
  }

  progress('UI-login');
  await page.goto(config.baseUrl);
  progress('UI-login-open-settings');
  await page.getByRole('button', { name: /我的空间/ }).click();
  progress('UI-login-fill-token');
  await page.getByRole('dialog', { name: '连接你的 Peri' }).locator('#access-token').fill(config.token);
  progress('UI-login-save');
  await page.getByRole('button', { name: '保存设置', exact: true }).click();
  progress('UI-login-confirm');
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  progress('UI-login-list-ready');
  await waitFor(async () => await page.getByRole('navigation', { name: '历史会话' }).getAttribute('aria-busy') === 'false',
    config.readyMs, 'authenticated-list-not-ready');

  progress('first-real-reply');
  let baseline = updateCount();
  await submit(firstPrompt);
  await reply(0, `READY ${runId}`, baseline);
  check(sockets.some(socket => socket.snapshots > 0), 'missing-authenticated-websocket-snapshot');
  const chatId = await page.locator('.chat-item.selected').getAttribute('data-chat-id');
  check(/^[0-9a-f-]{36}$/i.test(chatId ?? ''), 'missing-selected-chat');
  const selectHistory = () => page.getByRole('navigation', { name: '历史会话' })
    .locator(`button[data-chat-id="${chatId}"]`);

  async function resources(expectedPhase) {
    const observation = await page.evaluate(async ({ chatId, token }) => {
      const response = await fetch(`/api/chats/${chatId}/resources`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      return { status: response.status, cache: response.headers.get('Cache-Control'),
        body: response.ok ? await response.json() : null };
    }, { chatId, token: config.token });
    check(observation.status === 200 && observation.cache?.includes('no-store'), 'resources-query-failed');
    const instance = observation.body?.instance;
    check(instance?.phase === expectedPhase && instance.memory?.allocatedBytes > 0 &&
      instance.memory.pages * 65536 === instance.memory.allocatedBytes && instance.generationId,
    'invalid-wasm-resource-observation');
    check(instance.cpu.supported === false && instance.cpu.timeMs === null &&
      instance.cpu.utilizationPercent === null, 'fabricated-instance-cpu');
    check(instance.memory.observation === (expectedPhase === 'closed' ? 'last-observed' : 'live'),
      'incorrect-memory-lifecycle');
    return instance.instanceId;
  }

  const firstInstanceId = await resources('closed');

  progress('second-real-reply');
  baseline = updateCount();
  await submit(secondPrompt);
  await reply(1, 'SECOND', baseline);
  check(await resources('closed') !== firstInstanceId, 'wasm-instance-identity-reused');
  check((await assistants.nth(1).locator('.markdown').innerText()).includes(runId), 'second-turn-context-lost');
  const completed = await assistants.locator('.markdown').allInnerTexts();

  progress('refresh-and-history');
  const socketBaseline = sockets.length;
  await page.reload();
  await selectHistory().click();
  await history(completed, 2);
  check(sockets.slice(socketBaseline).some(socket => socket.snapshots > 0), 'missing-refresh-websocket-snapshot');
  await page.getByRole('button', { name: /开启新对话/ }).click();
  await waitFor(async () => await assistants.count() === 0, config.readyMs, 'new-chat-not-empty');
  await idle();
  await selectHistory().click();
  await history(completed, 2);

  progress('stop-real-stream');
  baseline = updateCount();
  await submit('Explicit live E2E cancellation test. Do not use tools. Write 500 numbered paragraphs of a detailed original story, at least 60 words per paragraph. Start the story immediately and do not summarize.');
  await waitFor(async () => await assistants.count() === 3 && await stop.isEnabled() &&
    (await assistants.nth(2).locator('.markdown').allTextContents()).join('').trim().length > 0,
  config.replyMs, 'long-reply-not-streaming');
  await resources('ready');
  check(updateCount() > baseline, 'missing-stream-update-before-stop');
  await stop.click();
  await waitFor(async () => await assistants.nth(2).getByText('已停止生成', { exact: true }).count() === 1,
    config.stopMs, 'cancellation-not-confirmed');
  await idle(config.stopMs);
  await resources('closed');
  const partial = await assistants.nth(2).locator('.markdown').innerText();
  check(partial.trim().length > 0, 'cancelled-content-not-preserved');

  progress('continue-after-stop');
  baseline = updateCount();
  await submit('Continue this explicit E2E test after stopping. Do not resume the story or use tools. Reply with only: CONTINUED');
  await reply(3, 'CONTINUED', baseline);
  check(await assistants.nth(2).locator('.markdown').innerText() === partial, 'stopped-reply-changed');
  const finalHistory = await assistants.locator('.markdown').allInnerTexts();
  await page.reload();
  await selectHistory().click();
  await history(finalHistory, 4);
  check(await assistants.nth(2).getByText('已停止生成', { exact: true }).count() === 1, 'cancel-state-not-persisted');

  progress('mobile-overflow');
  await page.setViewportSize({ width: 390, height: 844 });
  async function noOverflow() {
    check(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1 &&
      document.body.scrollWidth <= window.innerWidth + 1), 'mobile-horizontal-overflow');
  }
  await noOverflow();
  await page.getByRole('button', { name: '打开会话列表', exact: true }).click();
  await page.getByRole('button', { name: '关闭会话列表', exact: true }).waitFor({ state: 'visible' });
  await noOverflow();
  await selectHistory().click();
  await page.getByRole('button', { name: '关闭会话列表', exact: true }).waitFor({ state: 'hidden' });
  await history(finalHistory, 4);
  await input.fill('Mobile composer layout check; not sent.');
  await noOverflow();
  await input.fill('');
  await delay(1_000);
  await healthy();
  check(await page.getByRole('alert').count() === 0 && await page.locator('.has-error').count() === 0,
    'visible-application-error');
  console.log('PASS: UI login, two live turns, WebSocket sync, refresh/history, stop/continue, mobile layout; no HTTP 5xx or pageerrors.');
} catch {
  console.error(`FAIL: ${stage} [${failureCode}]. Raw browser errors and payloads are suppressed to protect credentials.`);
  process.exitCode = 1;
} finally {
  if (browser) {
    try { await browser.close(); } catch {
      console.error('FAIL: browser cleanup failed (details suppressed).');
      process.exitCode = 1;
    }
  }
}
