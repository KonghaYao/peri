import type { ChatRuntimeReason } from '@shared/chat';
import { connect } from '@tursodatabase/serverless';
import type { ServerEnv } from '../env';

/** A slow Store must not delay API startup; reachability is decided within this budget. */
export const STORE_PROBE_TIMEOUT_MS = 2_000;

export interface StoreTarget {
  readonly url: string;
  readonly authToken: string;
}

export type StoreProbe = (target: StoreTarget) => Promise<void>;

/** The subset of runtime reasons the Store probe can discover itself. */
export type StoreProbeFailureReason = Extract<
  ChatRuntimeReason,
  'store_token_missing' | 'store_unreachable' | 'store_auth_failed' | 'store_database_missing'
>;

export type StoreProbeFailureKind = 'http' | 'timeout' | 'network' | 'unknown';

export interface StoreProbeSuccess {
  readonly reachable: true;
}

export interface StoreProbeFailure {
  readonly reachable: false;
  readonly reason: StoreProbeFailureReason;
  readonly kind: StoreProbeFailureKind;
  /** HTTP status when the Store answered; metadata only, never the URL or token. */
  readonly status?: number;
}

export type StoreProbeResult = StoreProbeSuccess | StoreProbeFailure;

/**
 * Read-only reachability probe. The timeout is configured on the connection so an expired query
 * aborts its HTTP request and closes the socket; racing a promise alone would leave it open.
 */
export async function probeStoreOnce(
  target: StoreTarget,
  timeoutMs: number = STORE_PROBE_TIMEOUT_MS,
): Promise<void> {
  const connection = connect({
    url: target.url,
    authToken: target.authToken,
    defaultQueryTimeout: timeoutMs,
  });
  try {
    // `exec` uses the pipeline endpoint, whose errors keep `HTTP error! status: N` in the message.
    // `all` streams through the cursor endpoint, which 1.4.1 reports without the HTTP status, so
    // auth/database failures could not be told apart from a network outage.
    await connection.exec('SELECT 1');
  } finally {
    // A timed-out query already cleared the baton, so close resolves without another round trip.
    await connection.close();
  }
}

export interface StoreProbeOptions {
  readonly probe?: StoreProbe;
  readonly timeoutMs?: number;
}

interface ErrorFields {
  readonly name?: unknown;
  readonly message?: unknown;
  readonly code?: unknown;
  readonly status?: unknown;
}

function errorFields(error: unknown): ErrorFields | null {
  if (typeof error !== 'object' || error === null) return null;
  return error as ErrorFields;
}

function asStatus(value: unknown): number | undefined {
  const parsed = typeof value === 'number' ? value : typeof value === 'string' ? Number(value) : NaN;
  return Number.isInteger(parsed) && parsed >= 100 && parsed <= 599 ? parsed : undefined;
}

/**
 * `@tursodatabase/serverless` 1.4.1 only puts the status in the message (`HTTP error! status:
 * 401`); newer builds attach it as `error.status`. Accept both so classification does not depend
 * on the packaging detail.
 */
export function storeStatusFromError(error: unknown): number | undefined {
  const fields = errorFields(error);
  if (fields === null) return undefined;

  const direct = asStatus(fields.status);
  if (direct !== undefined) return direct;

  const code = typeof fields.code === 'string' ? /(?:^|_)(\d{3})$/.exec(fields.code) : null;
  if (code !== null) return asStatus(code[1]);

  const message = typeof fields.message === 'string' ? /status:\s*(\d{3})/i.exec(fields.message) : null;
  return message === null ? undefined : asStatus(message[1]);
}

function isTimeoutError(error: unknown): boolean {
  const name = errorFields(error)?.name;
  return name === 'TimeoutError' || name === 'AbortError';
}

/** Some Stores answer 404 with a message instead of an HTTP status; recognize both shapes. */
function isDatabaseMissing(error: unknown): boolean {
  const fields = errorFields(error);
  if (fields === null) return false;
  if (typeof fields.code === 'string' && /DATABASE_NOT_FOUND|DB_NOT_FOUND/i.test(fields.code)) return true;
  return (
    typeof fields.message === 'string' &&
    /(database|db)[^.]{0,80}(not found|does not exist|missing)/i.test(fields.message)
  );
}

/**
 * Maps a probe failure to the reason the API reports. Credentials and a missing database need a
 * configuration change, so they must not look like a transient outage to retrying clients.
 */
export function classifyStoreProbeError(error: unknown): StoreProbeFailure {
  const status = storeStatusFromError(error);

  // Status wins over message text: a 5xx may mention a missing database while still being a
  // transient outage, and misreading it as permanent would disable retries.
  if (status === 401 || status === 403) {
    return { reachable: false, reason: 'store_auth_failed', kind: 'http', status };
  }
  if (status === 404) {
    return { reachable: false, reason: 'store_database_missing', kind: 'http', status };
  }
  if (status !== undefined) {
    // 5xx and unexpected HTTP failures are retryable; no credential problem was identified.
    return { reachable: false, reason: 'store_unreachable', kind: 'http', status };
  }
  if (isDatabaseMissing(error)) {
    return { reachable: false, reason: 'store_database_missing', kind: 'http' };
  }
  if (isTimeoutError(error)) {
    return { reachable: false, reason: 'store_unreachable', kind: 'timeout' };
  }
  // fetch failures (DNS, connection refused) and unknown protocol errors: retrying is safe.
  return { reachable: false, reason: 'store_unreachable', kind: 'network' };
}

/**
 * Resolves a typed failure and never throws, so callers can only report a truthful status.
 * The token is forwarded to the probe but never logged or included in errors; logs carry only
 * the category and HTTP status.
 */
export async function probeStoreReachability(
  env: Pick<ServerEnv, 'storeUrl' | 'storeToken'>,
  options: StoreProbeOptions = {},
): Promise<StoreProbeResult> {
  const authToken = env.storeToken;
  if (authToken === undefined) {
    return { reachable: false, reason: 'store_token_missing', kind: 'unknown' };
  }

  const timeoutMs = options.timeoutMs ?? STORE_PROBE_TIMEOUT_MS;
  // The default probe owns the same budget internally; the race stays as a guard for injected
  // probes that never settle.
  const probe = options.probe ?? ((target: StoreTarget) => probeStoreOnce(target, timeoutMs));
  let timer: ReturnType<typeof setTimeout> | undefined;
  let timedOut = false;

  try {
    await Promise.race([
      probe({ url: env.storeUrl, authToken }),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          timedOut = true;
          reject(new Error('Store probe timed out'));
        }, timeoutMs);
      }),
    ]);
    return { reachable: true };
  } catch (error) {
    const failure: StoreProbeFailure = timedOut
      ? { reachable: false, reason: 'store_unreachable', kind: 'timeout' }
      : classifyStoreProbeError(error);
    console.warn('[agent-builder] store probe failed', {
      reason: failure.reason,
      kind: failure.kind,
      ...(failure.status === undefined ? {} : { status: failure.status }),
    });
    return failure;
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}
