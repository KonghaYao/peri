import { logError, publicError } from "./logging";
import type { Chat, ChatRepository, Defer, StartHost } from "./types";
import { DeadlineError, PeriWasmHostStartupError, withDeadline, type PeriWasmHostCleanupOutcome, type WasmAcpTransport } from "@peri-code/sdk/wasm";
import { ChatInteractionQueue } from "./interactions";
import { initializeTransport, WORKSPACE_CWD } from "./bootstrap";
import { ChatProjection } from "./projection";
import { defaultDefer } from "./defer";
import { CF_WASM_CLEANUP_TIMEOUT_MS } from "../../shared/runtime-limits";
import type { JsonRpcNotification } from "@peri-code/sdk/portable";
import type { SyncState } from "../../shared/sync";

export const MAX_REPLY_BYTES = 524_288;
/** 浏览器单次载荷下载上限（与 SDK 默认展示预算一致）；更大的结果需要流式下载（未实现）。 */
export const MAX_PAYLOAD_RESPONSE_BYTES = 1_048_576;
/** ACP control requests and cancellation keep their own budgets; neither proves a stop on timeout. */
export const ACP_CONTROL_TIMEOUT_MS = 20_000;
export const CANCELLATION_TIMEOUT_MS = 65_000;

const encoder = new TextEncoder();

/**
 * 本地命令（浏览器宿主的唯一执行入口；控制帧已删除，命令不再经网络编码）。
 * 失败码保持稳定字符串（与视图层约定一致）：
 * - prompt：`busy`、`blocked`（宿主关闭未确认）；
 * - cancel：`cancel_timeout`（停止未确认）、`cancel_failed`（携带公开诊断文案）；
 * - interaction：`settled`（迟到/重复/跨会话/已失效/kind 不符）、`invalid_answer`（答案结构与 SDK 契约不符）；
 * - payload：`unavailable` / `integrity_error` / `too_large`（沿用 SDK 载荷读取失败码）；
 * - 会话不存在：`not_found`；其余内部失败：`internal_error`（不回内部细节）。
 */
export type ChatCommand =
  | { op: "prompt"; content: string }
  | { op: "cancel" }
  | { op: "interaction"; interactionId: string; answer: unknown }
  | { op: "payload"; refId: string; bytes?: number };

export type ChatCommandPayload = { id: string; bytes: number; json: string };

export class ChatCommandError extends Error {
  constructor(readonly code: string, message: string) { super(message); this.name = "ChatCommandError"; }
}

interface ActiveRun {
  transport?: WasmAcpTransport;
  sessionId?: string;
  promptWritten: boolean;
  cancelRequested: boolean;
  cancelDelivery?: Promise<void>;
  cancelFailure?: unknown;
  settlement: Promise<void>;
  settle(error?: unknown): void;
  cancelExpired: Promise<never>;
  expireCancel(error: Error): void;
  cancelTimer?: ReturnType<typeof setTimeout>;
  startup: AbortController;
  startupCleanupConfirmed?: boolean;
  /** 本轮执行的 live 交互登记表；轮次结束/宿主关闭时撤销。 */
  interactions: ChatInteractionQueue;
}

function activeRun(): ActiveRun {
  let settle!: (error?: unknown) => void;
  const settlement = new Promise<void>((resolve, reject) => {
    settle = (error) => error !== undefined ? reject(error) : resolve();
  });
  let expireCancel!: (error: Error) => void;
  const cancelExpired = new Promise<never>((_, reject) => { expireCancel = reject; });
  void settlement.catch(() => {});
  void cancelExpired.catch(() => {});
  return { promptWritten: false, cancelRequested: false, settlement, settle, cancelExpired, expireCancel,
    startup: new AbortController(), interactions: new ChatInteractionQueue() };
}

/** 只有本会话根 agent 的展示事件进入投影；子代理内容由各自通道呈现。 */
function belongsToRoot(notification: JsonRpcNotification, sessionId: string): boolean {
  const params = notification.params as {
    sessionId?: unknown;
    _meta?: { "peri.sourceAgentId"?: unknown; peri?: { sourceAgentId?: unknown } };
  } | undefined;
  if (!params || params.sessionId !== sessionId) return false;
  const subagent = params._meta?.["peri.sourceAgentId"] || params._meta?.peri?.sourceAgentId;
  if (!subagent) return true;
  const update = (params as { update?: { sessionUpdate?: unknown } }).update;
  return !["agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update"]
    .includes(String(update?.sessionUpdate));
}

/** 重放内容标记：Rust 侧只在客户端声明 `peri.replay` 时打这个标（见 session_replay.rs）。 */
function isReplayChunk(notification: JsonRpcNotification): boolean {
  const params = notification.params as { update?: { sessionUpdate?: unknown; _meta?: { periReplay?: unknown } } } | undefined;
  if (notification.method !== "session/update") return false;
  if (!["user_message_chunk", "agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update"]
    .includes(String(params?.update?.sessionUpdate))) return false;
  return params?.update?._meta?.periReplay === true;
}

/**
 * 单会话运行期（浏览器宿主）：展示态只存在于内存，冷启动经 ACP `session/load` 重放复原。
 * 刷新/关闭标签页即运行期结束，在途执行丢失且不复原；持久事实只有远端会话存储。
 */
export class ChatRuntime {
  private loading?: Promise<void>;
  private loaded = false;
  private chat?: Chat;
  private active?: ActiveRun;
  private startupBlock?: ActiveRun;
  private unavailable = false;
  private readonly projection = new ChatProjection();

  constructor(private readonly chatId: string, private readonly startHost: StartHost,
    private readonly repository: Pick<ChatRepository, "get">) {}

  // —— 观测面：只读当前内存态，绝不触发复原 ——

  get running(): boolean { return this.active !== undefined; }
  get executionBlocked(): boolean { return this.unavailable; }
  /** 无活跃执行：注册表可以回收这个运行期（下一次使用会重新复原）。 */
  get recyclable(): boolean { return this.active === undefined; }

  state(): SyncState { return this.projection.state(); }
  /** 投影变化订阅（连接层据此推送最新读面）。 */
  subscribe(listener: () => void): () => void { return this.projection.subscribe(listener); }
  inFlight(): Promise<unknown> | undefined { return this.active?.settlement; }

  /** 展示复原：repository 取会话元数据 → 重放 → 投影；并发调用共享同一 promise，失败不粘住。 */
  hydrate(defer?: Defer): Promise<void> {
    if (this.loaded) return Promise.resolve();
    const pending = this.loading ??= this.restore(defer).then(
      () => { this.loaded = true; this.loading = undefined; },
      (error) => { this.loading = undefined; throw error; });
    return pending;
  }

  /** 详情读面：先复原展示材料，再以 Store 为权威复核会话元数据。 */
  async read(defer?: Defer): Promise<SyncState> {
    await this.hydrate(defer);
    const chat = await this.repository.get(this.chatId);
    if (!chat || chat.id !== this.chatId) throw new Error("Chat not found");
    this.chat = chat;
    // 元数据变更即时反映到读面（展示材料不因此重建）。
    this.refreshRunState(this.active !== undefined);
    return this.projection.state();
  }

  /** 运行期回收：释放文档（内存态，无持久副作用）。 */
  dispose(): void {
    this.projection.dispose();
  }

  /** 本地命令入口：失败以 ChatCommandError 抛出；内部失败只回稳定码，诊断留在 console。 */
  async run(command: ChatCommand, defer?: Defer): Promise<ChatCommandPayload | undefined> {
    try {
      return await this.dispatch(command, defer);
    } catch (error) {
      if (error instanceof ChatCommandError) throw error;
      logError("Peri chat command failed", error, { sessionId: this.chatId, op: command.op });
      throw new ChatCommandError("internal_error", "命令执行失败；诊断已记录");
    }
  }

  private async dispatch(command: ChatCommand, defer?: Defer): Promise<ChatCommandPayload | undefined> {
    await this.hydrate(defer);
    if (command.op === "prompt") { await this.prompt(command.content, defer); return undefined; }
    if (command.op === "cancel") { await this.cancelActive(); return undefined; }
    if (command.op === "interaction") { await this.answerInteraction(command.interactionId, command.answer); return undefined; }
    return this.readPayload(command.refId, command.bytes);
  }

  /** 重放复原：启动宿主 → 先订阅 → session/load → 通知喂投影 → seedConfig → 关宿主并等清理证据。 */
  private async restore(defer?: Defer): Promise<void> {
    const chat = await this.repository.get(this.chatId);
    if (!chat || chat.id !== this.chatId) throw new Error("Chat not found");
    this.chat = chat;
    // 上一次失败尝试可能已吃到部分重放：丢弃后再复原，避免重复内容叠加。
    this.projection.reset();
    this.refreshRunState(false);
    const run = activeRun();
    let transport: WasmAcpTransport | undefined;
    let unsubscribe: (() => void) | undefined;
    try {
      transport = run.transport = await this.launchHost(run, defer);
      // 冷加载不承载 prompt，反向请求保持安全默认（与创建路径一致）。
      transport.setRequestHandler((method) => {
        if (method === "session/request_permission") return { outcome: { outcome: "cancelled" } };
        throw new Error("ACP client capability is not authorized");
      });
      await initializeTransport(transport);
      run.sessionId = chat.id;
      // 关键顺序：先订阅再 session/load —— ACP 在响应之前把持久历史以 session/update 重放
      // （peri-acp/src/host/requests/session_restore.rs），订阅晚于 load 会丢弃全部重放。
      let replayPending = false;
      unsubscribe = transport.subscribe((notification) => {
        if (!belongsToRoot(notification, chat.id)) return;
        if (isReplayChunk(notification)) replayPending = true;
        this.projection.accept(notification);
      });
      const loaded = await withDeadline(transport.request("session/load", {
        cwd: WORKSPACE_CWD, mcpServers: [], sessionId: chat.id,
      }), 20_000, "ACP session/load");
      if (!loaded || typeof loaded !== "object") throw new Error("Invalid ACP session/load response");
      this.projection.seedConfig(loaded);
      // ACP 在 load 响应前完成历史重放，且没有独立的回放 turn-done 通知。
      if (replayPending) this.projection.closeReplayedTurn();
      await this.projection.flush();
    } catch (error) {
      logError("Peri chat replay restore failed", error, { sessionId: this.chatId, generationId: transport?.generationId });
      throw error;
    } finally {
      unsubscribe?.();
      this.projection.setTaskSnapshotRequester(null);
      if (transport) {
        try { await withDeadline(transport.close(), 20_000, "ACP host close after replay"); }
        catch (error) {
          logError("Peri chat replay host close failed", error, { sessionId: this.chatId, generationId: transport.generationId });
          // 展示态已复原，但宿主关闭未确认：本实例不得再执行（与执行路径同一阻断语义）。
          this.blockExecution();
        }
      }
    }
  }

  private async prompt(content: string, defer?: Defer): Promise<void> {
    if (this.active) throw new ChatCommandError("busy", "Chat is busy");
    if (this.unavailable)
      throw new ChatCommandError("blocked", "Previous ACP host did not close; this instance cannot run again");
    const run = activeRun();
    this.active = run;
    try {
      // 先发布运行元数据再投影用户输入：否则订阅者会先看到条目而 running 仍为 false（误判轮次结束）。
      this.refreshRunState(true);
      await this.projection.start(content);
      (defer ?? defaultDefer)(this.execute(run, content, defer));
    } catch (error) {
      this.active = undefined;
      run.interactions.close();
      clearTimeout(run.cancelTimer);
      run.settle(new Error(publicError(error)));
      if (this.chat) {
        this.refreshRunState(false);
        await this.projection.complete("error", publicError(error));
      }
      throw error;
    }
  }

  private async cancelActive(): Promise<void> {
    const run = this.active;
    if (!run && this.unavailable) throw new ChatCommandError("blocked", "ACP host stop is not confirmed");
    if (!run) return;
    await this.cancel(run);
    try { await withDeadline(run.settlement, 45_000, "Cancel settlement"); }
    catch (error) {
      if (error instanceof DeadlineError)
        throw new ChatCommandError("cancel_timeout", "ACP cancellation timed out; execution stop is not confirmed");
      throw new ChatCommandError("cancel_failed", publicError(error));
    }
  }

  /** 交互回答：直接结算本标签页宿主上的 live reverse request；不写 Y.Doc。 */
  private async answerInteraction(interactionId: string, body: unknown): Promise<void> {
    const result = this.active?.interactions.answer(this.chatId, interactionId, body) ?? "unknown";
    if (result === "accepted") return;
    if (result === "invalid") throw new ChatCommandError("invalid_answer", "Invalid interaction response");
    // 迟到/重复/跨会话/已失效与 kind 不符统一按「不可再回答」回给前端：刷新后重试是唯一可行动作。
    if (result === "kind_mismatch") throw new ChatCommandError("settled", "Interaction kind does not match");
    throw new ChatCommandError("settled", "Interaction is not pending");
  }

  /** 载荷全文读取：失败码沿用 SDK 契约，未知引用不泄露其它会话存在性。 */
  private readPayload(refId: string, expectedBytes?: number): ChatCommandPayload {
    const json = this.projection.docs.readPayload(refId);
    if (json === undefined)
      throw new ChatCommandError("unavailable", "载荷引用不存在或已被回收");
    const bytes = encoder.encode(json).byteLength;
    if (expectedBytes !== undefined && expectedBytes !== bytes)
      throw new ChatCommandError("integrity_error", "载荷字节数与引用不符");
    if (bytes > MAX_PAYLOAD_RESPONSE_BYTES)
      throw new ChatCommandError("too_large",
        `载荷 ${bytes} 字节超过单次下载上限 ${MAX_PAYLOAD_RESPONSE_BYTES}，需要流式下载（未实现）`);
    return { id: refId, bytes, json };
  }

  private async cancel(run: ActiveRun): Promise<void> {
    if (this.active !== run) return;
    run.cancelRequested = true;
    if (!run.transport) run.startup.abort(new Error("ACP host startup cancelled by explicit Stop"));
    run.cancelTimer ??= setTimeout(() => {
      run.expireCancel(new DeadlineError("ACP cancellation timed out; execution stop is not confirmed"));
    }, CANCELLATION_TIMEOUT_MS);
    if (!run.transport || !run.sessionId || !run.promptWritten) return;
    // 当前 ACP 的取消是有目标的会话级通知；停止是否成立仍以 prompt 结算与宿主关闭证据为准。
    run.cancelDelivery ??= withDeadline(run.transport.notify("session/cancel", { sessionId: run.sessionId }),
      ACP_CONTROL_TIMEOUT_MS, "ACP session/cancel").catch((error) => {
      run.cancelFailure = error;
      throw error;
    });
    await run.cancelDelivery;
  }

  private async execute(run: ActiveRun, content: string, defer?: Defer): Promise<void> {
    let unsubscribe: (() => void) | undefined;
    let failure: unknown;
    let protocolFailure: Error | undefined;
    let replyBytes = 0;
    let status: "completed" | "cancelled" | "error" = "completed";
    try {
      run.transport = await this.launchHost(run, defer);
      const transport = run.transport;
      // 反向交互交由 SDK 投影处理：回答经本地命令回灌；未接线的方法保持默认拒绝。
      transport.setRequestHandler((method, params) => {
        if (method !== "session/request_permission" && method !== "elicitation/create") {
          throw new Error("ACP client capability is not authorized");
        }
        return this.projection.handleRequest(method, params, run.sessionId, {
          onPermissionRequest: (request) => run.interactions.requestPermission(request),
          onElicitation: (request) => run.interactions.requestElicitation(request),
        });
      });
      await initializeTransport(transport);
      run.sessionId = this.chat!.id;
      const loaded = await withDeadline(transport.request("session/load", {
        cwd: WORKSPACE_CWD, mcpServers: [], sessionId: run.sessionId,
      }), 20_000, "ACP session/load");
      if (!loaded || typeof loaded !== "object") throw new Error("Invalid ACP session/load response");
      this.projection.seedConfig(loaded);
      this.projection.setTaskSnapshotRequester(() => transport.request("session/bg-tasks", { sessionId: run.sessionId! }));
      if (run.cancelRequested) {
        status = "cancelled";
      } else {
        unsubscribe = transport.subscribe((notification) => {
          if (protocolFailure || !belongsToRoot(notification, run.sessionId!)) return;
          const params = notification.params as { update?: { sessionUpdate?: unknown; content?: { type?: unknown; text?: unknown } } };
          if (notification.method !== "session/update" || params.update?.sessionUpdate !== "agent_message_chunk"
            || params.update.content?.type !== "text") {
            this.projection.accept(notification);
            return;
          }
          const chunk = params.update.content;
          if (typeof chunk.text !== "string") {
            protocolFailure = new Error("Invalid ACP text chunk");
            void this.cancel(run).catch((error) => logError("Peri invalid-chunk stop failed", error));
            return;
          }
          const chunkBytes = encoder.encode(JSON.stringify(chunk.text)).byteLength - 2;
          if (replyBytes + chunkBytes > MAX_REPLY_BYTES) {
            protocolFailure = new Error("Assistant reply exceeds the 512 KiB encoded output limit");
            void this.cancel(run).catch((error) => logError("Peri output-limit stop failed", error));
            return;
          }
          replyBytes += chunkBytes;
          this.projection.accept(notification);
        });
        const sent = await withDeadline(transport.sendRequest<{ stopReason: string }>("session/prompt", {
          sessionId: run.sessionId, prompt: [{ type: "text", text: content }],
        }), 5_000, "ACP prompt delivery");
        run.promptWritten = true;
        if (run.cancelRequested) await this.cancel(run);
        const result = await withDeadline(Promise.race([sent.response, run.cancelExpired]), 300_000, "ACP prompt");
        if (run.cancelDelivery) await run.cancelDelivery;
        if (protocolFailure) throw protocolFailure;
        if (run.cancelFailure) throw run.cancelFailure;
        if (result?.stopReason === "cancelled") status = "cancelled";
        else if (result?.stopReason === "end_turn") status = "completed";
        else throw new Error(`ACP turn did not complete (${String(result?.stopReason)})`);
      }
    } catch (error) {
      logError("Peri chat execution failed", error, { sessionId: this.chat?.id, generationId: run.transport?.generationId });
      if (run.cancelRequested && !run.transport && run.startupCleanupConfirmed) status = "cancelled";
      else failure = error ?? new Error("Execution rejected without an error value", { cause: error });
      if (run.transport && run.promptWritten) {
        try { await this.cancel(run); }
        catch (stopError) { logError("Peri failed execution stop failed", stopError); failure = new AggregateError([error, stopError], "Execution and stop failed"); }
      }
    } finally {
      unsubscribe?.();
      this.projection.setTaskSnapshotRequester(null);
      if (run.transport) {
        try {
          await withDeadline(run.transport.close(), 20_000, "ACP host close");
        } catch (error) {
          logError("Peri chat shutdown failed", error, { sessionId: run.sessionId, generationId: run.transport.generationId });
          this.blockExecution();
          const closeError = error ?? new Error("Host close rejected without an error value", { cause: error });
          failure = failure === undefined ? closeError : new AggregateError([failure, closeError], "Execution failed and host shutdown is unconfirmed");
        }
      }
      try {
        // 先让投影结算：abortAll 把未决交互置 expired 并中止信号，再兜底撤销 live 队列，
        // 这样宿主关闭导致的挂起不会被记成用户拒绝。
        await this.projection.complete(failure !== undefined ? "error" : status, failure !== undefined ? publicError(failure) : undefined);
        run.interactions.close();
      } catch (error) {
        logError("Peri chat projection settlement failed", error);
        failure ??= error ?? new Error("Projection settlement rejected without an error value", { cause: error });
      }
      try { await this.projection.flush(); }
      catch (flushError) {
        logError("Peri terminal chat projection failed", flushError);
        failure = new AggregateError([...(failure !== undefined ? [failure] : []), flushError], "Chat terminal projection failed");
      }
      this.active = undefined;
      this.refreshRunState(false);
      clearTimeout(run.cancelTimer);
      try {
        // 终态元数据（running=false）必须在结算前推出：否则订阅者要等下一次状态发布才看到运行结束。
        await this.projection.flush();
      } catch (terminalFlushError) {
        logError("Peri terminal run-state flush failed", terminalFlushError);
      }
      run.settle(failure);
    }
  }

  /** 启动一次浏览器宿主；失败时保留清理证据观察与阻断语义。 */
  private async launchHost(run: ActiveRun, defer?: Defer): Promise<WasmAcpTransport> {
    const starting = Promise.resolve().then(() => this.startHost({ signal: run.startup.signal }));
    try { return await withDeadline(starting, 20_000, "ACP host startup"); }
    catch (error) {
      run.startup.abort(error);
      this.startupBlock = run;
      this.blockExecution();
      const eventualCleanup: Promise<PeriWasmHostCleanupOutcome> = error instanceof PeriWasmHostStartupError ? error.eventualCleanup :
        starting.then(async (lateTransport) => {
          await lateTransport.close();
          return { confirmed: true };
        }, (startupError) => startupError instanceof PeriWasmHostStartupError ? startupError.eventualCleanup :
          { confirmed: false, error: startupError });
      const cleanup = error instanceof PeriWasmHostStartupError ? error.cleanup : eventualCleanup;
      try {
        const outcome = await withDeadline(cleanup, CF_WASM_CLEANUP_TIMEOUT_MS + 100, "ACP startup cleanup observation");
        if (outcome.confirmed) this.confirmStartupCleanup(run);
        else {
          logError("Peri startup cleanup unconfirmed", outcome.error ?? error);
          this.observeLateStartupCleanup(run, eventualCleanup, defer);
        }
      } catch (cleanupError) {
        logError("Peri startup cleanup observation failed", cleanupError);
        this.observeLateStartupCleanup(run, eventualCleanup, defer);
      }
      throw error;
    }
  }

  private observeLateStartupCleanup(run: ActiveRun, cleanup: Promise<PeriWasmHostCleanupOutcome>, defer?: Defer): void {
    const observation = withDeadline(cleanup, CF_WASM_CLEANUP_TIMEOUT_MS, "Late ACP startup cleanup observation")
      .then((outcome) => {
        if (!outcome.confirmed) { logError("Peri late startup cleanup unconfirmed", outcome.error); return; }
        this.confirmStartupCleanup(run);
      }).catch((error) => logError("Peri late startup cleanup observation failed", error));
    (defer ?? defaultDefer)(observation);
  }

  private confirmStartupCleanup(run: ActiveRun): void {
    if (this.startupBlock !== run || run.transport) return;
    run.startupCleanupConfirmed = true;
    this.startupBlock = undefined;
    this.unavailable = false;
    this.refreshRunState(this.active !== undefined);
  }

  /** 宿主关闭未确认：本实例不得再启动新执行，直到有确证的清理证据。 */
  private blockExecution(): void {
    this.unavailable = true;
    this.refreshRunState(this.active !== undefined);
  }

  private refreshRunState(running: boolean): void {
    if (!this.chat) return;
    this.projection.setRunState({ chat: this.chat, running, executionBlocked: this.unavailable });
  }
}
