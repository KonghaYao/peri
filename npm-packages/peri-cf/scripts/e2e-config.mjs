import { existsSync } from 'node:fs';
import { readFile, readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const packageRoot = fileURLToPath(new URL('../', import.meta.url));

function duration(name, fallback) {
  const value = Number(process.env[name] ?? fallback);
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error('Invalid E2E duration configuration');
  return value;
}

export async function configuration() {
  const baseUrl = new URL(process.env.BASE_URL ?? 'http://127.0.0.1:8791');
  if (!['http:', 'https:'].includes(baseUrl.protocol) || baseUrl.username || baseUrl.password ||
      baseUrl.search || baseUrl.hash || baseUrl.pathname !== '/') {
    throw new Error('BASE_URL must be an HTTP origin without credentials');
  }
  const vars = await readFile(resolve(packageRoot, process.env.E2E_DEV_VARS ?? '.dev.vars'), 'utf8');
  const entries = [...vars.matchAll(/^\s*(?:export\s+)?APP_AUTH_TOKEN\s*=\s*(.*?)\s*$/gm)];
  if (entries.length !== 1) throw new Error('Expected one APP_AUTH_TOKEN in server .dev.vars');
  const raw = entries[0][1];
  const quoted = raw.match(/^(["'])(.*?)\1\s*(?:#.*)?$/);
  const token = quoted ? quoted[2] : raw.replace(/\s+#.*$/, '').trim();
  if (!token || /[\s\u0000-\u001f\u007f"']/.test(token)) throw new Error('Invalid APP_AUTH_TOKEN');
  return {
    baseUrl: baseUrl.origin,
    token,
    actionMs: duration('E2E_ACTION_TIMEOUT_MS', 30_000),
    readyMs: duration('E2E_READY_TIMEOUT_MS', 180_000),
    replyMs: duration('E2E_REPLY_TIMEOUT_MS', 300_000),
    stopMs: duration('E2E_STOP_TIMEOUT_MS', 120_000),
    headed: process.env.E2E_HEADED === '1',
    slowMo: duration('E2E_SLOW_MO_MS', 50),
  };
}

export async function loadPlaywright() {
  process.env.DEBUG = '';
  delete process.env.PWDEBUG;
  const require = createRequire(import.meta.url);
  const candidates = [];
  if (process.env.E2E_PLAYWRIGHT_MODULE) {
    candidates.push(resolve(process.env.E2E_PLAYWRIGHT_MODULE));
  } else {
    for (const name of ['playwright', '@playwright/test']) {
      try { candidates.push(require.resolve(name)); } catch {}
    }
    const cacheRoot = join(process.env.npm_config_cache ?? join(homedir(), '.npm'), '_npx');
    const entries = await readdir(cacheRoot).catch(() => []);
    for (const entry of entries.sort()) {
      const modulePath = join(cacheRoot, entry, 'node_modules/playwright/index.mjs');
      if (existsSync(modulePath)) candidates.push(modulePath);
    }
  }
  for (const candidate of candidates) {
    try {
      const module = await import(pathToFileURL(candidate).href);
      if (module.chromium && existsSync(module.chromium.executablePath())) return module;
    } catch {}
  }
  throw new Error('Playwright with an installed Chromium browser is required');
}
