import { createHash } from 'node:crypto';
import { existsSync, lstatSync, realpathSync, rmSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import type { SandboxProfile } from '../instance/types.js';
import { SandboxError } from '../instance/types.js';
import { srtRuntime } from './srt.js';

const sandboxIdPattern = /^sbx_[a-f0-9]{32}$/;

export function sandboxPath(sandboxesRoot: string, id: string): string {
  if (!sandboxIdPattern.test(id)) throw new SandboxError('conflict', 'invalid sandbox ID');
  return join(sandboxesRoot, id);
}

export function macTmpPath(id: string): string {
  if (!sandboxIdPattern.test(id)) throw new SandboxError('conflict', 'invalid sandbox ID');
  return join('/private/tmp', `peri-${id.slice(4, 20)}`);
}

export function removeMacTmp(id: string): void {
  if (process.platform !== 'darwin') return;
  const tmp = macTmpPath(id);
  const entry = lstatSync(tmp, { throwIfNoEntry: false });
  if (!entry) return;
  if (entry.isSymbolicLink() || !entry.isDirectory() || entry.uid !== process.getuid?.() || entry.mode & 0o077)
    throw new SandboxError('stop_incomplete', `untrusted sandbox tmp: ${tmp}`);
  rmSync(tmp, { recursive: true, force: true });
}

/** Reject symlinks and paths outside the managed root before creating or deleting. */
export function checkDirectory(path: string, root: string): void {
  if (resolve(path) === resolve(root) || !resolve(path).startsWith(resolve(root) + sep))
    throw new SandboxError('conflict', 'path outside managed root');
  let cursor = resolve(path);
  while (cursor !== resolve(root)) {
    if (existsSync(cursor) && lstatSync(cursor).isSymbolicLink())
      throw new SandboxError('conflict', `symlink in managed path: ${cursor}`);
    cursor = resolve(cursor, '..');
  }
  if (existsSync(root) && lstatSync(root).isSymbolicLink()) throw new SandboxError('conflict', 'symlink root');
}

function overlapsManagedRoot(candidate: string, root: string): boolean {
  const left = resolve(candidate),
    right = resolve(root);
  return left === right || !relative(left, right).startsWith('..') || !relative(right, left).startsWith('..');
}

/**
 * Build the SRT policy for one sandbox.
 *
 * `allowRead` is limited to runtime paths plus workspace/home/tmp; the
 * sandbox root itself (policy, token, lock, diagnostics) stays unreadable.
 */
export function policyFor(options: {
  sandboxesRoot: string;
  sandboxId: string;
  profile: SandboxProfile;
  periPath: string;
}): { json: string; hash: string } {
  const base = sandboxPath(options.sandboxesRoot, options.sandboxId);
  const root = existsSync(options.sandboxesRoot) ? realpathSync(options.sandboxesRoot) : resolve(options.sandboxesRoot);
  const reads: string[] = [];
  for (const path of [...options.profile.runtimeReadPaths, srtRuntime().packageRoot, options.periPath]) {
    if (!existsSync(path)) throw new SandboxError('unsupported_environment', `runtime path missing: ${path}`);
    const target = realpathSync(path);
    for (const candidate of [path, target]) {
      if (overlapsManagedRoot(candidate, root))
        throw new SandboxError('unsupported_environment', `runtime path overlaps managed state: ${path}`);
      if (!reads.includes(candidate)) reads.push(candidate);
    }
  }
  const json = JSON.stringify({
    filesystem: {
      denyRead: ['/'],
      allowRead: [
        ...reads,
        join(base, 'workspace'),
        join(base, 'home'),
        process.platform === 'darwin' ? macTmpPath(options.sandboxId) : '/tmp',
      ],
      allowWrite: [
        join(base, 'workspace'),
        join(base, 'home'),
        process.platform === 'darwin' ? macTmpPath(options.sandboxId) : '/tmp/claude',
      ],
      denyWrite: [],
    },
    network: {
      allowedDomains: options.profile.allowedDomains,
      deniedDomains: [],
      strictAllowlist: true,
      allowAllUnixSockets: false,
    },
  });
  return { json, hash: createHash('sha256').update(json).digest('hex') };
}
