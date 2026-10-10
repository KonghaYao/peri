/** 精确执行身份：一个 turn 内的某次 attempt。 */
export type ExecutionBinding = { turnId: string; attemptId: string };
/** 执行域控制状态；由准入后端持久化，SDK 不再自带控制协议端点。 */
export type ControlState = {
    lifecycle: number;
    revision: number;
    controlGeneration: number;
    status: "active" | "paused" | "closing" | "closed";
    attempt: ExecutionBinding | null;
};

export type InstanceDescriptor = {
    instanceId: string;
    generationId: string;
    /** 实例停止证据的来源；进程 PID 证明不属于 SDK 契约。 */
    proofRoute: { kind: "wasmHost" | "external"; reference: string };
};
export type InstanceStoppedProof = {
    kind: "instanceStopped";
    instanceId: string;
    generationId: string;
    evidenceId: string;
};
export type AttemptStoppedProof = {
    kind: "attemptStopped";
    instanceId: string;
    generationId: string;
    execution: ExecutionBinding;
    evidenceId: string;
};
export type EntryNotAppliedProof = { kind: "entryNotApplied"; ticket: ExecutionTicket };
export type StoppedProof = InstanceStoppedProof | AttemptStoppedProof | EntryNotAppliedProof;
export type ExecutionTicket = {
    sessionId: string;
    admissionId: string;
    instanceId: string;
    generationId: string;
    lifecycle: number;
    controlGeneration: number;
    workId: string;
    workRevision: number;
    execution: ExecutionBinding;
};
export type AttemptPhase = "queued" | "entering" | "running" | "exiting" | "settled" | "blocked";
export type AttemptRecord = {
    ticket: ExecutionTicket;
    phase: AttemptPhase;
    invalidated: boolean;
    reason?: string;
    stoppedProof?: StoppedProof;
    executionError?: { code: number; message: string };
    enteredEvidenceId?: string;
};
export type ExecutionDisaster = {
    kind: "dataLoss";
    reason: "enteredAdmissionMissing";
    ticket: ExecutionTicket;
    enteredEvidenceId?: string;
};
export class ExecutionDataLossError extends Error {
    constructor(readonly disaster: ExecutionDisaster) {
        super(`Execution DataLoss: ${disaster.reason}`);
        this.name = "ExecutionDataLossError";
    }
}
export type InstanceRecord = InstanceDescriptor & {
    status: "registered" | "stopped";
    stoppedProof?: InstanceStoppedProof;
};
export type WorkBudget = { workId: string; attempts: number; maxAttempts: number };
export type ExecutionRecord = {
    sessionId: string;
    revision: number;
    instance: InstanceRecord | null;
    previousInstances: InstanceRecord[];
    control: ControlState | null;
    attempt: AttemptRecord | null;
    completedAttempts: AttemptRecord[];
    budgets: WorkBudget[];
    disaster?: ExecutionDisaster;
};
export type ExecutionAction =
    | { kind: "registerInstance"; instance: InstanceDescriptor; previousStoppedProof?: InstanceStoppedProof }
    | { kind: "stopInstance"; proof: InstanceStoppedProof }
    | { kind: "observeControl"; control: ControlState }
    | { kind: "reserveAttempt"; ticket: ExecutionTicket; maxAttempts: number }
    | { kind: "enterAttempt" | "exitAttempt"; ticket: ExecutionTicket }
    | { kind: "runAttempt"; ticket: ExecutionTicket; enteredEvidenceId?: string }
    | { kind: "recordDisaster"; disaster: ExecutionDisaster }
    | { kind: "finishAttempt"; ticket: ExecutionTicket; proof: StoppedProof; executionError?: { code: number; message: string } }
    | { kind: "blockAttempt"; ticket: ExecutionTicket; reason: string };
export type ExecutionMutation = {
    sessionId: string;
    mutationId: string;
    expectedRevision: number;
    action: ExecutionAction;
};
export type ExecutionDecision =
    | { kind: "accepted" }
    | { kind: "rejected" | "blocked" | "busy"; reason: string };
export type ExecutionReceipt = {
    sessionId: string;
    mutationId: string;
    decision: ExecutionDecision;
    state: ExecutionRecord;
};
export type ExecutionResolution =
    | { status: "applied"; receipt: ExecutionReceipt }
    | { status: "unknown" }
    | { status: "notApplied" };
export interface ExecutionRegistry {
    readonly durability: "durable" | "bestEffort";
    read(sessionId: string): Promise<ExecutionRecord>;
    apply(command: ExecutionMutation): Promise<ExecutionResolution>;
    resolve(command: ExecutionMutation): Promise<ExecutionResolution>;
}
export type RunnableWork = { workId: string; revision: number; lifecycle: number; controlGeneration: number };
export type DomainWorkCommand = { sessionId: string; recipientLifecycle: number; mutationId: string; action: Readonly<Record<string, unknown>> };
export type DomainWorkResolution = { status: "applied"; receipt: unknown } | { status: "notApplied" | "unknown" };
export type WorkQuery = { control: ControlState; work: RunnableWork | null; blocked?: boolean; pendingCommands?: DomainWorkCommand[] };
export type ExecutionReply =
    | { status: "running"; ticket: ExecutionTicket }
    | { status: "settled"; ticket: ExecutionTicket; proof: AttemptStoppedProof; executionError?: { code: number; message: string } }
    | { status: "notApplied"; ticket: ExecutionTicket; reason: string };
export type EntryResolution =
    | { status: "applied"; reply: ExecutionReply }
    | { status: "unknown" }
    | { status: "notApplied" };
export interface ExecutionDomainPort {
    queryWork(sessionId: string): Promise<WorkQuery>;
    execute(ticket: ExecutionTicket): Promise<ExecutionReply>;
    resolveExecution(ticket: ExecutionTicket): Promise<EntryResolution>;
    resolveWorkCommand(command: DomainWorkCommand): Promise<DomainWorkResolution>;
}
export interface InstanceProofProvider {
    proveStopped(instance: InstanceDescriptor): Promise<
        { status: "stopped"; proof: InstanceStoppedProof } | { status: "unknown" }
    >;
}
export type ActivationSource = "send" | "inboxScan" | "notification" | "recovery" | "cron";
