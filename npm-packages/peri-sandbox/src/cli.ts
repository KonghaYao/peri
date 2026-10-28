#!/usr/bin/env node
import { join, resolve } from 'node:path';
import { resolveSandboxId } from './instance/options.js';
import { defaultSettingsPath, initSettings, loadSettings } from './instance/settings.js';
import { findStaleLocks } from './instance/workspace.js';
import { startSandbox } from './instance/start.js';
import { providerFor } from './runtime/provider.js';

function usage(): never {
  throw new Error(
    [
      'Usage:',
      '  peri-sandbox init   [--settings /absolute/settings.json]',
      '  peri-sandbox doctor [--settings /absolute/settings.json]',
      '  peri-sandbox run    [--settings /absolute/settings.json] [--sandbox-id sbx_...] [--session-key key] [--port n] [--profile name]',
    ].join('\n'),
  );
}

async function main(): Promise<void> {
  const [command, ...args] = process.argv.slice(2);
  const option = (key: string): string | undefined => {
    const index = args.indexOf(key);
    return index < 0 ? undefined : args[index + 1];
  };
  const settingsOption = option('--settings');
  if (command === 'init') {
    console.log(await initSettings(settingsOption));
    return;
  }

  const settingsPath = settingsOption === undefined ? undefined : resolve(settingsOption);
  if (command === 'doctor') {
    const settings = loadSettings(settingsPath);
    const provider = providerFor(process.platform, {
      unitPrefix: settings.unitPrefix,
      stopTimeoutMs: settings.stopTimeoutMs,
    });
    await provider.doctor({
      sandboxesRoot: settings.sandboxesRoot,
      periPath: settings.periPath,
      periSha256: settings.periSha256,
      unitPrefix: settings.unitPrefix ?? 'peri-sbx-',
      profiles: settings.profiles,
      startupTimeoutMs: settings.startupTimeoutMs ?? 45_000,
    });
    console.log(
      JSON.stringify(
        {
          supported: true,
          platform: process.platform,
          settings: settingsPath ?? defaultSettingsPath(),
          periPath: settings.periPath,
          residualUnits: provider.findResidualUnits ? await provider.findResidualUnits() : [],
          staleLocks: findStaleLocks(settings.sandboxesRoot),
        },
        null,
        2,
      ),
    );
    return;
  }

  if (command === 'run') {
    const settings = loadSettings(settingsPath);
    const sandboxId = resolveSandboxId({ sandboxId: option('--sandbox-id'), sessionKey: option('--session-key') });
    const portOption = option('--port');
    const port = portOption === undefined ? undefined : Number(portOption);
    const instance = await startSandbox({
      settingsPath,
      sandboxId,
      tokenFile: join(settings.sandboxesRoot, sandboxId, 'token'),
      ...(port === undefined ? {} : { port }),
      ...(option('--profile') === undefined ? {} : { profileName: option('--profile') }),
    });
    console.log(
      JSON.stringify(
        {
          id: instance.id,
          mcpUrl: instance.mcpUrl,
          workspacePath: instance.workspacePath,
          tokenFile: instance.tokenFile ?? join(settings.sandboxesRoot, sandboxId, 'token'),
        },
        null,
        2,
      ),
    );
    await new Promise<void>((resolveSignal) => {
      const shutdown = () => resolveSignal();
      process.once('SIGTERM', shutdown);
      process.once('SIGINT', shutdown);
    });
    await instance.close();
    return;
  }

  if (command === undefined) usage();
  usage();
}

void main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
