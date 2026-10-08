import type {
  AdmissionLedger,
  AdmissionRequestRecord,
  AdmissionStepClaim,
  AtomicExecutionKv,
  ExecutionKvValue,
  ExecutionMutation,
} from "../sdk";

export interface ExecutionTransaction {
  get<Value>(key: string): Promise<Value | undefined>;
  put<Value>(key: string, value: Value): Promise<void>;
  delete(key: string): Promise<boolean>;
}

export interface ExecutionStorage extends ExecutionTransaction {
  transaction<Result>(operation: (transaction: ExecutionTransaction) => Promise<Result>): Promise<Result>;
}

function storageKey(kind: "registry" | "claim" | "request" | "step", key: string): string {
  return `peri:cf-execution:${kind}:${encodeURIComponent(key)}`;
}

export class DurableObjectExecutionKv implements AtomicExecutionKv {
  readonly durability = "durable" as const;

  constructor(private readonly storage: ExecutionStorage) {}

  async readExecutionValue(key: string): Promise<ExecutionKvValue | null> {
    return structuredClone(await this.storage.get<ExecutionKvValue>(storageKey("registry", key)) ?? null);
  }

  compareAndSetExecutionValue(key: string, expectedVersion: number | null, next: ExecutionKvValue): Promise<boolean> {
    const value = structuredClone(next);
    return this.storage.transaction(async (transaction) => {
      const name = storageKey("registry", key);
      const previous = await transaction.get<ExecutionKvValue>(name);
      if ((previous?.version ?? null) !== expectedVersion) return false;
      await transaction.put(name, value);
      return true;
    });
  }

  claimIfAbsent(key: string, owner: string): Promise<boolean> {
    return this.storage.transaction(async (transaction) => {
      const name = storageKey("claim", key);
      if (await transaction.get<string>(name) !== undefined) return false;
      await transaction.put(name, owner);
      return true;
    });
  }

  releaseIfOwner(key: string, owner: string): Promise<boolean> {
    return this.storage.transaction(async (transaction) => {
      const name = storageKey("claim", key);
      if (await transaction.get<string>(name) !== owner) return false;
      return transaction.delete(name);
    });
  }
}

export class DurableObjectAdmissionLedger implements AdmissionLedger {
  constructor(private readonly storage: ExecutionStorage) {}

  async readRequest(requestKey: string): Promise<AdmissionRequestRecord | null> {
    return structuredClone(await this.storage.get<AdmissionRequestRecord>(storageKey("request", requestKey)) ?? null);
  }

  claimRequest(requestKey: string, proposed: AdmissionRequestRecord): Promise<AdmissionRequestRecord> {
    const record = structuredClone(proposed);
    return this.storage.transaction(async (transaction) => {
      const name = storageKey("request", requestKey);
      const existing = await transaction.get<AdmissionRequestRecord>(name);
      if (existing !== undefined) return structuredClone(existing);
      await transaction.put(name, record);
      return structuredClone(record);
    });
  }

  claimStep(stepId: string, proposed: ExecutionMutation): Promise<AdmissionStepClaim> {
    const command = structuredClone(proposed);
    return this.storage.transaction(async (transaction) => {
      const name = storageKey("step", stepId);
      const existing = await transaction.get<ExecutionMutation>(name);
      if (existing !== undefined) return { inserted: false, command: structuredClone(existing) };
      await transaction.put(name, command);
      return { inserted: true, command: structuredClone(command) };
    });
  }
}
