/** Timeout without settlement evidence; callers must not treat the operation as confirmed. */
export class DeadlineError extends Error {}

/** Race a host operation against a deadline. The timer is always cleared and the original rejection is preserved. */
export async function withDeadline<T>(promise: Promise<T>, milliseconds: number, operation: string): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
        return await Promise.race([
            promise,
            new Promise<never>((_, reject) => {
                timer = setTimeout(() => reject(new DeadlineError(`${operation} timed out`)), milliseconds);
            }),
        ]);
    } finally {
        clearTimeout(timer);
    }
}
