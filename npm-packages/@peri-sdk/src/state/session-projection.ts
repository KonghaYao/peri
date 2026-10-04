import { DocModel } from "./doc-model";
import { TurnMachine } from "./turn-machine";
import { finite, object, string, type RecordValue } from "./protocol";

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
        if (type === "agent_execution_failed") return this.turns.exit("error");
        if (type === "compact_started") {
            info.set("compactionStatus", "running");
            return true;
        }
        if (type === "compact_completed" && value) {
            const safe: RecordValue = { status: "completed" };
            const trigger = string(value.trigger);
            const strategy = string(value.strategy);
            if (trigger === "auto" || trigger === "manual") safe.trigger = trigger;
            if (strategy) safe.strategy = strategy;
            for (const field of ["affected_count", "estimated_tokens_saved"] as const) {
                const number = finite(value[field]);
                if (number !== null) safe[field] = number;
            }
            info.set("compaction", safe);
            info.set("compactionStatus", "completed");
            return true;
        }
        if (type === "rewind_completed") {
            info.set("rewindRevision", (finite(info.get("rewindRevision")) ?? 0) + 1);
            return true;
        }
        return false;
    }
}
