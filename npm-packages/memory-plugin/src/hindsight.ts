import { z } from 'zod';
import { type Config, MemoryError } from './config.js';

export const querySchema = z.object({
  query: z.string().trim().min(1).max(8192),
  budget: z.enum(['low', 'mid', 'high']).default('mid'),
  maxTokens: z.number().int().min(128).max(8192).optional(),
  tags: z.array(z.string().min(1).max(256)).max(32).optional(),
}).strict();
export type Query = z.infer<typeof querySchema>;
const resultSchema = z.object({
  id: z.string(), text: z.string(), type: z.string().optional(),
  context: z.string().nullable().optional(), document_id: z.string().nullable().optional(),
  occurred_start: z.string().nullable().optional(), occurred_end: z.string().nullable().optional(),
  metadata: z.record(z.string(), z.string().nullable()).nullable().optional(),
});
export type Memory = z.infer<typeof resultSchema>;
export class HindsightClient {
  private active = 0;
  private readonly shutdown = new AbortController();
  constructor(readonly config: Config) {}
  close() { this.shutdown.abort(); }
  async request(path: string, body?: unknown, signal?: AbortSignal): Promise<unknown> {
    if (this.active >= 4) throw new MemoryError('BUSY', 'At most four backend requests may run concurrently', true);
    this.active++;
    const timeout = AbortSignal.timeout(this.config.timeoutMs);
    const combined = AbortSignal.any([timeout, this.shutdown.signal, ...(signal ? [signal] : [])]);
    try {
      const response = await fetch(`${this.config.baseUrl}/v1/default/banks/${encodeURIComponent(this.config.bankId)}${path}`, {
        method: body === undefined ? 'GET' : 'POST', redirect: 'error', signal: combined,
        headers: { Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
          ...(this.config.apiKey ? { Authorization: `Bearer ${this.config.apiKey}` } : {}) },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      if (!response.ok) {
        await response.body?.cancel();
        const code = response.status === 401 || response.status === 403 ? 'AUTH' : response.status === 404 ? 'NOT_FOUND' : 'BACKEND_HTTP';
        throw new MemoryError(code, `Hindsight returned HTTP ${response.status}`, response.status === 429 || response.status >= 500);
      }
      const reader = response.body?.getReader();
      const chunks: Uint8Array[] = []; let bytes = 0;
      if (reader) try {
        while (true) {
          const chunk = await reader.read(); if (chunk.done) break;
          bytes += chunk.value.byteLength;
          if (bytes > 2 * 1024 * 1024) { await reader.cancel(); throw new MemoryError('RESPONSE_LIMIT', 'Hindsight response exceeds 2 MiB'); }
          chunks.push(chunk.value);
        }
      } finally { reader.releaseLock(); }
      try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); }
      catch { throw new MemoryError('BACKEND_SCHEMA', 'Hindsight returned invalid JSON'); }
    } catch (error) {
      if (error instanceof MemoryError) throw error;
      if (timeout.aborted) throw new MemoryError('TIMEOUT', 'Hindsight request timed out; write outcome may be unknown', true);
      if (combined.aborted) throw new MemoryError('CANCELLED', 'Memory operation cancelled');
      throw new MemoryError('NETWORK', 'Cannot reach Hindsight', true);
    } finally { this.active--; }
  }
  private queryBody(query: Query) {
    return { query: query.query, budget: query.budget,
      max_tokens: Math.min(query.maxTokens ?? this.config.maxTokens, this.config.maxTokens),
      ...(query.tags?.length ? { tags: query.tags, tags_match: 'all_strict' } : {}) };
  }
  async recall(query: Query, signal?: AbortSignal): Promise<{ memories: Memory[]; truncated: boolean }> {
    const parsed = z.object({ results: z.array(resultSchema) }).safeParse(await this.request('/memories/recall', this.queryBody(query), signal));
    if (!parsed.success) throw new MemoryError('BACKEND_SCHEMA', 'Invalid Hindsight recall response');
    const memories: Memory[] = []; let size = 0;
    for (const result of parsed.data.results) {
      const nextSize = Buffer.byteLength(JSON.stringify(result));
      if (memories.length === 50 || size + nextSize > 48000) break;
      size += nextSize; memories.push(result);
    }
    return { memories, truncated: memories.length !== parsed.data.results.length };
  }
  async reflect(query: Query, signal?: AbortSignal) {
    const parsed = z.object({ text: z.string(), based_on: z.unknown().optional() }).safeParse(
      await this.request('/reflect', { ...this.queryBody(query), include: { facts: {} } }, signal));
    if (!parsed.success) throw new MemoryError('BACKEND_SCHEMA', 'Invalid Hindsight reflect response');
    if (Buffer.byteLength(JSON.stringify(parsed.data)) > 64000) throw new MemoryError('RESPONSE_LIMIT', 'Reflection and evidence exceed 64 KB');
    return parsed.data;
  }
  async retain(item: Record<string, unknown>, signal?: AbortSignal) {
    const parsed = z.object({ success: z.literal(true), bank_id: z.string(), items_count: z.literal(1), async: z.literal(false) })
      .safeParse(await this.request('/memories', { items: [item], async: false }, signal));
    if (!parsed.success || parsed.data.bank_id !== this.config.bankId)
      throw new MemoryError('BACKEND_SCHEMA', 'Hindsight did not confirm synchronous retain completion');
  }
}
