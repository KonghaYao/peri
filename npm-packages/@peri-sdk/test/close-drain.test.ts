import { expect, test } from "bun:test";
import { drainClose, type CloseDrainClock } from "../src/agent/close-drain";
import {
  SessionCloseIncompleteError, SessionCloseUnknownError, SessionControlNotAppliedError,
  type ControlCommand, type ControlReceipt, type ControlResolution, type ControlSnapshot,
} from "../src/agent/session-control";
import { closeCommand } from "./control-fixture";

const command: ControlCommand = { ...closeCommand, sessionId: "s1", action: { kind: "close" } };

function fakeClock() {
  let elapsed = 0;
  const sleeps: number[] = [];
  const clock: CloseDrainClock = {
    now: () => elapsed,
    sleep: async (milliseconds) => { sleeps.push(milliseconds); elapsed += milliseconds; },
    limit: async (operation) => operation,
  };
  return { clock, sleeps, elapsed: () => elapsed };
}

function domain() {
  const receipt: ControlReceipt = {
    sessionId: "s1", commandId: command.commandId, decision: { kind: "accepted" },
    state: { lifecycle: 1, revision: 1, controlGeneration: 1, status: "closing", attempt: null },
  };
  let advances = 0;
  return {
    receipt,
    apply: async () => { advances++; return receipt; },
    resolve: async (): Promise<ControlResolution> => { advances++; return { status: "applied", receipt }; },
    snapshot: async (): Promise<ControlSnapshot> => ({
      state: { ...receipt.state, status: advances >= 3 ? "closed" : "closing", revision: advances >= 3 ? 2 : 1 },
      settlement: { status: advances >= 3 ? "settled" : "pending" },
    }),
    isUnresolved: () => false,
    advances: () => advances,
  };
}

test("bounded drain advances the domain using original Close replay, not cached receipt", async () => {
  const backend = domain();
  const timing = fakeClock();
  const receipt = await drainClose(command, backend, { drainTimeoutMs: 3_000 }, timing.clock);
  expect(receipt).toBe(backend.receipt);
  expect(receipt.state.status).toBe("closing");
  expect(backend.advances()).toBe(3);
  expect(timing.sleeps).toEqual([50, 50]);
});

test("deadline returns Incomplete without another apply at or after the budget", async () => {
  const backend = domain();
  const timing = fakeClock();
  const error = await drainClose(command, backend, { drainTimeoutMs: 75 }, timing.clock).catch((failure) => failure);
  expect(error).toBeInstanceOf(SessionCloseIncompleteError);
  expect(error.receipt).toBe(backend.receipt);
  expect(error.snapshot.settlement.status).toBe("pending");
  expect(backend.advances()).toBe(2);
  expect(timing.sleeps).toEqual([50, 25]);
  expect(timing.elapsed()).toBe(75);
});

test("Unknown only resolves original command until its receipt is final", async () => {
  const backend = domain();
  const timing = fakeClock();
  let applied = 0;
  let resolved = 0;
  const operations = {
    ...backend,
    apply: async () => { applied++; throw new Error("ACK lost"); },
    resolve: async (): Promise<ControlResolution> => {
      resolved++;
      if (resolved === 1) return { status: "unknown" };
      await backend.apply();
      await backend.apply();
      return backend.resolve();
    },
  };
  expect(await drainClose(command, operations, { drainTimeoutMs: 3_000 }, timing.clock)).toBe(backend.receipt);
  expect(applied).toBe(1);
  expect(resolved).toBe(2);
});

test("Unknown deadline and terminal NotApplied cannot trigger blind replay", async () => {
  for (const status of ["unknown", "notApplied"] as const) {
    const backend = domain();
    const timing = fakeClock();
    let applied = 0;
    let resolved = 0;
    const operations = {
      ...backend,
      apply: async () => { applied++; throw new Error("ACK lost"); },
      resolve: async (): Promise<ControlResolution> => { resolved++; return { status }; },
    };
    const error = await drainClose(command, operations, { drainTimeoutMs: 125 }, timing.clock).catch((failure) => failure);
    expect(error).toBeInstanceOf(status === "unknown" ? SessionCloseUnknownError : SessionControlNotAppliedError);
    expect(error.command).toBe(command);
    expect(applied).toBe(1);
    expect(resolved).toBe(status === "unknown" ? 2 : 1);
  }
});

test("a stalled mutation is Unknown at the real drain deadline", async () => {
  const backend = domain();
  let applied = 0;
  const error = await drainClose(command, {
    ...backend,
    apply: () => { applied++; return new Promise<ControlReceipt>(() => {}); },
    resolve: async () => ({ status: "unknown" }),
  }, { drainTimeoutMs: 5 }).catch((failure) => failure);
  expect(error).toBeInstanceOf(SessionCloseUnknownError);
  expect(error.command).toBe(command);
  expect(applied).toBe(1);
});

test("single-probe and invalid budgets never silently broaden close authority", async () => {
  const backend = domain();
  const timing = fakeClock();
  await expect(drainClose(command, backend, { drainTimeoutMs: 0 }, timing.clock)).rejects.toBeInstanceOf(SessionCloseIncompleteError);
  expect(backend.advances()).toBe(1);
  expect(timing.sleeps).toEqual([]);
  await expect(drainClose(command, backend, { drainTimeoutMs: -1 }, timing.clock)).rejects.toBeInstanceOf(TypeError);
  expect(backend.advances()).toBe(1);
});
