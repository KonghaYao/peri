import { Buffer } from 'node:buffer';
import { MAX_CURSOR_LENGTH } from '../pagination';
import type {
  PlatformEnvironmentFileRow,
  PlatformEnvironmentFilesRepository,
} from '../repositories/environments';
import { decodeInlineFileContent, sealFileContent } from './content';
import { EnvironmentRequestError } from './errors';
import { assertNoSymlink, normalizeEnvironmentPath } from './workspace';

export interface InlineEnvironmentFile {
  readonly path: string;
  readonly data: string;
}

/**
 * Validates, seals, and stores one inline file. Writing the same path replaces the existing
 * row (and keeps its ID) instead of accumulating duplicates.
 */
export function writeInlineEnvironmentFile(
  files: PlatformEnvironmentFilesRepository,
  environmentId: string,
  file: InlineEnvironmentFile,
  workspace: string,
): PlatformEnvironmentFileRow {
  const relativePath = normalizeEnvironmentPath(file.path, 'path');
  assertNoSymlink(workspace, relativePath, 'path');
  const bytes = decodeInlineFileContent(file.data, 'data');
  const sealed = sealFileContent(bytes);

  const existing = files.getByPath(environmentId, relativePath);
  if (existing !== null) {
    const updated = files.updateContent(existing.id, sealed, bytes.byteLength);
    if (updated === null) throw new Error(`Environment file ${existing.id} could not be read back`);
    return updated;
  }
  return files.create({
    environmentId,
    path: relativePath,
    contentCiphertext: sealed,
    sizeBytes: bytes.byteLength,
  });
}

/** File pages are keyed by path, the only stable identifier in the documented file resource. */
export function encodeFilePage(path: string): string {
  return Buffer.from(path, 'utf8').toString('base64url');
}

export function decodeFilePage(raw: string | undefined): string | null {
  if (raw === undefined || raw.length === 0) return null;
  if (raw.length > MAX_CURSOR_LENGTH) {
    throw new EnvironmentRequestError('invalid_page', 'page cursor is too long', 400, 'page');
  }
  const decoded = Buffer.from(raw, 'base64url').toString('utf8');
  if (decoded.length === 0 || encodeFilePage(decoded) !== raw) {
    throw new EnvironmentRequestError('invalid_page', 'page cursor is invalid', 400, 'page');
  }
  return decoded;
}
