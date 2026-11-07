import { BusinessError } from './errors';

export type CursorDirection = 'before' | 'after';

export interface CursorPayload {
  /** 游标版本。 */
  v: 1;
  /** 资源范围，例如 `room:rm_1`。 */
  scope: string;
  /** 翻页方向。 */
  dir: CursorDirection;
  /** 已应用到的事件/消息序号。 */
  seq: number;
  /** 复合排序的次级键；普通序号游标不使用。 */
  key?: string;
  /** 首次查询时固定的快照上界；历史翻页沿用它。 */
  snapshot: number;
}

const encoder = new TextEncoder();

function toBase64Url(text: string): string {
  const bytes = encoder.encode(text);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/u, '');
}

function fromBase64Url(value: string): string {
  const padded = value.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (value.length % 4)) % 4);
  return atob(padded);
}

export function encodeCursor(payload: CursorPayload): string {
  return toBase64Url(JSON.stringify(payload));
}

export function decodeCursor(cursor: string, scope: string): CursorPayload {
  let payload: unknown;
  try {
    payload = JSON.parse(fromBase64Url(cursor));
  } catch {
    throw new BusinessError('INVALID_ARGUMENT', 'cursor 不是合法的版本化游标');
  }
  if (typeof payload !== 'object' || payload === null) {
    throw new BusinessError('INVALID_ARGUMENT', 'cursor 不是合法的版本化游标');
  }
  const record = payload as Partial<CursorPayload>;
  if (
    record.v !== 1 ||
    typeof record.scope !== 'string' ||
    (record.dir !== 'before' && record.dir !== 'after') ||
    typeof record.seq !== 'number' ||
    !Number.isFinite(record.seq) ||
    typeof record.snapshot !== 'number' ||
    !Number.isFinite(record.snapshot) ||
    (record.key !== undefined && typeof record.key !== 'string')
  ) {
    throw new BusinessError('INVALID_ARGUMENT', 'cursor 字段不完整或版本不受支持');
  }
  if (record.scope !== scope) {
    throw new BusinessError('INVALID_ARGUMENT', 'cursor 不能跨 scope 重用', {
      expected: scope,
      actual: record.scope,
    });
  }
  return record as CursorPayload;
}
