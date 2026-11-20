import type { Context } from 'hono';
import { millisToUnixSeconds } from '../json';
import type {
  PlatformEnvironmentFileRow,
  PlatformEnvironmentRow,
  PlatformEnvironmentTemplateRow,
} from '../repositories/environments';
import type { EnvironmentConfig, SafeTemplateFile, TemplateConfig } from './config';
import { storedWorkspacePath, workspaceAbsolutePath } from './workspace';

export interface IdListPage<T> {
  readonly object: 'list';
  readonly data: readonly T[];
  readonly first_id: string | null;
  readonly last_id: string | null;
  readonly has_more: boolean;
}

/**
 * Builds an ID-paginated page from `limit + 1` rows. The extra row marks `has_more`
 * without a second count query.
 */
export function buildIdListPage<T>(
  rows: readonly T[],
  limit: number,
  idOf: (row: T) => string,
): IdListPage<T> {
  const data = rows.slice(0, limit);
  const first = data[0];
  const last = data[data.length - 1];
  return {
    object: 'list',
    data,
    first_id: first === undefined ? null : idOf(first),
    last_id: last === undefined ? null : idOf(last),
    has_more: rows.length > limit,
  };
}

/** Degradation is part of the response contract, never a silent substitution. */
export function setDegradationHeader(c: Context, reasons: readonly string[]): void {
  if (reasons.length > 0) c.header('x-platform-degraded', reasons.join(', '));
}

/** Rows store workspace-relative paths; the documented resource reports absolute ones. */
export function environmentFileResource(row: PlatformEnvironmentFileRow): Record<string, unknown> {
  return {
    object: 'agent.environment.file',
    environment_id: row.environment_id,
    path: workspaceAbsolutePath(row.path),
    size_bytes: row.size_bytes,
  };
}

export function environmentFileMetadata(row: PlatformEnvironmentFileRow): Record<string, unknown> {
  return {
    type: 'inline',
    id: row.id,
    path: workspaceAbsolutePath(row.path),
    size_bytes: row.size_bytes,
  };
}

/**
 * Public environment resource. Only the documented fields are exposed; local execution details
 * are reported through `x-platform-degraded` instead of a parallel response shape.
 */
export function environmentResource(
  row: PlatformEnvironmentRow,
  config: EnvironmentConfig,
  files: readonly PlatformEnvironmentFileRow[],
): Record<string, unknown> {
  return {
    id: row.id,
    object: 'agent.environment',
    type: row.type,
    status: row.status,
    files: files.map(environmentFileMetadata),
    skills: config.skills,
    plugins: config.plugins,
  };
}

/** Template file metadata keeps the same absolute `path` semantics as `EnvironmentFile`. */
function templateFileResource(file: SafeTemplateFile): SafeTemplateFile {
  return { ...file, path: storedWorkspacePath(file.path) };
}

export function environmentTemplateResource(
  row: PlatformEnvironmentTemplateRow,
  config: TemplateConfig,
): Record<string, unknown> {
  return {
    id: row.id,
    name: row.name.length === 0 ? null : row.name,
    object: 'agent.environment.template',
    created_at: millisToUnixSeconds(row.created_at),
    updated_at: millisToUnixSeconds(row.updated_at),
    packages: config.packages,
    network: { access: config.network.access, allowed_domains: config.network.allowed_domains },
    desktop: config.desktop,
    capability_directories: config.capability_directories,
    skills: config.skills,
    plugins: config.plugins,
    files: config.files.map(templateFileResource),
  };
}
