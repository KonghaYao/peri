import type { ExecutionMutation, ExecutionResolution } from "./types";

export type MutationEntry = { mutationId: string; digest: string; result: ExecutionResolution };

export class ExecutionMutationConflictError extends Error {
    constructor(readonly command: ExecutionMutation) {
        super("Execution mutationId cannot be reused with different parameters");
        this.name = "ExecutionMutationConflictError";
    }
}

export function replayMutation(command: ExecutionMutation, digest: string, entry: MutationEntry): ExecutionResolution {
    if (entry.digest !== digest) throw new ExecutionMutationConflictError(command);
    return structuredClone(entry.result);
}
