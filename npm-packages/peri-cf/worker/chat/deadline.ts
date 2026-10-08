export class DeadlineError extends Error {}

export const ACP_CONTROL_TIMEOUT_MS = 20_000;
export const CANCELLATION_TIMEOUT_MS = 65_000;

export async function withDeadline<T>(promise: Promise<T>, milliseconds: number, operation: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new DeadlineError(`${operation} timed out; execution stop is not confirmed`)), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
