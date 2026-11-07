const encoder = new TextEncoder();
const decoder = new TextDecoder();

export function nowMs(): number {
  return Date.now();
}

export function newId(prefix: string): string {
  return `${prefix}_${crypto.randomUUID().replace(/-/g, '')}`;
}

export function utf8Bytes(text: string): number {
  return encoder.encode(text).length;
}

/** 按 UTF-8 边界截断，返回不超过 maxBytes 的前缀。 */
export function truncateUtf8(text: string, maxBytes: number): string {
  const bytes = encoder.encode(text);
  if (bytes.length <= maxBytes) return text;
  let end = maxBytes;
  while (end > 0 && (bytes[end]! & 0xc0) === 0x80) end -= 1;
  return decoder.decode(bytes.subarray(0, end));
}

/** 从 UTF-8 字节数组切出完整字符块，返回文本与下一块起点。 */
export function sliceUtf8Chunk(
  text: string,
  start: number,
  maxBytes: number,
): { text: string; next: number; total: number } {
  const bytes = encoder.encode(text);
  if (start >= bytes.length) return { text: '', next: bytes.length, total: bytes.length };
  let end = Math.min(start + maxBytes, bytes.length);
  while (end < bytes.length && (bytes[end]! & 0xc0) === 0x80) end -= 1;
  if (end <= start) end = Math.min(start + maxBytes, bytes.length);
  return { text: decoder.decode(bytes.subarray(start, end)), next: end, total: bytes.length };
}

export function stableStringify(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(',')}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, item]) => item !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${stableStringify(item)}`).join(',')}}`;
}

export async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', encoder.encode(text));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

export async function hashToken(token: string): Promise<string> {
  return sha256Hex(`agent-network:token:${token}`);
}

export function randomToken(bytes = 32): string {
  const buffer = new Uint8Array(bytes);
  crypto.getRandomValues(buffer);
  let binary = '';
  for (const byte of buffer) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/u, '');
}

export function sortedPairKey(a: string, b: string): string {
  return a < b ? `${a}|${b}` : `${b}|${a}`;
}

export function addJitter(ms: number, ratio = 0.2): number {
  const delta = ms * ratio;
  return Math.max(0, Math.round(ms - delta + Math.random() * delta * 2));
}

export function backoffMs(attempt: number): number {
  const base = Math.min(30_000, 1_000 * 2 ** Math.max(0, attempt));
  return addJitter(base);
}
