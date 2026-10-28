/** Race a host operation against a deadline; the timer is always cleared. */
export async function withTimeout<T>(promise: Promise<T>, milliseconds: number, error: () => Error): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(error()), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
