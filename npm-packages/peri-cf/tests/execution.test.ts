import { describe, expect, spyOn, test } from "bun:test";
import { KvExecutionRegistry, type AdmissionOutcome, type AdmissionRequest, type ExecutionKvValue,
  type ExecutionMutation, type ExecutionTicket, type SettlementRequest,
  type ControlCommand, type ControlReceipt, type ControlSnapshot } from "../worker/sdk";
import { DurableObjectAdmissionLedger, DurableObjectExecutionKv,
  type ExecutionStorage, type ExecutionTransaction } from "../worker/execution/storage";
import { WorkerExecution } from "../worker/execution/dispatcher";
import type { AcpTransport, PausedSession } from "../worker/types";

function barrier() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => { release = resolve; });
  return { promise, release };
}

class TransactionalStorage implements ExecutionStorage {
  private values = new Map<string, unknown>();
  private tail: Promise<unknown> = Promise.resolve();

  private serialize<Result>(operation: () => Promise<Result>): Promise<Result> {
    const result = this.tail.catch(() => {}).then(operation);
    this.tail = result;
    return result;
  }

  get<Value>(key: string): Promise<Value | undefined> {
    return this.serialize(async () => structuredClone(this.values.get(key)) as Value | undefined);
  }

  put<Value>(key: string, value: Value): Promise<void> {
    const snapshot = structuredClone(value);
    return this.transaction((transaction) => transaction.put(key, snapshot));
  }

  delete(key: string): Promise<boolean> {
    return this.transaction((transaction) => transaction.delete(key));
  }

  transaction<Result>(operation: (transaction: ExecutionTransaction) => Promise<Result>): Promise<Result> {
    return this.serialize(async () => {
      const draft = structuredClone(this.values);
      const result = await operation({
        async get<Value>(key: string) { return structuredClone(draft.get(key)) as Value | undefined; },
        async put<Value>(key: string, value: Value) { draft.set(key, structuredClone(value)); },
        async delete(key: string) { return draft.delete(key); },
      });
      this.values = structuredClone(draft);
      return result;
    });
  }
}

function admissionRequest(requestId = "request-1", sessionId = "session-1"): AdmissionRequest {
  return { requestId, snapshot: {
    sessionId, control: { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null },
    candidates: [{ workId: "work-1", workRevision: 0, stage: "reasonReady", requiresRecovery: false }],
    blocked: false,
  } };
}

function settlement(admission: ExecutionTicket): SettlementRequest {
  return { admission, proof: {
    kind: "attemptStopped", instanceId: admission.instanceId, generationId: admission.generationId,
    execution: admission.execution, evidenceId: "fixture-attempt-completed",
  } };
}

class ControlSnapshotTransport implements AcpTransport {
  generationId = "fixture-generation-1";
  closeFailure?: Error;
  stopFailure?: Error;
  stopDecision: ControlReceipt["decision"] = { kind: "accepted" };
  stopResolution: "applied" | "unknown" | "notApplied" = "applied";
  controlSnapshot: ControlSnapshot = {
    state: { lifecycle: 2, revision: 7, controlGeneration: 3, status: "active",
      attempt: { turnId: "fixture-turn", attemptId: "fixture-attempt" } },
    settlement: { status: "pending" },
  };
  initialSnapshot?: ControlSnapshot;
  private hostClosed = false;
  executionStopped?: () => Promise<boolean> = async () => this.hostClosed;
  readonly requests: { method: string; params?: unknown }[] = [];
  readonly controlReceipts = new Map<string, ControlReceipt>();

  async request<Result>(method: string, params?: unknown): Promise<Result> {
    this.requests.push({ method, params: structuredClone(params) });
    if (method === "session/control/state") {
      const snapshot = this.initialSnapshot ?? this.controlSnapshot;
      this.initialSnapshot = undefined;
      return structuredClone(snapshot) as Result;
    }
    if (method === "session/control" || method === "session/control/resolve") {
      const command = params as ControlCommand;
      let receipt = this.controlReceipts.get(command.commandId);
      if (!receipt) {
        const state = structuredClone(this.controlSnapshot.state);
        if (this.stopDecision.kind === "accepted") {
          state.status = command.action.kind === "stop" ? "paused" : "active";
          state.attempt = null;
          state.revision++;
          state.controlGeneration++;
        }
        receipt = { sessionId: command.sessionId, commandId: command.commandId,
          decision: this.stopDecision, state };
        this.controlReceipts.set(command.commandId, structuredClone(receipt));
        if (this.stopDecision.kind === "accepted") this.controlSnapshot.state = structuredClone(state);
      }
      if (method === "session/control") {
        if (this.stopFailure) throw this.stopFailure;
        return receipt as Result;
      }
      return (this.stopResolution === "applied" ? { status: "applied", receipt }
        : { status: this.stopResolution }) as Result;
    }
    if (method === "session/work/query") return {
      control: admissionRequest().snapshot.control,
      work: { workId: "work-1", revision: 0, lifecycle: 1, controlGeneration: 0 },
    } as Result;
    if (method === "session/execute/resolve") {
      const ticket = (params as { ticket: ExecutionTicket }).ticket;
      return { status: "applied", reply: this.hostClosed
        ? { status: "settled", ticket, proof: settlement(ticket).proof }
        : { status: "running", ticket } } as Result;
    }
    throw new Error(`Unexpected domain request: ${method}`);
  }

  async sendRequest<Result>(): Promise<{ response: Promise<Result> }> { throw new Error("Unexpected prompt"); }
  async notify(): Promise<void> { throw new Error("Unexpected notification"); }
  async *events() {}
  subscribe(): () => void { return () => {}; }
  setRequestHandler(): void {}
  async close(): Promise<void> {
    if (this.closeFailure) throw this.closeFailure;
    this.hostClosed = true;
  }
}

function fixture() {
  const storage = new TransactionalStorage();
  const transport = new ControlSnapshotTransport();
  const dispatcher = new WorkerExecution(transport, storage, transport.generationId);
  const registry = new KvExecutionRegistry(new DurableObjectExecutionKv(storage));
  return { storage, transport, dispatcher, registry };
}

async function pause(app: ReturnType<typeof fixture>): Promise<PausedSession> {
  const marker = await app.dispatcher.stop("session-1");
  if (!marker) throw new Error("Own Stop did not provide its pause marker");
  return marker;
}

async function admit(dispatcher: WorkerExecution, request = admissionRequest()): Promise<ExecutionTicket> {
  const result = await dispatcher.handle("peri/execution/admit", request) as AdmissionOutcome;
  expect(result.status).toBe("admitted");
  if (result.status !== "admitted") throw new Error(`Admission failed: ${JSON.stringify(result)}`);
  return result.admission;
}

describe("DO execution storage transactions", () => {
  test("rolls back writes and deletes after failure, isolating the queued reader", async () => {
    const storage = new TransactionalStorage();
    await storage.put("existing", { value: "original" });
    const entered = barrier();
    const release = barrier();
    const failed = storage.transaction(async (transaction) => {
      await transaction.delete("existing");
      await transaction.put("partial", { value: "uncommitted" });
      entered.release();
      await release.promise;
      throw new Error("rollback");
    });
    const outcome = failed.then(() => "unexpected commit", (error: unknown) => error);
    await entered.promise;
    const reader = storage.get<{ value: string }>("existing");
    release.release();
    expect(await outcome).toBeInstanceOf(Error);
    expect((await outcome as Error).message).toBe("rollback");
    expect(await reader).toEqual({ value: "original" });
    expect(await storage.get("partial")).toBeUndefined();
  });

  test("serializes concurrent transactions and copies their committed state", async () => {
    const storage = new TransactionalStorage();
    const entered = barrier();
    const release = barrier();
    const trace: string[] = [];
    const original = { count: 1 };
    const first = storage.transaction(async (transaction) => {
      trace.push("first:start");
      await transaction.put("counter", original);
      entered.release();
      await release.promise;
      trace.push("first:end");
    });
    await entered.promise;
    const second = storage.transaction(async (transaction) => {
      trace.push("second:start");
      const previous = (await transaction.get<{ count: number }>("counter"))!;
      await transaction.put("counter", { count: previous.count + 1 });
    });
    expect(trace).toEqual(["first:start"]);
    original.count = 999;
    release.release();
    await Promise.all([first, second]);
    expect(trace).toEqual(["first:start", "first:end", "second:start"]);
    expect(await storage.get<{ count: number }>("counter")).toEqual({ count: 2 });
    const read = (await storage.get<{ count: number }>("counter"))!;
    read.count = 1000;
    expect(await storage.get<{ count: number }>("counter")).toEqual({ count: 2 });
  });

  test("versioned CAS race has exactly one winner and rejects a stale writer", async () => {
    const storage = new TransactionalStorage();
    const adapter = new DurableObjectExecutionKv(storage);
    const empty = await new KvExecutionRegistry(adapter).read("session-1");
    const next: ExecutionKvValue = { protocolVersion: 1, version: 1, record: empty, mutations: [] };
    const rival = { ...next, record: { ...empty, revision: 9 } };
    expect(await Promise.all([
      adapter.compareAndSetExecutionValue("race", null, next),
      adapter.compareAndSetExecutionValue("race", null, rival),
    ])).toEqual([true, false]);
    expect(await adapter.readExecutionValue("race")).toEqual(next);
    expect(await adapter.compareAndSetExecutionValue("race", 0, rival)).toBe(false);
    expect(await adapter.compareAndSetExecutionValue("race", 1, { ...next, version: 2 })).toBe(true);
    expect((await adapter.readExecutionValue("race"))?.version).toBe(2);
  });

  test("ownership claims race atomically and only the original owner can release", async () => {
    const adapter = new DurableObjectExecutionKv(new TransactionalStorage());
    expect(await Promise.all([adapter.claimIfAbsent("claim", "first"), adapter.claimIfAbsent("claim", "second")])).toEqual([true, false]);
    expect(await adapter.releaseIfOwner("claim", "second")).toBe(false);
    expect(await adapter.claimIfAbsent("claim", "second")).toBe(false);
    expect(await adapter.releaseIfOwner("claim", "first")).toBe(true);
    expect(await adapter.claimIfAbsent("claim", "second")).toBe(true);
  });

  test("claimStep replays the exact persisted command, including its original revision", async () => {
    const storage = new TransactionalStorage();
    const ledger = new DurableObjectAdmissionLedger(storage);
    const command: ExecutionMutation = { sessionId: "session-1", mutationId: "stable-step", expectedRevision: 0,
      action: { kind: "observeControl", control: admissionRequest().snapshot.control } };
    const original = structuredClone(command);
    const [first, rival] = await Promise.all([
      ledger.claimStep("stable-step", command),
      ledger.claimStep("stable-step", { ...command, expectedRevision: 55 }),
    ]);
    expect(first).toEqual({ inserted: true, command: original });
    expect(rival).toEqual({ inserted: false, command: original });
    command.expectedRevision = 77;
    first.command.expectedRevision = 88;
    expect(await new DurableObjectAdmissionLedger(storage).claimStep("stable-step", command)).toEqual({ inserted: false, command: original });
  });

  test("the SDK registry and mutation receipts survive adapter reconstruction", async () => {
    const storage = new TransactionalStorage();
    let registry = new KvExecutionRegistry(new DurableObjectExecutionKv(storage));
    const command: ExecutionMutation = { sessionId: "session-1", mutationId: "register", expectedRevision: 0,
      action: { kind: "registerInstance", instance: { instanceId: "instance", generationId: "generation",
        proofRoute: { kind: "wasmHost", reference: "generation" } } } };
    const receipt = await registry.apply(command);
    expect(receipt).toMatchObject({ status: "applied", receipt: { decision: { kind: "accepted" } } });
    const persisted = await registry.read("session-1");
    registry = new KvExecutionRegistry(new DurableObjectExecutionKv(storage));
    expect(registry.durability).toBe("durable");
    expect(await registry.read("session-1")).toEqual(persisted);
    expect(await registry.resolve(command)).toEqual(receipt);
    expect(await registry.apply(command)).toEqual(receipt);
  });
});

describe("WorkerExecution with real portable SDK admission services", () => {
  test("own Stop returns a stable pause marker and matching resume uses its durable command identity", async () => {
    const app = fixture();
    const marker = await pause(app);
    const stop = app.transport.requests[1].params as ControlCommand;
    expect(marker).toEqual({ lifecycle: 2, controlGeneration: 4, resumeCommandId: `resume:${stop.commandId}` });
    expect(await pause(app)).toEqual(marker);
    await app.dispatcher.resume("session-1", marker);
    const resume = app.transport.requests.find(({ params }) => (params as ControlCommand | undefined)?.action?.kind === "resume")!.params as ControlCommand;
    expect(resume).toEqual({ sessionId: "session-1", commandId: marker.resumeCommandId,
      expectedLifecycle: 2, expectedRevision: 8, expectedControlGeneration: 4, action: { kind: "resume" } });
    expect(await app.storage.get<ControlCommand>(`peri:cf-control:${encodeURIComponent(marker.resumeCommandId)}`)).toEqual(resume);
    expect(app.transport.controlSnapshot.state.status).toBe("active");
  });

  test.each(["lifecycle", "controlGeneration"] as const)("resume rejects changed %s authority without issuing a control mutation", async (field) => {
    const app = fixture();
    const marker = await pause(app);
    app.transport.controlSnapshot.state[field]++;
    const before = app.transport.requests.filter(({ method }) => method === "session/control").length;
    await expect(app.dispatcher.resume("session-1", marker)).rejects.toThrow("refusing to resume another authority");
    expect(app.transport.requests.filter(({ method }) => method === "session/control")).toHaveLength(before);
    expect(await app.storage.get(`peri:cf-control:${encodeURIComponent(marker.resumeCommandId)}`)).toBeUndefined();
  });

  test("resume rejects a matching generation that is not paused", async () => {
    const app = fixture();
    const marker = await pause(app);
    app.transport.controlSnapshot.state.status = "active";
    await expect(app.dispatcher.resume("session-1", marker)).rejects.toThrow("refusing to resume another authority");
    expect(app.transport.requests.some(({ params }) => (params as ControlCommand | undefined)?.action?.kind === "resume")).toBe(false);
  });

  test("resume resolves an unknown receipt using the original durable command across dispatcher reconstruction", async () => {
    const app = fixture();
    const marker = await pause(app);
    app.transport.stopFailure = new Error("resume response lost");
    app.transport.stopResolution = "unknown";
    await expect(app.dispatcher.resume("session-1", marker)).rejects.toThrow("resume response lost");
    const command = (await app.storage.get<ControlCommand>(`peri:cf-control:${encodeURIComponent(marker.resumeCommandId)}`))!;
    const requestCount = app.transport.requests.length;
    const replacement = new WorkerExecution(app.transport, app.storage, app.transport.generationId);
    app.transport.stopResolution = "applied";
    await replacement.resume("session-1", marker);
    expect(app.transport.requests.slice(requestCount).map(({ method }) => method)).toEqual(["session/control", "session/control/resolve"]);
    for (const request of app.transport.requests.slice(requestCount)) expect(request.params).toEqual(command);
    expect(await app.storage.get<ControlCommand>(`peri:cf-control:${encodeURIComponent(marker.resumeCommandId)}`)).toEqual(command);
  });

  test("a rejected resume does not become successful activation", async () => {
    const app = fixture();
    const marker = await pause(app);
    app.transport.stopDecision = { kind: "rejected", reason: "staleControlGeneration" };
    await expect(app.dispatcher.resume("session-1", marker)).rejects.toThrow("Resume rejected: staleControlGeneration");
    expect(app.transport.controlSnapshot.state.status).toBe("paused");
  });

  test("typed Stop uses the observed lifecycle, revision, generation and exact attempt target", async () => {
    const app = fixture();
    await app.dispatcher.stop("session-1");
    expect(app.transport.requests.map(({ method }) => method)).toEqual(["session/control/state", "session/control"]);
    expect(app.transport.requests[0].params).toEqual({ sessionId: "session-1" });
    const command = app.transport.requests[1].params as ControlCommand;
    expect(command).toEqual({ sessionId: "session-1", commandId: command.commandId,
      expectedLifecycle: 2, expectedRevision: 7, expectedControlGeneration: 3,
      action: { kind: "stop", target: { turnId: "fixture-turn", attemptId: "fixture-attempt" } } });
    expect(command.commandId).toBeTruthy();
    expect((await app.registry.read("session-1")).instance).toBeNull();
  });

  test("lost Stop response resolves the exact original command rather than creating another mutation", async () => {
    const app = fixture();
    app.transport.stopFailure = new Error("control response lost");
    await app.dispatcher.stop("session-1");
    expect(app.transport.requests.map(({ method }) => method)).toEqual([
      "session/control/state", "session/control", "session/control/resolve",
    ]);
    expect(app.transport.requests[2].params).toEqual(app.transport.requests[1].params);
  });

  test.each(["unknown", "notApplied"] as const)("Stop resolution %s cannot be reported as successful cancellation", async (status) => {
    const app = fixture();
    app.transport.stopFailure = new Error("control response lost");
    app.transport.stopResolution = status;
    await expect(app.dispatcher.stop("session-1")).rejects.toThrow("control response lost");
    expect(app.transport.requests[2].params).toEqual(app.transport.requests[1].params);
    expect((await app.registry.read("session-1")).instance?.stoppedProof).toBeUndefined();
  });

  test("retry after Unknown only resolves the saved Stop command, even when the snapshot target changes", async () => {
    const app = fixture();
    app.transport.stopFailure = new Error("control response lost");
    app.transport.stopResolution = "unknown";
    await expect(app.dispatcher.stop("session-1")).rejects.toThrow("control response lost");
    const original = structuredClone(app.transport.requests[1].params);
    app.transport.controlSnapshot.state.attempt = { turnId: "replacement-turn", attemptId: "replacement-attempt" };
    app.transport.stopResolution = "applied";
    await app.dispatcher.stop("session-1");
    expect(app.transport.requests.map(({ method }) => method)).toEqual([
      "session/control/state", "session/control", "session/control/resolve", "session/control/resolve",
    ]);
    expect(app.transport.requests[3].params).toEqual(original);
  });

  test("rejected exact-target Stop is a failure rather than an accepted cancellation", async () => {
    const app = fixture();
    app.transport.stopDecision = { kind: "rejected", reason: "staleAttempt" };
    await expect(app.dispatcher.stop("session-1")).rejects.toThrow("Stop rejected: staleAttempt");
    expect(app.transport.requests.map(({ method }) => method)).toEqual(["session/control/state", "session/control"]);
  });

  test("a rejected resolve receipt is not accepted after an unknown apply", async () => {
    const app = fixture();
    app.transport.stopFailure = new Error("control response lost");
    app.transport.stopDecision = { kind: "rejected", reason: "staleRevision" };
    await expect(app.dispatcher.stop("session-1")).rejects.toThrow("Stop rejected: staleRevision");
    expect(app.transport.requests[2].params).toEqual(app.transport.requests[1].params);
  });

  test("polls a not-yet-bound snapshot and sends Stop only after an exact target appears", async () => {
    const app = fixture();
    app.transport.initialSnapshot = structuredClone(app.transport.controlSnapshot);
    app.transport.initialSnapshot.state.attempt = null;
    await app.dispatcher.stop("session-1");
    expect(app.transport.requests.map(({ method }) => method)).toEqual([
      "session/control/state", "session/control/state", "session/control",
    ]);
    expect((app.transport.requests[2].params as ControlCommand).action).toEqual({
      kind: "stop", target: { turnId: "fixture-turn", attemptId: "fixture-attempt" },
    });
  });

  test("an unbound target at the polling deadline fails without sending a targetless command", async () => {
    const app = fixture();
    app.transport.controlSnapshot.state.attempt = null;
    const clock = spyOn(Date, "now").mockReturnValueOnce(0).mockReturnValue(3000);
    try {
      await expect(app.dispatcher.stop("session-1")).rejects.toThrow("No exact execution target");
      expect(app.transport.requests.map(({ method }) => method)).toEqual(["session/control/state"]);
    } finally {
      clock.mockRestore();
    }
  });

  test("admit, entered and settle persist the exact ticket and replay idempotently", async () => {
    const app = fixture();
    const ticket = await admit(app.dispatcher);
    expect((await app.registry.read(ticket.sessionId)).attempt).toMatchObject({ phase: "entering", ticket });
    expect(await admit(app.dispatcher)).toEqual(ticket);
    const entry = { admission: ticket, entryEvidenceId: "fixture-rust-store-entry" };
    expect(await app.dispatcher.handle("peri/execution/entered", entry)).toEqual({ status: "applied", receipt: entry });
    expect(await app.dispatcher.handle("peri/execution/entered", entry)).toEqual({ status: "applied", receipt: entry });
    expect((await app.registry.read(ticket.sessionId)).attempt).toMatchObject({ phase: "running", enteredEvidenceId: entry.entryEvidenceId });
    const completed = settlement(ticket);
    const result = await app.dispatcher.handle("peri/execution/settle", completed);
    expect(result).toEqual({ status: "applied", receipt: { admission: ticket, evidenceId: completed.proof.evidenceId } });
    expect(await app.dispatcher.handle("peri/execution/settle", completed)).toEqual(result);
    const restored = new KvExecutionRegistry(new DurableObjectExecutionKv(app.storage));
    expect((await restored.read(ticket.sessionId)).attempt).toMatchObject({ phase: "settled", ticket, stoppedProof: completed.proof });
    expect((await restored.read(ticket.sessionId)).budgets[0].attempts).toBe(1);
  });

  test("seal rejects new admit and entry but permits settlement of the admitted attempt", async () => {
    const app = fixture();
    const ticket = await admit(app.dispatcher);
    app.dispatcher.seal();
    expect(await app.dispatcher.handle("peri/execution/admit", admissionRequest("new"))).toEqual({ status: "blocked", reason: "dispatcherGenerationSealed" });
    expect(await app.dispatcher.handle("peri/execution/entered", { admission: ticket, entryEvidenceId: "entry" })).toEqual({ status: "blocked", reason: "dispatcherGenerationSealed" });
    expect(await app.dispatcher.handle("peri/execution/settle", settlement(ticket))).toMatchObject({ status: "applied" });
    expect(app.dispatcher.handle("session/request_permission", {})).toBeUndefined();
  });

  test("rejects mismatched constructor generation rather than inventing a stopped identity", () => {
    const app = fixture();
    expect(() => new WorkerExecution(app.transport, app.storage, "other-generation")).toThrow("generation identity");
    expect(() => new WorkerExecution(app.transport, app.storage, "")).toThrow("generation ID");
  });

  test("does not persist stopped proof before confirmed host close and allows proof-checked retry", async () => {
    const app = fixture();
    const ticket = await admit(app.dispatcher);
    await expect(app.dispatcher.stopAfterHostClose(ticket.sessionId)).rejects.toThrow("exit evidence is unknown");
    expect((await app.registry.read(ticket.sessionId)).instance).toMatchObject({ status: "registered", generationId: ticket.generationId });
    expect((await app.registry.read(ticket.sessionId)).instance?.stoppedProof).toBeUndefined();
    await app.transport.close();
    await app.dispatcher.stopAfterHostClose(ticket.sessionId);
    const stopped = await app.registry.read(ticket.sessionId);
    expect(stopped.instance).toMatchObject({ status: "stopped", stoppedProof: {
      kind: "instanceStopped", instanceId: ticket.instanceId, generationId: ticket.generationId,
    } });
    expect(stopped.instance?.stoppedProof?.evidenceId).toBeTruthy();
    const revision = stopped.revision;
    await app.dispatcher.stopAfterHostClose(ticket.sessionId);
    expect((await app.registry.read(ticket.sessionId)).revision).toBe(revision);
  });

  test("failed close cannot produce stopped proof", async () => {
    const app = fixture();
    const ticket = await admit(app.dispatcher);
    app.transport.closeFailure = new Error("close unconfirmed");
    await expect(app.transport.close()).rejects.toThrow("close unconfirmed");
    await expect(app.dispatcher.stopAfterHostClose(ticket.sessionId)).rejects.toThrow("exit evidence is unknown");
    expect((await app.registry.read(ticket.sessionId)).instance?.status).toBe("registered");
    expect((await app.registry.read(ticket.sessionId)).instance?.stoppedProof).toBeUndefined();
  });

  test("missing executionStopped capability is not proof even after fixture close", async () => {
    const app = fixture();
    const ticket = await admit(app.dispatcher);
    delete app.transport.executionStopped;
    await app.transport.close();
    await expect(app.dispatcher.stopAfterHostClose(ticket.sessionId)).rejects.toThrow("exit evidence is unknown");
    expect((await app.registry.read(ticket.sessionId)).instance?.stoppedProof).toBeUndefined();
  });

  test("stopping a different transport generation is rejected even after confirmed close", async () => {
    const app = fixture();
    const ticket = await admit(app.dispatcher);
    await app.transport.close();
    app.transport.generationId = "replacement-generation";
    await expect(app.dispatcher.stopAfterHostClose(ticket.sessionId)).rejects.toThrow("exit evidence is unknown");
    expect((await app.registry.read(ticket.sessionId)).instance?.stoppedProof).toBeUndefined();
  });

  test("settlement with another generation proof is rejected without settling the attempt", async () => {
    const app = fixture();
    const ticket = await admit(app.dispatcher);
    const invalid = settlement(ticket);
    invalid.proof.generationId = "forged-generation";
    expect(await app.dispatcher.handle("peri/execution/settle", invalid)).toEqual({ status: "rejected", reason: "attemptNotProvenStopped" });
    expect((await app.registry.read(ticket.sessionId)).attempt?.phase).toBe("entering");
  });

  test("a new dispatcher cannot take over an instance whose stop is unknown", async () => {
    const app = fixture();
    await admit(app.dispatcher);
    const transport = new ControlSnapshotTransport();
    transport.generationId = "fixture-generation-2";
    const replacement = new WorkerExecution(transport, app.storage, transport.generationId);
    expect(await replacement.handle("peri/execution/admit", admissionRequest("replacement"))).toEqual({ status: "blocked", reason: "oldInstanceNotProvenStopped" });
    expect((await app.registry.read("session-1")).instance?.generationId).toBe(app.transport.generationId);
  });
});
