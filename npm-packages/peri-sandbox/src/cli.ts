#!/usr/bin/env node
import { resolve, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { loadConfig } from './config.js';
import { connectSandboxControl } from './control/client.js';
import { SystemdProvider } from './runtime/provider.js';
import { MacProvider } from './runtime/mac-provider.js';
import { initMacConfig, macConfigPath } from './runtime/mac-config.js';
import { handleShutdown, serve } from './server/service.js';
import { startPeriSandbox } from './start.js';

async function main(): Promise<void> {
  const [command, ...args] = process.argv.slice(2);
  const option = (key: string) => {
    const index = args.indexOf(key);
    return index < 0 ? undefined : args[index + 1];
  };
  if (command === 'init') {
    console.log(await initMacConfig(option('--config') ? resolve(option('--config')!) : undefined));
    return;
  }
  if (command === 'up') {
    const session = await startPeriSandbox({ configPath: option('--config') });
    handleShutdown(session);
    console.log(JSON.stringify({ mcpUrl: session.mcpUrl, tokenFile: session.tokenFile }, null, 2));
    return;
  }
  const file = resolve(
    option('--config') ??
      process.env.PERI_SANDBOX_CONFIG ??
      (process.platform === 'darwin' ? macConfigPath() : 'config.json'),
  );
  const config = loadConfig(file);
  if (command === 'doctor') {
    await (process.platform === 'darwin' ? new MacProvider(config) : new SystemdProvider(config)).doctor();
    console.log('Environment supported');
    return;
  }
  if (command === 'serve') {
    await serve(config);
    return;
  }
  const control = connectSandboxControl({ socketPath: join(config.controlRoot, 'control.sock') });
  const requestId = option('--request-id') ?? randomUUID();
  let result: unknown;
  switch (command) {
    case 'create':
      result = await control.create(
        { name: option('--name'), profile: option('--profile') ?? 'workspace-default' },
        { requestId },
      );
      break;
    case 'start':
      result = await control.start(args[0], { requestId });
      break;
    case 'stop':
      result = await control.stop(args[0], { requestId });
      break;
    case 'destroy':
      if (!args.includes('--delete-data')) throw new Error('destroy requires --delete-data');
      result = await control.destroy(args[0], { requestId, deleteData: true });
      break;
    case 'inspect':
      result = await control.inspect(args[0]);
      break;
    case 'list':
      result = await control.list();
      break;
    default:
      throw new Error(
        'Usage: peri-sandbox [doctor|serve|create|start|stop|destroy|inspect|list] --config /absolute/config.json',
      );
  }
  console.log(JSON.stringify({ requestId, result }, null, 2));
}
main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
