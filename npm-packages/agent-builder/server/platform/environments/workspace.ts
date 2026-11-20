import { lstatSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { EnvironmentRequestError } from './errors';

export const MAX_ENVIRONMENT_PATH_LENGTH = 4096;

/** The documented absolute workspace root; paths below it map to workspace-relative storage. */
export const WORKSPACE_ROOT = '/workspace';

/** Renders a stored workspace-relative path in the documented absolute form. */
export function workspaceAbsolutePath(relative: string): string {
  return relative.length === 0 ? WORKSPACE_ROOT : `${WORKSPACE_ROOT}/${relative}`;
}

/**
 * Renders a stored path as the documented absolute form, tolerating rows written before
 * paths were normalized. A path already below `/workspace` is returned unchanged.
 */
export function storedWorkspacePath(stored: string): string {
  if (stored === WORKSPACE_ROOT || stored.startsWith(`${WORKSPACE_ROOT}/`)) return stored;
  return workspaceAbsolutePath(stored);
}

/** Returns the workspace-relative remainder, or null when `raw` is not below `/workspace`. */
function workspaceRelative(raw: string): string | null {
  if (raw === WORKSPACE_ROOT) return '';
  if (raw.startsWith(`${WORKSPACE_ROOT}/`)) return raw.slice(WORKSPACE_ROOT.length + 1);
  return null;
}

/**
 * The deployment workspace is the only directory this process owns. `server/env.ts`
 * derives it from `PERI_WORKSPACE_PATH` (defaulting to the process directory) and rejects
 * non-canonical paths; the platform routes see no `ServerEnv`, so they read the same key.
 */
export function resolveDeploymentWorkspace(env: NodeJS.ProcessEnv = process.env): string {
  const configured = env.PERI_WORKSPACE_PATH?.trim();
  const resolved = path.resolve(
    process.cwd(),
    configured === undefined || configured.length === 0 ? '.' : configured,
  );
  try {
    return realpathSync(resolved);
  } catch {
    return resolved;
  }
}

function canonicalCandidate(value: string): string {
  const resolved = path.resolve(value);
  try {
    return realpathSync(resolved);
  } catch {
    return resolved;
  }
}

/**
 * Accepts only the deployment workspace. Both the configured spelling and its canonical
 * form are accepted so macOS temp paths (`/var` vs `/private/var`) resolve consistently.
 */
export function assertDeploymentWorkspace(raw: string | undefined, param: string): string {
  const workspace = resolveDeploymentWorkspace();
  if (raw === undefined) return workspace;
  const candidate = canonicalCandidate(raw);
  if (candidate !== workspace && path.resolve(raw) !== workspace) {
    throw new EnvironmentRequestError(
      'workspace_not_allowed',
      'workspace_directory must be the deployment workspace',
      400,
      param,
    );
  }
  return workspace;
}

function invalidPath(param: string): EnvironmentRequestError {
  return new EnvironmentRequestError(
    'environment_path_invalid',
    'Environment file path must stay inside the deployment workspace',
    400,
    param,
  );
}

/**
 * Normalizes a workspace-relative file path. The documented `/workspace/...` form is accepted
 * and mapped to the workspace root; any other absolute path, traversal, empty segment,
 * backslash, or control character is rejected instead of normalized away.
 */
export function normalizeEnvironmentPath(raw: string, param: string): string {
  if (raw.length === 0 || raw.trim().length === 0 || raw.length > MAX_ENVIRONMENT_PATH_LENGTH) {
    throw invalidPath(param);
  }
  if (raw.includes('\0') || raw.includes('\\')) throw invalidPath(param);
  for (const character of raw) {
    const code = character.codePointAt(0) ?? 0;
    if (code < 0x20 || code === 0x7f) throw invalidPath(param);
  }

  const relative = workspaceRelative(raw);
  if (relative !== null) {
    if (relative.length === 0) throw invalidPath(param);
    return normalizeEnvironmentPath(relative, param);
  }
  if (path.posix.isAbsolute(raw) || /^[A-Za-z]:/.test(raw)) throw invalidPath(param);

  const segments = raw.split('/');
  for (const segment of segments) {
    if (segment.length === 0 || segment === '.' || segment === '..') throw invalidPath(param);
  }
  return segments.join('/');
}

/** Optional `path` filter; absolute values must name the workspace or its `/workspace` form. */
export function normalizeDirectoryFilter(raw: string | undefined, param: string): string {
  if (raw === undefined || raw.trim().length === 0) return '';
  const relative = workspaceRelative(raw);
  if (relative !== null) return relative.length === 0 ? '' : normalizeEnvironmentPath(relative, param);
  if (path.posix.isAbsolute(raw) || /^[A-Za-z]:/.test(raw)) {
    const workspace = resolveDeploymentWorkspace();
    const candidate = canonicalCandidate(raw);
    if (candidate === workspace || path.resolve(raw) === workspace) return '';
    throw new EnvironmentRequestError(
      'workspace_not_allowed',
      'path filter must stay inside the deployment workspace',
      400,
      param,
    );
  }
  return normalizeEnvironmentPath(raw, param);
}

/**
 * Rejects symbolic links in any existing component under the deployment workspace.
 * Stored file contents live in SQLite, so this only guards paths that also exist on disk.
 */
export function assertNoSymlink(workspace: string, relativePath: string, param: string): void {
  const segments = relativePath.split('/');
  let current = path.resolve(workspace);
  for (const [index, segment] of segments.entries()) {
    current = path.join(current, segment);
    const entry = lstatSync(current, { throwIfNoEntry: false });
    if (entry === undefined) return;
    if (entry.isSymbolicLink()) {
      throw new EnvironmentRequestError(
        'environment_path_symlink',
        'Environment file path must not traverse symbolic links',
        400,
        param,
      );
    }
    if (!entry.isDirectory() && index < segments.length - 1) throw invalidPath(param);
    if (entry.isDirectory() && index === segments.length - 1) throw invalidPath(param);
  }
}
