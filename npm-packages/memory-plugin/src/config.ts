import { homedir } from 'node:os';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';

export class MemoryError extends Error {
  constructor(public readonly code: string, message: string, public readonly retryable = false) { super(message); }
}
export interface Config {
  baseUrl: string; apiKey?: string; bankId: string; agentId: string; sessionId: string;
  stateDir: string; timeoutMs: number; pollMs: number; maxTokens: number; bootstrapQuery?: string;
}
export const digest = (s: string) => createHash('sha256').update(s).digest('hex');
export function loadConfig(env: NodeJS.ProcessEnv = process.env): Config {
  const required = (key: string) => {
    const value = env[key]?.trim();
    if (!value || value.length > 512 || /[\r\n\0]/.test(value)) throw new MemoryError('CONFIG', `${key} is required (1..512 characters)`);
    return value;
  };
  const integer = (key: string, fallback: number, min: number, max: number) => {
    const n = env[key] === undefined ? fallback : Number(env[key]);
    if (!Number.isInteger(n) || n < min || n > max) throw new MemoryError('CONFIG', `Invalid ${key}`);
    return n;
  };
  let url: URL;
  try { url = new URL(required('HINDSIGHT_BASE_URL')); } catch { throw new MemoryError('CONFIG', 'HINDSIGHT_BASE_URL must be an absolute HTTP(S) URL'); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash)
    throw new MemoryError('CONFIG', 'HINDSIGHT_BASE_URL must not contain credentials, query or fragment');
  if (url.protocol === 'http:' && !['localhost', '127.0.0.1', '[::1]'].includes(url.hostname) && env.MEMORY_ALLOW_HTTP !== '1')
    throw new MemoryError('CONFIG', 'Use HTTPS, or explicitly set MEMORY_ALLOW_HTTP=1 for a trusted private network');
  const baseUrl = url.href.replace(/\/+$/, '');
  const agentId = required('MEMORY_AGENT_ID');
  const bankId = required('HINDSIGHT_BANK_ID');
  if (bankId === '.' || bankId === '..') throw new MemoryError('CONFIG', 'Invalid HINDSIGHT_BANK_ID');
  const sessionId = required('MEMORY_SESSION_ID');
  if (env.MEMORY_BOOTSTRAP_QUERY && env.MEMORY_BOOTSTRAP_QUERY.length > 8192)
    throw new MemoryError('CONFIG', 'MEMORY_BOOTSTRAP_QUERY exceeds 8192 characters');
  return {
    baseUrl, apiKey: env.HINDSIGHT_API_KEY || undefined, bankId, agentId, sessionId,
    stateDir: resolve(env.MEMORY_STATE_DIR || resolve(homedir(), '.local/state/peri-memory'), digest(JSON.stringify([baseUrl, bankId, agentId]))),
    timeoutMs: integer('MEMORY_TIMEOUT_MS', 120000, 100, 600000),
    pollMs: integer('MEMORY_POLL_MS', 1000, 100, 60000),
    maxTokens: integer('MEMORY_MAX_TOKENS', 2048, 128, 8192),
    bootstrapQuery: env.MEMORY_BOOTSTRAP_QUERY || undefined,
  };
}
export function errorInfo(error: unknown) {
  return error instanceof MemoryError
    ? { code: error.code, message: error.message, retryable: error.retryable }
    : { code: 'INTERNAL', message: 'Memory operation failed', retryable: false };
}
