import { expect, test } from "bun:test";
import type { ControlState } from "../src/execution/types";
import { ExecutionCoordinator } from "../src/execution/coordinator";
import { MemoryExecutionRegistry } from "../src/execution/memory-registry";
import type { ExecutionTicket, RunnableWork } from "../src/execution/types";

function barrier() {
    let release!: () => void;
    const reached = new Promise<void>((resolve) => { release = resolve; });
    return { reached, release };
}

async function fixture() {
    const registry = new MemoryExecutionRegistry();
    const state = {
        control: { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null } as ControlState,
        work: null as RunnableWork | null,
        queries: 0, executes: 0, resolutions: 0,
        execute: async (ticket: ExecutionTicket) => {
            state.work = null;
            return { status: "settled" as const, ticket, proof: {
                kind: "attemptStopped" as const, instanceId: ticket.instanceId, generationId: ticket.generationId,
                execution: ticket.execution, evidenceId: "isolated-exit",
            } };
        },
    };
    const coordinator = new ExecutionCoordinator({
        registry, allowBestEffort: true, maxAttemptsPerWork: 1,
        instance: { instanceId: "wake-instance", generationId: "wake-generation",
            proofRoute: { kind: "external", reference: "isolated-memory-test" } },
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) },
        domain: {
            queryWork: async () => {
                state.queries++;
                return structuredClone({ control: state.control, work: state.work });
            },
            execute: async (ticket) => { state.executes++; return state.execute(ticket); },
            resolveExecution: async () => { state.resolutions++; return { status: "unknown" }; },
            resolveWorkCommand: async () => ({ status: "unknown" }),
        },
    });
    expect(await coordinator.registerInstance("session", "register")).toEqual({ status: "idle" });
    const observed = barrier();
    const resume = barrier();
    const originalApply = registry.apply.bind(registry);
    let pauseNext = true;
    registry.apply = async (command) => {
        if (command.action.kind === "observeControl" && pauseNext) {
            pauseNext = false;
            observed.release();
            await resume.reached;
        }
        return originalApply(command);
    };
    const wake = () => coordinator.ensureProcessing({ sessionId: "session", source: "notification" });
    const addWork = () => {
        state.work = { workId: "new-work", revision: 0, lifecycle: 1, controlGeneration: state.control.controlGeneration };
    };
    return { registry, coordinator, state, observed, resume, wake, addWork };
}

test("overlapping notifications refresh stale idle without a third activation or duplicate execution", async () => {
    const context = await fixture();
    const first = context.wake();
    await context.observed.reached;
    context.addWork();
    const second = context.wake();
    expect(second).toBe(first);
    context.resume.release();
    expect(await first).toEqual({ status: "idle" });
    expect(await second).toEqual({ status: "idle" });
    expect(context.state.executes).toBe(1);
    expect(context.state.queries).toBe(4);
    expect((await context.registry.read("session")).budgets[0]?.attempts).toBe(1);
});

test("activation after idle completion starts a fresh dispatcher", async () => {
    const context = await fixture();
    context.resume.release();
    expect(await context.wake()).toEqual({ status: "idle" });
    context.addWork();
    expect(await context.wake()).toEqual({ status: "idle" });
    expect(context.state.executes).toBe(1);
});

test("resume notification refreshes an overlapping stopped control snapshot", async () => {
    const context = await fixture();
    context.state.control = { ...context.state.control, status: "paused", revision: 1, controlGeneration: 1 };
    const stopped = context.wake();
    await context.observed.reached;
    context.state.control = { ...context.state.control, status: "active", revision: 2, controlGeneration: 2 };
    context.addWork();
    expect(context.wake()).toBe(stopped);
    context.resume.release();
    expect(await stopped).toEqual({ status: "idle" });
    expect(context.state.executes).toBe(1);
    expect((await context.registry.read("session")).control).toEqual(context.state.control);
});

test("stop notification rechecks control before executing newly available work", async () => {
    const context = await fixture();
    const activation = context.wake();
    await context.observed.reached;
    context.state.control = { ...context.state.control, status: "paused", revision: 1, controlGeneration: 1 };
    context.addWork();
    expect(context.wake()).toBe(activation);
    context.resume.release();
    expect(await activation).toEqual({ status: "blocked", reason: "domain:paused" });
    expect(context.state.executes).toBe(0);
    expect((await context.registry.read("session")).control?.status).toBe("paused");
});

test("retirement seals dirty activation before any new execution", async () => {
    const context = await fixture();
    const activation = context.wake();
    await context.observed.reached;
    context.addWork();
    expect(context.wake()).toBe(activation);
    context.coordinator.seal();
    context.resume.release();
    expect(await activation).toEqual({ status: "blocked", reason: "dispatcherGenerationSealed" });
    expect(await context.wake()).toEqual({ status: "blocked", reason: "dispatcherGenerationSealed" });
    expect(context.state.executes).toBe(0);
});

test("overlapping wake never retries an Unknown execution or resets its budget", async () => {
    const context = await fixture();
    const executing = barrier();
    const releaseExecution = barrier();
    context.state.execute = async () => {
        executing.release();
        await releaseExecution.reached;
        throw new Error("isolated execution outcome unknown");
    };
    context.addWork();
    context.resume.release();
    const activation = context.wake();
    await executing.reached;
    expect(context.wake()).toBe(activation);
    releaseExecution.release();
    expect(await activation).toMatchObject({ status: "unknown" });
    expect(context.state.executes).toBe(1);
    expect(context.state.resolutions).toBe(0);
    expect(await context.wake()).toMatchObject({ status: "unknown" });
    expect(context.state.executes).toBe(1);
    expect(context.state.resolutions).toBe(1);
    expect((await context.registry.read("session")).budgets[0]?.attempts).toBe(1);
});

test("dirty notifications cannot override work budget exhaustion", async () => {
    const context = await fixture();
    context.addWork();
    context.resume.release();
    context.state.execute = async (ticket) => {
        context.wake();
        return { status: "settled", ticket, proof: {
            kind: "attemptStopped", instanceId: ticket.instanceId, generationId: ticket.generationId,
            execution: ticket.execution, evidenceId: "work-still-pending",
        } };
    };
    expect(await context.wake()).toMatchObject({ status: "blocked", reason: "workBudgetExhausted" });
    expect(context.state.executes).toBe(1);
    expect((await context.registry.read("session")).budgets[0]?.attempts).toBe(1);
});

test("continuous stale-idle notifications remain bounded by the activation drain limit", async () => {
    const context = await fixture();
    context.resume.release();
    const originalApply = context.registry.apply.bind(context.registry);
    context.registry.apply = async (command) => {
        if (command.action.kind === "observeControl") context.wake();
        return originalApply(command);
    };
    expect(await context.wake()).toEqual({ status: "blocked", reason: "activationDrainLimit" });
    expect(context.state.queries).toBe(128);
    expect(context.state.executes).toBe(0);
});
