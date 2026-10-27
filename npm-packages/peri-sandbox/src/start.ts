import { randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { loadConfig } from './config.js';
import { connectSandboxControl } from './control/client.js';
import { ControlError } from './control/types.js';
import { initMacConfig, macConfigPath } from './runtime/mac-config.js';
import { startServer } from './server/service.js';

export interface PeriSandboxSession {
  readonly id: string;
  readonly mcpUrl: string;
  readonly tokenFile: string;
  readonly configPath: string;
  close(): Promise<void>;
}

export async function startPeriSandbox(options: { configPath?: string } = {}): Promise<PeriSandboxSession> {
  if (process.platform !== 'darwin')
    throw new ControlError('unsupported_environment', 'automatic startup requires macOS');
  const configPath = resolve(options.configPath ?? process.env.PERI_SANDBOX_CONFIG ?? macConfigPath());
  if (!existsSync(configPath)) await initMacConfig(configPath);
  const config = loadConfig(configPath);
  const server = await startServer(config);
  try {
    const control = connectSandboxControl({ socketPath: join(config.controlRoot, 'control.sock') });
    const previous = (await control.list()).find(
      (sandbox) => sandbox.name === 'default' && sandbox.state !== 'deleted',
    );
    const sandbox =
      previous ??
      (await control.create({ name: 'default', profile: 'workspace-default' }, { requestId: randomUUID() }));
    const running = await control.start(sandbox.id, { requestId: randomUUID() });
    return {
      id: running.id,
      mcpUrl: running.mcpUrl,
      tokenFile: config.auth.tokenFile,
      configPath,
      close: server.close,
    };
  } catch (error) {
    try {
      await server.close();
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], 'startup and cleanup both failed');
    }
    throw error;
  }
}
