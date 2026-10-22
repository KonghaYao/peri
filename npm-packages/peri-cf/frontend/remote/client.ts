import type { InteractionAnswer } from "@peri-code/sdk/view";
import {
  errorText, publicError,
  type Chat, type ChatCommand, type ChatCommandPayload, type ChatRepository, type ChatRuntimePort,
  type ChatSummary, type SyncState,
} from "@core/agent";
import { clientCommandFailure, fromCommandError, type CommandFailure, type CommandOutcome } from "./commandFailure";

/** 浏览器视图阶段；由客户端组合根维护，不属于运行期端口。 */
export type Phase = "idle" | "loading" | "streaming" | "cancelling" | "syncing" | "blocked";

/** 会话读面的独立生命周期；错误横幅被关闭后仍保留失败事实。 */
export type ChatLoadState = "idle" | "loading" | "ready" | "failed";

export type SyncStatus = "connecting" | "synced" | "failed";

/** 运行期可选上报的实时连接状态；不属于 `ChatRuntimePort` 契约。 */
export interface ObservableRuntime {
  subscribeStreamStatus?(listener: (status: SyncStatus, error?: Error) => void): () => void;
}

/** Agent 门面发布的只读快照；UI 只消费，不修改。 */
export interface AgentView {
  selected: Chat | null;
  state: SyncState | null;
  phase: Phase;
  historyReady: boolean;
  loadState: ChatLoadState;
  /** 实时事件流状态：与读面失败分开，内容已就绪时也能提示断线。 */
  liveSync: SyncStatus;
  error: string;
}

/** 兼容视图层既有命名。 */
export type ChatView = AgentView;

export interface AgentManagerDeps {
  repository: Pick<ChatRepository, "list" | "create">;
  runtimeFor(chatId: string): ChatRuntimePort;
}

/** 服务端确定拒绝：命令未生效、执行状态已知，只给提示，不进入 uncertain。 */
const REFUSAL_TEXT: Record<string, string> = {
  busy: "会话正在执行中，请等待本轮完成或先停止任务。",
  blocked: "会话执行被阻断，请先确认停止状态。",
  settled: "本轮任务已经结算，无需再次操作。",
  unavailable: "会话暂时不可用，请稍后重试。",
};

function refusalText(failure: CommandFailure): string {
  return REFUSAL_TEXT[failure.code] ?? `命令未被接受：${failure.message}`;
}

/** 客户端预检失败：命令未发送，提示可重试且不锁定会话。 */
function notSentText(failure: CommandFailure): string {
  return `${failure.message}（命令未发送，可重试）`;
}

/** 交互回答失败码 → 用户可执行的提示（沿用原有结算文案）。 */
function answerFailureText(failure: CommandFailure): string {
  if (failure.code === "settled" || failure.code === "expired") return "这个交互已经结算或失效，请刷新后重试。";
  if (failure.disposition === "not_sent") return `${failure.message}请刷新后重试。`;
  if (failure.disposition === "unknown") return `回答提交结果未知，请刷新后确认：${failure.message}`;
  return `回答未能提交：${failure.message}`;
}

/**
 * 会话管理的唯一门面：清单/创建、选择与复原、发送/停止、交互回答、载荷读取。
 * 不依赖 React 或浏览器存储；运行期与宿主经构造参数注入，视图层只订阅 `getState()`。
 */
export class AgentManager {
  private view: AgentView = {
    selected: null, state: null, phase: "idle", historyReady: true, loadState: "idle", liveSync: "connecting", error: "",
  };
  private readonly listeners = new Set<() => void>();
  private revision = 0;
  private selectionRequest = 0;
  private disposed = false;
  private navigation: AbortController | undefined;
  private runtime: ChatRuntimePort | undefined;
  private watchClose: (() => void) | undefined;
  private watchStatusClose: (() => void) | undefined;
  private synced = false;
  private everSynced = false;
  private syncStatus: SyncStatus = "connecting";
  private state: SyncState | undefined;
  private selecting = false;
  private stopping = false;
  private uncertain = false;
  private pending = false;
  private baseline = "";
  private inFlight: Promise<void> | undefined;
  private releaseReady: ((ready: boolean) => void) | undefined;
  /** 用户已关闭的错误文本；同一错误再次到达时不重新弹出，直到出现新错误。 */
  private dismissedError = "";

  constructor(private readonly deps: AgentManagerDeps) {}

  // —— 只读快照与订阅 ——

  getState = (): AgentView => this.view;

  subscribe = (listener: () => void): (() => void) => {
    if (this.disposed) return () => {};
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  // —— 读侧：列表、创建、详情 ——

  async listChats(signal?: AbortSignal): Promise<ChatSummary[]> {
    signal?.throwIfAborted();
    const chats = await this.deps.repository.list();
    signal?.throwIfAborted();
    return chats;
  }

  async createChat(signal?: AbortSignal): Promise<Chat> {
    signal?.throwIfAborted();
    const chat = await this.deps.repository.create("New chat");
    signal?.throwIfAborted();
    return chat;
  }

  /** 详情读面：先复原展示材料，再由 Store 复核会话元数据。 */
  async readChat(chatId: string, signal?: AbortSignal): Promise<SyncState> {
    return await this.readState(this.deps.runtimeFor(chatId), chatId, signal);
  }

  private async readState(runtime: ChatRuntimePort, chatId: string, signal?: AbortSignal): Promise<SyncState> {
    signal?.throwIfAborted();
    const state = await runtime.read();
    signal?.throwIfAborted();
    if (state.chat.id !== chatId) throw new Error("本地宿主返回了不匹配的会话。");
    if (state.readiness === "unsupported")
      throw new Error("会话使用了不受支持的数据版本，请刷新页面或升级应用后再继续。");
    return state;
  }

  // —— 选择与命令 ——

  /** 会话命令通道：执行驻留在服务端运行期；未选择会话时为 undefined。 */
  private channel(): ChatRuntimePort | undefined {
    return this.disposed ? undefined : this.runtime;
  }

  private closeWatch(): void {
    const close = this.watchClose;
    this.watchClose = undefined;
    this.watchStatusClose?.();
    this.watchStatusClose = undefined;
    this.runtime = undefined;
    close?.();
  }

  /** 客户端侧命令封装：区分未发送（not_sent）与执行方处置（rejected/unknown）。 */
  private async command(command: ChatCommand, signal?: AbortSignal): Promise<CommandOutcome> {
    if (this.disposed) return clientCommandFailure("unavailable", "会话连接已释放，命令未发送。");
    if (signal?.aborted) return clientCommandFailure("aborted", "命令已中止，未发送。");
    const runtime = this.channel();
    if (!runtime) return clientCommandFailure("unavailable", "会话尚未建立本地连接，命令未发送。");
    try {
      const payload = await runtime.command(command);
      return { ok: true, ...(payload ? { payload } : {}) };
    } catch (error) {
      return fromCommandError(error);
    }
  }

  private update(change: Partial<AgentView>) {
    if (this.disposed) return;
    // 新的错误文本代表新事实，允许再次展示；同一文本被关闭后继续保持隐藏。
    if (change.error) this.dismissedError = "";
    this.view = { ...this.view, ...change };
    for (const listener of this.listeners) listener();
  }

  /** 执行状态不确定：阻断后续发送，并要求先确认停止。 */
  private block(reason: string, suffix: string) {
    this.uncertain = true;
    this.update({ phase: this.phase(), error: `${reason} ${suffix}` });
  }

  private phase(): Phase {
    if (this.stopping) return "cancelling";
    if (this.uncertain || this.state?.executionBlocked) return "blocked";
    if (this.pending || this.state?.running) return "streaming";
    return this.synced || this.syncStatus === "failed" ? "idle" : "syncing";
  }

  dismissError() {
    this.dismissedError = this.view.error;
    this.update({ error: "" });
  }

  /** 服务端错误与执行错误合并展示；已关闭的同一错误不因下一帧重复出现。 */
  private visibleError(serverError: string | undefined): string {
    const candidate = serverError ?? (this.uncertain ? this.view.error : "");
    if (!candidate) {
      this.dismissedError = "";
      return "";
    }
    return candidate === this.dismissedError ? "" : candidate;
  }

  private receive(state: SyncState) {
    this.state = state;
    if (this.pending && (state.running || JSON.stringify(state.entries) !== this.baseline)) this.pending = false;
    this.update({
      selected: state.chat, state,
      error: this.visibleError(state.error), phase: this.phase(),
    });
  }

  private status(status: SyncStatus, error?: Error) {
    this.syncStatus = status;
    this.synced = status === "synced";
    if (this.synced) this.everSynced = true;
    const failedBeforeFirstSync = status === "failed" && !this.everSynced;
    this.dismissedError = error ? "" : this.dismissedError;
    this.update({
      // 已同步过的内容在断线时仍然可读；首帧失败才是真正的读面失败。
      historyReady: failedBeforeFirstSync ? false : this.synced || this.view.historyReady,
      loadState: status === "synced" ? "ready" : failedBeforeFirstSync ? "failed" : this.view.loadState,
      liveSync: status,
      phase: this.phase(), ...(error ? { error: errorText(error) } : {}),
    });
    if (this.synced || status === "failed") {
      this.releaseReady?.(this.synced);
      this.releaseReady = undefined;
    }
  }

  /**
   * 选择会话并建立命令连接；`null` 新建。返回 false 表示导航被取代、取消或连接失败。
   * 同 ID 仅在已就绪且无错误时 no-op；`force` 供失败恢复强制重读，不发 cancel。
   */
  async openChat(chat: Chat | null, options: { force?: boolean } = {}): Promise<boolean> {
    if (this.disposed || this.stopping) return false;
    // 已就绪且无错误：重复点击不重建连接，也不触碰运行中的任务。
    if (!options.force && chat && this.view.selected?.id === chat.id &&
      this.state !== undefined && this.view.loadState === "ready" && !this.view.error) return true;
    const request = ++this.selectionRequest;
    this.selecting = true;
    try {
      const revision = ++this.revision;
      this.navigation?.abort();
      this.closeWatch();
      this.releaseReady?.(false);
      this.releaseReady = undefined;
      this.synced = false;
      this.everSynced = false;
      this.syncStatus = "connecting";
      this.state = undefined;
      // 命令状态属于上一个会话：切换后不得把旧会话的 pending/uncertain 带入新会话。
      this.pending = false;
      this.inFlight = undefined;
      this.uncertain = false;
      this.dismissedError = "";
      const controller = new AbortController();
      this.navigation = controller;
      const current = () => !this.disposed && this.revision === revision && !controller.signal.aborted;
      this.update({
        selected: chat, state: null, historyReady: false, loadState: "loading", liveSync: "connecting", phase: "loading", error: "",
      });
      try {
        const target = chat ?? await this.createChat(controller.signal);
        if (!current()) return false;
        this.update({ selected: target });
        // 一次选择只取一个运行期：初始读面与订阅共用它，避免各自启动一条事件流。
        const runtime = this.deps.runtimeFor(target.id);
        const history = await this.readState(runtime, target.id, controller.signal);
        if (!current()) return false;
        this.update({ selected: history.chat, state: history, phase: "syncing" });
        // 命令通道在订阅建立前就可使用（与旧连接语义一致）；watch 只负责订阅与首帧。
        this.runtime = runtime;
        const observable = runtime as ChatRuntimePort & ObservableRuntime;
        this.watchStatusClose = observable.subscribeStreamStatus?.((status, error) => {
          if (!current()) return;
          if (status === "synced") {
            // EventSource 自动重连成功后恢复同步事实，而不是永久停在“同步失败”。
            if (this.syncStatus !== "synced") this.status("synced");
            return;
          }
          if (status === "connecting") {
            this.update({ liveSync: "connecting" });
            return;
          }
          // 首帧失败走读面失败；已有内容后断线只标记实时流，保留可读历史。
          this.status("failed", error ?? new Error("会话事件流连接中断"));
        });
        const ready = new Promise<boolean>(resolve => { this.releaseReady = resolve; });
        const subscription = await runtime.watch(state => { if (current()) this.receive(state); });
        if (!current()) { subscription.close(); return false; }
        this.watchClose = subscription.close;
        this.status("synced");
        this.receive(subscription.initial);
        return await ready;
      } catch (failure) {
        this.watchStatusClose?.();
        this.watchStatusClose = undefined;
        if (current()) this.status("failed", failure instanceof Error ? failure : new Error(publicError(failure)));
        return false;
      }
    } finally { if (request === this.selectionRequest) this.selecting = false; }
  }

  /**
   * 失败恢复：关闭当前订阅后重新 read/watch，绕过同 ID no-op。
   * 不 dispose 服务端运行期、不发 cancel；运行中只允许在读面或事件流已失败时重载。
   */
  async reloadChat(): Promise<boolean> {
    if (this.disposed || this.stopping) return false;
    // 运行中只在读面/事件流已失败时重载：只重读与重订阅，不发 cancel。
    if (this.state?.running && this.view.loadState !== "failed" && this.view.liveSync !== "failed") return false;
    return await this.openChat(this.view.selected, { force: true });
  }

  /**
   * 发送 prompt：返回实际接收命令的会话 id，失败返回 null。
   * 隐式创建时也返回创建出的会话，调用方不得再从当前视图猜目标身份。
   */
  async send(content: string): Promise<string | null> {
    if (this.disposed || !content.trim() || this.selecting || this.stopping || this.pending ||
      this.inFlight || this.uncertain || this.state?.running || this.state?.executionBlocked) return null;
    if (!this.view.selected && !await this.openChat(null)) return null;
    if (this.disposed || !this.synced || !this.view.selected || this.state?.running || this.state?.executionBlocked) return null;
    const targetId = this.view.selected.id;
    const revision = this.revision;
    this.pending = true;
    this.baseline = JSON.stringify(this.state?.entries ?? []);
    this.update({ phase: "streaming", error: "" });
    const issued = Promise.resolve().then(() => this.command({ op: "prompt", content: content.trim() }));
    const settled = issued.then(() => undefined, () => undefined);
    this.inFlight = settled;
    try {
      const result = await issued;
      if (this.disposed) return null;
      // 命令已被接受：即使同会话 reload 已替换视图，也不能误报“未发送”。
      if (result.ok) return targetId;
      // 旧会话的 prompt 可能在切换后结算：拒绝/未知只对当时的连接有意义。
      if (revision !== this.revision) return null;
      this.pending = false;
      // rejected 说明命令明确未生效；unknown 无法证明未执行，必须阻断重跑；not_sent 可安全重试。
      if (result.disposition === "unknown")
        this.block(errorText(result.message), "尚未确认执行状态，请先停止任务。");
      else if (result.disposition === "not_sent") this.update({ phase: this.phase(), error: notSentText(result) });
      else this.update({ phase: this.phase(), error: refusalText(result) });
      return null;
    } catch (failure) {
      if (!this.disposed && revision === this.revision) {
        this.pending = false;
        this.block(errorText(failure), "尚未确认执行状态，请先停止任务。");
      }
      return null;
    } finally {
      if (this.inFlight === settled) this.inFlight = undefined;
    }
  }

  async stop(): Promise<boolean> {
    if (this.disposed || this.stopping) return false;
    if (!this.view.selected || (!this.pending && !this.inFlight && !this.uncertain && !this.state?.running &&
      !this.state?.executionBlocked)) return true;
    this.stopping = true;
    this.update({ phase: "cancelling", error: "" });
    try {
      // 等待在途 prompt 结算，避免取消先于命令到达；取消只在真实确认后成立。
      await this.inFlight;
      if (this.disposed) return false;
      const connected = this.channel() !== undefined;
      const result = connected
        ? await this.command({ op: "cancel" })
        : clientCommandFailure("unavailable", "会话尚未建立本地连接。");
      if (!result.ok) {
        // 明确 rejected/not_sent 不能证明任务仍在执行，不锁定会话；只有 unknown 进入 uncertain。
        if (result.disposition === "unknown") {
          this.uncertain = true;
          this.update({ error: `${errorText(result.message)} 尚未确认任务停止，请再次点击停止。` });
        } else {
          this.update({ error: result.disposition === "not_sent" ? notSentText(result) : refusalText(result) });
        }
        return false;
      }
      if (this.disposed) return false;
      this.pending = false;
      this.uncertain = false;
      return true;
    } catch (failure) {
      this.uncertain = true;
      this.update({ error: `${errorText(failure)} 尚未确认任务停止，请再次点击停止。` });
      return false;
    } finally {
      this.stopping = false;
      this.update({ phase: this.phase() });
    }
  }

  /** 交互回答：经当前会话的命令通道提交；未就绪时明确 not_sent，其余按处置给出不同文案。 */
  async answerInteraction(interactionId: string, answer: InteractionAnswer): Promise<void> {
    if (!this.channel())
      throw new Error(answerFailureText(clientCommandFailure("unavailable", "会话尚未就绪，回答未提交。")));
    const result = await this.command({ op: "interaction", interactionId, answer });
    if (!result.ok) throw new Error(answerFailureText(result));
  }

  /** 工具载荷全文：同一条连接读取；未就绪时返回 not_sent，由 reader 映射成 unavailable。 */
  async readPayload(refId: string, signal?: AbortSignal): Promise<CommandOutcome> {
    if (!this.channel()) return clientCommandFailure("unavailable", "会话尚未就绪，无法读取全文。");
    return await this.command({ op: "payload", refId }, signal);
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.revision++;
    this.navigation?.abort();
    this.closeWatch();
    this.releaseReady?.(false);
    this.releaseReady = undefined;
    this.listeners.clear();
  }
}
