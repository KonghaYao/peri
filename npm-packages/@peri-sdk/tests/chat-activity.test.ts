import { expect, test } from "bun:test";
import { ChatActivityTracker, type ChatActivitySourceSnapshot } from "../src/chat/activity";
import type { ChatObservation } from "../src/contracts/observability";

function observation(overrides: {
    running?: boolean;
    queued?: number;
    interactions?: number;
} = {}): ChatObservation {
    return {
        chatId: "chat_1",
        sessionId: "session_1",
        bindingRevision: 1,
        phase: "bound",
        phaseLimited: true,
        workspace: { id: "workspace_1", runtimePath: "/workspace" },
        instance: {
            kind: "wasm",
            state: "live",
            running: overrides.running ?? false,
            pendingInputCount: overrides.queued ?? 0,
            pendingInteractionCount: overrides.interactions ?? 0,
        },
        mcp: { source: "session-init", servers: [] },
    };
}

function report(overrides: Partial<ChatActivitySourceSnapshot> = {}): ChatActivitySourceSnapshot {
    return { complete: true, ...overrides };
}

test("没有完整 source 时快照保持不可释放", () => {
    const tracker = new ChatActivityTracker();
    tracker.start();
    tracker.applyObservation(observation({ running: false }));

    expect(tracker.snapshot().complete).toBe(false);
    expect(tracker.snapshot().rootRunning).toBe(false);
});

test("完整 source 决定完成后才发布可释放快照", () => {
    let current = report({ complete: false });
    const tracker = new ChatActivityTracker({
        source: {
            snapshot: () => current,
            subscribe: () => () => {},
        },
    });
    tracker.start();
    expect(tracker.snapshot().complete).toBe(false);

    current = report({ activeInternalTasks: 2, pendingDeliveries: 1 });
    tracker.applyFact({ kind: "run-state", running: true, executionBlocked: false });

    expect(tracker.snapshot()).toMatchObject({
        complete: true,
        rootRunning: true,
        activeInternalTasks: 2,
        pendingDeliveries: 1,
    });
});

test("task-snapshot 只统计非终态任务，unobserved 与非字符串状态保持保守", () => {
    const tracker = new ChatActivityTracker({ source: { snapshot: () => report() } });
    tracker.start();

    tracker.applyFact({
        kind: "task-snapshot",
        snapshot: {
            tasks: [
                { taskId: "a", status: "running" },
                { taskId: "b", status: "unobserved:waiting" },
                { taskId: "g", status: "unobserved:completed" },
                { taskId: "c", status: "completed" },
                { taskId: "d", status: "failed" },
                { taskId: "e", status: "cancelled" },
                { taskId: "f" },
                null,
            ],
        },
    });

    expect(tracker.snapshot().activeInternalTasks).toBe(4);
});

test("observation 提供根任务、排队输入和交互计数", () => {
    const tracker = new ChatActivityTracker({ source: { snapshot: () => report() } });
    tracker.start();

    tracker.applyObservation(observation({ running: true, queued: 2, interactions: 1 }));

    expect(tracker.snapshot()).toMatchObject({
        rootRunning: true,
        queuedInputs: 2,
        pendingInteractions: 1,
    });
});

test("本地操作在释放前保持账本非空", () => {
    const tracker = new ChatActivityTracker();
    tracker.start();
    const release = tracker.beginLocalOperation();
    expect(tracker.snapshot().localOperations).toBe(1);

    release();
    release();
    expect(tracker.snapshot().localOperations).toBe(0);
});

test("关闭清零运行计数但保留本地操作", () => {
    const tracker = new ChatActivityTracker({ source: { snapshot: () => report() } });
    tracker.start();
    tracker.applyObservation(observation({ running: true, queued: 1, interactions: 1 }));
    const release = tracker.beginLocalOperation();

    tracker.applyFact({ kind: "closed" });

    expect(tracker.snapshot()).toMatchObject({
        closing: true,
        rootRunning: false,
        queuedInputs: 0,
        pendingInteractions: 0,
        localOperations: 1,
    });
    release();
});

test("相同投影不增加 revision", () => {
    const tracker = new ChatActivityTracker({ source: { snapshot: () => report() } });
    tracker.start();
    const revision = tracker.snapshot().revision;

    tracker.applyObservation(observation());
    tracker.applyFact({ kind: "run-state", running: false, executionBlocked: false });

    expect(tracker.snapshot().revision).toBe(revision);
});

test("source 异常通过回调报告且不发布完整快照", () => {
    const errors: unknown[] = [];
    const tracker = new ChatActivityTracker({
        source: {
            snapshot: () => {
                throw new Error("source failed");
            },
        },
        onSourceError: (error) => errors.push(error),
    });

    tracker.start();

    expect(errors).toHaveLength(1);
    expect(tracker.snapshot().complete).toBe(false);
});

test("无变化的关闭不重复通知", () => {
    const tracker = new ChatActivityTracker();
    let notifications = 0;
    tracker.subscribe(() => {
        notifications += 1;
    });
    tracker.start();
    tracker.applyFact({ kind: "closed" });
    tracker.applyFact({ kind: "closed" });

    expect(notifications).toBe(1);
});
