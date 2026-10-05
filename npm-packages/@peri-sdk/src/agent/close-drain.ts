import {
    SessionCloseIncompleteError, SessionCloseUnknownError, SessionControlNotAppliedError,
    type CloseOptions, type ControlCommand, type ControlReceipt,
    type ControlResolution, type ControlSnapshot,
} from "./session-control";

type CloseDrainOperations = {
    apply: () => Promise<ControlReceipt>;
    resolve: () => Promise<ControlResolution>;
    snapshot: () => Promise<ControlSnapshot>;
    isUnresolved: () => boolean;
};

export type CloseDrainClock = {
    now: () => number;
    sleep: (milliseconds: number) => Promise<unknown>;
    limit: <Result>(operation: Promise<Result>, milliseconds: number) => Promise<Result>;
};

const clock: CloseDrainClock = {
    now: () => performance.now(),
    sleep: (milliseconds) => Bun.sleep(milliseconds),
    limit: async (operation, milliseconds) => {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const expired = new Promise<never>((_resolve, reject) => {
            timer = setTimeout(() => reject(new Error("Close drain deadline exceeded")), milliseconds);
        });
        try { return await Promise.race([operation, expired]); }
        finally { clearTimeout(timer); }
    },
};

export function closeDrainBudget(options: CloseOptions): number {
    const milliseconds = options.drainTimeoutMs ?? 3_000;
    if (!Number.isSafeInteger(milliseconds) || milliseconds < 0)
        throw new TypeError("Close drainTimeoutMs must be a nonnegative safe integer");
    return milliseconds;
}

export async function drainClose(
    command: ControlCommand,
    operations: CloseDrainOperations,
    options: CloseOptions,
    timing: CloseDrainClock = clock,
): Promise<ControlReceipt> {
    const budget = closeDrainBudget(options);
    const deadline = timing.now() + budget;
    const request = <Result>(operation: Promise<Result>) => budget === 0
        ? operation : timing.limit(operation, Math.max(0, deadline - timing.now()));
    let receipt: ControlReceipt | undefined;
    let snapshot: ControlSnapshot | undefined;
    let unknown = operations.isUnresolved();
    let failure: unknown;
    const expired = () => unknown ? new SessionCloseUnknownError(command, failure)
        : new SessionCloseIncompleteError(receipt!, snapshot);
    let firstRound = true;
    while (true) {
        if (!firstRound && timing.now() >= deadline) throw expired();
        firstRound = false;
        try {
            if (unknown) {
                const resolution = await request(operations.resolve());
                if (resolution.status === "notApplied") throw new SessionControlNotAppliedError(command);
                if (resolution.status === "applied") {
                    receipt = resolution.receipt;
                    unknown = false;
                }
            } else {
                receipt = await request(operations.apply());
            }
        } catch (error) {
            if (error instanceof SessionControlNotAppliedError) throw error;
            failure = error;
            unknown = true;
        }
        if (!unknown && receipt) {
            if (receipt.decision.kind !== "accepted" || receipt.state.lifecycle !== command.expectedLifecycle)
                throw new SessionCloseIncompleteError(receipt);
            try {
                snapshot = await request(operations.snapshot());
                if (snapshot.state.lifecycle !== receipt.state.lifecycle)
                    throw new SessionCloseIncompleteError(receipt, snapshot);
                if (snapshot.state.status === "closed" && snapshot.settlement.status === "settled" &&
                    snapshot.state.revision >= receipt.state.revision &&
                    snapshot.state.controlGeneration >= receipt.state.controlGeneration)
                    return receipt;
            } catch (error) {
                if (error instanceof SessionCloseIncompleteError) throw error;
                failure = error;
            }
        }
        const remaining = deadline - timing.now();
        if (remaining <= 0 || budget === 0) throw expired();
        await timing.sleep(Math.min(50, remaining));
    }
}
