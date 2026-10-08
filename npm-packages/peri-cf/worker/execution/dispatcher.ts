import {
  AcpExecutionDomain,
  ExecutionAdmissionCore,
  ExecutionCoordinator,
  ExecutionMutationConflictError,
  KvExecutionRegistry,
  SessionControl,
  type AdmissionRequest,
  type EntryRequest,
  type ExecutionMutation,
  type InstanceDescriptor,
  type InstanceProofProvider,
  type SettlementRequest,
  type ControlCommand,
  type ControlReceipt,
} from "../sdk";
import {
  DurableObjectAdmissionLedger,
  DurableObjectExecutionKv,
  type ExecutionStorage,
} from "./storage";
import type { AcpTransport, ExecutionDispatcher, PausedSession } from "../types";

export class WorkerExecution implements ExecutionDispatcher {
  private readonly registry: KvExecutionRegistry;
  private readonly ledger: DurableObjectAdmissionLedger;
  private readonly core: ExecutionAdmissionCore;
  private readonly coordinator: ExecutionCoordinator;
  private readonly instance: InstanceDescriptor;
  private readonly evidenceId = crypto.randomUUID();
  private readonly ownedCalls = new Set<Promise<void>>();
  private readonly stopCalls = new Map<string, Promise<void>>();
  private readonly controls = new SessionControl();
  private readonly stopCommands = new Map<string, ControlCommand>();
  private readonly shutdownCommands = new Map<string, ExecutionMutation>();
  private phase: "accepting" | "sealed" | "closed" = "accepting";

  constructor(private readonly transport: AcpTransport, private readonly storage: ExecutionStorage, generationId: string) {
    if (typeof generationId !== "string" || !generationId) throw new TypeError("WASM execution requires a transport generation ID");
    if (transport.generationId !== generationId) throw new TypeError("WASM transport generation identity does not match the dispatcher");
    this.instance = {
      instanceId: crypto.randomUUID(), generationId,
      proofRoute: { kind: "wasmHost", reference: generationId },
    };
    this.registry = new KvExecutionRegistry(new DurableObjectExecutionKv(storage));
    this.ledger = new DurableObjectAdmissionLedger(storage);
    const proofProvider: InstanceProofProvider = { proveStopped: async () => ({ status: "unknown" }) };
    this.coordinator = new ExecutionCoordinator({
      registry: this.registry, domain: new AcpExecutionDomain(transport),
      instance: this.instance, proofProvider, maxAttemptsPerWork: 8,
    });
    this.core = new ExecutionAdmissionCore({
      registry: this.registry, ledger: this.ledger, instance: this.instance,
      proofProvider, maxAttempts: 8,
      verifyEntered: (record) => this.coordinator.verifyRecordedEntries(record),
    });
  }

  handle(method: string, params: unknown): Promise<unknown> | undefined {
    if (!["peri/execution/admit", "peri/execution/entered", "peri/execution/settle"].includes(method)) return undefined;
    if (this.phase === "closed" || (this.phase !== "accepting" && method !== "peri/execution/settle"))
      return Promise.resolve({ status: "blocked", reason: "dispatcherGenerationSealed" });
    const operation = (async (): Promise<unknown> => {
      if (method === "peri/execution/admit") return this.core.admit(params as AdmissionRequest);
      if (method === "peri/execution/entered") return this.core.entered(params as EntryRequest);
      return this.core.settle(params as SettlementRequest);
    })().catch((error: unknown) => {
      if (error instanceof TypeError || error instanceof ExecutionMutationConflictError) throw error;
      return { status: "unknown" };
    });
    const joined = operation.then(() => {}, () => {});
    this.ownedCalls.add(joined);
    void joined.then(() => this.ownedCalls.delete(joined));
    return operation;
  }

  seal(): void {
    if (this.phase === "accepting") this.phase = "sealed";
    this.coordinator.seal();
  }

  async stop(sessionId: string): Promise<PausedSession | void> {
    let command = this.stopCommands.get(sessionId);
    if (!command) {
      const deadline = Date.now() + 2_000;
      for (;;) {
        if (this.phase === "closed") return;
        const snapshot = await this.controls.snapshot(this.transport, sessionId);
        if (snapshot.state.attempt) {
          command = {
            sessionId, commandId: crypto.randomUUID(),
            expectedLifecycle: snapshot.state.lifecycle,
            expectedRevision: snapshot.state.revision,
            expectedControlGeneration: snapshot.state.controlGeneration,
            action: { kind: "stop", target: snapshot.state.attempt },
          };
          this.stopCommands.set(sessionId, command);
          break;
        }
        if (Date.now() >= deadline) throw new Error("No exact execution target is available for cancellation");
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    }
    let receipt: ControlReceipt;
    try {
      receipt = await this.controls.apply(this.transport, command);
    } catch (error) {
      const resolution = await this.controls.resolve(this.transport, command);
      if (resolution.status !== "applied") throw error;
      receipt = resolution.receipt;
    }
    if (receipt.decision.kind !== "accepted") throw new Error(`Stop rejected: ${receipt.decision.reason}`);
    return { lifecycle: receipt.state.lifecycle, controlGeneration: receipt.state.controlGeneration,
      resumeCommandId: `resume:${receipt.commandId}` };
  }

  async resume(sessionId: string, paused: PausedSession): Promise<void> {
    const key = `peri:cf-control:${encodeURIComponent(paused.resumeCommandId)}`;
    let command = await this.storage.get<ControlCommand>(key);
    if (!command) {
      const snapshot = await this.controls.snapshot(this.transport, sessionId);
      if (snapshot.state.status !== "paused" || snapshot.state.lifecycle !== paused.lifecycle
        || snapshot.state.controlGeneration !== paused.controlGeneration)
        throw new Error("Session control changed after this chat's stop; refusing to resume another authority's pause");
      command = { sessionId, commandId: paused.resumeCommandId, expectedLifecycle: paused.lifecycle,
        expectedRevision: snapshot.state.revision, expectedControlGeneration: paused.controlGeneration,
        action: { kind: "resume" } };
      await this.storage.put(key, command);
    }
    let receipt: ControlReceipt;
    try {
      receipt = await this.controls.apply(this.transport, command);
    } catch (error) {
      const resolution = await this.controls.resolve(this.transport, command);
      if (resolution.status !== "applied") throw error;
      receipt = resolution.receipt;
    }
    if (receipt.decision.kind !== "accepted") throw new Error(`Resume rejected: ${receipt.decision.reason}`);
  }

  stopAfterHostClose(sessionId: string): Promise<void> {
    const existing = this.stopCalls.get(sessionId);
    if (existing) return existing;
    const operation = this.stopOnce(sessionId);
    this.stopCalls.set(sessionId, operation);
    void operation.catch(() => {
      if (this.stopCalls.get(sessionId) === operation) this.stopCalls.delete(sessionId);
    });
    return operation;
  }

  private async stopOnce(sessionId: string): Promise<void> {
    this.seal();
    this.phase = "closed";
    await Promise.all([...this.ownedCalls]);
    const native = this.transport as AcpTransport & { executionStopped?: () => Promise<boolean> };
    if (this.transport.generationId !== this.instance.generationId
      || typeof native.executionStopped !== "function" || !await native.executionStopped())
      throw new Error("WASM host exit evidence is unknown for this transport generation");
    let proposed = this.shutdownCommands.get(sessionId);
    if (!proposed) {
      const record = await this.registry.read(sessionId);
      if (!record.instance || record.instance.instanceId !== this.instance.instanceId
        || record.instance.generationId !== this.instance.generationId) return;
      proposed = {
        sessionId,
        mutationId: `shutdown:${this.instance.instanceId}:${this.instance.generationId}:${encodeURIComponent(sessionId)}`,
        expectedRevision: record.revision,
        action: {
          kind: "stopInstance",
          proof: { kind: "instanceStopped", instanceId: this.instance.instanceId,
            generationId: this.instance.generationId, evidenceId: this.evidenceId },
        },
      };
      this.shutdownCommands.set(sessionId, proposed);
    }
    const claimed = await this.ledger.claimStep(proposed.mutationId, proposed);
    this.shutdownCommands.set(sessionId, claimed.command);
    const result = claimed.inserted ? await this.registry.apply(claimed.command) : await this.registry.resolve(claimed.command);
    if (result.status !== "applied" || result.receipt.decision.kind !== "accepted")
      throw new Error("Execution instance shutdown receipt is not confirmed");
  }
}
