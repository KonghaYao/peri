import {
  ChatUserAction,
  type InteractionAnswer,
  type ToolPayloadResponse,
} from "@peri-code/sdk";
import { logError } from "./logging";
import { type ActionTargetDTO, type Chat, type ChatSummary } from "./contracts";
import type { ChatDisplayState } from "./display-state";
import { ChatProjection } from "./projection";
import {
  RemoteActionError, RemoteSendReceipt, createRemoteActions,
  type ActionReceipt, type RemoteActions,
} from "./actions";
import { ChatSyncProvider, type SyncConnectionStatus, type WebSocketLike } from "./sync";
import { parseActionTarget } from "./actions";

/** 页面阶段；`streaming`/`cancelling` 由副本运行事实驱动，不由列表推断。 */
export type Phase = "idle" | "loading" | "streaming" | "cancelling" | "syncing" | "blocked";

/** 读面状态：`unsupported` 与「空历史」明确区分。 */
export type ChatLoadState = "idle" | "loading" | "ready" | "failed" | "unsupported";

export type SyncStatus = "connecting" | "synced" | "failed";

/** 恢复入口：错误文案必须有可执行动作，不能让用户停留在无法解释的阻断态。 */
export type ChatRecovery = "none" | "reload";

/** 发送结果：202 只代表入队；`delivered` 由副本队列/用户条目结算。 */
export type ChatSendOutcome =
  | { delivered: true; sessionId: string }
  | { delivered: false; sessionId: string | null; message: string };

/** 控制器发布的只读视图；UI 只消费，不直接修改。 */
export interface ChatView {
  selected: Chat | null;
  state: ChatDisplayState | null;
  phase: Phase;
  historyReady: boolean;
  loadState: ChatLoadState;
  liveSync: SyncStatus;
  error: string;
  /** 当前错误对应的恢复动作；`none` 表示只能等待或 dismiss。 */
  recovery: ChatRecovery;
}

interface ChatHandleRecord {
  /** Action/WS 地址：新句柄用 handle id；409 复用已有控制者时用 sessionId。 */
  address: string;
  /** 本客户端创建的句柄；为 null 表示复用服务端既有控制者。 */
  handleId: string | null;
  chat: Chat;
}

interface PendingDelivery {
  inputId: string;
  resolve(): void;
  reject(error: Error): void;
}

interface ActiveChat extends ChatHandleRecord {
  projection: ChatProjection;
  sync: ChatSyncProvider;
  unsubscribe: () => void;
  everReady: boolean;
  initialResolve?: (ok: boolean) => void;
  pending: PendingDelivery[];
}

export interface ChatControllerDeps {
  actions: RemoteActions;
  list(signal?: AbortSignal): Promise<ChatSummary[]>;
  syncUrl(chatAddress: string): string;
  webSocketFactory?: (url: string) => WebSocketLike;
  /** 首次同步预算；超时只报告失败，不关闭服务端句柄。 */
  initialSyncTimeoutMs?: number;
  now?: () => Date;
}

const UNSUPPORTED = "会话使用了不受支持的数据版本，请刷新页面或升级应用后再继续。";
const INITIAL_SYNC_TIMEOUT_MS = 30_000;
const DELIVERY_UNCONFIRMED = "消息尚未确认送达，请检查连接后重试。";

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function sameChat(left: Chat | null, right: Chat | null): boolean {
  return left?.id === right?.id && left?.title === right?.title && left?.updatedAt === right?.updatedAt;
}

/**
 * 页面级 Chat 控制器：句柄身份、两步打开历史、绑定代次、Action 与同步连接。
 *
 * 不持有服务端执行状态，也不伪造运行态：列表只来自目录；运行/交互/队列
 * 只来自 `ChatReplica`。切换会话只释放本地连接，不 cancel、不 closeChat。
 */
export class ChatController {
  private view: ChatView = {
    selected: null, state: null, phase: "idle", historyReady: true,
    loadState: "idle", liveSync: "connecting", error: "", recovery: "none",
  };
  private readonly listeners = new Set<() => void>();
  private readonly handles = new Map<string, ChatHandleRecord>();
  private active: ActiveChat | undefined;
  private disposed = false;
  private selection = 0;
  private actionError = "";
  private connectionError = "";
  private dismissedError = "";
  private uncertain = false;
  private recovery: ChatRecovery = "none";
  private sending = false;
  private sendInFlight: Promise<unknown> | undefined;
  /** 取消意图只对发起时的 running turn 有效；turn 结算或换代后自动失效。 */
  private cancelRequested = false;
  private cancelTurnId: string | null = null;

  constructor(private readonly deps: ChatControllerDeps) {}

  getState = (): ChatView => this.view;

  subscribe = (listener: () => void): (() => void) => {
    if (this.disposed) return () => {};
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  listChats(signal?: AbortSignal): Promise<ChatSummary[]> {
    return this.deps.list(signal);
  }

  currentChatId(): string | null {
    return this.view.selected?.id ?? null;
  }

  /**
   * 打开历史会话：先 POST /api/chats 取得句柄，再 change_session，收到 202 后才连 WS。
   * 同一客户端已绑定过的会话复用句柄，不重复换绑。
   */
  async openChat(target: ChatSummary | null): Promise<boolean> {
    if (this.disposed) return false;
    const selected = target === null ? null : toChat(target);
    const boundId = this.active?.projection.state().chat.id ?? null;
    if (target !== null && boundId === selected!.id
      && this.view.loadState !== "failed" && this.view.liveSync !== "failed") return true;

    const token = ++this.selection;
    this.releaseActive();
    this.actionError = "";
    this.connectionError = "";
    this.uncertain = false;
    this.recovery = "none";
    this.clearCancel();
    this.update({
      selected, state: null, phase: "loading", historyReady: false,
      loadState: "loading", liveSync: "connecting", error: "", recovery: "none",
    });

    try {
      if (target === null) return await this.openNew(token);
      return await this.openExisting(target, token);
    } catch (error) {
      if (token !== this.selection || this.disposed) return false;
      this.actionError = messageOf(error);
      this.recovery = "reload";
      this.update({
        phase: "idle", loadState: "failed", historyReady: false,
        error: this.visibleError(), recovery: this.recovery,
      });
      return false;
    }
  }

  /**
   * 失败恢复：先释放本地连接，再真正重开当前会话；不 cancel、不关闭服务端句柄。
   * 新建失败（尚无 selected）时重试新会话创建，避免「重新加载」无请求可发。
   */
  async reloadChat(): Promise<boolean> {
    if (this.disposed) return false;
    if (!this.view.selected) return await this.openChat(null);
    if (this.view.state?.running && !this.view.state.executionBlocked
      && this.view.loadState !== "failed" && this.view.liveSync !== "failed") return false;
    const selected = this.view.selected;
    this.handles.delete(selected.id);
    this.uncertain = false;
    this.recovery = "none";
    this.actionError = "";
    this.connectionError = "";
    this.clearCancel();
    // 先释放 active，`openChat` 也不会命中同 id 的「已打开」短路。
    this.releaseActive();
    return await this.openChat({
      ...selected, sessionId: selected.id, title: selected.title, cwd: "", messageCount: 0, createdAt: selected.updatedAt,
    });
  }

  /**
   * 关闭未确认时的逃生入口：显式关闭当前句柄（`actions.close`）后新建会话。
   * 关闭失败保留当前连接与错误，`close_incomplete` 原样上报，不假装已关闭。
   */
  async rebuildChat(): Promise<boolean> {
    if (this.disposed) return false;
    const address = this.active?.address ?? this.view.selected?.id ?? null;
    if (address) {
      try {
        await this.deps.actions.close(address);
      } catch (error) {
        this.failAction(error);
        return false;
      }
    }
    const selected = this.view.selected;
    if (selected) this.handles.delete(selected.id);
    this.releaseActive();
    this.selection += 1;
    this.uncertain = false;
    this.recovery = "none";
    this.actionError = "";
    this.connectionError = "";
    this.clearCancel();
    return await this.openChat(null);
  }

  /**
   * 发送输入：202 只代表入队。等副本确认投递后才算送达；
   * 撤回/连接释放时返回未送达并保留可重试的可见错误，不静默丢输入。
   */
  async send(content: string): Promise<ChatSendOutcome> {
    const sent = await this.dispatchSend(content);
    if (!sent) {
      return {
        delivered: false,
        sessionId: null,
        message: this.actionError || DELIVERY_UNCONFIRMED,
      };
    }
    try {
      await sent.receipt;
      this.actionError = "";
      this.uncertain = false;
      this.recovery = "none";
      this.refresh();
      return { delivered: true, sessionId: sent.sessionId };
    } catch (error) {
      const message = messageOf(error);
      this.failAction(new RemoteActionError("input_not_delivered", `消息未送达：${message}`, "rejected"));
      return {
        delivered: false,
        sessionId: sent.sessionId,
        message: this.actionError || message,
      };
    }
  }

  /**
   * 返回远程发送回执与目标会话。回执本身是 thenable（等待投递），
   * 因此用包装对象跨 async 边界，避免 `await` 把回执摊平成 `void`。
   */
  async dispatchSend(content: string): Promise<{ receipt: RemoteSendReceipt; sessionId: string } | null> {
    const text = content.trim();
    if (this.disposed || !text || this.sending || this.uncertain || this.cancelPending()) return null;
    if (this.view.loadState !== "ready" && this.active) return null;
    if (!this.active) {
      if (!await this.openChat(null)) return null;
    }
    const active = this.active;
    const target = active ? this.bindingTarget(active) : null;
    if (!active || !target) {
      this.actionError = "会话尚未完成绑定，消息未发送。";
      this.refresh();
      return null;
    }
    this.sending = true;
    const issued = this.deps.actions.execute(active.address, ChatUserAction.send({ text }), target);
    this.sendInFlight = issued.catch(() => undefined);
    try {
      const receipt = await issued;
      if (!receipt.inputId) throw new RemoteActionError("invalid_receipt", "发送回执缺少 inputId。", "unknown");
      const delivery = this.waitForDelivery(active, receipt.inputId);
      const remote = new RemoteSendReceipt(
        receipt.requestId,
        receipt.inputId,
        { chatId: receipt.chatId, sessionId: receipt.sessionId, bindingRevision: receipt.bindingRevision },
        Promise.resolve(),
        delivery,
      );
      return { receipt: remote, sessionId: receipt.sessionId };
    } catch (error) {
      this.failAction(error);
      return null;
    } finally {
      this.sending = false;
      this.refresh();
    }
  }

  /**
   * 请求取消：`requested` 只表示通知已写出，不是停止确认。
   * 任务是否结束继续由副本的 `running` 决定。
   */
  async stop(): Promise<boolean> {
    if (this.disposed) return false;
    const active = this.active;
    const target = active ? this.bindingTarget(active) : null;
    if (!active || !target) return true;
    if (!this.view.state?.running && !this.view.state?.executionBlocked) return true;
    if (this.cancelPending()) return true;
    this.cancelRequested = true;
    this.cancelTurnId = this.view.state?.activeTurn.turnId ?? null;
    this.refresh();
    // 保证取消不先于在途发送到达；不等待整轮执行。
    await this.sendInFlight;
    try {
      const receipt = await this.deps.actions.execute(active.address, ChatUserAction.cancel(), target);
      if (receipt.status === "idle") this.clearCancel();
      // 未知结果仍需要 reload 的说明，不能被一次成功的取消静默抹掉。
      if (!this.uncertain) {
        this.actionError = "";
        this.recovery = "none";
      }
      return true;
    } catch (error) {
      this.clearCancel();
      this.failAction(error);
      return false;
    } finally {
      this.refresh();
    }
  }

  async answerInteraction(interactionId: string, answer: InteractionAnswer): Promise<void> {
    const active = this.active;
    const target = active ? this.bindingTarget(active) : null;
    if (!active || !target) throw new Error("会话尚未就绪，回答未提交。");
    try {
      await this.deps.actions.execute(
        active.address,
        ChatUserAction.answer({ interactionId, answer }),
        target,
      );
      this.actionError = "";
      this.recovery = "none";
      this.refresh();
    } catch (error) {
      this.failAction(error);
      throw new Error(this.actionError || messageOf(error));
    }
  }

  /**
   * 全文读取：只接受副本里仍被引用的载荷 ID，字节数取自引用事实。
   * 旧会话/丢失引用的 ID 明确 unavailable，不猜字节数。
   */
  readPayloadById(id: string, signal?: AbortSignal): Promise<ToolPayloadResponse> {
    const active = this.active;
    if (!active) {
      return Promise.reject(new Error("会话尚未就绪，无法读取全文。"));
    }
    return this.payloadRefBytes(id).then((bytes) => {
      if (bytes === undefined) throw new Error("全文引用已不在当前会话中。");
      return this.deps.actions.readPayload(active.address, { id, bytes }, { signal });
    });
  }

  private payloadRefBytes(id: string): Promise<number | undefined> {
    const state = this.active?.projection.state();
    for (const tool of state?.toolCalls ?? []) {
      if (tool.argumentsRef?.id === id) return Promise.resolve(tool.argumentsRef.bytes);
      if (tool.resultRef?.id === id) return Promise.resolve(tool.resultRef.bytes);
    }
    return Promise.resolve(undefined);
  }

  dismissError(): void {
    this.dismissedError = this.rawError();
    this.refresh();
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.selection += 1;
    this.releaseActive();
    this.handles.clear();
    this.listeners.clear();
  }

  // ------------------------------------------------------------------
  // Open flows
  // ------------------------------------------------------------------

  private async openNew(token: number): Promise<boolean> {
    const handle = await this.deps.actions.createHandle();
    if (token !== this.selection || this.disposed) {
      await this.closeCandidate(handle.id);
      return false;
    }
    let receipt: ActionReceipt;
    try {
      receipt = await this.deps.actions.execute(
        handle.id,
        ChatUserAction.createNewSession({ title: "新会话" }),
        { sessionId: null, bindingRevision: 0 },
      );
    } catch (error) {
      await this.closeCandidate(handle.id);
      throw error;
    }
    const chat: Chat = {
      id: receipt.sessionId,
      title: "新会话",
      updatedAt: this.deps.now?.().toISOString() ?? new Date().toISOString(),
    };
    const record: ChatHandleRecord = { address: handle.id, handleId: handle.id, chat };
    if (token !== this.selection || this.disposed) {
      await this.closeCandidate(handle.id);
      return false;
    }
    this.handles.set(chat.id, record);
    return await this.activate(record, token);
  }

  private async openExisting(target: ChatSummary, token: number): Promise<boolean> {
    const sessionId = target.sessionId;
    let record = this.handles.get(sessionId);
    if (!record) {
      const handle = await this.deps.actions.createHandle(sessionId);
      if (token !== this.selection || this.disposed) {
        await this.closeCandidate(handle.id);
        return false;
      }
      let bound = false;
      try {
        await this.deps.actions.execute(
          handle.id,
          ChatUserAction.changeSessionTo(sessionId),
          { sessionId: null, bindingRevision: 0 },
        );
        bound = true;
        record = { address: handle.id, handleId: handle.id, chat: toChat(target) };
      } catch (error) {
        if (error instanceof RemoteActionError && error.code === "binding_conflict") {
          // 该 Session 已有控制者：以真实 Session ID 寻址既有句柄，不重复绑定。
          await this.closeCandidate(handle.id);
          record = { address: sessionId, handleId: null, chat: toChat(target) };
        } else {
          await this.closeCandidate(handle.id);
          throw error;
        }
      }
      if (token !== this.selection || this.disposed) {
        if (bound) await this.closeCandidate(handle.id);
        return false;
      }
      this.handles.set(sessionId, record);
    }
    if (token !== this.selection || this.disposed) return false;
    return await this.activate(record, token);
  }

  private async activate(record: ChatHandleRecord, token: number): Promise<boolean> {
    const projection = new ChatProjection(record.chat);
    let active: ActiveChat | undefined;
    const sync = new ChatSyncProvider({
      url: this.deps.syncUrl(record.address),
      projection,
      ...(this.deps.webSocketFactory ? { webSocketFactory: this.deps.webSocketFactory } : {}),
      onStatus: (status, error) => {
        if (active) this.onSyncStatus(active, status, error);
      },
    });
    active = {
      ...record, projection, sync, unsubscribe: () => {}, everReady: false, pending: [],
    };
    this.active = active;
    active.unsubscribe = projection.subscribe(() => this.onProjection(active));
    // 先挂订阅再连接：首帧 SNAPSHOT 与后续 UPDATE 都不能落在订阅之前。
    sync.start();
    const ok = await this.waitInitial(active, token);
    if (token !== this.selection || this.disposed) {
      if (this.active === active) this.releaseActive();
      return false;
    }
    this.refresh();
    return ok;
  }

  private waitInitial(active: ActiveChat, token: number): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      let settled = false;
      const finish = (ok: boolean) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        active.initialResolve = undefined;
        resolve(ok);
      };
      const timer = setTimeout(() => {
        if (token !== this.selection) { finish(false); return; }
        this.connectionError = "会话同步超时，请重新连接。";
        finish(false);
      }, this.deps.initialSyncTimeoutMs ?? INITIAL_SYNC_TIMEOUT_MS);
      active.initialResolve = finish;
      // 已就绪的副本（理论上的复用）立即结算。
      if (active.projection.state().readiness !== "not-ready") finish(true);
    });
  }

  // ------------------------------------------------------------------
  // Projection / connection
  // ------------------------------------------------------------------

  private onProjection(active: ActiveChat): void {
    if (this.disposed || this.active !== active) return;
    const state = active.projection.state();
    if (state.readiness !== "not-ready") {
      active.everReady = true;
      active.initialResolve?.(true);
    }
    if (this.cancelRequested && !this.cancelPending()) this.clearCancel();
    this.settleDelivery(active, state);
    this.refresh();
  }

  private onSyncStatus(active: ActiveChat, status: SyncConnectionStatus, error?: Error): void {
    if (this.disposed || this.active !== active) return;
    if (status === "failed") {
      this.connectionError = error ? messageOf(error) : "实时同步连接失败。";
      active.initialResolve?.(false);
    } else if (status === "open") {
      this.connectionError = "";
    }
    this.refresh();
  }

  private settleDelivery(active: ActiveChat, state: ChatDisplayState): void {
    if (active.pending.length === 0) return;
    const items = state.inputQueue?.items ?? [];
    for (const pending of [...active.pending]) {
      const item = items.find((candidate) => candidate.inputId === pending.inputId);
      // 投递事实只有两种：队列结算为 delivered，或用户条目已进入时间线。
      const deliveredEntry = state.entries.some(
        (entry) => entry.role === "user" && entry.entryId === `turn:${pending.inputId}:user`,
      );
      if (item?.state === "delivered" || deliveredEntry) {
        active.pending = active.pending.filter((candidate) => candidate !== pending);
        pending.resolve();
      } else if (item?.state === "withdrawn") {
        active.pending = active.pending.filter((candidate) => candidate !== pending);
        pending.reject(new Error("输入已撤回，消息未送达。"));
      }
    }
  }

  private waitForDelivery(active: ActiveChat, inputId: string): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      active.pending.push({ inputId, resolve, reject });
      // 立即检查一次，避免投递事实早于回执登记。
      this.settleDelivery(active, active.projection.state());
    });
  }

  private bindingTarget(active: ActiveChat): ActionTargetDTO | null {
    const state = active.projection.state();
    if (!state.sessionId || state.bindingRevision === null) return null;
    try {
      return parseActionTarget({ sessionId: state.sessionId, bindingRevision: state.bindingRevision });
    } catch {
      return null;
    }
  }

  // ------------------------------------------------------------------
  // View state
  // ------------------------------------------------------------------

  private refresh(): void {
    if (this.disposed) return;
    const active = this.active;
    const state = active?.projection.state() ?? null;
    const readiness = state?.readiness ?? "not-ready";
    if (state && readiness !== "not-ready") active!.everReady = true;
    if (this.cancelRequested && !this.cancelPending()) this.clearCancel();
    // 换绑后句柄记录也跟随绑定身份，避免复用旧 sessionId 快照。
    if (active && state?.sessionId && state.sessionId !== active.chat.id) {
      const previousId = active.chat.id;
      active.chat = state.chat;
      if (this.handles.get(previousId)?.address === active.address) this.handles.delete(previousId);
      this.handles.set(active.chat.id, {
        address: active.address, handleId: active.handleId, chat: active.chat,
      });
    }
    const liveSync: SyncStatus = active?.projection.status === "synced"
      ? "synced"
      : active?.projection.status === "failed" || active?.sync.status === "failed" ? "failed" : "connecting";
    const loadState = this.loadState(active, readiness, liveSync);
    const historyReady = loadState === "ready";
    const phase = this.phase(active, state, loadState, historyReady);
    this.update({
      // 展示身份只跟随副本绑定；记录里的 Chat 只是尚未绑定时的占位。
      selected: state?.chat ?? active?.chat ?? this.view.selected,
      state,
      phase,
      historyReady,
      loadState,
      liveSync,
      error: this.visibleError(),
      recovery: this.recovery,
    });
  }

  private loadState(active: ActiveChat | undefined, readiness: ChatDisplayState["readiness"], liveSync: SyncStatus): ChatLoadState {
    if (!active) return this.view.selected ? "loading" : "idle";
    if (readiness === "ready") return "ready";
    if (readiness === "unsupported") return "unsupported";
    if (active.everReady) return this.view.loadState === "unsupported" ? "unsupported" : "ready";
    return liveSync === "failed" || this.connectionError ? "failed" : "loading";
  }

  private phase(
    active: ActiveChat | undefined,
    state: ChatDisplayState | null,
    loadState: ChatLoadState,
    historyReady: boolean,
  ): Phase {
    if (!active) return "idle";
    if (loadState === "loading") return "loading";
    if (this.uncertain || state?.executionBlocked) return "blocked";
    // 已就绪但动作需要重新同步（例如绑定代次过期）：先恢复，避免重复发送同一条 409。
    if (this.recovery === "reload" && loadState === "ready") return "blocked";
    if (state?.running) return this.cancelPending() ? "cancelling" : "streaming";
    if (!historyReady && loadState !== "failed" && loadState !== "unsupported") return "syncing";
    return "idle";
  }

  /**
   * 取消意图只在发起时的 running 窗口内有效。
   * `running=false` 或 active turn 已换代即失效，避免停止后输入被永久拦住。
   */
  private cancelPending(): boolean {
    if (!this.cancelRequested) return false;
    const state = this.active?.projection.state() ?? this.view.state;
    if (!state?.running) return false;
    if (this.cancelTurnId !== null && state.activeTurn.turnId !== null
      && state.activeTurn.turnId !== this.cancelTurnId) return false;
    return true;
  }

  private clearCancel(): void {
    this.cancelRequested = false;
    this.cancelTurnId = null;
  }

  private rawError(): string {
    if (this.actionError) return this.actionError;
    if (this.connectionError) return this.connectionError;
    if (this.view.loadState === "unsupported") return UNSUPPORTED;
    return "";
  }

  private visibleError(): string {
    const candidate = this.rawError();
    if (!candidate) {
      this.dismissedError = "";
      return "";
    }
    return candidate === this.dismissedError ? "" : candidate;
  }

  private failAction(error: unknown): void {
    const remote = error instanceof RemoteActionError
      ? error
      : new RemoteActionError("internal_error", messageOf(error), "unknown");
    if (remote.disposition === "unknown") this.uncertain = true;
    // 未知结果与过期绑定都需要重新同步；普通拒绝（例如校验失败）没有恢复动作。
    this.recovery = remote.disposition === "unknown" || remote.code === "binding_conflict"
      ? "reload"
      : "none";
    const message = remote.code === "close_incomplete"
      ? `关闭未完成：${remote.message}`
      : remote.message;
    this.actionError = remote.disposition === "unknown"
      ? `${message} 执行状态尚未确认，请重新加载会话后再继续。`
      : remote.code === "binding_conflict"
        ? `${message} 会话绑定已过期，请重新同步后再发送。`
        : message;
    // 新的错误必须重新可见，不能沿用上一次 dismiss 的结果。
    this.dismissedError = "";
    this.refresh();
  }

  private update(patch: Partial<ChatView>): void {
    const next = { ...this.view, ...patch };
    if (sameChat(next.selected, this.view.selected)
      && next.state === this.view.state
      && next.phase === this.view.phase
      && next.historyReady === this.view.historyReady
      && next.loadState === this.view.loadState
      && next.liveSync === this.view.liveSync
      && next.error === this.view.error
      && next.recovery === this.view.recovery) return;
    this.view = next;
    for (const listener of [...this.listeners]) listener();
  }

  private releaseActive(): void {
    const active = this.active;
    this.active = undefined;
    this.clearCancel();
    if (!active) return;
    for (const pending of active.pending) pending.reject(new Error("会话连接已释放。"));
    active.pending = [];
    active.unsubscribe();
    void active.sync.close().catch((error) => logError("Chat sync close failed", error, { chatId: active.chat.id }));
    void active.projection.dispose().catch((error) => logError("Chat projection dispose failed", error, { chatId: active.chat.id }));
  }

  private async closeCandidate(handleId: string): Promise<void> {
    try {
      await this.deps.actions.close(handleId);
    } catch (error) {
      // 候选句柄清理失败必须可见；不覆盖主流程错误。
      logError("Chat candidate handle close failed", error, { handleId });
    }
  }
}

function toChat(summary: ChatSummary): Chat {
  return { id: summary.sessionId, title: summary.title ?? "新会话", updatedAt: summary.updatedAt };
}
