import { afterEach, describe, expect, test } from 'bun:test';
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  CREDENTIAL_KEY_ENV,
  CredentialDecryptionError,
  CredentialKeyError,
  CredentialKeyMismatchError,
  decryptCredential,
  encryptCredential,
  keyIdFor,
  readCredentialKey,
  resolveCredentialKey,
} from '../../../server/platform/hashing';

const KEY = Buffer.alloc(32, 7);
const OTHER_KEY = Buffer.alloc(32, 8);
const KEY_FILE_NAME = 'platform-credential.key';

const tempDirs: string[] = [];

function tempDir(): string {
  const dir = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-platform-key-')));
  tempDirs.push(dir);
  return dir;
}

function tamper(bytes: Uint8Array, index = 0): Uint8Array {
  const copy = Uint8Array.from(bytes);
  copy[index] = (copy[index] ?? 0) ^ 0xff;
  return copy;
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe('credential key resolution', () => {
  test('fails explicitly when the key is missing', () => {
    expect(() => readCredentialKey({})).toThrow(CredentialKeyError);
    try {
      readCredentialKey({});
    } catch (error) {
      expect(error).toBeInstanceOf(CredentialKeyError);
      expect((error as CredentialKeyError).code).toBe('credential_key_missing');
    }
  });

  test('rejects a key with the wrong shape', () => {
    try {
      readCredentialKey({ [CREDENTIAL_KEY_ENV]: 'too-short' });
    } catch (error) {
      expect((error as CredentialKeyError).code).toBe('credential_key_invalid');
    }
  });

  test('accepts hex and base64 encodings of a 32-byte key', () => {
    expect(readCredentialKey({ [CREDENTIAL_KEY_ENV]: KEY.toString('hex') })).toEqual(KEY);
    expect(readCredentialKey({ [CREDENTIAL_KEY_ENV]: KEY.toString('base64') })).toEqual(KEY);
  });

  test('prefers the deployment key over the data directory key file', () => {
    const dir = tempDir();

    expect(
      resolveCredentialKey({ env: { [CREDENTIAL_KEY_ENV]: KEY.toString('base64') }, dataDir: dir }),
    ).toEqual(KEY);
    expect(existsSync(path.join(dir, KEY_FILE_NAME))).toBe(false);
  });

  test('creates a private key file once and reuses it', () => {
    const dir = tempDir();

    const first = resolveCredentialKey({ env: {}, dataDir: dir });
    const keyFile = path.join(dir, KEY_FILE_NAME);

    expect(first).toHaveLength(32);
    expect(existsSync(keyFile)).toBe(true);
    expect(statSync(keyFile).mode & 0o777).toBe(0o600);
    expect(readFileSync(keyFile, 'utf8').trim()).toBe(first.toString('base64'));
    expect(resolveCredentialKey({ env: {}, dataDir: dir })).toEqual(first);
  });

  test('derives an independent key per data directory', () => {
    expect(resolveCredentialKey({ env: {}, dataDir: tempDir() })).not.toEqual(
      resolveCredentialKey({ env: {}, dataDir: tempDir() }),
    );
  });

  test('fails with a stable code when no key can be established', () => {
    const dir = tempDir();
    const notADirectory = path.join(dir, 'file-not-directory');
    writeFileSync(notADirectory, 'x');

    try {
      resolveCredentialKey({ env: {}, dataDir: notADirectory });
      throw new Error('expected resolveCredentialKey to throw');
    } catch (error) {
      expect(error).toBeInstanceOf(CredentialKeyError);
      expect((error as CredentialKeyError).code).toBe('credential_key_unavailable');
    }
  });

  test('rejects an invalid deployment key instead of falling back to a file', () => {
    const dir = tempDir();

    expect(() => resolveCredentialKey({ env: { [CREDENTIAL_KEY_ENV]: 'not-a-key' }, dataDir: dir })).toThrow(
      CredentialKeyError,
    );
    expect(existsSync(path.join(dir, KEY_FILE_NAME))).toBe(false);
  });
});

describe('credential encryption', () => {
  test('round-trips a secret as a cipher/iv/tag/keyId quartet', () => {
    const secret = encryptCredential('super-secret-value', KEY);

    expect(secret.iv).toHaveLength(12);
    expect(secret.tag).toHaveLength(16);
    expect(secret.cipher.length).toBeGreaterThan(0);
    expect(secret.keyId).toBe(keyIdFor(KEY));
    expect(secret.keyId).toHaveLength(16);
    expect(secret.keyId).toMatch(/^[0-9a-f]{16}$/);

    expect(Buffer.from(secret.cipher).toString('utf8')).not.toContain('super-secret-value');
    expect(decryptCredential(secret, KEY)).toBe('super-secret-value');
  });

  test('uses a fresh nonce for every payload', () => {
    const first = encryptCredential('same', KEY);
    const second = encryptCredential('same', KEY);

    expect(Buffer.from(first.iv)).not.toEqual(Buffer.from(second.iv));
    expect(Buffer.from(first.cipher)).not.toEqual(Buffer.from(second.cipher));
  });

  test('fails authentication when the ciphertext is tampered with', () => {
    const secret = encryptCredential('secret', KEY);
    const tampered = { ...secret, cipher: tamper(secret.cipher) };

    try {
      decryptCredential(tampered, KEY);
      throw new Error('expected decryptCredential to throw');
    } catch (error) {
      expect(error).toBeInstanceOf(CredentialDecryptionError);
      expect((error as CredentialDecryptionError).code).toBe('credential_decryption_failed');
    }
  });

  test('fails authentication when the tag is tampered with', () => {
    const secret = encryptCredential('secret', KEY);

    expect(() => decryptCredential({ ...secret, tag: tamper(secret.tag) }, KEY)).toThrow(
      CredentialDecryptionError,
    );
  });

  test('detects a rotated key by its fingerprint', () => {
    const secret = encryptCredential('secret', KEY);

    try {
      decryptCredential(secret, OTHER_KEY);
      throw new Error('expected decryptCredential to throw');
    } catch (error) {
      expect(error).toBeInstanceOf(CredentialKeyMismatchError);
      expect(error).toBeInstanceOf(CredentialDecryptionError);
      expect((error as CredentialDecryptionError).code).toBe('credential_key_rotated');
    }
  });

  test('rejects malformed iv and tag lengths', () => {
    const secret = encryptCredential('secret', KEY);

    expect(() => decryptCredential({ ...secret, iv: new Uint8Array(8) }, KEY)).toThrow(
      CredentialDecryptionError,
    );
    expect(() => decryptCredential({ ...secret, tag: new Uint8Array(8) }, KEY)).toThrow(
      CredentialDecryptionError,
    );
  });

  test('rejects a forged key id even when the key bytes match', () => {
    const secret = encryptCredential('secret', KEY);

    expect(() => decryptCredential({ ...secret, keyId: '0000000000000000' }, KEY)).toThrow(
      CredentialKeyMismatchError,
    );
  });
});
