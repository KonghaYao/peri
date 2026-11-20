import type { Context } from 'hono';
import { platformError } from '../errors';
import {
  CredentialDecryptionError,
  CredentialKeyError,
  decryptCredential,
  type EncryptedSecret,
} from '../hashing';
import type { PlatformCredentialRow } from '../repositories/vaults';

/**
 * Stable internal error for credentials whose secret can no longer be read: a rotated key,
 * a tampered payload, or an unavailable key. There is deliberately no plaintext fallback.
 */
export class CredentialUnavailableError extends Error {
  readonly code: 'credential_unavailable' = 'credential_unavailable';

  constructor(cause: unknown) {
    super('Stored credential is unavailable', { cause });
    this.name = 'CredentialUnavailableError';
  }
}

function toEncryptedSecret(credential: PlatformCredentialRow): EncryptedSecret {
  return {
    cipher: credential.secret_cipher,
    iv: credential.secret_iv,
    tag: credential.secret_tag,
    keyId: credential.key_id,
  };
}

/**
 * Internal read path for later session tasks. The four stored columns are recombined into
 * the encrypted quartet; key errors and failed authentication both surface as
 * `CredentialUnavailableError` so callers cannot mistake tampering for a valid secret.
 */
export function readCredentialSecret(credential: PlatformCredentialRow, key?: Buffer): string {
  try {
    return decryptCredential(toEncryptedSecret(credential), key);
  } catch (error) {
    if (error instanceof CredentialKeyError || error instanceof CredentialDecryptionError) {
      throw new CredentialUnavailableError(error);
    }
    throw error;
  }
}

/** Maps an unreadable credential to the documented 500 response without echoing details. */
export function credentialUnavailable(c: Context): Response {
  return platformError(c, 'credential_unavailable', 'Stored credential is unavailable', 500);
}
