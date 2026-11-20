import { createCipheriv, createDecipheriv, createHash, randomBytes } from 'node:crypto';
import path from 'node:path';
import { createPrivateFileIfMissing } from '../filesystem';

const ALGORITHM = 'aes-256-gcm';
const KEY_BYTES = 32;
const IV_BYTES = 12;
const TAG_BYTES = 16;
/** `key_id` is the first 16 hex characters of the key fingerprint. */
const KEY_ID_LENGTH = 16;
/** Fallback key file inside the private data directory (created 0600 on first use). */
const KEY_FILE_NAME = 'platform-credential.key';

export const CREDENTIAL_KEY_ENV = 'PLATFORM_CREDENTIAL_KEY';

export type CredentialKeyErrorCode =
  | 'credential_key_missing'
  | 'credential_key_invalid'
  | 'credential_key_unavailable';

export type CredentialDecryptionErrorCode = 'credential_decryption_failed' | 'credential_key_rotated';

/** Raised when the platform cannot resolve a usable encryption key. */
export class CredentialKeyError extends Error {
  readonly code: CredentialKeyErrorCode;

  constructor(code: CredentialKeyErrorCode, message: string) {
    super(message);
    this.name = 'CredentialKeyError';
    this.code = code;
  }
}

/** Raised when a stored credential payload cannot be authenticated or decoded. */
export class CredentialDecryptionError extends Error {
  readonly code: CredentialDecryptionErrorCode = 'credential_decryption_failed';

  constructor(message = 'Stored credential could not be decrypted') {
    super(message);
    this.name = 'CredentialDecryptionError';
  }
}

/** Raised when the payload was sealed with a different key than the one supplied. */
export class CredentialKeyMismatchError extends CredentialDecryptionError {
  override readonly code: CredentialDecryptionErrorCode = 'credential_key_rotated';

  constructor() {
    super('Stored credential was encrypted with a different key');
    this.name = 'CredentialKeyMismatchError';
  }
}

/**
 * The four columns stored by `platform_credentials`: AES-256-GCM ciphertext split into
 * cipher/iv/tag plus the fingerprint of the key that sealed it.
 */
export interface EncryptedSecret {
  readonly cipher: Uint8Array;
  readonly iv: Uint8Array;
  readonly tag: Uint8Array;
  readonly keyId: string;
}

/** Fingerprint used as `key_id`; stable across restarts for the same key. */
export function keyIdFor(key: Uint8Array): string {
  return createHash('sha256').update(key).digest('hex').slice(0, KEY_ID_LENGTH);
}

function parseKey(raw: string): Buffer {
  const trimmed = raw.trim();
  if (/^[0-9a-fA-F]{64}$/.test(trimmed)) return Buffer.from(trimmed, 'hex');
  if (/^[A-Za-z0-9+/_-]{43}=?$/.test(trimmed)) {
    const base64 = trimmed.replaceAll('-', '+').replaceAll('_', '/');
    const key = Buffer.from(base64, 'base64');
    if (key.length === KEY_BYTES) return key;
  }
  throw new CredentialKeyError(
    'credential_key_invalid',
    `${CREDENTIAL_KEY_ENV} must be a 32-byte key encoded as 64 hex characters or base64`,
  );
}

/**
 * Resolves the key from the environment only. Kept separate from file fallback so callers
 * that must fail without an explicit deployment key (for example readiness probes) can say so.
 */
export function readCredentialKey(env: NodeJS.ProcessEnv = process.env): Buffer {
  const raw = env[CREDENTIAL_KEY_ENV];
  if (raw === undefined || raw.trim().length === 0) {
    throw new CredentialKeyError(
      'credential_key_missing',
      `${CREDENTIAL_KEY_ENV} is required to store credentials`,
    );
  }
  return parseKey(raw);
}

export interface CredentialKeyOptions {
  readonly env?: NodeJS.ProcessEnv;
  /** Private directory for the fallback key file; defaults to the database directory. */
  readonly dataDir?: string;
}

/**
 * Resolves the encryption key: `PLATFORM_CREDENTIAL_KEY` wins, otherwise a private key file in
 * the data directory is created on first use (0600). There is no plaintext fallback.
 */
export function resolveCredentialKey(options: CredentialKeyOptions = {}): Buffer {
  const env = options.env ?? process.env;
  const configured = env[CREDENTIAL_KEY_ENV];
  if (configured !== undefined && configured.trim().length > 0) return parseKey(configured);

  const dataDir =
    options.dataDir ?? path.dirname(path.resolve(env.DATABASE_PATH ?? '.data/agent-builder.db'));
  let raw: string;
  try {
    raw = createPrivateFileIfMissing(
      path.join(dataDir, KEY_FILE_NAME),
      `${randomBytes(KEY_BYTES).toString('base64')}\n`,
    );
  } catch {
    throw new CredentialKeyError(
      'credential_key_unavailable',
      `${CREDENTIAL_KEY_ENV} is not set and a private key file could not be established in the data directory`,
    );
  }
  return parseKey(raw);
}

export function encryptCredential(plaintext: string, key: Buffer = resolveCredentialKey()): EncryptedSecret {
  const iv = randomBytes(IV_BYTES);
  const cipher = createCipheriv(ALGORITHM, key, iv);
  const ciphertext = Buffer.concat([cipher.update(plaintext, 'utf8'), cipher.final()]);
  return { cipher: ciphertext, iv, tag: cipher.getAuthTag(), keyId: keyIdFor(key) };
}

export function decryptCredential(secret: EncryptedSecret, key: Buffer = resolveCredentialKey()): string {
  // Detect rotation before attempting decryption so callers can distinguish a stale key id
  // from a tampered or corrupted payload.
  if (secret.keyId !== keyIdFor(key)) throw new CredentialKeyMismatchError();
  if (secret.iv.length !== IV_BYTES || secret.tag.length !== TAG_BYTES) {
    throw new CredentialDecryptionError();
  }
  try {
    const decipher = createDecipheriv(ALGORITHM, key, secret.iv);
    decipher.setAuthTag(secret.tag);
    return Buffer.concat([decipher.update(secret.cipher), decipher.final()]).toString('utf8');
  } catch {
    throw new CredentialDecryptionError();
  }
}
