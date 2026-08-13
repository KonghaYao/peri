/**
 * Runs a server-authoritative form mutation.
 *
 * `send` must wire its two callbacks to committed/duplicate and error/timeout.
 * The form unlocks for every terminal path, while destructive UI cleanup runs
 * only after the server confirms the mutation. Callback delivery is idempotent.
 */
export function runConfirmedMutation(
  start: () => void,
  stop: () => void,
  send: (onCommitted: () => void, onFailed: () => void) => boolean,
  onCommitted: () => void,
): boolean {
  start();
  let committed = false;
  const settle = (commit: boolean) => {
    if (commit && committed) return;
    if (commit) committed = true;
    stop();
    if (commit) onCommitted();
  };
  const sent = send(() => settle(true), () => settle(false));
  if (!sent) settle(false);
  return sent;
}
