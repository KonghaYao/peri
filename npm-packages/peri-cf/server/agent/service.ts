import {
  SessionCreator, logError,
  type AgentEnvironment, type Chat, type ChatCommand, type ChatCommandPayload,
  type ChatSessionStore, type ChatRuntimeSubscription, type ChatSummary,
  type RuntimeSummary, type AcquireHost, type SyncState,
} from "../../core/agent";
import { PeriInstance } from "./peri-instance";

/** 驻留实例上限：超限时按 LRU 回收空闲实例。 */
const MAX_RESIDENT_INSTANCES = 8;
/** 创建成功但 Store 尚不可见时的稳定错误；调用方看到 500，诊断由调用层记录。 */
const CREATED_CHAT_NOT_VISIBLE = "Created ACP session is not visible in store";

/** `ChatService` 的注入依赖；全部由 `ChatManager` 装配。 */
export interface ChatServiceDeps {
  /** 只读会话存储；实例复原与创建后读取都走它。 */
  store: ChatSessionStore;
  /** 获取一个宿主租约；启动/关闭证据由租约维护。 */
  acquireHost: AcquireHost;
  /** 每个实例一个独立环境；创建会话不传 `chatId`。 */
  createEnvironment: (chatId?: string) => AgentEnvironment;
}

/** 存储层清单 + 当前实例驻留实例的实时事实；非驻留会话保持存储默认值。 */
export function mergeChatSummaries(
  chats: readonly ChatSummary[], runtimeSummaries: ReadonlyMap<string, RuntimeSummary>,
): ChatSummary[] {
  return chats.map((chat) => {
    const runtime = runtimeSummaries.get(chat.id);
    return {
      ...chat,
      running: runtime?.running ?? false,
      pendingInteractionCount: runtime?.pendingInteractionCount ?? 0,
      lastTurnStatus: runtime?.lastTurnStatus ?? null,
      lastCompletedAt: runtime?.lastCompletedAt ?? null,
    };
  });
}

/** 订阅建立时的完整投影由 `initial` 返回；后续变化经 `onState` 推送。 */
export type ChatSubscription = ChatRuntimeSubscription;

/**
 * 会话服务：持有 `PeriInstance` 映射并执行驻留 LRU；存储、宿主启动器与
 * 环境工厂都由 `ChatManager` 注入。每次执行/复原仍是独立 WASM 实例。
 */
export class ChatService {
  private readonly instances = new Map<string, PeriInstance>();
  /** 并发 `shutdown()` 共享同一轮释放；结束后清空，允许下一轮兜底。 */
  private shutdownPromise: Promise<void> | undefined;

  /** 只保存依赖；实例在首次访问时惰性创建。 */
  constructor(private readonly deps: ChatServiceDeps) {}

  /** 存储清单与当前驻留实例的事实合并；不改变存储中的会话。 */
  async list(): Promise<ChatSummary[]> {
    return mergeChatSummaries(await this.deps.store.list(), this.summaries());
  }

  /** 创建会话（一次性宿主执行 `session/new`），再从 Store 读取身份。 */
  async create(title: string): Promise<Chat> {
    const creator = new SessionCreator(this.deps.acquireHost, this.deps.createEnvironment());
    const chatId = await creator.create(title);
    const chat = await this.deps.store.get(chatId);
    if (!chat) throw new Error(CREATED_CHAT_NOT_VISIBLE);
    return chat;
  }

  /** 读取会话完整快照；首次访问会触发冷复原，可能启动新宿主。 */
  read(chatId: string): Promise<SyncState> {
    return this.instanceFor(chatId).read();
  }

  /** 提交命令；`chatId` 必须来自已持久化的会话。 */
  run(chatId: string, command: ChatCommand): Promise<ChatCommandPayload | undefined> {
    return this.instanceFor(chatId).command(command);
  }

  /** 订阅会话投影；返回的订阅建立后由调用方负责关闭。 */
  watch(chatId: string, onState: (state: SyncState) => void): Promise<ChatSubscription> {
    return this.instanceFor(chatId).watch(onState);
  }

  /** 释放所有驻留实例并清空映射；幂等且可并发调用。 */
  shutdown(): Promise<void> {
    this.shutdownPromise ??= this.disposeInstances().finally(() => {
      this.shutdownPromise = undefined;
    });
    return this.shutdownPromise;
  }

  /** 测试可替换实例构造；生产固定构造 `PeriInstance`。 */
  protected createInstance(chatId: string): PeriInstance {
    return new PeriInstance(chatId, this.deps.store, this.deps.createEnvironment(chatId), this.deps.acquireHost);
  }

  /** 取得（必要时创建）会话实例；不触发复原。 */
  private instanceFor(chatId: string): PeriInstance {
    const existing = this.instances.get(chatId);
    if (existing) {
      this.touch(chatId, existing);
      return existing;
    }
    const instance = this.createInstance(chatId);
    this.instances.set(chatId, instance);
    this.evict(chatId);
    return instance;
  }

  /** 驻留实例的只读摘要表；未驻留会话不出现在结果中。 */
  private summaries(): ReadonlyMap<string, RuntimeSummary> {
    return new Map([...this.instances].map(([chatId, instance]) => [chatId, instance.summary()]));
  }

  /** 淘汰最久未用且空闲的实例：`keep`（刚插入的）不参与淘汰；全部忙碌时允许超限。 */
  private evict(keep?: string): void {
    for (const [chatId, instance] of [...this.instances]) {
      if (this.instances.size <= MAX_RESIDENT_INSTANCES) return;
      if (chatId === keep) continue;
      if (!instance.recyclable) continue;
      this.instances.delete(chatId);
      void instance.dispose().catch((error) => {
        logError("Peri instance disposal failed after eviction", error, { chatId });
      });
    }
  }

  /** 把实例移到 LRU 尾部；映射已被替换时不动。 */
  private touch(chatId: string, instance: PeriInstance): void {
    if (this.instances.get(chatId) !== instance) return;
    this.instances.delete(chatId);
    this.instances.set(chatId, instance);
  }

  /** 逐个释放实例；单个失败只记录，不阻断其余实例。 */
  private async disposeInstances(): Promise<void> {
    const instances = [...this.instances.values()];
    this.instances.clear();
    const results = await Promise.allSettled(instances.map((instance) => instance.dispose()));
    for (const result of results) {
      if (result.status === "rejected")
        logError("Peri instance disposal failed during shutdown", result.reason);
    }
  }
}
