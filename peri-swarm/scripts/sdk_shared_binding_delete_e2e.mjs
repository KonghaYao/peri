#!/usr/bin/env node
// Run against a local Swarm gateway with a Bun bridge and a fresh, disposable state store.
// npm install --prefix /tmp/peri-sdk openai@7.60.0
// NODE_PATH=/tmp/peri-sdk/node_modules node peri-swarm/scripts/sdk_shared_binding_delete_e2e.mjs
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const OpenAI = createRequire(import.meta.url)('openai');
const client = new OpenAI({
  baseURL: process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:8080/v1',
  apiKey: process.env.PERI_SWARM_API_KEY,
  maxRetries: 0,
});
const sessions = client.beta.agents.sessions;

async function hosted(environmentId) {
  const environment = { type: 'openai_hosted' };
  if (environmentId) environment.environment_id = environmentId;
  return sessions.create({ environment, agent: { model: process.env.PERI_SWARM_MODEL ?? 'stub-model' } });
}

for (const first of ['creator', 'sharer']) {
  const a = await hosted();
  const env = a.environment.id;
  assert.ok(env);
  const c = await hosted(env);
  assert.equal(c.environment.id, env);
  assert.notEqual(a.id, c.id);
  const [removed, survivor] = first === 'creator' ? [a, c] : [c, a];
  await sessions.delete(removed.id);
  assert.equal((await sessions.retrieve(survivor.id)).environment.id, env);
  // This create resolves the persisted binding. A surviving session's cached
  // environment field alone would hide a deleted binding.
  const newcomer = await hosted(env);
  assert.equal(newcomer.environment.id, env);
  assert.equal(newcomer.sandbox.sandboxId, a.sandbox.sandboxId);
  await sessions.delete(survivor.id);
  await sessions.delete(newcomer.id);
  await assert.rejects(
    hosted(env),
    error => error?.status >= 400 && error?.status < 500 && error?.error?.details?.reason === 'not_bound',
  );
  console.log(JSON.stringify({ firstDeleted: first, survivorCanShare: true, lastReferenceReleased: true }));
}
