import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import { randomBytes } from 'node:crypto';
import { AgentDraft, AgentPreviewResponse, AgentResponse } from '@shared/agent';
import { ApiErrorBody } from '@shared/api';
import { ProviderCreate, ProviderResponse } from '@shared/provider';
import { Hono } from 'hono';
import { AgentsRepository } from '../../../server/agents/repository';
import { createApp } from '../../../server/app';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { errorResponse } from '../../../server/errors';
import { ProvidersRepository } from '../../../server/providers/repository';
import { createProvidersRoutes } from '../../../server/providers/routes';
import { ProviderService } from '../../../server/providers/service';
import { testEnv } from '../helpers';

let db: Database;
afterEach(() => db?.close());
function setup() {
  db = openDatabase(':memory:');
  migrate(db);
  const env = testEnv();
  const repository = new ProvidersRepository(db);
  return {
    env,
    repository,
    service: new ProviderService(repository),
    agents: new AgentsRepository(db),
    app: createApp({ db, env }),
  };
}
function input(key?: string) {
  return ProviderCreate.parse({
    name: 'Shared',
    type: 'anthropic',
    credentialSource: 'local',
    ...(key ? { credential: { action: 'replace', value: key } } : {}),
  });
}
function request(method: string, body?: unknown, revision?: number) {
  return {
    method,
    headers: { 'content-type': 'application/json', ...(revision ? { 'If-Match': String(revision) } : {}) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  };
}

describe('Provider persistence and API', () => {
  test('failed metadata/source/credential update rolls back together, including stale CAS and invalid association', async () => {
    const { app, repository, agents } = setup();
    const key = randomBytes(32).toString('hex');
    const created = ProviderResponse.parse(
      await (await app.request('/api/providers', request('POST', input(key)))).json(),
    ).provider;
    db.exec(
      "CREATE TRIGGER reject_provider_update BEFORE UPDATE ON providers BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END",
    );
    const failed = await app.request(
      `/api/providers/${created.id}`,
      request('PUT', { name: 'Candidate', baseUrl: null, credential: { action: 'clear' } }, 1),
    );
    expect(failed.status).toBe(500);
    expect((await failed.text()).includes(key)).toBe(false);
    const record = repository.get(created.id);
    expect(record?.apiKey === key).toBe(true);
    expect(record?.credentialSource).toBe('local');
    expect(record?.revision).toBe(1);
    expect(record?.name).toBe('Shared');
    const draft = AgentDraft.parse({
      name: 'Original',
      config: { provider: { kind: 'reference', providerId: created.id, model: 'm' } },
    });
    const agent = agents.create(draft);
    expect(() =>
      agents.update(
        agent.id,
        AgentDraft.parse({
          ...draft,
          name: 'Candidate',
          config: { provider: { kind: 'reference', providerId: crypto.randomUUID(), model: 'changed' } },
        }),
        1,
      ),
    ).toThrow();
    expect(agents.get(agent.id)).toEqual(agent);
  });
  test('safe CRUD never returns the independently stored local credential; omitted/replace/clear are atomic', async () => {
    const { app, repository } = setup();
    const key = randomBytes(32).toString('hex');
    const response = await app.request('/api/providers', request('POST', input(key)));
    expect(response.status).toBe(201);
    expect(response.headers.get('Cache-Control')).toBe('no-store');
    const raw = await response.text();
    expect(raw.includes(key)).toBe(false);
    expect(raw.includes('apiKey')).toBe(false);
    let provider = ProviderResponse.parse(JSON.parse(raw)).provider;
    expect(provider.hasCredential).toBe(true);
    const update = { name: 'Renamed', baseUrl: null, credentialSource: 'local' };
    const put = await app.request(`/api/providers/${provider.id}`, request('PUT', update, provider.revision));
    provider = ProviderResponse.parse(await put.json()).provider;
    expect(provider.revision).toBe(2);
    expect(repository.get(provider.id)?.apiKey === key).toBe(true);
    const replacement = randomBytes(32).toString('hex');
    await app.request(
      `/api/providers/${provider.id}`,
      request('PUT', { ...update, credential: { action: 'replace', value: replacement } }, 2),
    );
    expect(repository.get(provider.id)?.apiKey === replacement).toBe(true);
    const clear = await app.request(
      `/api/providers/${provider.id}`,
      request('PUT', { ...update, credential: { action: 'clear' } }, 3),
    );
    expect(ProviderResponse.parse(await clear.json()).provider.hasCredential).toBe(false);
    const get = await app.request(`/api/providers/${provider.id}`);
    expect(get.status).toBe(200);
    const list = await (await app.request('/api/providers')).text();
    expect(list.includes(key) || list.includes(replacement)).toBe(false);
    expect((await app.request(`/api/providers/${provider.id}`, request('DELETE', undefined, 4))).status).toBe(
      204,
    );
  });

  test('Provider and Agent stale or missing baselines return revision_conflict without publishing candidates', async () => {
    const { app } = setup();
    const provider = ProviderResponse.parse(
      await (await app.request('/api/providers', request('POST', input()))).json(),
    ).provider;
    for (const revision of [undefined, 8]) {
      const result = await app.request(
        `/api/providers/${provider.id}`,
        request('PUT', { name: 'Bad', credentialSource: 'local' }, revision),
      );
      expect(result.status).toBe(409);
      expect(ApiErrorBody.parse(await result.json()).error.code).toBe('revision_conflict');
    }
    expect(
      ProviderResponse.parse(await (await app.request(`/api/providers/${provider.id}`)).json()).provider.name,
    ).toBe('Shared');
    const draft = AgentDraft.parse({
      name: 'A',
      config: { provider: { kind: 'reference', providerId: provider.id, model: 'model-a' } },
    });
    const agent = AgentResponse.parse(
      await (await app.request('/api/agents', request('POST', draft))).json(),
    ).agent;
    expect((await app.request(`/api/agents/${agent.id}`, request('PUT', draft))).status).toBe(409);
    const saved = AgentResponse.parse(
      await (await app.request(`/api/agents/${agent.id}`, request('PUT', { ...draft, name: 'B' }, 1))).json(),
    ).agent;
    expect(saved.revision).toBe(2);
    const conflict = await app.request(`/api/agents/${agent.id}`, request('DELETE', undefined, 1));
    expect(ApiErrorBody.parse(await conflict.json()).error.code).toBe('revision_conflict');
    const refusedDelete = await app.request(`/api/providers/${provider.id}`, request('DELETE', undefined, 1));
    expect(ApiErrorBody.parse(await refusedDelete.json()).error.code).toBe('provider_in_use');
    expect((await app.request(`/api/agents/${agent.id}`, request('DELETE', undefined, 2))).status).toBe(204);
    expect((await app.request(`/api/providers/${provider.id}`, request('DELETE', undefined, 1))).status).toBe(
      204,
    );
  });

  test('reference storage has only model and FK, null/inline roundtrip with no automatic id association', () => {
    const { service, agents } = setup();
    const provider = service.create(input());
    const shared = agents.create(
      AgentDraft.parse({
        name: 'Ref',
        config: { provider: { kind: 'reference', providerId: provider.id, model: 'own-model' } },
      }),
    );
    const row = db
      .query<{ config: string; provider_id: string }, [string]>(
        'SELECT config, provider_id FROM agents WHERE id = ?',
      )
      .get(shared.id);
    expect(JSON.parse(row?.config ?? '').provider).toEqual({ model: 'own-model' });
    expect(row?.provider_id).toBe(provider.id);
    expect(agents.get(shared.id)?.config).toEqual(shared.config);
    expect(() => service.delete(provider.id, 1)).toThrow();
    for (const config of [
      { provider: null },
      { provider: { id: provider.id, type: 'anthropic', model: 'legacy' } },
    ]) {
      const draft = AgentDraft.parse({ name: 'Legacy', config });
      const agent = agents.create(draft);
      expect(agents.get(agent.id)?.config).toEqual(draft.config);
      expect(
        db
          .query<{ provider_id: string | null }, [string]>('SELECT provider_id FROM agents WHERE id = ?')
          .get(agent.id)?.provider_id,
      ).toBeNull();
    }
    expect(() =>
      agents.create(
        AgentDraft.parse({
          name: 'Dangling',
          config: { provider: { kind: 'reference', providerId: crypto.randomUUID(), model: 'm' } },
        }),
      ),
    ).toThrow();
  });

  test('POST/PUT accept only SQLite credentials and reject env without changing a saved key', async () => {
    const { app, repository } = setup();
    const key = randomBytes(32).toString('hex');
    const rejected = await app.request(
      '/api/providers',
      request('POST', { ...input(key), credentialSource: 'env' }),
    );
    expect(rejected.status).toBe(400);
    expect((await rejected.text()).includes(key)).toBe(false);
    expect(repository.list()).toHaveLength(0);
    const { credentialSource: _source, ...sqlite } = input(key);
    const created = await app.request('/api/providers', request('POST', sqlite));
    expect(created.status).toBe(201);
    const provider = ProviderResponse.parse(await created.json()).provider;
    const failed = await app.request(
      `/api/providers/${provider.id}`,
      request(
        'PUT',
        {
          name: 'Rejected',
          credentialSource: 'env',
          credential: { action: 'clear' },
        },
        1,
      ),
    );
    expect(failed.status).toBe(400);
    expect(ApiErrorBody.parse(await failed.json()).error.code).toBe('invalid_input');
    expect(repository.get(provider.id)?.apiKey === key).toBe(true);
    expect(repository.get(provider.id)?.revision).toBe(1);
    expect(repository.get(provider.id)?.name).toBe('Shared');
  });

  test.each(['replace', 'clear'] as const)(
    'concurrent %s and metadata CAS publish one atomic candidate',
    async (action) => {
      const { app, repository } = setup();
      const first = randomBytes(32).toString('hex');
      const next = randomBytes(32).toString('hex');
      const provider = ProviderResponse.parse(
        await (await app.request('/api/providers', request('POST', input(first)))).json(),
      ).provider;
      const credential = action === 'replace' ? { action, value: next } : { action };
      const results = await Promise.all([
        app.request(
          `/api/providers/${provider.id}`,
          request('PUT', { name: 'Credential winner', credential }, 1),
        ),
        app.request(`/api/providers/${provider.id}`, request('PUT', { name: 'Metadata winner' }, 1)),
      ]);
      expect(results.map((item) => item.status).sort()).toEqual([200, 409]);
      const record = repository.get(provider.id);
      expect(record?.revision).toBe(2);
      expect(record?.credentialSource).toBe('local');
      const credentialWon = results[0]?.status === 200;
      expect(record?.name).toBe(credentialWon ? 'Credential winner' : 'Metadata winner');
      expect(record?.apiKey === (credentialWon ? (action === 'clear' ? null : next) : first)).toBe(true);
      for (const result of results) {
        const body = await result.text();
        expect(body.includes(first) || body.includes(next)).toBe(false);
      }
    },
  );

  test('legacy env rows stay unconfigured until explicit SQLite replace; reads never change revision', () => {
    const { service, repository } = setup();
    const local = service.create(input());
    const ref = { kind: 'reference' as const, providerId: local.id, model: 'm' };
    expect(local.hasCredential).toBe(false);
    expect(service.resolve(ref).apiKey).toBeUndefined();
    db.query("UPDATE providers SET credential_source = 'env' WHERE id = ?").run(local.id);
    expect(service.get(local.id).hasCredential).toBe(false);
    expect(service.list()[0]?.credentialSource).toBe('local');
    expect(repository.get(local.id)?.credentialSource).toBe('env');
    expect(repository.get(local.id)?.revision).toBe(1);
    expect(service.resolve(ref).apiKey).toBeUndefined();
    const key = randomBytes(32).toString('hex');
    const saved = service.update(
      local.id,
      {
        name: local.name,
        baseUrl: null,
        credentialSource: 'local',
        credential: { action: 'replace', value: key },
      },
      1,
    );
    expect(saved.hasCredential).toBe(true);
    expect(repository.get(local.id)?.credentialSource).toBe('local');
    expect(saved.revision).toBe(2);
    expect(service.resolve(ref).apiKey === key).toBe(true);
    expect(
      service.update(
        local.id,
        {
          name: local.name,
          baseUrl: null,
          credentialSource: 'local',
          credential: { action: 'clear' },
        },
        2,
      ).hasCredential,
    ).toBe(false);
    expect(service.resolve(ref).apiKey).toBeUndefined();
  });

  test('rejects unsafe URLs, protocol changes and credential echo; origin reuse requires explicit confirmation', async () => {
    const { app } = setup();
    const key = randomBytes(24).toString('hex');
    for (const baseUrl of [
      ' https://example.com',
      'https://example.com ',
      'http://remote.example',
      `https://user:${key}@example.com`,
      'https://example.com/?',
      'https://example.com/#',
      'file:///tmp/x',
      'https://@example.com',
      'http://127.evil.example',
      'https://example.com/\u0000',
    ]) {
      const response = await app.request('/api/providers', request('POST', { ...input(key), baseUrl }));
      expect(response.status).toBe(400);
      expect((await response.text()).includes(key)).toBe(false);
    }
    const provider = ProviderResponse.parse(
      await (await app.request('/api/providers', request('POST', input(key)))).json(),
    ).provider;
    const update = { name: 'New', credentialSource: 'local', baseUrl: 'https://other.example/v1' };
    const refused = await app.request(`/api/providers/${provider.id}`, request('PUT', update, 1));
    expect(ApiErrorBody.parse(await refused.json()).error.code).toBe(
      'credential_origin_confirmation_required',
    );
    expect(
      (
        await app.request(
          `/api/providers/${provider.id}`,
          request('PUT', { ...update, confirmCredentialOriginChange: true }, 1),
        )
      ).status,
    ).toBe(200);
    const immutable = await app.request(
      `/api/providers/${provider.id}`,
      request('PUT', { ...update, type: 'openai' }, 2),
    );
    expect(immutable.status).toBe(400);
  });

  test('reference preview uses the same resolver without exposing secrets or persisting/opening chats', async () => {
    const { app } = setup();
    const key = randomBytes(24).toString('hex');
    const provider = ProviderResponse.parse(
      await (await app.request('/api/providers', request('POST', input(key)))).json(),
    ).provider;
    const draft = AgentDraft.parse({
      name: 'Preview',
      config: { provider: { kind: 'reference', providerId: provider.id, model: 'own' } },
    });
    const result = await app.request('/api/agents/preview', request('POST', draft));
    const raw = await result.text();
    expect(raw.includes(key)).toBe(false);
    const preview = AgentPreviewResponse.parse(JSON.parse(raw));
    expect(preview.target.agent.settings.config.providers[0]?.apiKey).toBe('[redacted]');
    expect(preview.warnings.some((warning) => warning.code === 'api_key_missing')).toBe(false);
    expect(db.query<{ count: number }, []>('SELECT count(*) AS count FROM chats').get()?.count).toBe(0);
  });

  test('arbitrary Error name/message/stack/cause/code never reach API or logs', async () => {
    const { app } = setup();
    const secret = randomBytes(24).toString('hex');
    const spy = spyOn(console, 'error').mockImplementation(() => {});
    const keyOperation = { ...input(secret), credential: { action: 'replace', value: ' ' } };
    try {
      const invalid = await app.request('/api/providers', request('POST', keyOperation));
      expect(invalid.status).toBe(400);
      db.exec(
        `CREATE TRIGGER fail_provider BEFORE INSERT ON providers BEGIN SELECT RAISE(ABORT, 'failure'); END;`,
      );
      const failed = await app.request('/api/providers', request('POST', input(secret)));
      expect(failed.status).toBe(500);
      expect((await failed.text()).includes(secret)).toBe(false);
      const fault = Object.assign(new Error(secret, { cause: new Error(secret) }), {
        name: secret,
        stack: secret,
        code: secret,
      });
      const service = new ProviderService(new ProvidersRepository(db));
      service.create = () => {
        throw fault;
      };
      const boundary = new Hono();
      boundary.route('/api/providers', createProvidersRoutes(service));
      boundary.onError(errorResponse);
      const thrown = await boundary.request('/api/providers', request('POST', input(secret)));
      expect(thrown.status).toBe(500);
      expect((await thrown.text()).includes(secret)).toBe(false);
      expect(JSON.stringify(spy.mock.calls).includes(secret)).toBe(false);
    } finally {
      spy.mockRestore();
    }
  });
});
