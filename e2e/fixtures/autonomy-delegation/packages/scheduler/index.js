export function nextRetryDelay(attempt) {
  return 1000 * (attempt + 1);
}
