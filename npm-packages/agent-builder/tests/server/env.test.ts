import { afterEach, describe, expect, test } from 'bun:test';
import { mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { probeChatRuntime } from '../../server/chats/engine';
import { loadEnv } from '../../server/env';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});
function source(extra: Record<string, string> = {}) {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-env-isolated-')));
  roots.push(root);
  return {
    DATABASE_PATH: path.join(root, 'config.db'),
    PERI_WORKSPACE_PATH: root,
    PERI_SANDBOX_PERI_PATH: path.join(root, 'no-peri'),
    ...extra,
  };
}

describe('loadEnv', () => {
  test('treats blank optional secrets as missing instead of invalid configuration', () => {
    const input = source({ PERI_STORE_TOKEN: '', ANTHROPIC_API_KEY: '   ', OPENAI_API_KEY: '\t' });
    expect(() => loadEnv(input)).not.toThrow();
    const env = loadEnv(input);
    expect(env.storeToken).toBeUndefined();
    expect(env).not.toHaveProperty('anthropicApiKey');
    expect(env).not.toHaveProperty('openaiApiKey');
    const status = probeChatRuntime(env);
    expect(status.mode).toBe('not_configured');
    expect(status.reasons).toContain('store_token_missing');
    expect(status.reasons).not.toContain('provider_key_missing');
  });
  test('trims infrastructure secrets and ignores Provider environment keys without reading them', () => {
    const input = source({ PERI_STORE_TOKEN: '  store-token  ' });
    for (const name of ['ANTHROPIC_API_KEY', 'OPENAI_API_KEY']) {
      Object.defineProperty(input, name, {
        enumerable: true,
        get() {
          throw new Error('Provider environment key must not be read');
        },
      });
    }
    const env = loadEnv(input);
    expect(env.storeToken).toBe('store-token');
    expect(env).not.toHaveProperty('anthropicApiKey');
    expect(env).not.toHaveProperty('openaiApiKey');
  });
  test('treats a blank engine data directory as missing', () => {
    expect(loadEnv(source({ RIVETKIT_STORAGE_PATH: '   ' })).rivetStoragePath).toBeUndefined();
  });
});
