import {
  chmodSync,
  closeSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  type Stats,
  statSync,
  writeSync,
} from 'node:fs';
import path from 'node:path';

export const PRIVATE_DIRECTORY_MODE = 0o700;
export const PRIVATE_FILE_MODE = 0o600;

/**
 * Rejects any symlink in the existing path instead of realpath-normalizing it, then creates missing
 * components one level at a time. This is a pre-open static check: a writer able to replace an
 * ancestor between validation and open can still race the process.
 */
export function ensurePrivateDirectory(directory: string): void {
  const resolved = path.resolve(directory);
  const root = path.parse(resolved).root;
  let current = root;

  for (const segment of resolved.slice(root.length).split(path.sep).filter(Boolean)) {
    current = path.join(current, segment);
    const existing = lstatSync(current, { throwIfNoEntry: false });
    if (existing !== undefined) {
      assertRealDirectory(existing);
      continue;
    }

    // Non-recursive creation verifies the nearest existing ancestor before any child is created.
    mkdirSync(current, { mode: PRIVATE_DIRECTORY_MODE });
    assertRealDirectory(lstatSync(current));
    chmodSync(current, PRIVATE_DIRECTORY_MODE);
  }

  const final = lstatSync(resolved);
  assertRealDirectory(final);
  if ((final.mode & 0o777) !== PRIVATE_DIRECTORY_MODE)
    throw new Error(`Directory ${directory} must have mode 0700 because it holds local credentials`);
}

function assertRealDirectory(entry: Stats): void {
  if (entry.isSymbolicLink() || !entry.isDirectory())
    throw new Error('Credential directory path must contain only real directories');
}

/** Reads a private file and rejects group/world access instead of chmodding a user-owned file. */
export function readPrivateFile(file: string): string {
  const mode = statSync(file).mode & 0o777;
  if ((mode & 0o077) !== 0) {
    throw new Error(`File ${file} must not be group/world accessible`);
  }
  return readFileSync(file, 'utf8');
}

/**
 * Atomically creates a private file or returns the existing content. `wx` makes the create
 * single-winner, so two processes cannot publish different identities at the same path.
 */
export function createPrivateFileIfMissing(file: string, content: string): string {
  ensurePrivateDirectory(path.dirname(file));
  let handle: number | null = null;
  try {
    handle = openSync(file, 'wx', PRIVATE_FILE_MODE);
    writeSync(handle, content);
    chmodSync(file, PRIVATE_FILE_MODE);
    return content;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
    return readPrivateFile(file);
  } finally {
    if (handle !== null) closeSync(handle);
  }
}
