export class DeadlineError extends Error {}

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
