import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { Hono } from 'hono';
import { openDatabase } from '../../../../server/db/database';
import { migrate } from '../../../../server/db/migrate';
import {
  CREDENTIAL_KEY_ENV,
  CredentialDecryptionError,
  CredentialKeyError,
  CredentialKeyMismatchError,
  encryptCredential,
} from '../../../../server/platform/hashing';
import {
  createPlatformRepositories,
  type PlatformRepositories,
} from '../../../../server/platform/repositories';
import type { PlatformCredentialRow } from '../../../../server/platform/repositories/vaults';
import {
  CredentialUnavailableError,
  credentialUnavailable,
  readCredentialSecret,
} from '../../../../server/platform/vaults/secrets';

const KEY = Buffer.alloc(32, 7);
const ROTATED_KEY = Buffer.alloc(32, 8);
const SECRET = 'stored-secret-value';
const STORED_AUTH = '{"type":"static_bearer"}';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function setup(): PlatformRepositories {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  return createPlatformRepositories(database);
}

function seed(repos: PlatformRepositories): PlatformCredentialRow {
  const vault = repos.vaults.create({ name: 'Secrets' });
  return repos.credentials.create({
    vaultId: vault.id,
    name: 'token',
    authKind: 'static_bearer',
    auth: STORED_AUTH,
    secret: encryptCredential(SECRET, KEY),
  });
}

function tamper(bytes: Uint8Array, index = 0): Uint8Array {
  const copy = Uint8Array.from(bytes);
  copy[index] = (copy[index] ?? 0) ^ 0xff;
  return copy;
}

function captureUnavailable(run: () => unknown): CredentialUnavailableError {
  try {
    run();
  } catch (error) {
    expect(error).toBeInstanceOf(CredentialUnavailableError);
    return error as CredentialUnavailableError;
  }
  throw new Error('Expected the credential read to fail');
}

describe('credential secret encryption', () => {
  test('round-trips the secret and keeps the plaintext out of the stored columns', () => {
    const repos = setup();
    const credential = seed(repos);

    expect(readCredentialSecret(credential, KEY)).toBe(SECRET);
    expect(credential.key_id.length).toBeGreaterThan(0);
    expect(credential.auth).toBe(STORED_AUTH);
    expect(Buffer.from(credential.secret_cipher).includes(Buffer.from(SECRET))).toBe(false);
    expect(Buffer.from(credential.secret_cipher)).not.toEqual(Buffer.from(SECRET));
  });

  test('reads with the configured deployment key by default', () => {
    const repos = setup();
    const credential = seed(repos);
    const configured = process.env[CREDENTIAL_KEY_ENV];
    process.env[CREDENTIAL_KEY_ENV] = KEY.toString('hex');
    try {
      expect(readCredentialSecret(credential)).toBe(SECRET);
    } finally {
      if (configured === undefined) delete process.env[CREDENTIAL_KEY_ENV];
      else process.env[CREDENTIAL_KEY_ENV] = configured;
    }
  });

  test('fails with a stable error for tampered cipher, iv or tag', () => {
    const repos = setup();
    const credential = seed(repos);
    const variants: ReadonlyArray<readonly [string, PlatformCredentialRow]> = [
      ['cipher', { ...credential, secret_cipher: tamper(credential.secret_cipher) }],
      ['iv', { ...credential, secret_iv: tamper(credential.secret_iv) }],
      ['tag', { ...credential, secret_tag: tamper(credential.secret_tag) }],
    ];

    for (const [label, variant] of variants) {
      const error = captureUnavailable(() => readCredentialSecret(variant, KEY));
      expect({ label, code: error.code }).toEqual({ label, code: 'credential_unavailable' });
      expect(error.message).not.toContain(SECRET);
      expect(error.cause).toBeInstanceOf(CredentialDecryptionError);
    }
  });

  test('fails with a stable error when the key id does not match', () => {
    const repos = setup();
    const credential = seed(repos);

    const error = captureUnavailable(() => readCredentialSecret(credential, ROTATED_KEY));
    expect(error.code).toBe('credential_unavailable');
    expect(error.cause).toBeInstanceOf(CredentialKeyMismatchError);
    expect(error.message).not.toContain(SECRET);
  });

  test('fails with a stable error when no usable key is configured', () => {
    const repos = setup();
    const credential = seed(repos);
    const configured = process.env[CREDENTIAL_KEY_ENV];

    process.env[CREDENTIAL_KEY_ENV] = 'not-a-key';
    try {
      const error = captureUnavailable(() => readCredentialSecret(credential));
      expect(error.code).toBe('credential_unavailable');
      expect(error.cause).toBeInstanceOf(CredentialKeyError);
      expect((error.cause as CredentialKeyError).code).toBe('credential_key_invalid');
    } finally {
      if (configured === undefined) delete process.env[CREDENTIAL_KEY_ENV];
      else process.env[CREDENTIAL_KEY_ENV] = configured;
    }
  });

  test('maps an unreadable credential to the 500 credential_unavailable response', async () => {
    const repos = setup();
    const credential = seed(repos);
    const tampered = { ...credential, secret_tag: tamper(credential.secret_tag) };
    const app = new Hono();

    app.get('/probe', (c) => {
      try {
        readCredentialSecret(tampered, KEY);
      } catch (error) {
        if (error instanceof CredentialUnavailableError) return credentialUnavailable(c);
        throw error;
      }
      return c.text('unexpected');
    });

    const response = await app.request('/probe');
    expect(response.status).toBe(500);
    expect(await response.json()).toEqual({
      error: {
        type: 'server_error',
        code: 'credential_unavailable',
        message: 'Stored credential is unavailable',
        param: null,
      },
    });
  });
});
