// 主入口与 /wasm 独立打包；同一 realm 内用稳定品牌识别不同副本的错误。
const RPC_ERROR_BRAND: unique symbol = Symbol.for("@peri-code/sdk/RpcError");

export class RpcError extends Error {
    readonly [RPC_ERROR_BRAND] = true;

    constructor(readonly code: number, message: string, readonly data?: unknown) {
        super(message);
        this.name = "RpcError";
    }
}

export function isRpcError(error: unknown): error is RpcError {
    return error instanceof Error
        && RPC_ERROR_BRAND in error
        && error[RPC_ERROR_BRAND] === true
        && error.name === "RpcError"
        && "code" in error
        && typeof error.code === "number"
        && typeof error.message === "string";
}
