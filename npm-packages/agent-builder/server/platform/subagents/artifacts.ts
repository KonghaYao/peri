import { lstatSync, readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { resolveDeploymentWorkspace } from '../environments/workspace';
import { millisToUnixSeconds } from '../json';
import type { PlatformArtifactRow } from '../repositories/artifacts';

/** Wire shape and guarded byte reader for `/v1/agents/sessions/{session_id}/artifacts`. */

export interface ArtifactResourceWire {
  readonly object: 'agent.session.artifact';
  readonly id: string;
  readonly session_id: string;
  /**
   * Documented as strings, so an artifact published outside a hosted environment or completed
   * turn reports the stored value, or an empty string when no such row was recorded.
   */
  readonly environment_id: string;
  readonly turn_id: string;
  readonly path: string;
  readonly size_bytes: number;
  readonly created_at: number;
}

/** Artifact reads are bounded so one request cannot materialize an unbounded file. */
export const MAX_ARTIFACT_BYTES = 16 * 1024 * 1024;

const MAX_ARTIFACT_PATH_LENGTH = 4096;
const MEDIA_TYPE = /^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/;
const OCTET_STREAM = 'application/octet-stream';

export type ArtifactContentStatus = 403 | 404 | 409;

/** A content read that must surface as a documented platform error rather than raw bytes. */
export class ArtifactContentError extends Error {
  readonly code: string;
  readonly status: ArtifactContentStatus;

  constructor(code: string, message: string, status: ArtifactContentStatus) {
    super(message);
    this.name = 'ArtifactContentError';
    this.code = code;
    this.status = status;
  }
}

export function serializeArtifact(row: PlatformArtifactRow): ArtifactResourceWire {
  return {
    object: 'agent.session.artifact',
    id: row.id,
    session_id: row.session_id,
    environment_id: row.environment_id ?? '',
    turn_id: row.turn_id ?? '',
    path: row.path,
    size_bytes: row.size_bytes,
    created_at: millisToUnixSeconds(row.created_at),
  };
}

function unsafePath(): ArtifactContentError {
  return new ArtifactContentError(
    'artifact_path_unsafe',
    'Artifact path must stay inside the deployment workspace',
    403,
  );
}

function missingContent(): ArtifactContentError {
  return new ArtifactContentError('artifact_content_unavailable', 'Artifact content is unavailable', 404);
}

/**
 * Resolves the stored path, then requires the canonical target to stay inside the deployment
 * workspace. Relative paths are workspace-relative; absolute paths must already point inside it.
 * Traversal, NUL bytes, non-regular files and oversize or size-mismatched content are rejected.
 */
export function readArtifactContent(row: PlatformArtifactRow): {
  readonly bytes: Uint8Array<ArrayBuffer>;
  readonly contentType: string;
} {
  const raw = row.path;
  if (raw.length === 0 || raw.length > MAX_ARTIFACT_PATH_LENGTH || raw.includes('\0')) {
    throw unsafePath();
  }
  const workspace = resolveDeploymentWorkspace();
  const candidate = path.isAbsolute(raw) ? path.resolve(raw) : path.resolve(workspace, raw);

  let canonical: string;
  try {
    canonical = realpathSync(candidate);
  } catch {
    throw missingContent();
  }
  if (canonical !== workspace && !canonical.startsWith(`${workspace}${path.sep}`)) {
    throw unsafePath();
  }

  const stat = lstatSync(canonical, { throwIfNoEntry: false });
  if (stat === undefined || !stat.isFile() || stat.isSymbolicLink()) throw missingContent();
  if (stat.size !== row.size_bytes) {
    throw new ArtifactContentError(
      'artifact_size_mismatch',
      'Artifact content does not match the recorded size',
      409,
    );
  }
  if (stat.size > MAX_ARTIFACT_BYTES) {
    throw new ArtifactContentError('artifact_too_large', 'Artifact exceeds the download limit', 409);
  }

  try {
    return {
      bytes: new Uint8Array(readFileSync(canonical)),
      contentType: row.media_type !== null && MEDIA_TYPE.test(row.media_type) ? row.media_type : OCTET_STREAM,
    };
  } catch {
    throw missingContent();
  }
}
