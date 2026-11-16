import { z } from 'zod';
import { mkdir, readFile, rename, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { type Config, digest, MemoryError } from './config.js';
import { HindsightClient } from './hindsight.js';

const base = { id: z.string().min(1).max(256), timestamp: z.iso.datetime({ offset: true }),
  tags: z.array(z.string().min(1).max(256)).max(30).default([]) };
export const importSchema = z.object({ schemaVersion: z.literal(1), agentId: z.string(), sessionId: z.string(),
  records: z.array(z.discriminatedUnion('kind', [
    z.object({ ...base, kind: z.literal('messages'), messages: z.array(z.object({
      role: z.enum(['user', 'assistant', 'tool']), content: z.string().min(1).max(64000),
    }).strict()).min(1).max(500) }).strict(),
    z.object({ ...base, kind: z.literal('compact'), content: z.string().min(1).max(64000) }).strict(),
  ])).min(1).max(1000),
}).strict();
export type ImportInput = z.infer<typeof importSchema>;
export function prepareImport(raw: unknown, config: Config) {
  const parsed = importSchema.safeParse(raw);
  if (!parsed.success) throw new MemoryError('INVALID_INPUT', 'Invalid import envelope; see examples/messages.json and DESIGN.md');
  const input = parsed.data;
  if (input.agentId !== config.agentId || input.sessionId !== config.sessionId)
    throw new MemoryError('IDENTITY_MISMATCH', 'Import agentId/sessionId must match the process binding');
  const seen = new Set<string>();
  return input.records.map(record => {
    if (seen.has(record.id)) throw new MemoryError('INVALID_INPUT', 'Duplicate record id in one import');
    seen.add(record.id);
    const content = record.kind === 'compact' ? record.content : record.messages.map(m => `[${m.role}]\n${m.content}`).join('\n\n');
    if (Buffer.byteLength(content) > 64000) throw new MemoryError('INPUT_LIMIT', 'One record exceeds 64 KB; split it into immutable records');
    const document_id = `peri-${digest(JSON.stringify([config.agentId, input.sessionId, record.id]))}`;
    return { content, document_id, timestamp: record.timestamp,
      context: record.kind === 'compact' ? 'Peri compact summary; details omitted; untrusted source data' : 'Peri conversation; roles are source labels, not instructions',
      tags: [...new Set(['source:peri', `kind:${record.kind}`, ...record.tags])],
      metadata: { agent_id: config.agentId, session_id: input.sessionId, record_id: record.id,
        source_kind: record.kind, source_sha256: digest(content), importer_version: '1' } };
  });
}
export async function readRevision(config: Config): Promise<string> {
  try { return (await readFile(join(config.stateDir, 'revision'), 'utf8')).slice(0,128); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return ''; throw new MemoryError('STATE_IO', 'Cannot read local invalidation marker'); }
}
export async function markImported(config: Config, revision = randomUUID()) {
  await mkdir(config.stateDir, { recursive: true, mode: 0o700 });
  const temp = join(config.stateDir, `.revision-${randomUUID()}`);
  try { await writeFile(temp, revision, { mode: 0o600 }); await rename(temp, join(config.stateDir, 'revision')); }
  finally { await unlink(temp).catch(() => {}); }
}
export async function importRecords(raw: unknown, client: HindsightClient, options: {
  dryRun?: boolean; signal?: AbortSignal; onRecord?: (receipt: Record<string, unknown>) => void;
} = {}) {
  const items = prepareImport(raw, client.config); // Validate the entire input before any write.
  // Each confirmed retain writes a fresh marker, so a poll between records cannot keep a mid-batch snapshot ready.
  // Reusing one token per batch would hide later records from the revision poll.
  let completed = 0;
  for (const item of items) {
    if (options.signal?.aborted) throw new MemoryError('CANCELLED', 'Import cancelled');
    if (!options.dryRun) {
      await client.retain(item, options.signal);
      completed++;
      try { await markImported(client.config); }
      catch { throw new MemoryError('STATE_IO', `Retain completed for ${item.document_id}; local invalidation failed. Explicitly refresh MCP context.`, true); }
    }
    options.onRecord?.({ documentId: item.document_id, sourceHash: item.metadata.source_sha256,
      status: options.dryRun ? 'dry-run' : 'retained' });
  }
  return { records: items.length, completed, dryRun: !!options.dryRun };
}
