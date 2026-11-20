import { afterAll, afterEach, beforeAll, describe, expect, test } from 'bun:test';
import { CREDENTIAL_KEY_ENV } from '../../../../server/platform/hashing';
import { createPlatformRepositories } from '../../../../server/platform/repositories';
import { readCredentialSecret } from '../../../../server/platform/vaults/secrets';
import {
  api,
  CredentialBody,
  createTestContext,
  createVault,
  ListBody,
  readError,
  type TestContext,
  VaultBody,
} from './helpers';

const KEY_HEX = '11'.repeat(32);
const STATIC_SECRET = 'bearer-plaintext-2f6c9a';
const ENVIRONMENT_SECRET = 'environment-plaintext-7d1e4b';

let previousKey: string | undefined;
let context: TestContext | null = null;

beforeAll(() => {
  previousKey = process.env[CREDENTIAL_KEY_ENV];
  process.env[CREDENTIAL_KEY_ENV] = KEY_HEX;
});

afterAll(() => {
  if (previousKey === undefined) delete process.env[CREDENTIAL_KEY_ENV];
  else process.env[CREDENTIAL_KEY_ENV] = previousKey;
});

function start(): TestContext {
  context = createTestContext();
  return context;
}

afterEach(() => {
  context?.db.close();
  context = null;
});

async function createStaticCredential(
  app: TestContext['app'],
  vaultId: string,
  name = 'Deploy token',
  token = STATIC_SECRET,
): Promise<Response> {
  return api(app, 'POST', `/vaults/${vaultId}/credentials`, {
    name,
    auth: { type: 'static_bearer', token },
    metadata: { environment: 'test' },
  });
}

describe('credential CRUD', () => {
  test('creates, lists, retrieves and deletes without exposing the secret', async () => {
    const { app, db } = start();
    const vault = await createVault(app);

    const created = await createStaticCredential(app, vault.id);
    expect(created.status).toBe(201);
    const rawCreated = await created.text();
    expect(rawCreated).not.toContain(STATIC_SECRET);
    const credential = CredentialBody.parse(JSON.parse(rawCreated));
    expect(credential).toEqual({
      id: expect.any(String),
      object: 'vault.credential',
      vault_id: vault.id,
      name: 'Deploy token',
      auth: { type: 'static_bearer', mcp_server_url: '' },
      metadata: { environment: 'test' },
      created_at: expect.any(Number),
      updated_at: expect.any(Number),
    });
    expect(credential.updated_at).toBe(credential.created_at);

    const stored = createPlatformRepositories(db).credentials.get(credential.id);
    if (stored === null) throw new Error('created credential was not persisted');
    expect(readCredentialSecret(stored, Buffer.from(KEY_HEX, 'hex'))).toBe(STATIC_SECRET);
    const cipherBase64 = Buffer.from(stored.secret_cipher).toString('base64');
    expect(cipherBase64.length).toBeGreaterThan(0);
    expect(rawCreated).not.toContain(cipherBase64);
    expect(rawCreated).not.toContain(Buffer.from(stored.secret_cipher).toString('hex'));

    const listedResponse = await api(app, 'GET', `/vaults/${vault.id}/credentials?limit=10`);
    expect(listedResponse.status).toBe(200);
    const rawListed = await listedResponse.text();
    expect(rawListed).not.toContain(STATIC_SECRET);
    const page = ListBody(CredentialBody).parse(JSON.parse(rawListed));
    expect(page.object).toBe('list');
    expect(page.data).toEqual([credential]);
    expect(page.first_id).toBe(credential.id);
    expect(page.last_id).toBe(credential.id);
    expect(page.has_more).toBe(false);

    const fetchedResponse = await api(app, 'GET', `/vaults/${vault.id}/credentials/${credential.id}`);
    expect(fetchedResponse.status).toBe(200);
    const rawFetched = await fetchedResponse.text();
    expect(rawFetched).not.toContain(STATIC_SECRET);
    expect(CredentialBody.parse(JSON.parse(rawFetched))).toEqual(credential);

    const deleted = await api(app, 'DELETE', `/vaults/${vault.id}/credentials/${credential.id}`);
    expect(deleted.status).toBe(200);
    expect(await deleted.json()).toEqual({
      id: credential.id,
      object: 'vault.credential.deleted',
      deleted: true,
    });

    const gone = await api(app, 'GET', `/vaults/${vault.id}/credentials/${credential.id}`);
    expect(gone.status).toBe(404);
    expect((await readError(gone)).error.code).toBe('not_found');

    const vaultStillReadable = await api(app, 'GET', `/vaults/${vault.id}`);
    expect(vaultStillReadable.status).toBe(200);
    expect(VaultBody.parse(await vaultStillReadable.json())).toEqual(vault);
  });

  test('stores an environment variable credential with its non-secret name', async () => {
    const { app } = start();
    const vault = await createVault(app);

    const created = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'Service key',
      auth: {
        type: 'environment_variable',
        secret_name: 'SERVICE_API_KEY',
        secret_value: ENVIRONMENT_SECRET,
      },
    });
    expect(created.status).toBe(201);
    const raw = await created.text();
    expect(raw).not.toContain(ENVIRONMENT_SECRET);
    const credential = CredentialBody.parse(JSON.parse(raw));
    expect(credential.auth).toEqual({
      type: 'environment_variable',
      secret_name: 'SERVICE_API_KEY',
      networking: { type: 'unrestricted' },
    });

    const fetched = await api(app, 'GET', `/vaults/${vault.id}/credentials/${credential.id}`);
    const rawFetched = await fetched.text();
    expect(rawFetched).not.toContain(ENVIRONMENT_SECRET);
    expect(CredentialBody.parse(JSON.parse(rawFetched)).auth).toEqual(credential.auth);
  });

  test('echoes the MCP server URL and networking without any secret', async () => {
    const { app } = start();
    const vault = await createVault(app);

    const bearer = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'Remote MCP',
      auth: {
        type: 'static_bearer',
        mcp_server_url: 'https://mcp.example.test/v1',
        token: STATIC_SECRET,
      },
    });
    expect(bearer.status).toBe(201);
    const rawBearer = await bearer.text();
    expect(rawBearer).not.toContain(STATIC_SECRET);
    expect(CredentialBody.parse(JSON.parse(rawBearer)).auth).toEqual({
      type: 'static_bearer',
      mcp_server_url: 'https://mcp.example.test/v1',
    });

    const environmentVariable = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'Restricted variable',
      auth: {
        type: 'environment_variable',
        secret_name: 'SERVICE_API_KEY',
        secret_value: ENVIRONMENT_SECRET,
        networking: { type: 'limited', allowed_hosts: ['API.Example.Test', '10.0.0.7'] },
      },
    });
    expect(environmentVariable.status).toBe(201);
    const rawEnvironmentVariable = await environmentVariable.text();
    expect(rawEnvironmentVariable).not.toContain(ENVIRONMENT_SECRET);
    expect(CredentialBody.parse(JSON.parse(rawEnvironmentVariable)).auth).toEqual({
      type: 'environment_variable',
      secret_name: 'SERVICE_API_KEY',
      networking: { type: 'limited', allowed_hosts: ['api.example.test', '10.0.0.7'] },
    });
  });

  test('keeps the vault addressable after deleting its credentials', async () => {
    const { app } = start();
    const vault = await createVault(app);

    const first = CredentialBody.parse(await (await createStaticCredential(app, vault.id, 'one')).json());
    const second = CredentialBody.parse(await (await createStaticCredential(app, vault.id, 'two')).json());

    expect((await api(app, 'DELETE', `/vaults/${vault.id}/credentials/${first.id}`)).status).toBe(200);
    const remaining = await api(app, 'GET', `/vaults/${vault.id}/credentials`);
    const page = ListBody(CredentialBody).parse(await remaining.json());
    expect(page.data.map((credential) => credential.id)).toEqual([second.id]);
    expect((await api(app, 'GET', `/vaults/${vault.id}`)).status).toBe(200);
  });
});

describe('credential updates', () => {
  test('patches metadata without rotating the stored secret and rejects name', async () => {
    const { app, db } = start();
    const vault = await createVault(app);
    const credential = CredentialBody.parse(await (await createStaticCredential(app, vault.id)).json());
    await Bun.sleep(1100);

    // RotateVaultCredentialParams defines only auth/metadata; name stays immutable on update.
    const rejected = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {
      name: 'Renamed token',
    });
    expect(rejected.status).toBe(400);
    expect((await readError(rejected)).error.code).toBe('invalid_input');

    const response = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {
      metadata: { environment: 'staging' },
    });
    expect(response.status).toBe(200);
    const raw = await response.text();
    expect(raw).not.toContain(STATIC_SECRET);
    const updated = CredentialBody.parse(JSON.parse(raw));
    expect(updated).toEqual({
      ...credential,
      metadata: { environment: 'staging' },
      updated_at: expect.any(Number),
    });
    expect(updated.updated_at).toBeGreaterThan(credential.updated_at);
    expect(updated.auth).toEqual(credential.auth);

    const stored = createPlatformRepositories(db).credentials.get(credential.id);
    if (stored === null) throw new Error('updated credential was not persisted');
    expect(readCredentialSecret(stored, Buffer.from(KEY_HEX, 'hex'))).toBe(STATIC_SECRET);

    const fetched = await api(app, 'GET', `/vaults/${vault.id}/credentials/${credential.id}`);
    expect(CredentialBody.parse(await fetched.json())).toEqual(updated);
  });

  test('rotates a static bearer token while keeping the stored descriptor', async () => {
    const { app, db } = start();
    const vault = await createVault(app);
    const credential = CredentialBody.parse(
      await (
        await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
          name: 'Rotating bearer',
          auth: {
            type: 'static_bearer',
            mcp_server_url: 'https://mcp.example.test/v1',
            token: STATIC_SECRET,
          },
        })
      ).json(),
    );
    const replacement = 'bearer-rotated-5c9e';

    const response = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {
      auth: { type: 'static_bearer', token: replacement },
    });
    expect(response.status).toBe(200);
    const raw = await response.text();
    expect(raw).not.toContain(replacement);
    expect(raw).not.toContain(STATIC_SECRET);
    const updated = CredentialBody.parse(JSON.parse(raw));
    expect(updated.name).toBe('Rotating bearer');
    expect(updated.auth).toEqual({
      type: 'static_bearer',
      mcp_server_url: 'https://mcp.example.test/v1',
    });

    const stored = createPlatformRepositories(db).credentials.get(credential.id);
    if (stored === null) throw new Error('rotated credential was not persisted');
    expect(readCredentialSecret(stored, Buffer.from(KEY_HEX, 'hex'))).toBe(replacement);
  });

  test('rotates an environment variable secret without changing its destination config', async () => {
    const { app, db } = start();
    const vault = await createVault(app);
    const credential = CredentialBody.parse(
      await (
        await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
          name: 'Rotating variable',
          auth: {
            type: 'environment_variable',
            secret_name: 'SERVICE_API_KEY',
            secret_value: ENVIRONMENT_SECRET,
            networking: { type: 'limited', allowed_hosts: ['api.example.test'] },
          },
        })
      ).json(),
    );
    const replacement = 'environment-rotated-3b7f';

    const response = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {
      auth: { type: 'environment_variable', secret_value: replacement },
    });
    expect(response.status).toBe(200);
    const raw = await response.text();
    expect(raw).not.toContain(replacement);
    expect(raw).not.toContain(ENVIRONMENT_SECRET);
    const updated = CredentialBody.parse(JSON.parse(raw));
    expect(updated.auth).toEqual({
      type: 'environment_variable',
      secret_name: 'SERVICE_API_KEY',
      networking: { type: 'limited', allowed_hosts: ['api.example.test'] },
    });

    const stored = createPlatformRepositories(db).credentials.get(credential.id);
    if (stored === null) throw new Error('rotated credential was not persisted');
    expect(readCredentialSecret(stored, Buffer.from(KEY_HEX, 'hex'))).toBe(replacement);
  });

  test('rejects changing the auth method, unsupported auth and empty updates', async () => {
    const { app } = start();
    const vault = await createVault(app);
    const credential = CredentialBody.parse(await (await createStaticCredential(app, vault.id)).json());

    const changedMethod = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {
      auth: { type: 'environment_variable', secret_value: 'replacement-secret' },
    });
    expect(changedMethod.status).toBe(400);
    const changedBody = await readError(changedMethod);
    expect(changedBody.error.code).toBe('invalid_input');
    expect(changedBody.error.param).toBe('auth.type');
    expect(changedBody.error.message).not.toContain('replacement-secret');

    const unsupported = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {
      auth: { type: 'mcp_oauth', access_token: 'oauth-secret' },
    });
    expect(unsupported.status).toBe(400);
    expect((await readError(unsupported)).error.code).toBe('credential_auth_unsupported');

    const empty = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {});
    expect(empty.status).toBe(400);
    expect((await readError(empty)).error.code).toBe('invalid_input');

    const unknownField = await api(app, 'POST', `/vaults/${vault.id}/credentials/${credential.id}`, {
      unexpected: true,
    });
    expect(unknownField.status).toBe(400);
    expect((await readError(unknownField)).error.code).toBe('invalid_input');

    const unchanged = await api(app, 'GET', `/vaults/${vault.id}/credentials/${credential.id}`);
    expect(CredentialBody.parse(await unchanged.json())).toEqual(credential);
  });

  test('returns 404 when the credential is missing or belongs to another vault', async () => {
    const { app } = start();
    const owner = await createVault(app, 'Owner');
    const other = await createVault(app, 'Other');
    const credential = CredentialBody.parse(await (await createStaticCredential(app, owner.id)).json());

    const missing = await api(
      app,
      'POST',
      `/vaults/${owner.id}/credentials/00000000-0000-7000-8000-000000000001`,
      { metadata: {} },
    );
    expect(missing.status).toBe(404);
    expect((await readError(missing)).error.code).toBe('not_found');

    const crossVault = await api(app, 'POST', `/vaults/${other.id}/credentials/${credential.id}`, {
      metadata: {},
    });
    expect(crossVault.status).toBe(404);

    const stillThere = await api(app, 'GET', `/vaults/${owner.id}/credentials/${credential.id}`);
    expect(CredentialBody.parse(await stillThere.json()).metadata).toEqual(credential.metadata);
  });
});

describe('credential pagination', () => {
  test('pages by ID and honours after and order', async () => {
    const { app } = start();
    const vault = await createVault(app);
    const ids: string[] = [];
    for (const name of ['one', 'two', 'three']) {
      const response = await createStaticCredential(app, vault.id, name, `secret-${name}`);
      ids.push(CredentialBody.parse(await response.json()).id);
    }

    const newestFirst = [...ids].reverse();

    const firstPageResponse = await api(app, 'GET', `/vaults/${vault.id}/credentials?limit=2`);
    const firstPage = ListBody(CredentialBody).parse(await firstPageResponse.json());
    expect(firstPage.data.map((credential) => credential.id)).toEqual(newestFirst.slice(0, 2));
    expect(firstPage.has_more).toBe(true);

    const nextPageResponse = await api(
      app,
      'GET',
      `/vaults/${vault.id}/credentials?limit=2&after=${firstPage.last_id}`,
    );
    const nextPage = ListBody(CredentialBody).parse(await nextPageResponse.json());
    expect(nextPage.data.map((credential) => credential.id)).toEqual(newestFirst.slice(2));
    expect(nextPage.has_more).toBe(false);

    const ascendingResponse = await api(app, 'GET', `/vaults/${vault.id}/credentials?order=asc`);
    const ascending = ListBody(CredentialBody).parse(await ascendingResponse.json());
    expect(ascending.data.map((credential) => credential.id)).toEqual(ids);
  });
});

describe('credential validation', () => {
  test('rejects unsupported auth types with a stable code', async () => {
    const { app } = start();
    const vault = await createVault(app);

    for (const auth of [
      { type: 'mcp_oauth', mcp_server_url: 'https://example.test/mcp', access_token: 'oauth-token' },
      { type: 'api_key', value: 'key-value' },
    ]) {
      const response = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
        name: 'Unsupported',
        auth,
      });
      expect(response.status).toBe(400);
      const body = await readError(response);
      expect(body.error.code).toBe('credential_auth_unsupported');
      expect(body.error.type).toBe('invalid_request_error');
      expect(body.error.param).toBe('auth.type');
      expect(body.error.message).not.toContain('oauth-token');
      expect(body.error.message).not.toContain('key-value');
    }

    const listed = await api(app, 'GET', `/vaults/${vault.id}/credentials`);
    expect(ListBody(CredentialBody).parse(await listed.json()).data).toEqual([]);
  });

  test('rejects unknown fields and invalid auth payloads', async () => {
    const { app } = start();
    const vault = await createVault(app);

    const unknownTopLevel = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'Extra',
      auth: { type: 'static_bearer', token: 'token-value' },
      unexpected: true,
    });
    expect(unknownTopLevel.status).toBe(400);
    expect((await readError(unknownTopLevel)).error.code).toBe('invalid_input');

    const unknownAuthField = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'Extra auth',
      auth: { type: 'static_bearer', token: 'token-value', scope: 'read' },
    });
    expect(unknownAuthField.status).toBe(400);
    const authBody = await readError(unknownAuthField);
    expect(authBody.error.code).toBe('invalid_input');
    expect(authBody.error.param).toBe('auth.scope');

    const missingAuth = await api(app, 'POST', `/vaults/${vault.id}/credentials`, { name: 'No auth' });
    expect(missingAuth.status).toBe(400);
    expect((await readError(missingAuth)).error.code).toBe('invalid_input');

    const missingToken = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'No token',
      auth: { type: 'static_bearer' },
    });
    expect(missingToken.status).toBe(400);

    const blankSecretName = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'Blank name',
      auth: { type: 'environment_variable', secret_name: '', secret_value: 'value' },
    });
    expect(blankSecretName.status).toBe(400);

    const seventeen = Object.fromEntries(
      Array.from({ length: 17 }, (_value, index) => [`key_${index}`, 'value']),
    );
    const tooMuchMetadata = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
      name: 'Metadata',
      auth: { type: 'static_bearer', token: 'token-value' },
      metadata: seventeen,
    });
    expect(tooMuchMetadata.status).toBe(400);
  });

  test('returns 404 for unknown vaults and credentials', async () => {
    const { app } = start();
    const missingVaultId = '00000000-0000-7000-8000-000000000000';

    const missingList = await api(app, 'GET', `/vaults/${missingVaultId}/credentials`);
    expect(missingList.status).toBe(404);
    expect((await readError(missingList)).error.code).toBe('not_found');

    const missingCreate = await api(app, 'POST', `/vaults/${missingVaultId}/credentials`, {
      name: 'Orphan',
      auth: { type: 'static_bearer', token: 'token-value' },
    });
    expect(missingCreate.status).toBe(404);

    const vault = await createVault(app);
    const missingCredential = await api(
      app,
      'GET',
      `/vaults/${vault.id}/credentials/00000000-0000-7000-8000-000000000001`,
    );
    expect(missingCredential.status).toBe(404);

    const missingDelete = await api(
      app,
      'DELETE',
      `/vaults/${vault.id}/credentials/00000000-0000-7000-8000-000000000001`,
    );
    expect(missingDelete.status).toBe(404);
  });

  test('scopes credentials to their vault', async () => {
    const { app } = start();
    const owner = await createVault(app, 'Owner');
    const other = await createVault(app, 'Other');
    const credential = CredentialBody.parse(await (await createStaticCredential(app, owner.id)).json());

    const crossRead = await api(app, 'GET', `/vaults/${other.id}/credentials/${credential.id}`);
    expect(crossRead.status).toBe(404);

    const crossDelete = await api(app, 'DELETE', `/vaults/${other.id}/credentials/${credential.id}`);
    expect(crossDelete.status).toBe(404);

    const stillThere = await api(app, 'GET', `/vaults/${owner.id}/credentials/${credential.id}`);
    expect(stillThere.status).toBe(200);
  });
});

describe('vault cascade deletion', () => {
  test('deletes the vault and every credential it contains', async () => {
    const { app, db } = start();
    const vault = await createVault(app);
    const first = CredentialBody.parse(await (await createStaticCredential(app, vault.id, 'one')).json());
    const second = CredentialBody.parse(await (await createStaticCredential(app, vault.id, 'two')).json());
    const repos = createPlatformRepositories(db);

    expect((await api(app, 'DELETE', `/vaults/${vault.id}`)).status).toBe(200);
    expect((await api(app, 'GET', `/vaults/${vault.id}`)).status).toBe(404);
    expect((await api(app, 'GET', `/vaults/${vault.id}/credentials`)).status).toBe(404);
    expect((await api(app, 'GET', `/vaults/${vault.id}/credentials/${first.id}`)).status).toBe(404);
    expect(repos.credentials.get(first.id)).toBeNull();
    expect(repos.credentials.get(second.id)).toBeNull();
    const remaining = db
      .query<{ count: number }, [string]>(
        'SELECT COUNT(*) AS count FROM platform_credentials WHERE vault_id = ?',
      )
      .get(vault.id);
    expect(remaining?.count).toBe(0);
  });
});

describe('credential key failures', () => {
  test('maps an unusable encryption key to 500 credential_unavailable', async () => {
    const { app } = start();
    const vault = await createVault(app);
    const configured = process.env[CREDENTIAL_KEY_ENV];
    process.env[CREDENTIAL_KEY_ENV] = 'not-a-key';
    try {
      const response = await api(app, 'POST', `/vaults/${vault.id}/credentials`, {
        name: 'Unavailable',
        auth: { type: 'static_bearer', token: 'token-value' },
      });
      expect(response.status).toBe(500);
      const body = await readError(response);
      expect(body.error.code).toBe('credential_unavailable');
      expect(body.error.type).toBe('server_error');
      expect(body.error.message).not.toContain('PLATFORM_CREDENTIAL_KEY');
    } finally {
      if (configured === undefined) delete process.env[CREDENTIAL_KEY_ENV];
      else process.env[CREDENTIAL_KEY_ENV] = configured;
    }

    const listed = await api(app, 'GET', `/vaults/${vault.id}/credentials`);
    expect(ListBody(CredentialBody).parse(await listed.json()).data).toEqual([]);
  });
});
