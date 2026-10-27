import { readFileSync, realpathSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SandboxError } from '../instance/types.js';

export function srtRuntime(): { cli: string; packageRoot: string } {
  const entry = realpathSync(fileURLToPath(import.meta.resolve('@anthropic-ai/sandbox-runtime')));
  const packageRoot = dirname(dirname(entry));
  const version = JSON.parse(readFileSync(join(packageRoot, 'package.json'), 'utf8')).version;
  const cli = join(packageRoot, 'dist', 'cli.js');
  if (version !== '0.0.81' || !statSync(cli).isFile()) {
    throw new SandboxError('unsupported_environment', 'npm sandbox-runtime 0.0.81 CLI unavailable');
  }
  return { cli, packageRoot };
}
