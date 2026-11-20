import { randomBytes } from 'node:crypto';

/**
 * Platform resource IDs are standard UUIDv7 values: 36 characters with hyphens and a
 * millisecond timestamp prefix, so they sort in creation order and satisfy `z.uuid()`.
 *
 * The 12 random bits after the timestamp double as a counter within one millisecond, which
 * keeps IDs generated in the same tick strictly increasing without a separate sequence column.
 */
const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

const MAX_TIMESTAMP = 0xff_ff_ff_ff_ff_ff;
const COUNTER_MASK = 0x0fff;

let lastTimestamp = -1;
let lastCounter = 0;

function formatUuid(bytes: Uint8Array): string {
  const hex = Buffer.from(bytes).toString('hex');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/**
 * Mints a UUIDv7. `now` exists for tests that need a deterministic timestamp; callers
 * never pass it in production code.
 */
export function newPlatformId(now: number = Date.now()): string {
  const bytes = randomBytes(16);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  // Never move the timestamp backwards: a clock step or an exhausted same-millisecond counter
  // would otherwise produce an ID that sorts before one already handed out.
  let timestamp = Math.min(Math.max(Math.trunc(now), lastTimestamp), MAX_TIMESTAMP);
  let counter: number;

  if (timestamp === lastTimestamp) {
    counter = (lastCounter + 1) & COUNTER_MASK;
    if (counter === 0) timestamp = Math.min(lastTimestamp + 1, MAX_TIMESTAMP);
  } else {
    counter = ((view.getUint8(6) & 0x0f) << 8) | view.getUint8(7);
  }
  lastTimestamp = timestamp;
  lastCounter = counter;

  const millis = BigInt(timestamp);
  for (let index = 0; index < 6; index += 1) {
    view.setUint8(index, Number((millis >> BigInt(8 * (5 - index))) & 0xffn));
  }
  view.setUint8(6, 0x70 | ((counter >> 8) & 0x0f));
  view.setUint8(7, counter & 0xff);
  view.setUint8(8, 0x80 | (view.getUint8(8) & 0x3f));

  return formatUuid(bytes);
}

export function isUuidV7(value: string): boolean {
  return UUID_V7.test(value);
}
