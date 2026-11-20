import { execFileSync } from 'node:child_process';
import { readFileSync, unlinkSync } from 'node:fs';
import path from 'node:path';

/**
 * The rivet engine is a native child process spawned by rivetkit. A JS `process.exit()` does not
 * reap it (verified: it keeps listening with ppid 1), so a shutdown that skips or times out the
 * registry teardown leaves an orphan. The wrapper records its PID before `exec` under the storage
 * path; the host may then terminate exactly that child. Nothing is matched by process name.
 */
const GRACEFUL_WAIT_MS = 1_500;
const KILL_WAIT_MS = 500;
const POLL_INTERVAL_MS = 50;

export type EngineTerminationStatus = 'none' | 'terminated' | 'forced' | 'blocked' | 'unreadable';

export interface EngineTermination {
  readonly status: EngineTerminationStatus;
  /** Pids that were signalled (terminated or forced), or that survived a forced stop. */
  readonly pids: readonly number[];
}

/** Per-host file name: concurrent hosts sharing a storage path never overwrite each other. */
export function enginePidFile(): string | null {
  const storage = process.env.RIVETKIT_STORAGE_PATH;
  if (storage === undefined || storage.length === 0) return null;
  return path.join(storage, '.rivetkit', 'var', 'engine', `host-${process.pid}.pid`);
}

type PidFileRead =
  | { readonly kind: 'ok'; readonly pids: readonly number[] }
  | { readonly kind: 'missing' | 'unreadable' };

function readPidFile(file: string): PidFileRead {
  let text: string;
  try {
    text = readFileSync(file, 'utf8');
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    return { kind: code === 'ENOENT' ? 'missing' : 'unreadable' };
  }
  const pids = new Set<number>();
  for (const token of text.split(/\s+/)) {
    if (!/^\d+$/.test(token)) continue;
    const pid = Number.parseInt(token, 10);
    if (Number.isSafeInteger(pid) && pid > 1) pids.add(pid);
  }
  return { kind: 'ok', pids: [...pids] };
}

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM';
  }
}

interface ProcessState {
  readonly parent: number;
  readonly state: string;
}

/** Null when the process is gone or its state cannot be read; never guessed. */
function processState(pid: number): ProcessState | null {
  if (process.platform === 'win32') return null;
  try {
    const output = execFileSync('ps', ['-o', 'ppid=,stat=', '-p', String(pid)], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    const [parent, state] = output.trim().split(/\s+/);
    const parsed = Number.parseInt(parent ?? '', 10);
    if (!Number.isInteger(parsed) || state === undefined || state.length === 0) return null;
    return { parent: parsed, state };
  } catch {
    return null;
  }
}

async function waitForExit(pid: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!isAlive(pid)) return true;
    const state = processState(pid);
    if (state?.state.startsWith('Z')) return true;
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
  }
  return !isAlive(pid);
}

function signal(pid: number, name: NodeJS.Signals): boolean {
  try {
    process.kill(pid, name);
    return true;
  } catch {
    return false;
  }
}

/**
 * Terminal cleanup for the engine child started by this process. Signals only PIDs recorded by our
 * wrapper and still parented to this process; a stale or recycled PID is never touched. A survivor
 * keeps the PID file so a later call can retry, and is reported as `blocked` for diagnostics.
 */
export async function terminateEngineProcess(
  options: { readonly gracefulMs?: number; readonly killMs?: number } = {},
): Promise<EngineTermination> {
  const file = enginePidFile();
  if (file === null) return { status: 'none', pids: [] };
  const read = readPidFile(file);
  if (read.kind !== 'ok') {
    return { status: read.kind === 'missing' ? 'none' : 'unreadable', pids: [] };
  }

  const terminated: number[] = [];
  const forced: number[] = [];
  let survivor: number | undefined;
  for (const pid of read.pids) {
    if (!isAlive(pid)) continue;
    const state = processState(pid);
    if (state === null) {
      // Alive but unreadable: never signal a PID whose ownership cannot be proven.
      survivor = pid;
      continue;
    }
    if (state.parent !== process.pid || state.state.startsWith('Z')) continue;
    if (!signal(pid, 'SIGTERM')) {
      // It is our child but could not be signalled; keep the file and report it.
      survivor = pid;
      continue;
    }
    if (await waitForExit(pid, options.gracefulMs ?? GRACEFUL_WAIT_MS)) {
      terminated.push(pid);
      continue;
    }
    if (signal(pid, 'SIGKILL') && (await waitForExit(pid, options.killMs ?? KILL_WAIT_MS))) {
      forced.push(pid);
    } else {
      survivor = pid;
    }
  }

  if (survivor === undefined) {
    try {
      unlinkSync(file);
    } catch {
      // A missing file is the normal case after a successful shutdown; nothing to report.
    }
  }

  const status: EngineTerminationStatus =
    survivor !== undefined
      ? 'blocked'
      : forced.length > 0
        ? 'forced'
        : terminated.length > 0
          ? 'terminated'
          : 'none';
  return { status, pids: survivor === undefined ? [...terminated, ...forced] : [survivor] };
}
