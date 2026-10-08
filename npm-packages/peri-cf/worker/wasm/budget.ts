import { CF_WASM_ISOLATE_RESERVED_BYTES, CF_WASM_MAX_MEMORY_BYTES } from "../../shared/runtime-limits";

export class WasmCapacityError extends Error {
  constructor() { super("WASM isolate reservation exhausted; previous host cleanup must be confirmed"); }
}

export class WasmBudget {
  private readonly leases = new Set<string>();

  get reservedBytes(): number { return this.leases.size * CF_WASM_MAX_MEMORY_BYTES; }

  acquire(): { releaseAfterCleanup(): void } {
    if (this.reservedBytes + CF_WASM_MAX_MEMORY_BYTES > CF_WASM_ISOLATE_RESERVED_BYTES)
      throw new WasmCapacityError();
    const identity = crypto.randomUUID();
    this.leases.add(identity);
    return { releaseAfterCleanup: () => { this.leases.delete(identity); } };
  }
}

export const isolateWasmBudget = new WasmBudget();
