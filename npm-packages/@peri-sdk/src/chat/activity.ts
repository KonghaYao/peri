import type { ChatFact } from "../contracts/chat";
import type { ChatObservation } from "../contracts/observability";

const TERMINAL_TASK_STATUSES = new Set(["completed", "failed", "cancelled"]);
const UNOBSERVED_PREFIX = "unobserved:";

/**
 * Counters the public chat observation cannot see.
 *
 * A host reports them only when every field is authoritative for the current activation;
 * `complete` is what lets a lifecycle adapter release the runtime unit.
 */
export interface ChatActivitySourceSnapshot {
    readonly complete: boolean;
    /** Omit when the host cannot observe this counter; the tracker then uses task facts. */
    readonly activeInternalTasks?: number;
    /** Omit when the host cannot observe this counter. */
    readonly pendingDeliveries?: number;
}

export interface ChatActivitySource {
    snapshot(): ChatActivitySourceSnapshot;
    subscribe?(listener: () => void): () => void;
}

export interface ChatActivityTrackerOptions {
    readonly source?: ChatActivitySource;
    readonly onSourceError?: (error: unknown) => void;
}

/** A complete-enough projection of one activation's execution state. */
export interface ChatActivitySnapshot {
    readonly revision: number;
    readonly complete: boolean;
    readonly rootRunning: boolean;
    readonly activeInternalTasks: number;
    readonly queuedInputs: number;
    readonly pendingDeliveries: number;
    readonly pendingInteractions: number;
    readonly localOperations: number;
    readonly closing: boolean;
}

const EMPTY_ACTIVITY: ChatActivitySnapshot = Object.freeze({
    revision: 0,
    complete: false,
    rootRunning: false,
    activeInternalTasks: 0,
    queuedInputs: 0,
    pendingDeliveries: 0,
    pendingInteractions: 0,
    localOperations: 0,
    closing: false,
});

/**
 * Projects chat observation and facts onto one activity snapshot.
 *
 * The tracker is the authority for fact semantics; hosts map the snapshot onto their own
 * lifecycle primitive (for example Rivet's keep-awake). Without a complete source the snapshot
 * stays incomplete, so consumers must not treat it as releasable.
 */
export class ChatActivityTracker {
    readonly #source: ChatActivitySource | undefined;
    readonly #onSourceError: ((error: unknown) => void) | undefined;
    readonly #listeners = new Set<(snapshot: ChatActivitySnapshot) => void>();
    #rootRunning = false;
    #queuedInputs = 0;
    #pendingInteractions = 0;
    #observedInternalTasks = 0;
    #localOperations = 0;
    #closing = false;
    #closed = false;
    #started = false;
    #unsubscribeSource: (() => void) | undefined;
    #snapshot: ChatActivitySnapshot = EMPTY_ACTIVITY;

    constructor(options: ChatActivityTrackerOptions = {}) {
        this.#source = options.source;
        this.#onSourceError = options.onSourceError;
    }

    snapshot(): ChatActivitySnapshot {
        return this.#snapshot;
    }

    start(): void {
        if (this.#started) return;
        this.#started = true;
        if (this.#source?.subscribe !== undefined) {
            this.#unsubscribeSource = this.#source.subscribe(() => {
                this.#sync();
            });
        }
        this.#sync();
    }

    subscribe(listener: (snapshot: ChatActivitySnapshot) => void): () => void {
        this.#listeners.add(listener);
        return () => {
            this.#listeners.delete(listener);
        };
    }

    /** Marks a host-side operation (for example session creation) that must keep the unit alive. */
    beginLocalOperation(): () => void {
        this.#localOperations += 1;
        this.#sync();
        let released = false;
        return () => {
            if (released) return;
            released = true;
            this.#localOperations = Math.max(0, this.#localOperations - 1);
            this.#sync();
        };
    }

    /**
     * Refreshes the counters visible through `ChatService.observe()`.
     *
     * A missing observation is treated as "not running" for counters, but the snapshot stays
     * incomplete unless a source says otherwise.
     */
    applyObservation(observation: ChatObservation | null): void {
        this.#rootRunning = observation?.instance?.running ?? false;
        this.#queuedInputs = observation?.instance?.pendingInputCount ?? 0;
        this.#pendingInteractions = observation?.instance?.pendingInteractionCount ?? 0;
        this.#sync();
    }

    applyFact(fact: ChatFact): void {
        switch (fact.kind) {
            case "task-snapshot":
                this.#observedInternalTasks = countActiveTasks(fact.snapshot);
                this.#sync();
                return;
            case "run-state":
                this.#rootRunning = fact.running;
                this.#sync();
                return;
            case "turn-complete":
            case "turn-cancel":
                this.#rootRunning = false;
                this.#sync();
                return;
            case "closed":
                this.markClosed();
                return;
            default:
                return;
        }
    }

    markClosed(): void {
        if (this.#closed) return;
        this.#closed = true;
        this.#closing = true;
        this.#rootRunning = false;
        this.#queuedInputs = 0;
        this.#pendingInteractions = 0;
        this.#observedInternalTasks = 0;
        this.#sync();
    }

    dispose(): void {
        this.#unsubscribeSource?.();
        this.#unsubscribeSource = undefined;
        this.#closed = true;
        this.#started = false;
        this.#listeners.clear();
    }

    #sync(): void {
        const source = this.#readSource();
        const next = this.#project(source);
        if (sameActivity(next, this.#snapshot)) return;
        this.#snapshot = Object.freeze({ ...next, revision: this.#snapshot.revision + 1 });
        for (const listener of this.#listeners) {
            listener(this.#snapshot);
        }
    }

    #project(source: ChatActivitySourceSnapshot | undefined): Omit<ChatActivitySnapshot, "revision"> {
        return {
            complete: source?.complete ?? false,
            rootRunning: this.#closed ? false : this.#rootRunning,
            activeInternalTasks: this.#closed ? 0 : count(source?.activeInternalTasks ?? this.#observedInternalTasks),
            queuedInputs: this.#closed ? 0 : count(this.#queuedInputs),
            pendingDeliveries: this.#closed ? 0 : count(source?.pendingDeliveries ?? 0),
            pendingInteractions: this.#closed ? 0 : count(this.#pendingInteractions),
            localOperations: this.#localOperations,
            closing: this.#closing,
        };
    }

    #readSource(): ChatActivitySourceSnapshot | undefined {
        if (this.#source === undefined) return undefined;
        try {
            return this.#source.snapshot();
        } catch (error) {
            this.#reportSourceError(error);
            return undefined;
        }
    }

    #reportSourceError(error: unknown): void {
        if (this.#onSourceError !== undefined) {
            this.#onSourceError(error);
            return;
        }
        queueMicrotask(() => {
            throw error;
        });
    }
}

function sameActivity(left: Omit<ChatActivitySnapshot, "revision">, right: ChatActivitySnapshot): boolean {
    return (
        left.complete === right.complete &&
        left.rootRunning === right.rootRunning &&
        left.activeInternalTasks === right.activeInternalTasks &&
        left.queuedInputs === right.queuedInputs &&
        left.pendingDeliveries === right.pendingDeliveries &&
        left.pendingInteractions === right.pendingInteractions &&
        left.localOperations === right.localOperations &&
        left.closing === right.closing
    );
}

function count(raw: number): number {
    return Number.isFinite(raw) ? Math.max(0, Math.trunc(raw)) : 0;
}

/** Counts tasks that are not in a terminal state; `unobserved:` statuses stay conservative. */
function countActiveTasks(snapshot: unknown): number {
    if (snapshot === null || typeof snapshot !== "object") return 0;
    const tasks = (snapshot as { tasks?: unknown }).tasks;
    if (!Array.isArray(tasks)) return 0;
    let active = 0;
    for (const task of tasks) {
        if (task === null || typeof task !== "object") continue;
        const status = (task as { status?: unknown }).status;
        if (typeof status !== "string") {
            active += 1;
            continue;
        }
        if (status.startsWith(UNOBSERVED_PREFIX)) {
            active += 1;
            continue;
        }
        if (!TERMINAL_TASK_STATUSES.has(status)) active += 1;
    }
    return active;
}
