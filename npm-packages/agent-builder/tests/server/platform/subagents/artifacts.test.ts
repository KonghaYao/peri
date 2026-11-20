import { afterEach, describe, expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createSession, del, get, type Harness, json, setup } from '../sessions/helpers';

let harness: Harness | null = null;
const tempDirs: string[] = [];
const envBefore = process.env.PERI_WORKSPACE_PATH;

afterEach(async () => {
  if (envBefore === undefined) delete process.env.PERI_WORKSPACE_PATH;
  else process.env.PERI_WORKSPACE_PATH = envBefore;
  await harness?.dispose();
  harness = null;
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function start(): Harness {
  harness = setup();
  return harness;
}

async function newSession(h: Harness): Promise<string> {
  const created = await createSession(h, {
    environment: { type: 'none' },
    agent_id: h.agentId,
    input: 'hello',
  });
  return created.id;
}

/** Creates a temp deployment workspace and points the platform layer at it. */
function workspace(): string {
  const parent = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-artifacts-')));
  tempDirs.push(parent);
  const root = path.join(parent, 'workspace');
  mkdirSync(root);
  process.env.PERI_WORKSPACE_PATH = root;
  return root;
}

function writeWorkspaceFile(root: string, relative: string, content: string): void {
  const target = path.join(root, relative);
  mkdirSync(path.dirname(target), { recursive: true });
  writeFileSync(target, content);
}

const EMPTY_PAGE = {
  object: 'list',
  data: [],
  first_id: null,
  last_id: null,
  has_more: false,
};

describe('session artifacts', () => {
  test('returns the empty documented page when nothing was published', async () => {
    const h = start();
    const sessionId = await newSession(h);

    const response = await get(h, `/agents/sessions/${sessionId}/artifacts`);
    expect(response.status).toBe(200);
    expect(await json(response)).toStrictEqual(EMPTY_PAGE);
  });

  test('returns 404 for an unknown artifact, its content, and its deletion', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const artifactId = crypto.randomUUID();

    for (const response of [
      await get(h, `/agents/sessions/${sessionId}/artifacts/${artifactId}`),
      await get(h, `/agents/sessions/${sessionId}/artifacts/${artifactId}/content`),
      await del(h, `/agents/sessions/${sessionId}/artifacts/${artifactId}`),
    ]) {
      expect(response.status).toBe(404);
      expect(await json(response)).toMatchObject({ error: { code: 'not_found' } });
    }
  });

  test('answers 404 for every artifact route when the session does not exist', async () => {
    const h = start();
    const sessionId = crypto.randomUUID();
    const artifactId = crypto.randomUUID();
    for (const response of [
      await get(h, `/agents/sessions/${sessionId}/artifacts`),
      await get(h, `/agents/sessions/${sessionId}/artifacts/${artifactId}`),
      await get(h, `/agents/sessions/${sessionId}/artifacts/${artifactId}/content`),
      await del(h, `/agents/sessions/${sessionId}/artifacts/${artifactId}`),
    ]) {
      expect(response.status).toBe(404);
    }
  });

  test('lists stored artifacts with seconds and pages by ID', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const older = h.repos.artifacts.create({
      sessionId,
      path: 'reports/first.txt',
      mediaType: 'text/plain',
      sizeBytes: 5,
    });
    const newer = h.repos.artifacts.create({
      sessionId,
      path: 'reports/second.txt',
      sizeBytes: 6,
    });

    const response = await get(h, `/agents/sessions/${sessionId}/artifacts?order=asc&limit=1`);
    const body = await json<{
      data: Record<string, unknown>[];
      first_id: string;
      last_id: string;
      has_more: boolean;
    }>(response);
    expect(body.data.map((artifact) => artifact.id)).toEqual([older.id]);
    expect(body.has_more).toBe(true);
    expect(body.first_id).toBe(older.id);

    const artifact = body.data[0];
    expect(Object.keys(artifact ?? {}).sort()).toEqual([
      'created_at',
      'environment_id',
      'id',
      'object',
      'path',
      'session_id',
      'size_bytes',
      'turn_id',
    ]);
    expect(artifact).toMatchObject({
      object: 'agent.session.artifact',
      id: older.id,
      session_id: sessionId,
      path: 'reports/first.txt',
      size_bytes: 5,
      environment_id: '',
      turn_id: '',
    });
    expect(artifact?.created_at).toBeNumber();
    expect(Math.abs(Number(artifact?.created_at) - Math.floor(Date.now() / 1000))).toBeLessThan(5);

    const next = await get(
      h,
      `/agents/sessions/${sessionId}/artifacts?order=asc&limit=1&after=${body.last_id}`,
    );
    expect(
      (await json<{ data: { id: string }[]; has_more: boolean }>(next)).data.map((entry) => entry.id),
    ).toEqual([newer.id]);

    const filtered = await get(h, `/agents/sessions/${sessionId}/artifacts?environment_id=env_unknown`);
    expect(await json(filtered)).toStrictEqual(EMPTY_PAGE);
  });

  test('serializes stored environment and publishing turn identifiers', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const published = h.repos.artifacts.create({
      sessionId,
      environmentId: 'env-1',
      turnId: 'turn-1',
      path: 'reports/published.txt',
      sizeBytes: 1,
    });
    const local = h.repos.artifacts.create({
      sessionId,
      path: 'reports/local.txt',
      sizeBytes: 1,
    });

    const detail = await get(h, `/agents/sessions/${sessionId}/artifacts/${published.id}`);
    expect(await json(detail)).toMatchObject({
      environment_id: 'env-1',
      turn_id: 'turn-1',
    });

    const filtered = await get(h, `/agents/sessions/${sessionId}/artifacts?environment_id=env-1`);
    const page = await json<{
      data: { id: string; environment_id: string; turn_id: string }[];
    }>(filtered);
    expect(page.data.map((artifact) => artifact.id)).toEqual([published.id]);
    expect(page.data[0]).toMatchObject({ environment_id: 'env-1', turn_id: 'turn-1' });

    // Rows without a hosted environment or publishing turn still serialize the documented strings.
    const localDetail = await get(h, `/agents/sessions/${sessionId}/artifacts/${local.id}`);
    expect(await json(localDetail)).toMatchObject({ environment_id: '', turn_id: '' });
  });

  test('retrieves artifact metadata and deletes the stored row', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const otherSessionId = await newSession(h);
    const artifact = h.repos.artifacts.create({ sessionId, path: 'reports/a.txt', sizeBytes: 1 });

    const detail = await get(h, `/agents/sessions/${sessionId}/artifacts/${artifact.id}`);
    expect(detail.status).toBe(200);
    expect(await json(detail)).toMatchObject({ id: artifact.id, path: 'reports/a.txt' });

    const scoped = await get(h, `/agents/sessions/${otherSessionId}/artifacts/${artifact.id}`);
    expect(scoped.status).toBe(404);

    const deleted = await del(h, `/agents/sessions/${sessionId}/artifacts/${artifact.id}`);
    expect(deleted.status).toBe(200);
    expect(await json(deleted)).toStrictEqual({
      id: artifact.id,
      object: 'agent.session.artifact.deleted',
      deleted: true,
    });
    expect(h.repos.artifacts.get(artifact.id)).toBeNull();
  });

  test('streams stored bytes only from inside the deployment workspace', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const root = workspace();
    writeWorkspaceFile(root, 'reports/result.txt', 'hello artifact');

    const artifact = h.repos.artifacts.create({
      sessionId,
      path: 'reports/result.txt',
      mediaType: 'text/plain',
      sizeBytes: 14,
    });

    const response = await get(h, `/agents/sessions/${sessionId}/artifacts/${artifact.id}/content`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toBe('text/plain');
    expect(await response.text()).toBe('hello artifact');

    const absolute = h.repos.artifacts.create({
      sessionId,
      path: path.join(root, 'reports/result.txt'),
      mediaType: 'application/octet-stream',
      sizeBytes: 14,
    });
    const absoluteResponse = await get(h, `/agents/sessions/${sessionId}/artifacts/${absolute.id}/content`);
    expect(absoluteResponse.status).toBe(200);
    expect(await absoluteResponse.text()).toBe('hello artifact');
  });

  test('refuses traversal, foreign absolute paths, NUL bytes and odd media types', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const root = workspace();
    const outside = path.join(path.dirname(root), 'outside');
    mkdirSync(outside, { recursive: true });
    writeFileSync(path.join(outside, 'secret.txt'), 'secret');

    const traversal = h.repos.artifacts.create({
      sessionId,
      path: '../outside/secret.txt',
      sizeBytes: 6,
    });
    const absolute = h.repos.artifacts.create({
      sessionId,
      path: path.join(outside, 'secret.txt'),
      sizeBytes: 6,
    });
    const nul = h.repos.artifacts.create({ sessionId, path: 'bad\0path', sizeBytes: 1 });

    writeWorkspaceFile(root, 'reports/odd.txt', 'body');
    const oddMedia = h.repos.artifacts.create({
      sessionId,
      path: 'reports/odd.txt',
      mediaType: 'text/html\r\nx-injected: 1',
      sizeBytes: 4,
    });

    for (const artifactId of [traversal.id, absolute.id, nul.id]) {
      const response = await get(h, `/agents/sessions/${sessionId}/artifacts/${artifactId}/content`);
      expect(response.status).toBe(403);
      expect(await json(response)).toMatchObject({ error: { code: 'artifact_path_unsafe' } });
    }

    const odd = await get(h, `/agents/sessions/${sessionId}/artifacts/${oddMedia.id}/content`);
    expect(odd.status).toBe(200);
    expect(odd.headers.get('content-type')).toBe('application/octet-stream');
  });

  test('reports missing and size-mismatched content without leaking bytes', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const root = workspace();
    writeWorkspaceFile(root, 'reports/short.txt', 'abc');

    const missing = h.repos.artifacts.create({ sessionId, path: 'reports/missing.txt', sizeBytes: 3 });
    const mismatch = h.repos.artifacts.create({ sessionId, path: 'reports/short.txt', sizeBytes: 99 });

    const missingResponse = await get(h, `/agents/sessions/${sessionId}/artifacts/${missing.id}/content`);
    expect(missingResponse.status).toBe(404);
    expect(await json(missingResponse)).toMatchObject({
      error: { code: 'artifact_content_unavailable' },
    });

    const mismatchResponse = await get(h, `/agents/sessions/${sessionId}/artifacts/${mismatch.id}/content`);
    expect(mismatchResponse.status).toBe(409);
    expect(await json(mismatchResponse)).toMatchObject({
      error: { code: 'artifact_size_mismatch', type: 'conflict_error' },
    });
  });
});
