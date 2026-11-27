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

// fixture 包含两个可投影答案和交错的内部行；SDK 可能用 data 的末项重读内部行。
const sessionId = 'agent-01';
const asc = ['itim_opener', 'itim_answer', 'itim_followup'];
for (const [order, after, limit, expected] of [
  ['asc', undefined, 1, asc],
  ['asc', undefined, 2, asc],
  ['asc', '1', 2, asc.slice(1)],
  ['desc', undefined, 1, [...asc].reverse()],
  ['desc', undefined, 2, [...asc].reverse()],
]) {
  const seen = [];
  for await (const item of client.beta.agents.sessions.items.list(sessionId, { order, after, limit })) {
    seen.push(item.id);
    assert.ok(seen.length <= expected.length, `${order} 重复返回条目`);
    if (item.id === 'itim_answer') assert.equal(item.content[0].text, 'assistant-4');
  }
  assert.deepEqual(seen, expected, `${order} after=${after} 的 SDK 迭代序列`);
}

async function nativePage(order, afterSeq) {
  const url = new URL(`${process.argv[2]}/agents/sessions/${sessionId}/items`);
  url.search = new URLSearchParams({ order, limit: '2' });
  if (afterSeq !== undefined) url.searchParams.set('afterSeq', String(afterSeq));
  const res = await fetch(url, {
    headers: { Authorization: `Bearer ${process.env.PERI_SWARM_API_KEY}` },
    signal: AbortSignal.timeout(5000),
  });
  assert.equal(res.status, 200);
  return res.json();
}

for (const order of ['asc', 'desc']) {
  const expected = order === 'asc' ? asc : [...asc].reverse();
  for (const alternating of [false, true]) {
    const seen = [];
    const rawSeqs = [];
    let body = await nativePage(order);
    for (let step = 0; ; step++) {
      const all = body.items.map((row) => row.seq);
      rawSeqs.push(...all);
      seen.push(...body.data.map((item) => item.id));
      const maxima = new Map();
      for (const row of body.items) {
        maxima.set(row.itemId, Math.max(maxima.get(row.itemId) ?? 0, row.seq));
      }
      assert.ok(body.data.length <= Math.min(2, maxima.size));
      if (!body.has_more) {
        assert.equal(body.nextCursor, null);
        break;
      }
      const tailSeq = (order === 'asc' ? Math.max : Math.min)(...maxima.values());
      assert.equal(body.nextCursor, tailSeq);
      assert.ok(step < 7, `${order} 游标没有到达末尾`);
      if (alternating && step % 2 === 0) {
        const after = body.data.at(-1)?.id;
        assert.ok(after, '有后续页时 SDK 必须拿到官方 id');
        body = (await client.beta.agents.sessions.items.list(sessionId, { order, limit: 2, after })).body;
      } else {
        body = await nativePage(order, body.nextCursor);
      }
    }
    assert.equal(new Set(seen).size, seen.length);
    assert.deepEqual(seen, expected, `${order} 的${alternating ? '混合' : '原生'}游标序列`);
    if (!alternating) {
      const full = [1, 2, 3, 4, 5, 6, 7];
      assert.deepEqual(rawSeqs, order === 'asc' ? full : full.reverse());
    } else if (order === 'asc') {
      assert.deepEqual(rawSeqs, [1, 2, 2, 3, 4, 5, 6, 7], 'SDK 只重读内部 receipt');
    }
  }
}
