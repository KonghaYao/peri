import { parseArgs } from 'node:util';
import { createReadStream, realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadConfig, errorInfo, MemoryError } from './config.js';
import { HindsightClient } from './hindsight.js';
import { importRecords } from './importer.js';

export { loadConfig, MemoryError, errorInfo } from './config.js';
export { HindsightClient, querySchema } from './hindsight.js';
export { importRecords, prepareImport, importSchema, markImported } from './importer.js';
export { MemoryService, CONTEXT_URI, STATUS_URI } from './service.js';
// Keep the importer path independent of MCP/HTTP framework module loading.
export async function startStdio(...args: Parameters<typeof import('./adapters.js').startStdio>) { return (await import('./adapters.js')).startStdio(...args); }
export async function createMemoryServer(...args: Parameters<typeof import('./adapters.js').createMemoryServer>) { return (await import('./adapters.js')).createMemoryServer(...args); }
export async function startHttp(...args: Parameters<typeof import('./adapters.js').startHttp>) { return (await import('./adapters.js')).startHttp(...args); }

const help = `memory-plugin 0.1.0
  memory-plugin mcp [--transport stdio|http] [--port 33221]
  memory-plugin import --file messages.json|- [--dry-run]
Required: HINDSIGHT_BASE_URL, HINDSIGHT_BANK_ID, MEMORY_AGENT_ID, MEMORY_SESSION_ID
Optional: HINDSIGHT_API_KEY, MEMORY_STATE_DIR, MEMORY_TIMEOUT_MS, MEMORY_MAX_TOKENS
HTTP: loopback only; MEMORY_MCP_TOKEN (32+ characters) is required.
Both commands use this same JS entry. MCP never imports messages.
`;
export async function main(argv = process.argv.slice(2)) {
  const { positionals, values } = parseArgs({ args: argv, allowPositionals: true, strict: true, options: {
    help: { type: 'boolean', short: 'h' }, version: { type: 'boolean' },
    transport: { type: 'string' }, port: { type: 'string' }, file: { type: 'string' }, 'dry-run': { type: 'boolean' },
  } });
  if (values.help || argv.length === 0) { process.stdout.write(help); return; }
  if (values.version) { process.stdout.write('0.1.0\n'); return; }
  if (positionals.length !== 1 || !['mcp', 'import'].includes(positionals[0])) throw new MemoryError('USAGE', 'Expected mcp or import; use --help');
  const config = loadConfig();
  if (positionals[0] === 'import') {
    if (!values.file || values.transport || values.port) throw new MemoryError('USAGE', 'import requires --file; transport/port are MCP options');
    const chunks: Buffer[] = []; let bytes = 0;
    const stream = values.file === '-' ? process.stdin : createReadStream(values.file);
    try { for await (const chunk of stream) {
      const buffer = Buffer.from(chunk); bytes += buffer.length;
      if (bytes > 8 * 1024 * 1024) throw new MemoryError('INPUT_LIMIT', 'Import file exceeds 8 MiB');
      chunks.push(buffer);
    } } catch (error) { if (error instanceof MemoryError) throw error; throw new MemoryError('INPUT_IO', 'Cannot read import file'); }
    let raw: unknown;
    try { raw = JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch { throw new MemoryError('INVALID_INPUT', 'Import file must be valid JSON'); }
    const client = new HindsightClient(config);
    const abort = new AbortController(); const cancel = () => abort.abort();
    process.once('SIGINT', cancel); process.once('SIGTERM', cancel);
    try {
      const result = await importRecords(raw, client, { dryRun: values['dry-run'], signal: abort.signal,
        onRecord: receipt => process.stdout.write(`${JSON.stringify(receipt)}\n`) });
      process.stdout.write(`${JSON.stringify(result)}\n`);
    } finally { client.close(); process.off('SIGINT', cancel); process.off('SIGTERM', cancel); }
    return;
  }
  if (values.file || values['dry-run']) throw new MemoryError('USAGE', 'file/dry-run are import options');
  const transport = values.transport ?? 'stdio';
  if (!['stdio', 'http'].includes(transport) || (transport === 'stdio' && values.port)) throw new MemoryError('USAGE', 'Use stdio, or http with --port');
  const { startStdio, startHttp } = await import('./adapters.js');
  const port = Number(values.port ?? '33221');
  if (!Number.isInteger(port) || port < 0 || port > 65535) throw new MemoryError('USAGE', 'Invalid port');
  const handle = transport === 'stdio' ? await startStdio(config) : await startHttp(config, process.env.MEMORY_MCP_TOKEN || '', port);
  if ('port' in handle) console.error(`memory-plugin: HTTP listening on 127.0.0.1:${handle.port}/mcp`);
  let closing = false;
  const close = () => { if (!closing) { closing = true; void handle.close().catch(() => { process.exitCode = 1; }); } };
  process.once('SIGINT', close); process.once('SIGTERM', close);
}
let invoked = false;
try { invoked = !!process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url)); } catch { /* imported */ }
if (invoked) main().catch(error => { console.error(JSON.stringify({ error: errorInfo(error) })); process.exitCode = 1; });
