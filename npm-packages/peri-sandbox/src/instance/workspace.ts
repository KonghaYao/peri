import { randomBytes } from 'node:crypto';
import {
  closeSync,
  ftruncateSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  rmSync,
  statfsSync,
  statSync,
  unlinkSync,
  writeSync,
} from 'node:fs';
import { join, resolve } from 'node:path';
import { assertSandboxId, verifyToken } from './options.js';
import { loadSettings } from './settings.js';
import { checkDirectory, macTmpPath, removeMacTmp, sandboxPath } from '../runtime/policy.js';
import { SandboxError } from './types.js';

export function ensureSandboxesRoot(root: string): void {
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const stats = lstatSync(root);
  if (stats.isSymbolicLink() || !statSync(root).isDirectory())
    throw new SandboxError('conflict', `sandboxesRoot must be a real directory: ${root}`);
  if (statSync(root).mode & 0o077) throw new SandboxError('conflict', `sandboxesRoot must be private (0700): ${root}`);
}

export function checkFreeSpace(root: string, minFreeBytes: number): void {
  if (minFreeBytes <= 0) return;
  let stats: ReturnType<typeof statfsSync>;
  try {
    stats = statfsSync(root);
  } catch (error) {
    throw new SandboxError('start_failed', `Cannot inspect free space: ${root}`, { cause: error });
  }
  const freeBytes = Number(stats.bavail) * Number(stats.bsize);
  if (!Number.isFinite(freeBytes) || freeBytes < minFreeBytes)
    throw new SandboxError(
      'start_failed',
      `Insufficient free space under ${root}: ${freeBytes} bytes available, ${minFreeBytes} required.`,
    );
}

/** Create `<sandboxesRoot>/<id>/{workspace,home}` (0700) and the macOS tmp dir. */
export function makeDirectories(sandboxesRoot: string, sandboxId: string): void {
  const base = sandboxPath(sandboxesRoot, sandboxId);
  checkDirectory(base, sandboxesRoot);
  mkdirSync(join(base, 'workspace'), { recursive: true, mode: 0o700 });
  mkdirSync(join(base, 'home'), { recursive: true, mode: 0o700 });
  mkdirSync(join(base, 'home', '.config'), { recursive: true, mode: 0o700 });
  // 空的 git 模板目录：SRT 默认保护 .git/hooks，git init/clone 复制样本
  // hooks 会被拒绝；模板为空即可跳过复制。
  mkdirSync(join(base, 'home', '.git-template'), { recursive: true, mode: 0o700 });
  if (process.platform === 'darwin') {
    const tmp = macTmpPath(sandboxId);
    const existing = lstatSync(tmp, { throwIfNoEntry: false });
    if (
      existing &&
      (existing.isSymbolicLink() ||
        !statSync(tmp).isDirectory() ||
        statSync(tmp).uid !== process.getuid?.() ||
        statSync(tmp).mode & 0o077)
    )
      throw new SandboxError('conflict', `untrusted sandbox tmp: ${tmp}`);
    mkdirSync(join(tmp, 'claude'), { recursive: true, mode: 0o700 });
  }
}

export interface SandboxLock {
  readonly path: string;
  /** Record the backend pid/start time once the process was spawned. */
  update(backendPid: number): void;
  release(): void;
}

interface LockRecord {
  ownerPid: number;
  backendPid?: number;
}

function processAlive(pid: number, description: string): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code === 'ESRCH') return false;
    if (code === 'EPERM') return true;
    throw new SandboxError('conflict', `Cannot verify ${description} process ${pid}.`, { cause: error });
  }
}

function writeRecord(descriptor: number, record: LockRecord): void {
  const text = `${JSON.stringify(record)}\n`;
  ftruncateSync(descriptor, 0);
  writeSync(descriptor, text, 0, 'utf8');
}

function readRecord(path: string): LockRecord {
  const entry = lstatSync(path, { throwIfNoEntry: false });
  if (!entry || entry.isSymbolicLink() || !entry.isFile())
    throw new SandboxError('conflict', `Sandbox lock is not a regular file: ${path}`);
  let record: Partial<LockRecord>;
  try {
    record = JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    throw new SandboxError('conflict', `Sandbox lock is unreadable: ${path}`, { cause: error });
  }
  if (!Number.isSafeInteger(record.ownerPid) || (record.ownerPid as number) < 2)
    throw new SandboxError('conflict', `Sandbox lock has no valid owner: ${path}`);
  return record as LockRecord;
}

/**
 * Take `<sandboxRoot>/.lock` with O_EXCL.
 *
 * A stale lock is removed only when both recorded pids are confirmed dead;
 * live or unverifiable owners keep the lock and surface `conflict`.
 */
export function acquireLock(sandboxRoot: string): SandboxLock {
  const path = join(sandboxRoot, '.lock');
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const descriptor = openSync(path, 'wx', 0o600);
      writeRecord(descriptor, { ownerPid: process.pid });
      return {
        path,
        update(backendPid: number) {
          if (!Number.isSafeInteger(backendPid) || backendPid < 2)
            throw new SandboxError('conflict', 'Invalid backend pid.');
          writeRecord(descriptor, { ownerPid: process.pid, backendPid });
        },
        release() {
          try {
            closeSync(descriptor);
          } finally {
            try {
              unlinkSync(path);
            } catch (error) {
              if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
            }
          }
        },
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
      const record = readRecord(path);
      const pids = [record.ownerPid, record.backendPid].filter(
        (pid): pid is number => typeof pid === 'number' && Number.isSafeInteger(pid),
      );
      const alive = pids.filter((pid) => processAlive(pid, 'sandbox lock owner'));
      if (alive.length > 0)
        throw new SandboxError('conflict', `Sandbox is already in use (pids: ${alive.join(', ')}).`);
      try {
        unlinkSync(path);
      } catch (unlinkError) {
        if ((unlinkError as NodeJS.ErrnoException).code !== 'ENOENT') throw unlinkError;
      }
    }
  }
  throw new SandboxError('conflict', 'Could not acquire the sandbox lock.');
}

function readTokenFile(path: string): string {
  const entry = lstatSync(path, { throwIfNoEntry: false });
  if (!entry || entry.isSymbolicLink() || !entry.isFile())
    throw new SandboxError('token_unreadable', `Sandbox token is not a regular file: ${path}`);
  if (entry.mode & 0o077) throw new SandboxError('token_unreadable', `Sandbox token must be private (0600): ${path}`);
  let token: string;
  try {
    token = readFileSync(path, 'utf8').trim();
  } catch (error) {
    throw new SandboxError('token_unreadable', `Cannot read sandbox token: ${path}`, { cause: error });
  }
  if (!verifyToken(token)) throw new SandboxError('token_unreadable', `Sandbox token is invalid: ${path}`);
  return token;
}

/**
 * Resolve the per-instance token.
 *
 * Without `token`/`tokenFile` a 32-byte random token stays in memory only.
 * `tokenFile` must live inside `<sandboxRoot>` and is created with O_EXCL.
 */
export function resolveToken(options: { sandboxRoot: string; token?: string; tokenFile?: string }): {
  token: string;
  tokenFile: string | null;
} {
  if (options.token !== undefined) {
    if (!verifyToken(options.token))
      throw new SandboxError('invalid_options', 'token must be 16-512 printable characters without whitespace.');
    return { token: options.token, tokenFile: null };
  }
  if (options.tokenFile !== undefined) {
    const path = resolve(options.tokenFile);
    checkDirectory(path, options.sandboxRoot);
    const existing = lstatSync(path, { throwIfNoEntry: false });
    if (existing) return { token: readTokenFile(path), tokenFile: path };
    const token = randomBytes(32).toString('hex');
    let descriptor: number;
    try {
      descriptor = openSync(path, 'wx', 0o600);
    } catch (error) {
      throw new SandboxError('token_unreadable', `Cannot create sandbox token: ${path}`, { cause: error });
    }
    try {
      writeSync(descriptor, token);
    } finally {
      closeSync(descriptor);
    }
    return { token, tokenFile: path };
  }
  return { token: randomBytes(32).toString('hex'), tokenFile: null };
}

export function findStaleLocks(sandboxesRoot: string): string[] {
  const entry = lstatSync(sandboxesRoot, { throwIfNoEntry: false });
  if (!entry || entry.isSymbolicLink() || !entry.isDirectory()) return [];
  const stale: string[] = [];
  for (const name of readdirSync(sandboxesRoot)) {
    const base = join(sandboxesRoot, name);
    const stats = lstatSync(base, { throwIfNoEntry: false });
    if (!stats || stats.isSymbolicLink() || !stats.isDirectory()) continue;
    const lock = join(base, '.lock');
    if (!lstatSync(lock, { throwIfNoEntry: false })) continue;
    try {
      const record = readRecord(lock);
      const alive = [record.ownerPid, record.backendPid].some(
        (pid) => typeof pid === 'number' && Number.isSafeInteger(pid) && processAlive(pid, 'sandbox lock owner'),
      );
      if (!alive) stale.push(lock);
    } catch {
      stale.push(lock);
    }
  }
  return stale;
}

/**
 * Delete `<sandboxesRoot>/<sandboxId>` only after path/symlink validation.
 *
 * Missing data is a no-op. Failures are `delete_incomplete`.
 */
export async function deleteSandboxData(options: {
  sandboxId: string;
  sandboxesRoot?: string;
  settingsPath?: string;
}): Promise<void> {
  const sandboxId = assertSandboxId(options.sandboxId);
  const root = resolve(options.sandboxesRoot ?? loadSettings(options.settingsPath).sandboxesRoot);
  const rootEntry = lstatSync(root, { throwIfNoEntry: false });
  if (!rootEntry) return;
  if (rootEntry.isSymbolicLink() || !rootEntry.isDirectory())
    throw new SandboxError('conflict', `sandboxesRoot must be a real directory: ${root}`);
  const base = sandboxPath(root, sandboxId);
  checkDirectory(base, root);
  const entry = lstatSync(base, { throwIfNoEntry: false });
  if (!entry) {
    removeMacTmp(sandboxId);
    return;
  }
  if (entry.isSymbolicLink() || !entry.isDirectory())
    throw new SandboxError('conflict', `Sandbox data is not a real directory: ${base}`);
  try {
    rmSync(base, { recursive: true });
    removeMacTmp(sandboxId);
  } catch (error) {
    if (error instanceof SandboxError) throw error;
    throw new SandboxError('delete_incomplete', `Cannot delete sandbox data: ${base}`, { cause: error });
  }
}
