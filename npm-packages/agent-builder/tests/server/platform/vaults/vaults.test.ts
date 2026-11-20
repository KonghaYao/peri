import { afterEach, describe, expect, test } from 'bun:test';
import {
  api,
  createTestContext,
  createVault,
  ListBody,
  readError,
  type TestContext,
  VaultBody,
} from './helpers';

let context: TestContext | null = null;

function start(): TestContext {
  context = createTestContext();
  return context;
}

afterEach(() => {
  context?.db.close();
  context = null;
});

describe('vault CRUD', () => {
  test('creates, retrieves and deletes a vault with the documented shape', async () => {
    const { app } = start();
    const before = Math.floor(Date.now() / 1000);

    const created = await api(app, 'POST', '/vaults', {
      name: '  Team secrets  ',
      metadata: { team: 'core' },
    });
    expect(created.status).toBe(201);
    const vault = VaultBody.parse(await created.json());
    expect(vault.name).toBe('Team secrets');
    expect(vault.metadata).toEqual({ team: 'core' });
    expect(vault.created_at).toBeGreaterThanOrEqual(before - 1);
    expect(vault.created_at).toBeLessThanOrEqual(Math.floor(Date.now() / 1000) + 1);

    const fetched = await api(app, 'GET', `/vaults/${vault.id}`);
    expect(fetched.status).toBe(200);
    expect(VaultBody.parse(await fetched.json())).toEqual(vault);

    const deleted = await api(app, 'DELETE', `/vaults/${vault.id}`);
    expect(deleted.status).toBe(200);
    expect(await deleted.json()).toEqual({ id: vault.id, object: 'vault.deleted', deleted: true });

    const missing = await api(app, 'GET', `/vaults/${vault.id}`);
    expect(missing.status).toBe(404);
    expect((await readError(missing)).error.code).toBe('not_found');

    const deletedAgain = await api(app, 'DELETE', `/vaults/${vault.id}`);
    expect(deletedAgain.status).toBe(404);
  });

  test('defaults metadata to an empty map and accepts an explicit null', async () => {
    const { app } = start();

    const implicit = await api(app, 'POST', '/vaults', { name: 'Implicit' });
    expect(VaultBody.parse(await implicit.json()).metadata).toEqual({});

    const explicit = await api(app, 'POST', '/vaults', { name: 'Explicit', metadata: null });
    expect(explicit.status).toBe(201);
    expect(VaultBody.parse(await explicit.json()).metadata).toEqual({});
  });

  test('patches name and metadata, clears fields and bumps updated_at', async () => {
    const { app, db } = start();
    const vault = await createVault(app, 'Original');
    const before = db
      .query<{ updated_at: number }, [string]>('SELECT updated_at FROM platform_vaults WHERE id = ?')
      .get(vault.id);
    if (before === null) throw new Error('created vault was not persisted');

    await Bun.sleep(5);
    const updated = await api(app, 'POST', `/vaults/${vault.id}`, {
      name: '  Renamed  ',
      metadata: { team: 'ops' },
    });
    expect(updated.status).toBe(200);
    expect(VaultBody.parse(await updated.json())).toEqual({
      ...vault,
      name: 'Renamed',
      metadata: { team: 'ops' },
    });

    const after = db
      .query<{ updated_at: number }, [string]>('SELECT updated_at FROM platform_vaults WHERE id = ?')
      .get(vault.id);
    expect(after?.updated_at).toBeGreaterThan(before.updated_at);

    const fetched = await api(app, 'GET', `/vaults/${vault.id}`);
    expect(VaultBody.parse(await fetched.json())).toEqual({
      ...vault,
      name: 'Renamed',
      metadata: { team: 'ops' },
    });

    // A null name clears the documented nullable field; omitted metadata keeps its value.
    const cleared = await api(app, 'POST', `/vaults/${vault.id}`, { name: null });
    expect(cleared.status).toBe(200);
    const clearedVault = VaultBody.parse(await cleared.json());
    expect(clearedVault.name).toBeNull();
    expect(clearedVault.metadata).toEqual({ team: 'ops' });

    // An explicit empty map clears metadata; omitted name keeps the cleared value.
    const emptied = await api(app, 'POST', `/vaults/${vault.id}`, { metadata: {} });
    const emptiedVault = VaultBody.parse(await emptied.json());
    expect(emptiedVault.name).toBeNull();
    expect(emptiedVault.metadata).toEqual({});
  });

  test('rejects updates for an unknown vault and unknown fields', async () => {
    const { app } = start();
    const missing = await api(app, 'POST', '/vaults/00000000-0000-7000-8000-000000000000', {
      name: 'Missing',
    });
    expect(missing.status).toBe(404);
    expect((await readError(missing)).error.code).toBe('not_found');

    const vault = await createVault(app);
    const unknownField = await api(app, 'POST', `/vaults/${vault.id}`, { unexpected: true });
    expect(unknownField.status).toBe(400);
    expect((await readError(unknownField)).error.code).toBe('invalid_input');

    const blankName = await api(app, 'POST', `/vaults/${vault.id}`, { name: '   ' });
    expect(blankName.status).toBe(400);
  });

  test('returns 404 for an unknown vault', async () => {
    const { app } = start();

    const response = await api(app, 'GET', '/vaults/00000000-0000-7000-8000-000000000000');
    expect(response.status).toBe(404);
    const body = await readError(response);
    expect(body.error.type).toBe('not_found_error');
    expect(body.error.param).toBeNull();
  });
});

describe('vault pagination', () => {
  test('pages by ID in descending order and honours after', async () => {
    const { app } = start();
    const ids: string[] = [];
    for (const name of ['first', 'second', 'third']) {
      ids.push((await createVault(app, name)).id);
    }

    const newestFirst = [...ids].reverse();

    const firstPageResponse = await api(app, 'GET', '/vaults?limit=2');
    expect(firstPageResponse.status).toBe(200);
    const firstPage = ListBody(VaultBody).parse(await firstPageResponse.json());
    expect(firstPage.data.map((vault) => vault.id)).toEqual(newestFirst.slice(0, 2));
    expect(firstPage.first_id).toBe(newestFirst[0] ?? null);
    expect(firstPage.last_id).toBe(newestFirst[1] ?? null);
    expect(firstPage.has_more).toBe(true);

    const secondPageResponse = await api(app, 'GET', `/vaults?limit=2&after=${firstPage.last_id}`);
    const secondPage = ListBody(VaultBody).parse(await secondPageResponse.json());
    expect(secondPage.data.map((vault) => vault.id)).toEqual(newestFirst.slice(2));
    expect(secondPage.first_id).toBe(newestFirst[2] ?? null);
    expect(secondPage.last_id).toBe(newestFirst[2] ?? null);
    expect(secondPage.has_more).toBe(false);

    const ascendingResponse = await api(app, 'GET', '/vaults?order=asc&limit=10');
    const ascending = ListBody(VaultBody).parse(await ascendingResponse.json());
    expect(ascending.data.map((vault) => vault.id)).toEqual(ids);
  });

  test('returns an empty page with null cursors', async () => {
    const { app } = start();

    const response = await api(app, 'GET', '/vaults');
    const page = ListBody(VaultBody).parse(await response.json());
    expect(page).toEqual({
      object: 'list',
      data: [],
      first_id: null,
      last_id: null,
      has_more: false,
    });
  });

  test('rejects invalid limit and order values', async () => {
    const { app } = start();

    for (const query of ['limit=0', 'limit=abc', 'order=sideways']) {
      const response = await api(app, 'GET', `/vaults?${query}`);
      expect(response.status).toBe(400);
      const body = await readError(response);
      expect(body.error.code).toBe('invalid_input');
      expect(body.error.param).toBe(query.startsWith('limit') ? 'limit' : 'order');
    }
  });
});

describe('vault validation', () => {
  test('requires a non-blank name and rejects unknown fields', async () => {
    const { app } = start();

    const missingName = await api(app, 'POST', '/vaults', {});
    expect(missingName.status).toBe(400);
    const missingBody = await readError(missingName);
    expect(missingBody.error.code).toBe('invalid_input');
    expect(missingBody.error.param).toBe('name');

    const blankName = await api(app, 'POST', '/vaults', { name: '   ' });
    expect(blankName.status).toBe(400);

    const unknownField = await api(app, 'POST', '/vaults', { name: 'Valid', unexpected: true });
    expect(unknownField.status).toBe(400);
    expect((await readError(unknownField)).error.code).toBe('invalid_input');
  });

  test('enforces the metadata size limits and accepts the documented maximum', async () => {
    const { app } = start();

    const seventeen = Object.fromEntries(
      Array.from({ length: 17 }, (_value, index) => [`key_${index}`, 'value']),
    );
    const tooManyPairs = await api(app, 'POST', '/vaults', { name: 'Pairs', metadata: seventeen });
    expect(tooManyPairs.status).toBe(400);
    expect((await readError(tooManyPairs)).error.code).toBe('invalid_input');

    const tooLongKey = await api(app, 'POST', '/vaults', {
      name: 'Key',
      metadata: { ['k'.repeat(65)]: 'value' },
    });
    expect(tooLongKey.status).toBe(400);

    const tooLongValue = await api(app, 'POST', '/vaults', {
      name: 'Value',
      metadata: { key: 'v'.repeat(513) },
    });
    expect(tooLongValue.status).toBe(400);

    const metadata: Record<string, string> = {};
    for (let index = 0; index < 16; index += 1) {
      const prefix = `key_${index}`;
      metadata[prefix.padEnd(64, 'x')] = 'v'.repeat(512);
    }
    const accepted = await api(app, 'POST', '/vaults', { name: 'Maximum metadata', metadata });
    expect(accepted.status).toBe(201);
    expect(VaultBody.parse(await accepted.json()).metadata).toEqual(metadata);
  });
});
