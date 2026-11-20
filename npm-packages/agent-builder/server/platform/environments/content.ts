import { Buffer } from 'node:buffer';
import { CredentialDecryptionError, decryptCredential, encryptCredential } from '../hashing';
import { EnvironmentRequestError } from './errors';

/** Local write ceiling for one environment file; deliberately smaller than the wire maximum. */
export const MAX_ENVIRONMENT_FILE_BYTES = 1_048_576;

/**
 * Sealed payload layout inside `platform_environment_files.content_ciphertext`:
 * magic (4) | key id (16 ASCII) | iv (12) | tag (16) | ciphertext.
 * Keeping the envelope in one BLOB matches the existing schema without a migration.
 */
const MAGIC = Buffer.from('pef1', 'ascii');
const KEY_ID_BYTES = 16;
const IV_BYTES = 12;
const TAG_BYTES = 16;
const HEADER_BYTES = MAGIC.length + KEY_ID_BYTES + IV_BYTES + TAG_BYTES;

const BASE64 = /^[A-Za-z0-9+/]*={0,2}$/;

/** Decodes standard base64 and enforces the local size ceiling before anything is sealed. */
export function decodeInlineFileContent(data: string, param = 'data'): Uint8Array {
  if (data.length % 4 !== 0 || !BASE64.test(data)) {
    throw new EnvironmentRequestError(
      'environment_file_content_invalid',
      'File content must be standard base64',
      400,
      param,
    );
  }
  const bytes = Buffer.from(data, 'base64');
  if (bytes.byteLength > MAX_ENVIRONMENT_FILE_BYTES) {
    throw new EnvironmentRequestError(
      'environment_file_too_large',
      `File content must not exceed ${MAX_ENVIRONMENT_FILE_BYTES} bytes`,
      400,
      param,
    );
  }
  return new Uint8Array(bytes);
}

export function sealFileContent(bytes: Uint8Array): Uint8Array {
  const secret = encryptCredential(Buffer.from(bytes).toString('base64'));
  return new Uint8Array(
    Buffer.concat([
      MAGIC,
      Buffer.from(secret.keyId, 'ascii'),
      Buffer.from(secret.iv),
      Buffer.from(secret.tag),
      Buffer.from(secret.cipher),
    ]),
  );
}

/**
 * Opens a sealed environment file. Only used to verify persistence; the public surface
 * never returns file contents, so a decoding failure is a server-side integrity problem.
 */
export function openSealedFileContent(sealed: Uint8Array): Uint8Array {
  const buffer = Buffer.from(sealed);
  if (buffer.length < HEADER_BYTES || !buffer.subarray(0, MAGIC.length).equals(MAGIC)) {
    throw new CredentialDecryptionError('Stored environment file could not be decoded');
  }
  const keyId = buffer.subarray(MAGIC.length, MAGIC.length + KEY_ID_BYTES).toString('ascii');
  const ivStart = MAGIC.length + KEY_ID_BYTES;
  const tagStart = ivStart + IV_BYTES;
  const cipherStart = tagStart + TAG_BYTES;
  const plaintext = decryptCredential({
    cipher: new Uint8Array(buffer.subarray(cipherStart)),
    iv: new Uint8Array(buffer.subarray(ivStart, tagStart)),
    tag: new Uint8Array(buffer.subarray(tagStart, cipherStart)),
    keyId,
  });
  return new Uint8Array(Buffer.from(plaintext, 'base64'));
}
