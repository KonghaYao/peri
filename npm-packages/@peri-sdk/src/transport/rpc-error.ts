export class RpcError extends Error {
    constructor(readonly code: number, message: string, readonly data?: unknown) {
        super(message);
        this.name = "RpcError";
    }
}
