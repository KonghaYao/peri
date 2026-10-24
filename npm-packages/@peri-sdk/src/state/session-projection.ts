import { DocModel } from "./doc-model";
import { TurnMachine } from "./turn-machine";
import { finite, object, string, type RecordValue } from "./protocol";

/**
 * Compact 展示状态（Session Doc 的 `compactionStatus`）。
 *
 * `running` 只表示「已开始、还没上报终态」；`completed` / `failed` / `cancelled` 都是
 * 终态。手动与自动 Compact 共用这一份取值，失败必须离开 `running`（REQ-C02、验收 A12）。
 */
export const COMPACTION_STATUS = {
    running: "running",
    completed: "completed",
    failed: "failed",
    cancelled: "cancelled",
} as const;

export type CompactionStatus = typeof COMPACTION_STATUS[keyof typeof COMPACTION_STATUS];

/** Display-safe session metadata; chat content and task lifecycles live in their own modules. */
export class SessionProjection {
    constructor(private readonly docs: DocModel, private readonly turns: TurnMachine) {}

    acceptUpdate(update: RecordValue, params: RecordValue): boolean {
        const kind = string(update.sessionUpdate);
        const sourceAgentId = string(object(object(params._meta)?.peri)?.sourceAgentId);
        if (kind === "usage_update") return sourceAgentId ? false : this.acceptUsage(update);
        if (kind === "session_info_update") return this.acceptSessionInfo(update);
        if (kind === "available_commands_update") return this.acceptCommands(update);
        if (kind === "plan") return this.acceptPlan(update);
        if (kind === "config_option_update") return this.acceptConfigOptions(update.configOptions);
        return false;
    }

    /** 运行侧上报的展示状态；不代表 View 可授予执行资格。 */
    acceptRunState(running: boolean, executionBlocked: boolean): boolean {
        this.docs.session.transact(() => {
            const info = this.docs.info();
            info.set("running", running);
            info.set("executionBlocked", executionBlocked);
        });
        return true;
    }

    seedConfig(response: unknown): void {
        const value = object(response);
        if (!value) return;
        this.acceptConfigOptions(value.configOptions);
        const modes = object(value.modes);
        if (!modes) return;
        const currentModeId = string(modes.currentModeId);
        const availableModes = Array.isArray(modes.availableModes)
            ? modes.availableModes.flatMap((raw) => {
                const mode = object(raw);
                const id = string(mode?.id);
                return id ? [{ id, name: string(mode?.name) ?? id, description: string(mode?.description) }] : [];
            }) : [];
        this.docs.info().set("modeState", { currentModeId, availableModes });
    }

    private acceptConfigOptions(raw: unknown): boolean {
        if (!Array.isArray(raw)) return false;
        const options = raw.flatMap((item) => {
            const value = object(item);
            const id = string(value?.id);
            const type = string(value?.type);
            if (!value || !id || (type !== "select" && type !== "boolean")) return [];
            const choices = Array.isArray(value.options) ? value.options.flatMap((choice) => {
                const option = object(choice);
                const choiceId = string(option?.value);
                return choiceId ? [{ value: choiceId, name: string(option?.name) ?? choiceId }] : [];
            }) : [];
            return [{ id, name: string(value.name) ?? id, type,
                category: string(value.category),
                currentValue: type === "boolean" ? value.currentValue === true : string(value.currentValue),
                ...(type === "select" ? { options: choices } : {}),
            }];
        });
        this.docs.info().set("configOptions", options);
        return true;
    }

    private acceptUsage(update: RecordValue): boolean {
        const turnId = this.turns.activeTurn();
        const used = finite(update.used);
        const size = finite(update.size);
        if (!turnId || !this.turns.writable() || used === null || size === null || size === 0) return false;
        const entryId = string(this.docs.info().get("activeAssistantEntryId")) ?? `${turnId}:assistant`;
        const entry = this.docs.entries().get(entryId);
        if (!entry) return false;
        const meta = object(update._meta);
        const usage: RecordValue = { totalTokens: used, contextWindow: size };
        for (const field of ["inputTokens", "outputTokens", "cacheCreationTokens", "cacheReadTokens"] as const) {
            const value = finite(meta?.[field]);
            if (value !== null) usage[field] = value;
        }
        for (const field of ["requestId", "model", "stopReason"] as const) {
            const value = string(meta?.[field]);
            if (value) usage[field] = value;
        }
        entry.set("tokenUsage", usage);
        return true;
    }

    private acceptSessionInfo(update: RecordValue): boolean {
        const patch: RecordValue = {};
        for (const key of ["title", "model", "mode", "updatedAt"] as const) {
            if (typeof update[key] === "string") patch[key] = update[key];
        }
        for (const key of ["modelState", "modeState"] as const) {
            const source = object(update[key]);
            if (!source) continue;
            const safe: RecordValue = {};
            for (const field of ["currentModelId", "currentModeId"] as const) {
                if (typeof source[field] === "string") safe[field] = source[field];
            }
            if (Object.keys(safe).length) patch[key] = safe;
        }
        if (!Object.keys(patch).length) return false;
        this.docs.session.transact(() => {
            for (const [key, value] of Object.entries(patch)) this.docs.info().set(key, value);
        });
        return true;
    }

    private acceptCommands(update: RecordValue): boolean {
        if (!Array.isArray(update.availableCommands)) return false;
        const safe = update.availableCommands.flatMap((raw) => {
            const command = object(raw);
            const name = string(command?.name);
            const hint = string(object(command?.input)?.hint);
            return name ? [{ name, description: typeof command?.description === "string" ? command.description : null,
                input: hint ? { hint } : null }] : [];
        });
        this.docs.info().set("availableCommands", safe);
        return true;
    }

    private acceptPlan(update: RecordValue): boolean {
        if (!Array.isArray(update.entries)) return false;
        const entries = update.entries.flatMap((raw) => {
            const entry = object(raw);
            const content = string(entry?.content);
            if (!content) return [];
            const activeForm = string(object(entry?._meta)?.activeForm);
            return [{ content, status: string(entry?.status) ?? "pending", ...(activeForm ? { activeForm } : {}) }];
        });
        this.docs.info().set("plan", entries);
        const turnId = this.turns.activeTurn();
        if (turnId) (this.docs.sessionRoot().get("plansByTurn") as import("yjs").Map<unknown>).set(turnId, entries);
        return true;
    }

    acceptAgentEvent(type: string, value: RecordValue | null): boolean {
        const info = this.docs.info();
        if (type === "state_snapshot_meta" && value) {
            const safe: RecordValue = {};
            for (const field of ["message_count", "total_tokens", "current_step", "consecutive_failures", "context_total_tokens"] as const) {
                const number = finite(value[field]);
                if (number !== null) safe[field] = number;
            }
            const pct = finite(value.budget_pct);
            if (pct !== null && pct <= 1) safe.budgetPct = pct;
            if (!Object.keys(safe).length) return false;
            info.set("stateSnapshotMeta", safe);
            return true;
        }
        if (type === "goal_snapshot" && value) {
            const safe: RecordValue = {};
            for (const field of ["status", "objective"] as const) {
                if (typeof value[field] === "string") safe[field] = value[field];
            }
            for (const field of ["token_budget", "tokens_used", "time_used_seconds", "continuation_count"] as const) {
                const number = finite(value[field]);
                if (number !== null) safe[field] = number;
            }
            info.set("goal", safe);
            return true;
        }
        if (type === "turn_suspended" && value) {
            const turnId = string(value.turn_id);
            if (!turnId) return false;
            info.set("suspendedTurnId", turnId);
            this.turns.exit("completed");
            return true;
        }
        if (type === "agent_execution_failed") {
            // 轮次失败时 Compact 不可能再上报终态：不能让展示停在 running（A12）。
            this.settleCompaction(COMPACTION_STATUS.failed);
            return this.turns.exit("error");
        }
        if (type === "command_feedback" && value) {
            // 手动 Compact 的失败/取消只经这条通道到达：pipeline 的 EarlyReturn / Cancelled
            // 映射成 `AcpEvent::CommandFeedback`（`{level, message, channel}`，
            // peri-acp/src/event/mod.rs）。反馈本身不带命令名，所以只有确认「有 Compact
            // 正在进行」时才把它当成 Compact 的终态；其它命令的反馈不得改写 Compact 展示。
            const message = string(value.message);
            const level = string(value.level);
            if (level === "error") return this.settleCompaction(COMPACTION_STATUS.failed, message);
            if (level === "warning") return this.settleCompaction(COMPACTION_STATUS.cancelled, message);
            return false;
        }
        if (type === "compact_started") {
            info.set("compactionStatus", COMPACTION_STATUS.running);
            return true;
        }
        if (type === "compact_completed" && value) {
            const safe: RecordValue = { status: COMPACTION_STATUS.completed };
            const trigger = string(value.trigger);
            const strategy = string(value.strategy);
            if (trigger === "auto" || trigger === "manual") safe.trigger = trigger;
            if (strategy) safe.strategy = strategy;
            for (const field of ["affected_count", "estimated_tokens_saved"] as const) {
                const number = finite(value[field]);
                if (number !== null) safe[field] = number;
            }
            info.set("compaction", safe);
            info.set("compactionStatus", COMPACTION_STATUS.completed);
            return true;
        }
        if (type === "rewind_completed") {
            info.set("rewindRevision", (finite(info.get("rewindRevision")) ?? 0) + 1);
            return true;
        }
        return false;
    }

    /**
     * Compact 的终态收敛（REQ-C02、A12）：只有确实在进行的 Compact 接受终态，
     * 终态一旦上报就不再被后续信号改写（`compact_completed` 之后紧跟的通用命令反馈
     * 不得把它翻回失败）。`compaction` 展示始终描述**最近一次** Compact 的终态。
     */
    private settleCompaction(status: CompactionStatus, message?: string | null): boolean {
        const info = this.docs.info();
        if (info.get("compactionStatus") !== COMPACTION_STATUS.running) return false;
        const display: RecordValue = { status };
        if (message) display.message = message;
        info.set("compaction", display);
        info.set("compactionStatus", status);
        return true;
    }
}
