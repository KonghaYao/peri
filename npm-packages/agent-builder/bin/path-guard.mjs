import { lstatSync } from 'node:fs';
import path from 'node:path';

/**
 * The packaged launcher runs before `dist/server/main.js` is imported, so it cannot reuse the
 * server's TypeScript guard. This check only prevents its recursive mkdir from traversing an
 * existing symlink; the server revalidates before creating or opening the database.
 */
export class PrivatePathError extends Error {
  constructor() {
    super('Private path contains a symlink or non-directory component');
    this.name = 'PrivatePathError';
  }
}

export function assertNoSymlinkAncestors(target) {
  const resolved = path.resolve(target);
  const root = path.parse(resolved).root;
  let current = root;

  for (const segment of resolved.slice(root.length).split(path.sep).filter(Boolean)) {
    current = path.join(current, segment);
    const entry = lstatSync(current, { throwIfNoEntry: false });
    if (entry === undefined) return;
    if (entry.isSymbolicLink() || !entry.isDirectory()) throw new PrivatePathError();
  }
}
