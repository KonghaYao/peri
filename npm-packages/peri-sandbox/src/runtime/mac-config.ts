import { createHash, randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { configSchema } from '../config.js';
import { ControlError } from '../control/types.js';

export function macConfigPath(): string {
  return join(homedir(), '.peri-sandbox', 'control', 'config.json');
}

export async function initMacConfig(file = macConfigPath()): Promise<string> {
  if (process.platform !== 'darwin')
    throw new ControlError('unsupported_environment', 'automatic setup requires macOS');
  if (existsSync(file)) throw new ControlError('conflict', `config already exists: ${file}`);
  const periPath = realpathSync(process.env.PERI_PATH ?? join(homedir(), '.peri', 'peri'));
  const root = dirname(dirname(file));
  mkdirSync(dirname(file), { recursive: true, mode: 0o700 });
  mkdirSync(join(root, 'sandboxes'), { recursive: true, mode: 0o700 });
  const controlRoot = realpathSync(dirname(file));
  const sandboxesRoot = realpathSync(join(root, 'sandboxes'));
  const tokenFile = join(controlRoot, 'token');
  if (existsSync(tokenFile)) throw new ControlError('conflict', `token already exists: ${tokenFile}`);
  const port = await new Promise<number>((resolvePort, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      if (!address || typeof address === 'string') {
        server.close();
        reject(new Error('no local port'));
        return;
      }
      server.close(() => resolvePort(address.port));
    });
  });
  const config = configSchema.parse({
    controlRoot,
    sandboxesRoot,
    listen: { host: '127.0.0.1', port },
    publicBaseUrl: `http://127.0.0.1:${port}`,
    auth: { tokenFile },
    http: { allowedHosts: [`127.0.0.1:${port}`], allowedOrigins: [] },
    runtime: { periPath, periSha256: createHash('sha256').update(readFileSync(periPath)).digest('hex') },
    provider: { apiUnit: 'peri-sandbox-api.service', unitPrefix: 'peri-sbx-' },
    limits: { maxSandboxes: 20, startupMs: 30000, stopMs: 10000, bashCalls: 10000 },
    profiles: {
      'workspace-default': {
        allowedDomains: ['github.com', 'api.github.com', 'registry.npmjs.org', 'api.anthropic.com', 'api.openai.com'],
        runtimeReadPaths: [
          '/usr',
          '/bin',
          '/sbin',
          '/opt/homebrew',
          '/usr/local',
          '/System',
          '/Library',
          '/private/etc',
          periPath,
        ]
          .filter(existsSync)
          .map((path) => resolve(path)),
        memoryMax: '1G',
        tasksMax: 128,
        cpuQuota: '100%',
      },
    },
  });
  writeFileSync(tokenFile, randomBytes(32).toString('hex'), { flag: 'wx', mode: 0o600 });
  writeFileSync(file, JSON.stringify(config, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  return file;
}
