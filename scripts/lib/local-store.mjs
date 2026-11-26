// Real PostgreSQL + Neon SQL-over-HTTP proxy for the WASM smoke tests.
import { execFile, spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { setTimeout as sleep } from 'node:timers/promises';

const exec = promisify(execFile);
const composeFile = fileURLToPath(new URL('./postgres-fixture.compose.yml', import.meta.url));

export async function lookupEnv() {
  const external = process.env.PERI_TEST_STORAGE_URL;
  const endpoint = process.env.PERI_TEST_STORAGE_HTTP_ENDPOINT;
  if (external) return { url: external, httpEndpoint: endpoint, logs: async () => '', stop: async () => {} };
  if (endpoint) throw new Error('PERI_TEST_STORAGE_HTTP_ENDPOINT requires PERI_TEST_STORAGE_URL');

  const project = 'peri-wasm-' + randomUUID();
  const password = randomUUID();
  const env = { ...process.env, PERI_FIXTURE_PASSWORD: password };
  const args = ['compose', '-p', project, '-f', composeFile];
  const run = (...command) => exec('docker', [...args, ...command], { env, timeout: 180_000 });
  const cleanup = () => spawnSync('docker', [...args, 'down', '-v'], { env, stdio: 'ignore', timeout: 20_000 });
  const interrupt = () => process.exit(130);
  const terminate = () => process.exit(143);
  process.once('exit', cleanup);
  process.once('SIGINT', interrupt);
  process.once('SIGTERM', terminate);
  const stop = async () => {
    await run('down', '-v');
    process.off('exit', cleanup);
    process.off('SIGINT', interrupt);
    process.off('SIGTERM', terminate);
  };
  try {
    await run('up', '-d');
    const { stdout } = await run('port', 'proxy', '4444');
    const httpEndpoint = `http://${stdout.trim()}/sql`;
    // The test proxy's self-signed certificate identifies this upstream host.
    const url = `postgresql://postgres:${password}@db.localtest.me/main`;
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
      const response = await fetch(httpEndpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Neon-Connection-String': url },
        body: JSON.stringify({ query: 'SELECT 1', params: [] }),
        signal: AbortSignal.timeout(1500),
        redirect: 'error',
      }).catch(() => null);
      if (response?.ok) return {
        url, httpEndpoint, stop,
        logs: async () => (await run('logs', '--tail=30')).stdout.replaceAll(password, '[redacted]'),
      };
      await sleep(200);
    }
    throw new Error('PostgreSQL fixture did not answer SELECT 1 through the HTTP proxy');
  } catch (error) {
    await stop().catch(() => {});
    throw error;
  }
}
