/** SQLite 值到 JSON 值的最小映射。 */

export type JsonCell =
  | string
  | number
  | boolean
  | null
  | { type: 'integer'; value: string }
  | { type: 'real'; value: string }
  | { type: 'blob'; base64: string; byteLength: number }
  | { type: 'unsupported'; valueType: string };

const MIN_SAFE_INTEGER = BigInt(Number.MIN_SAFE_INTEGER);
const MAX_SAFE_INTEGER = BigInt(Number.MAX_SAFE_INTEGER);

function blobCell(value: Uint8Array): JsonCell {
  return {
    type: 'blob',
    base64: Buffer.from(value).toString('base64'),
    byteLength: value.byteLength,
  };
}

/**
 * 安全整数直接输出为 JSON number；更大 INTEGER 和 BLOB 用带标签对象表示，
 * 因为 JSON 不能无歧义地表示它们。
 */
export function toJsonCell(value: unknown): JsonCell {
  if (value === null) return null;
  if (typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') {
    return Number.isFinite(value) ? value : { type: 'real', value: String(value) };
  }
  if (typeof value === 'bigint') {
    return value >= MIN_SAFE_INTEGER && value <= MAX_SAFE_INTEGER
      ? Number(value)
      : { type: 'integer', value: value.toString() };
  }
  if (value instanceof Uint8Array) return blobCell(value);
  if (value instanceof ArrayBuffer) return blobCell(new Uint8Array(value));
  return { type: 'unsupported', valueType: typeof value };
}

export function toJsonRow(row: readonly unknown[]): JsonCell[] {
  return row.map(toJsonCell);
}
