import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const OpenAI = require('openai');
assert.equal(require('openai/version').VERSION, '7.60.0');
const client = new OpenAI({
  baseURL: process.argv[2],
  apiKey: process.env.PERI_SWARM_API_KEY,
  maxRetries: 0,
  timeout: 5000,
});

// Rust fixture 在 opener 和 answer 之间建了 receipt / approval 两个纯内部原始页。
for (const [order, after, limit, expected] of [
  ['asc', undefined, 1, ['itim_opener', 'itim_answer']],
  ['asc', '1', 2, ['itim_answer']],
  ['desc', undefined, 1, ['itim_answer', 'itim_opener']],
]) {
  const seen = [];
  for await (const item of client.beta.agents.sessions.items.list('agent-01', { order, after, limit })) {
    seen.push(item.id);
    assert.ok(seen.length <= expected.length, `${order} 重复返回条目`);
    if (item.id === 'itim_answer') assert.equal(item.content[0].text, 'assistant-4');
  }
  assert.deepEqual(seen, expected, `${order} after=${after} 的 SDK 迭代序列`);
}
