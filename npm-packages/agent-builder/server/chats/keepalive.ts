/**
 * Bun.serve closes connections that stay idle for `idleTimeout` seconds (default ~10s). The SSE
 * heartbeat must fire comfortably inside that window, otherwise an idle chat reconnects forever.
 * Both values live together so the relation stays testable instead of drifting apart.
 */
export const SSE_HEARTBEAT_MS = 15_000;
export const SERVER_IDLE_TIMEOUT_SECONDS = 30;
export const SERVER_IDLE_TIMEOUT_MS = SERVER_IDLE_TIMEOUT_SECONDS * 1000;
