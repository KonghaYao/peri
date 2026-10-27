import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, realpathSync, rmSync, statSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import type { Config } from '../config.js';
import { ControlError } from '../control/types.js';
import { srtRuntime } from './srt.js';

export function sandboxPath(config: Config, id: string): string {
  if (!/^sbx_[a-f0-9]{32}$/.test(id)) throw new ControlError('conflict', 'invalid sandbox ID');
  return join(config.sandboxesRoot, id);
}

export function macTmpPath(id: string): string {
  if (!/^sbx_[a-f0-9]{32}$/.test(id)) throw new ControlError('conflict', 'invalid sandbox ID');
  return join('/private/tmp', `peri-${id.slice(4, 20)}`);
}

export function removeMacTmp(id: string): void {
  const tmp = macTmpPath(id);
  const entry = lstatSync(tmp, { throwIfNoEntry: false });
  if (
    entry &&
    (entry.isSymbolicLink() || !entry.isDirectory() || entry.uid !== process.getuid?.() || entry.mode & 0o077)
  )
    throw new ControlError('stop_incomplete', `untrusted sandbox tmp: ${tmp}`);
  rmSync(tmp, { recursive: true, force: true });
}

export function checkDirectory(path: string, root: string): void {
  if (resolve(path) === resolve(root) || !resolve(path).startsWith(resolve(root) + sep))
    throw new ControlError('conflict', 'path outside managed root');
  let cursor = resolve(path);
  while (cursor !== resolve(root)) {
    if (existsSync(cursor) && lstatSync(cursor).isSymbolicLink())
      throw new ControlError('conflict', `symlink in managed path: ${cursor}`);
    cursor = resolve(cursor, '..');
  }
  if (existsSync(root) && lstatSync(root).isSymbolicLink()) throw new ControlError('conflict', 'symlink root');
}

export function makeDirectories(config: Config, id: string): void {
  const base = sandboxPath(config, id);
  checkDirectory(base, config.sandboxesRoot);
  mkdirSync(join(base, 'workspace'), { recursive: true, mode: 0o700 });
  mkdirSync(join(base, 'home'), { recursive: true, mode: 0o700 });
  mkdirSync(join(base, 'home', '.config'), { recursive: true, mode: 0o700 });
  if (process.platform === 'darwin') {
    const tmp = macTmpPath(id);
    if (
      existsSync(tmp) &&
      (lstatSync(tmp).isSymbolicLink() ||
        !statSync(tmp).isDirectory() ||
        statSync(tmp).uid !== process.getuid?.() ||
        statSync(tmp).mode & 0o077)
    )
      throw new ControlError('conflict', `untrusted sandbox tmp: ${tmp}`);
    mkdirSync(join(tmp, 'claude'), { recursive: true, mode: 0o700 });
  }
}

export function policyFor(config: Config, id: string, name: string): { json: string; hash: string } {
  const selected = config.profiles[name];
  if (!selected) throw new ControlError('conflict', `unknown profile: ${name}`);
  const base = sandboxPath(config, id);
  const reads = [...selected.runtimeReadPaths, srtRuntime().packageRoot].flatMap((path) => {
    if (!existsSync(path)) throw new ControlError('unsupported_environment', `runtime path missing: ${path}`);
    const target = realpathSync(path);
    for (const protectedRoot of [config.controlRoot, config.sandboxesRoot]) {
      const root = existsSync(protectedRoot) ? realpathSync(protectedRoot) : resolve(protectedRoot);
      if (
        [path, target].some(
          (candidate) =>
            candidate === root ||
            !relative(candidate, root).startsWith('..') ||
            !relative(root, candidate).startsWith('..'),
        )
      )
        throw new ControlError('unsupported_environment', `runtime path overlaps managed state: ${path}`);
    }
    return path === target ? [path] : [path, target];
  });
  const json = JSON.stringify({
    filesystem: {
      denyRead: ['/'],
      allowRead: [
        ...reads,
        join(base, 'workspace'),
        join(base, 'home'),
        process.platform === 'darwin' ? macTmpPath(id) : '/tmp',
      ],
      allowWrite: [
        join(base, 'workspace'),
        join(base, 'home'),
        process.platform === 'darwin' ? macTmpPath(id) : '/tmp/claude',
      ],
      denyWrite: [],
    },
    network: {
      allowedDomains: selected.allowedDomains,
      deniedDomains: [],
      strictAllowlist: true,
      allowAllUnixSockets: false,
    },
  });
  return { json, hash: createHash('sha256').update(json).digest('hex') };
}
