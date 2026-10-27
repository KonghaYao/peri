import { cloneJsonValue } from "../util/json-value";
import type { ChatStoreConfig } from "./types";

/**
 * Manager 部署边界字段：由 `ChatManagerOptions.store` 独占，per-chat 不可声明。
 *
 * `storage` 指向 Store，`machineId` 指向执行环境；二者由整个 Manager 共享，
 * 不能由单个 Chat 改写，否则同一 Manager 下的会话会写到不同存储或不同机器身份。
 */
const RESERVED_KEYS = ["storage", "machineId"] as const;

/** 单个 Chat 的最终运行配置：冻结 JSON 值 + 该 Chat 独立的序列化快照。 */
export interface ResolvedChatAgentConfig {
    readonly agentConfig: unknown;
    readonly agentConfigJson: string;
}

function isJsonObject(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** 用 defineProperty 写入，避免 `__proto__` 等键改写组合结果的原型。 */
function defineEntry(
    target: Record<string, unknown>,
    key: string,
    value: unknown,
): void {
    Object.defineProperty(target, key, {
        value,
        enumerable: true,
        writable: true,
        configurable: true,
    });
}

function jsonErrorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}

/**
 * Manager 构造时校验并快照部署身份。
 *
 * `machineId` 必须是非空字符串，`storage` 必须是 JSON 纯数据；快照深拷贝并冻结，
 * 调用方后续修改原对象不会影响已创建或后续 Chat。非法输入抛 `TypeError`。
 */
export function snapshotChatStore(store: unknown): ChatStoreConfig {
    if (!isJsonObject(store)) {
        throw new TypeError(
            "ChatManager requires store: { storage, machineId }",
        );
    }
    const machineId = store.machineId;
    if (typeof machineId !== "string" || machineId.length === 0) {
        throw new TypeError(
            "ChatManager store.machineId must be a non-empty string",
        );
    }
    if (!Object.prototype.hasOwnProperty.call(store, "storage")) {
        throw new TypeError("ChatManager store.storage is required");
    }
    let storage: unknown;
    try {
        storage = cloneJsonValue(store.storage, "store.storage");
    } catch (error) {
        throw new TypeError(
            `ChatManager store.storage must be JSON data: ${jsonErrorMessage(error)}`,
            { cause: error },
        );
    }
    return Object.freeze({ storage, machineId });
}

/**
 * 组合单个 Chat 的内部 agentConfig JSON：per-chat agent 配置 + Manager store。
 *
 * - per-chat 值必须是 JSON 安全对象，`cwd` 必须是非空字符串；
 * - 顶层出现 `storage`/`machineId` 一律拒绝（即使值与 Manager 相同）；
 * - 注入 Manager 快照并再次深拷贝、冻结，每个 Chat 得到独立 JSON，不复用 Manager 引用。
 *
 * 所有校验失败抛 `TypeError`。
 */
export function resolveChatAgentConfig(
    store: ChatStoreConfig,
    agentConfig: unknown,
): ResolvedChatAgentConfig {
    if (!isJsonObject(agentConfig)) {
        throw new TypeError(
            "createChat requires agent to be a JSON object",
        );
    }
    // 保留键检查先于 JSON 克隆：即使值为 undefined/function 也按越权处理。
    for (const key of RESERVED_KEYS) {
        if (Object.prototype.hasOwnProperty.call(agentConfig, key)) {
            throw new TypeError(
                `agent.${key} is Manager-scoped via store and cannot be set per chat`,
            );
        }
    }
    let snapshot: Record<string, unknown>;
    try {
        snapshot = cloneJsonValue(agentConfig, "agentConfig");
    } catch (error) {
        throw new TypeError(
            `createChat requires a JSON-safe agent object: ${jsonErrorMessage(error)}`,
            { cause: error },
        );
    }
    if (typeof snapshot.cwd !== "string" || snapshot.cwd.length === 0) {
        throw new TypeError(
            "createChat requires agent.cwd to be a non-empty string",
        );
    }

    const composed: Record<string, unknown> = { ...snapshot };
    defineEntry(composed, "storage", store.storage);
    defineEntry(composed, "machineId", store.machineId);
    // 二次深拷贝：storage 是 Manager 快照，不能让多个 Chat 共享同一 JSON 对象。
    const frozen = cloneJsonValue(composed, "agentConfig");
    return {
        agentConfig: frozen,
        agentConfigJson: JSON.stringify(frozen),
    };
}
