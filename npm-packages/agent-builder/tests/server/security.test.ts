import { describe, expect, test } from 'bun:test';
import { Hono } from 'hono';
import { createOriginGuard } from '../../server/security';

const allowed = ['http://127.0.0.1:3000', 'http://localhost:3000'];

function app() {
  const instance = new Hono();
  instance.use('*', createOriginGuard(allowed));
  instance.get('/', (c) => c.text('ok'));
  instance.post('/', (c) => c.text('ok'));
  return instance;
}

describe('origin guard', () => {
  test('allows the same loopback origin', async () => {
    const response = await app().request('/', {
      headers: { host: '127.0.0.1:3000', origin: 'http://127.0.0.1:3000' },
    });
    expect(response.status).toBe(200);
  });

  test('rejects a cross-site browser origin even on POST', async () => {
    const response = await app().request('/', {
      method: 'POST',
      headers: { host: '127.0.0.1:3000', origin: 'https://evil.example' },
    });
    expect(response.status).toBe(403);
    expect(await response.json()).toMatchObject({ error: { code: 'invalid_identity' } });
  });

  test('rejects a non-loopback Host header (DNS rebinding)', async () => {
    const response = await app().request('/', { headers: { host: 'evil.example:3000' } });
    expect(response.status).toBe(403);
  });

  test('rejects Sec-Fetch-Site: cross-site without an Origin header', async () => {
    const response = await app().request('/', {
      method: 'POST',
      headers: { host: 'localhost:3000', 'sec-fetch-site': 'cross-site' },
    });
    expect(response.status).toBe(403);
  });

  test('rejects a loopback hostname that is not in the explicit origin allowlist', async () => {
    const response = await app().request('/', {
      headers: { host: 'localhost:3000', origin: 'http://localhost:5173' },
    });
    expect(response.status).toBe(403);
  });

  test('allows a non-browser client without an Origin header', async () => {
    const response = await app().request('/', { headers: { host: 'localhost:3000' } });
    expect(response.status).toBe(200);
  });
});
