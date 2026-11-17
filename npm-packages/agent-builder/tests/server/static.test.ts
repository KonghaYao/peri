import { afterEach, describe, expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { Hono } from 'hono';
import { findWebRoot, mountStatic } from '../../server/static';

const roots: string[] = [];

function webRoot(): string {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'agent-builder-web-')));
  roots.push(root);
  mkdirSync(path.join(root, 'assets'));
  writeFileSync(path.join(root, 'index.html'), '<!doctype html><title>Peri</title>');
  writeFileSync(path.join(root, 'assets/app.js'), 'console.log("peri")');
  return root;
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function app(root: string) {
  const instance = new Hono();
  instance.get('/api/health', (c) => c.json({ ok: true }));
  mountStatic(instance, root, 'test-version');
  return instance;
}

describe('static SPA serving', () => {
  test('serves the shell at / and immutable hashed assets', async () => {
    const root = webRoot();
    const shell = await app(root).request('/', { headers: { accept: 'text/html' } });
    expect(shell.status).toBe(200);
    expect(await shell.text()).toContain('Peri');
    expect(shell.headers.get('cache-control')).toBe('no-store');

    const asset = await app(root).request('/assets/app.js');
    expect(asset.status).toBe(200);
    expect(await asset.text()).toContain('peri');
    expect(asset.headers.get('cache-control')).toContain('immutable');
  });

  test('falls back to the shell for client routes but not for missing assets', async () => {
    const root = webRoot();
    const route = await app(root).request('/chats/abc', { headers: { accept: 'text/html' } });
    expect(route.status).toBe(200);

    const directProvider = await app(root).request('/providers', { headers: { accept: 'text/html' } });
    expect(directProvider.status).toBe(200);
    const missing = await app(root).request('/assets/missing.js');
    expect(missing.status).toBe(404);
  });

  test('does not leak /api into the SPA or escape the web root', async () => {
    const root = webRoot();
    const api = await app(root).request('/api/missing');
    expect(api.status).toBe(404);
    expect(api.headers.get('content-type')).toContain('application/json');

    const traversal = await app(root).request('/..%2f..%2fetc%2fpasswd');
    expect(traversal.status).toBe(400);
  });

  test('findWebRoot only accepts a directory with index.html', () => {
    const root = webRoot();
    expect(findWebRoot(root)).toBe(root);
    expect(findWebRoot(path.join(root, 'assets'))).toBeNull();
  });
});
