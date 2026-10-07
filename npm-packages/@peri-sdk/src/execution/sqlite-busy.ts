import { Database, SQLiteError } from "bun:sqlite";

export function isSqliteBusy(error: unknown): error is SQLiteError {
    return error instanceof SQLiteError && (error.errno & 0xff) === 5;
}

export async function retrySqliteBusy<Result>(operation: () => Result, retrySafe: () => boolean = () => true): Promise<Result> {
    const deadline = performance.now() + 5000;
    let backoffMs = 1;
    for (;;) {
        try { return operation(); }
        catch (error) {
            const remainingMs = deadline - performance.now();
            if (!isSqliteBusy(error) || !retrySafe() || remainingMs <= 0) throw error;
            await Bun.sleep(Math.min(backoffMs, remainingMs));
            backoffMs = Math.min(backoffMs * 2, 50);
            if (performance.now() >= deadline) throw error;
        }
    }
}

export function sqliteImmediate<Result>(database: Database, operation: () => Result): Promise<Result> {
    let callbackStarted = false;
    return retrySqliteBusy(() => {
        callbackStarted = false;
        return database.transaction(() => {
            callbackStarted = true;
            return operation();
        }).immediate();
    }, () => !callbackStarted);
}
