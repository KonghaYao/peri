import {
  ChatCommandError, ChatNotFoundError, ChatInteractionQueue, ChatProjection, defaultDefer, publicError,
  type AgentEnvironment, type Chat, type ChatCommand, type ChatCommandPayload,
  type ChatRuntimePort, type ChatRuntimeSubscription, type ChatSessionStore, type Defer,
  type RuntimeSummary, type AcquireHost, type SyncState,
} from "../../core/agent";
import { DeadlineError, PeriWasmHostStartupError, withDeadline, type PeriWasmHostCleanupOutcome, type PeriWasmHostLease, type WasmAcpTransport } from "@peri-code/sdk/wasm";
import { CF_WASM_CLEANUP_TIMEOUT_MS } from "../../core/shared";
import type { JsonRpcNotification } from "@peri-code/sdk/portable";

export const MAX_REPLY_BYTES = 524_288;
/** 单次载荷下载上限（与 SDK 默认展示预算一致）；更大的结果需要流式下载（未实现）。 */
export const MAX_PAYLOAD_RESPONSE_BYTES = 1_048_576;
/** ACP control requests and cancellation keep their own budgets; neither proves a stop on timeout. */
export const ACP_CONTROL_TIMEOUT_MS = 20_000;
export const CANCELLATION_TIMEOUT_MS = 65_000;

/** 释放实例时中止进行中的宿主启动；错误只作为 AbortSignal 的原因。 */
const DISPOSE_STARTUP_REASON = "Peri instance disposed";
/** 释放实例时宿主关闭失败：记录后继续释放其余宿主。 */
const DISPOSE_CLOSE_FAILED_LABEL = "Peri instance host close failed on dispose";

const encoder = new TextEncoder();

/** 一次执行/复原的宿主与取消状态；`settle` 负责释放等待者。 */
interface ActiveRun {
  /** 已启动的宿主；启动失败时为空。 */
  transport?: WasmAcpTransport;
  /** 宿主所有权租约；关闭证据与状态由它维护。 */
  lease?: PeriWasmHostLease;
  /** `session/load` 后的 ACP 会话 ID。 */
  sessionId?: string;
  /** `session/prompt` 是否已写出；决定失败后是否需要 cancel。 */
  promptWritten: boolean;
  /** 用户是否已请求停止。 */
  cancelRequested: boolean;
  /** 已发出的 cancel 请求；终态前等待它完成。 */
  cancelDelivery?: Promise<void>;
  /** cancel 失败原因；终态时优先抛出。 */
  cancelFailure?: unknown;
  /** 执行结算 promise；等待者据此判断本轮结束。 */
  settlement: Promise<void>;
  /** 结算本轮；`error` 非空时拒绝等待者。 */
  settle(error?: unknown): void;
  /** 取消超时 promise；超时后拒绝。 */
  cancelExpired: Promise<never>;
  /** 触发取消超时。 */
  expireCancel(error: Error): void;
  /** 取消超时定时器。 */
  cancelTimer?: ReturnType<typeof setTimeout>;
  /** 宿主启动中止信号。 */
  startup: AbortController;
  /** 启动清理是否已确认。 */
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
 * 单会话实例：展示态只在内存，冷启动经 ACP `session/load` 重放复原；持久事实只有远端存储。
 * 每个实例注入独立 `AgentEnvironment`；宿主启动、MCP 与 ACP 初始化由该环境提供。
 */
export class PeriInstance implements ChatRuntimePort {
  /** 冷复原进行中的 promise；并发 `hydrate` 共享。 */
  private loading?: Promise<void>;
  /** 是否已完成一次成功复原。 */
  private loaded = false;
  /** 当前会话元数据；`read`/`restore` 时刷新。 */
  private chat?: Chat;
  /** 当前执行；同一时刻最多一个。 */
  private active?: ActiveRun;
  /** 冷复原中的运行期。 */
  private restoring?: ActiveRun;
  /** 启动失败但清理未确认的运行期；用于阻断后续执行。 */
  private startupBlock?: ActiveRun;
  /** 宿主关闭未确认时置位；此后本实例拒绝执行。 */
  private unavailable = false;
  /** 是否已释放；释放后不再接受新工作。 */
  private disposed = false;
  /** 已建立的投影订阅数；非零时 `ChatService` 不得回收本实例。 */
  private watchers = 0;
  /** 内存投影：展示态的唯一副本，不持久化。 */
  private readonly projection = new ChatProjection();

  /** 绑定会话身份、只读存储、独立环境与宿主租约获取器。 */
  constructor(private readonly chatId: string,
    private readonly store: Pick<ChatSessionStore, "get">,
    private readonly environment: AgentEnvironment,
    private readonly acquireHost: AcquireHost,
    private readonly defer: Defer = defaultDefer) {}

  // 观测面：只读当前内存态，绝不触发复原。

  /** 是否有执行在途。 */
  get running(): boolean { return this.active !== undefined; }
  /** 宿主停止未确认，后续执行会被拒绝。 */
  get executionBlocked(): boolean { return this.unavailable; }
  /** 无活跃执行且无投影订阅：`ChatService` 可以回收这个实例（下一次使用会重新复原）。 */
  get recyclable(): boolean { return this.active === undefined && this.watchers === 0; }

  /** 清单摘要：只读当前内存态，不触发 hydrate，也不物化完整展示状态。 */
  summary(): RuntimeSummary {
    const projection = this.projection.summary();
    return {
      running: this.active !== undefined,
      pendingInteractionCount: projection.pendingInteractionCount,
      lastTurnStatus: projection.lastTurnStatus,
      lastCompletedAt: projection.lastCompletedAt,
    };
  }

  /** 当前投影快照；只读内存态。 */
  state(): SyncState { return this.projection.state(); }
  /** 投影变化订阅：`watch` 据此在初始快照之后推送最新读面。 */
  subscribe(listener: () => void): () => void { return this.projection.subscribe(listener); }
  /** 在途执行的结算 promise；空闲时为 undefined。 */
  inFlight(): Promise<unknown> | undefined { return this.active?.settlement; }

  /** 展示复原：store 取会话元数据 → 重放 → 投影；并发调用共享同一 promise，失败不粘住。 */
  private hydrate(): Promise<void> {
    if (this.loaded) return Promise.resolve();
    const pending = this.loading ??= this.restore().then(
      () => { this.loaded = true; this.loading = undefined; },
      (error) => { this.loading = undefined; throw error; });
    return pending;
  }

  /**
   * 订阅 + 初始读面：先订阅投影，再复原并读取，保证订阅之后的变化不丢。
   * resolve 前的投影变化不回调；调用方拿到 `initial` 即可开始消费。
   */
  async watch(onState: (state: SyncState) => void): Promise<ChatRuntimeSubscription> {
    let ready = false;
    const unsubscribe = this.subscribe(() => {
      if (ready) onState(this.state());
    });
    this.watchers += 1;
    let closed = false;
    const close = (): void => {
      if (closed) return;
      closed = true;
      this.watchers -= 1;
      unsubscribe();
    };
    try {
      const initial = await this.read();
      ready = true;
      return { initial, close };
    } catch (error) {
      close();
      throw error;
    }
  }

  /** 详情读面：先复原展示材料，再以 Store 为权威复核会话元数据。 */
  async read(): Promise<SyncState> {
    await this.hydrate();
    const chat = await this.store.get(this.chatId);
    if (!chat || chat.id !== this.chatId) throw new ChatNotFoundError(this.chatId);
    this.chat = chat;
    // 元数据变更即时反映到读面（展示材料不因此重建）。
    this.refreshRunState(this.active !== undefined);
    return this.projection.state();
  }

  /** 实例回收：中止启动中的宿主、关闭已启动宿主并释放投影；重复调用无副作用。 */
  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    const runs = [this.active, this.restoring].filter((run): run is ActiveRun => run !== undefined);
    this.active = undefined;
    this.restoring = undefined;
    for (const run of runs) run.startup.abort(new Error(DISPOSE_STARTUP_REASON));
    const leases = runs.flatMap((run) => run.lease ? [run.lease] : []);
    const reports = await Promise.all(leases.map((lease) => lease.close()));
    for (const report of reports) {
      if (!report.confirmed)
        this.environment.logError(DISPOSE_CLOSE_FAILED_LABEL, report.error, { sessionId: this.chatId, generationId: report.generationId });
    }
    this.projection.dispose();
  }

  /** 宿主命令入口：失败以 ChatCommandError 抛出；内部失败只回稳定码与 unknown 处置，诊断留在 console。 */
  async command(command: ChatCommand): Promise<ChatCommandPayload | undefined> {
    try {
      return await this.dispatch(command);
    } catch (error) {
      if (error instanceof ChatCommandError) throw error;
      if (error instanceof ChatNotFoundError)
        throw new ChatCommandError("not_found", error.message, "rejected");
      this.environment.logError("Peri chat command failed", error, { sessionId: this.chatId, op: command.op });
      throw new ChatCommandError("internal_error", "命令执行失败；诊断已记录", "unknown");
    }
  }

  /** 按命令类型分派；prompt 进入本轮执行，其余命令直接作用于当前运行期。 */
  private async dispatch(command: ChatCommand): Promise<ChatCommandPayload | undefined> {
    await this.hydrate();
    if (command.op === "prompt") { await this.prompt(command.content); return undefined; }
    if (command.op === "cancel") { await this.cancelActive(); return undefined; }
    if (command.op === "interaction") { await this.answerInteraction(command.interactionId, command.answer); return undefined; }
    return this.readPayload(command.refId, command.bytes);
  }

  /** 重放复原：启动宿主 → 先订阅 → session/load → 通知喂投影 → seedConfig → 关宿主并等清理证据。 */
  private async restore(): Promise<void> {
    const chat = await this.store.get(this.chatId);
    if (!chat || chat.id !== this.chatId) throw new ChatNotFoundError(this.chatId);
    this.chat = chat;
    // 上一次失败尝试可能已吃到部分重放：丢弃后再复原，避免重复内容叠加。
    this.projection.reset();
    this.refreshRunState(false);
    const run = activeRun();
    this.restoring = run;
    const mcp = this.environment.openMcp("load", chat.id);
    let transport: WasmAcpTransport | undefined;
    let unsubscribe: (() => void) | undefined;
    try {
      const lease = run.lease = await this.launchHost(run);
      transport = run.transport = lease.transport;
      // 冷加载不承载 prompt，反向请求保持安全默认（与创建路径一致）。
      transport.setRequestHandler((method, params) => {
        if (method === "mcp/message") return mcp.handleMessage(params);
        if (method === "session/request_permission") return { outcome: { outcome: "cancelled" } };
        throw new Error("ACP client capability is not authorized");
      });
      await this.environment.initialize(transport);
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
        cwd: this.environment.cwd, mcpServers: mcp.servers, sessionId: chat.id,
      }), 20_000, "ACP session/load");
      if (!loaded || typeof loaded !== "object") throw new Error("Invalid ACP session/load response");
      this.projection.seedConfig(loaded);
      // ACP 在 load 响应前完成历史重放，且没有独立的回放 turn-done 通知。
      if (replayPending) this.projection.closeReplayedTurn();
      if (mcp.ready) {
        // 自动恢复路径也要等 MCP 目录结算；否则宿主可能在 discovery 完成前关闭。
        try { await mcp.ready({ settle: false }); }
        catch (error) { this.environment.logError("Peri MCP readiness wait failed", error, { sessionId: chat.id }); }
      }
      await this.projection.flush();
    } catch (error) {
      this.environment.logError("Peri chat replay restore failed", error, { sessionId: this.chatId, generationId: transport?.generationId });
      throw error;
    } finally {
      if (this.restoring === run) this.restoring = undefined;
      unsubscribe?.();
      this.projection.setTaskSnapshotRequester(null);
      try { await mcp.close(); }
      catch (error) { this.environment.logError("Peri MCP bridge close failed", error, { sessionId: this.chatId }); }
      if (run.lease) {
        const report = await run.lease.close();
        if (!report.confirmed) {
          this.environment.logError("Peri chat replay host close failed", report.error, { sessionId: this.chatId, generationId: report.generationId });
          // 展示态已复原，但宿主关闭未确认：本实例不得再执行（与执行路径同一阻断语义）。
          this.blockExecution();
        }
      }
    }
  }

  /** 启动一轮 prompt：先发布 running，再投影用户输入，最后异步执行。 */
  private async prompt(content: string): Promise<void> {
    if (this.active) throw new ChatCommandError("busy", "Chat is busy", "rejected");
    if (this.unavailable)
      throw new ChatCommandError("blocked", "Previous ACP host did not close; this instance cannot run again", "rejected");
    const run = activeRun();
    this.active = run;
    try {
      // 先发布运行元数据再投影用户输入：否则订阅者会先看到条目而 running 仍为 false（误判轮次结束）。
      this.refreshRunState(true);
      await this.projection.start(content);
      this.defer(this.execute(run, content));
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

  /** 显式停止：先发 cancel，再等待本轮结算；超时/失败保持 unknown。 */
  private async cancelActive(): Promise<void> {
    const run = this.active;
    if (!run && this.unavailable) throw new ChatCommandError("blocked", "ACP host stop is not confirmed", "rejected");
    if (!run) return;
    await this.cancel(run);
    try { await withDeadline(run.settlement, 45_000, "Cancel settlement"); }
    catch (error) {
      if (error instanceof DeadlineError)
        throw new ChatCommandError("cancel_timeout", "ACP cancellation timed out; execution stop is not confirmed", "unknown");
      throw new ChatCommandError("cancel_failed", publicError(error), "unknown");
    }
  }

  /** 交互回答：直接结算本实例宿主上的 live reverse request；不写 Y.Doc。 */
  private async answerInteraction(interactionId: string, body: unknown): Promise<void> {
    const result = this.active?.interactions.answer(this.chatId, interactionId, body) ?? "unknown";
    if (result === "accepted") return;
    if (result === "invalid") throw new ChatCommandError("invalid_answer", "Invalid interaction response", "rejected");
    // 迟到/重复/跨会话/已失效与 kind 不符统一按「不可再回答」回给前端：刷新后重试是唯一可行动作。
    if (result === "kind_mismatch") throw new ChatCommandError("settled", "Interaction kind does not match", "rejected");
    throw new ChatCommandError("settled", "Interaction is not pending", "rejected");
  }

  /** 载荷全文读取：失败码沿用 SDK 契约，未知引用不泄露其它会话存在性。 */
  private readPayload(refId: string, expectedBytes?: number): ChatCommandPayload {
    const json = this.projection.docs.readPayload(refId);
    if (json === undefined)
      throw new ChatCommandError("unavailable", "载荷引用不存在或已被回收", "rejected");
    const bytes = encoder.encode(json).byteLength;
    if (expectedBytes !== undefined && expectedBytes !== bytes)
      throw new ChatCommandError("integrity_error", "载荷字节数与引用不符", "rejected");
    if (bytes > MAX_PAYLOAD_RESPONSE_BYTES)
      throw new ChatCommandError("too_large",
        `载荷 ${bytes} 字节超过单次下载上限 ${MAX_PAYLOAD_RESPONSE_BYTES}，需要流式下载（未实现）`, "rejected");
    return { id: refId, bytes, json };
  }

  /** 发送 ACP cancel 并等待送达；启动中的宿主直接中止启动。 */
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

  /** 执行一轮：启动宿主 → load → 订阅 → prompt → 结算投影；失败按 unknown 处理。 */
  private async execute(run: ActiveRun, content: string): Promise<void> {
    let unsubscribe: (() => void) | undefined;
    const chatId = this.chat?.id ?? run.sessionId ?? "";
    const mcp = this.environment.openMcp("prompt", chatId);
    let failure: unknown;
    let protocolFailure: Error | undefined;
    let replyBytes = 0;
    let status: "completed" | "cancelled" | "error" = "completed";
    try {
      const lease = run.lease = await this.launchHost(run);
      const transport = run.transport = lease.transport;
      // 反向交互交由 SDK 投影处理：回答经本地命令回灌；未接线的方法保持默认拒绝。
      transport.setRequestHandler((method, params) => {
        if (method === "mcp/message") return mcp.handleMessage(params);
        if (method !== "session/request_permission" && method !== "elicitation/create") {
          throw new Error("ACP client capability is not authorized");
        }
        return this.projection.handleRequest(method, params, run.sessionId, {
          onPermissionRequest: (request) => run.interactions.requestPermission(request),
          onElicitation: (request) => run.interactions.requestElicitation(request),
        });
      });
      await this.environment.initialize(transport);
      run.sessionId = chatId;
      const loaded = await withDeadline(transport.request("session/load", {
        cwd: this.environment.cwd, mcpServers: mcp.servers, sessionId: chatId,
      }), 20_000, "ACP session/load");
      if (!loaded || typeof loaded !== "object") throw new Error("Invalid ACP session/load response");
      this.projection.seedConfig(loaded);
      this.projection.setTaskSnapshotRequester(() => transport.request("session/bg-tasks", { sessionId: run.sessionId! }));
      if (mcp.ready) {
        // ACP server 在 setup 响应后异步发现；先等工具目录结算，避免模型在空目录上
        // 得出「没有 MCP 工具」的结论。超时只告警，不阻断本轮。
        try { await mcp.ready({ settle: true }); }
        catch (error) { this.environment.logError("Peri MCP readiness wait failed", error, { sessionId: chatId }); }
      }
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
            void this.cancel(run).catch((error) => this.environment.logError("Peri invalid-chunk stop failed", error));
            return;
          }
          const chunkBytes = encoder.encode(JSON.stringify(chunk.text)).byteLength - 2;
          if (replyBytes + chunkBytes > MAX_REPLY_BYTES) {
            protocolFailure = new Error("Assistant reply exceeds the 512 KiB encoded output limit");
            void this.cancel(run).catch((error) => this.environment.logError("Peri output-limit stop failed", error));
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
      this.environment.logError("Peri chat execution failed", error, { sessionId: this.chat?.id, generationId: run.transport?.generationId });
      if (run.cancelRequested && !run.transport && run.startupCleanupConfirmed) status = "cancelled";
      else failure = error ?? new Error("Execution rejected without an error value", { cause: error });
      if (run.transport && run.promptWritten) {
        try { await this.cancel(run); }
        catch (stopError) { this.environment.logError("Peri failed execution stop failed", stopError); failure = new AggregateError([error, stopError], "Execution and stop failed"); }
      }
    } finally {
      unsubscribe?.();
      this.projection.setTaskSnapshotRequester(null);
      try { await mcp.close(); }
      catch (error) { this.environment.logError("Peri MCP bridge close failed", error, { sessionId: run.sessionId }); }
      if (run.lease) {
        const report = await run.lease.close();
        if (!report.confirmed) {
          this.environment.logError("Peri chat shutdown failed", report.error, { sessionId: run.sessionId, generationId: report.generationId });
          this.blockExecution();
          const closeError = report.error ?? new Error("Host close rejected without an error value");
          failure = failure === undefined ? closeError : new AggregateError([failure, closeError], "Execution failed and host shutdown is unconfirmed");
        }
      }
      try {
        // 先让投影结算：abortAll 把未决交互置 expired 并中止信号，再兜底撤销 live 队列，
        // 这样宿主关闭导致的挂起不会被记成用户拒绝。
        await this.projection.complete(failure !== undefined ? "error" : status, failure !== undefined ? publicError(failure) : undefined);
        run.interactions.close();
      } catch (error) {
        this.environment.logError("Peri chat projection settlement failed", error);
        failure ??= error ?? new Error("Projection settlement rejected without an error value", { cause: error });
      }
      try { await this.projection.flush(); }
      catch (flushError) {
        this.environment.logError("Peri terminal chat projection failed", flushError);
        failure = new AggregateError([...(failure !== undefined ? [failure] : []), flushError], "Chat terminal projection failed");
      }
      this.active = undefined;
      this.refreshRunState(false);
      clearTimeout(run.cancelTimer);
      try {
        // 终态元数据（running=false）必须在结算前推出：否则订阅者要等下一次状态发布才看到运行结束。
        await this.projection.flush();
      } catch (terminalFlushError) {
        this.environment.logError("Peri terminal run-state flush failed", terminalFlushError);
      }
      run.settle(failure);
    }
  }

  /** 启动一次宿主；失败时保留清理证据观察与阻断语义。 */
  private async launchHost(run: ActiveRun): Promise<PeriWasmHostLease> {
    const starting = Promise.resolve().then(() => this.acquireHost({ signal: run.startup.signal }));
    try { return await starting; }
    catch (error) {
      run.startup.abort(error);
      this.startupBlock = run;
      this.blockExecution();
      const eventualCleanup: Promise<PeriWasmHostCleanupOutcome> = error instanceof PeriWasmHostStartupError ? error.eventualCleanup :
        starting.then((lateLease) => lateLease.close(), (startupError) => startupError instanceof PeriWasmHostStartupError ? startupError.eventualCleanup :
          { confirmed: false, error: startupError });
      const cleanup = error instanceof PeriWasmHostStartupError ? error.cleanup : eventualCleanup;
      try {
        const outcome = await withDeadline(cleanup, CF_WASM_CLEANUP_TIMEOUT_MS + 100, "ACP startup cleanup observation");
        if (outcome.confirmed) this.confirmStartupCleanup(run);
        else {
          this.environment.logError("Peri startup cleanup unconfirmed", outcome.error ?? error);
          this.observeLateStartupCleanup(run, eventualCleanup);
        }
      } catch (cleanupError) {
        this.environment.logError("Peri startup cleanup observation failed", cleanupError);
        this.observeLateStartupCleanup(run, eventualCleanup);
      }
      throw error;
    }
  }

  /** 启动清理未确认时继续观察；确认后解除阻断，否则保留。 */
  private observeLateStartupCleanup(run: ActiveRun, cleanup: Promise<PeriWasmHostCleanupOutcome>): void {
    const observation = withDeadline(cleanup, CF_WASM_CLEANUP_TIMEOUT_MS, "Late ACP startup cleanup observation")
      .then((outcome) => {
        if (!outcome.confirmed) { this.environment.logError("Peri late startup cleanup unconfirmed", outcome.error); return; }
        this.confirmStartupCleanup(run);
      }).catch((error) => this.environment.logError("Peri late startup cleanup observation failed", error));
    this.defer(observation);
  }

  /** 确认启动清理完成：清除阻断并刷新运行态。 */
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

  /** 把 running / 阻断状态推进投影；元数据未就绪时跳过。 */
  private refreshRunState(running: boolean): void {
    if (!this.chat) return;
    this.projection.setRunState({ chat: this.chat, running, executionBlocked: this.unavailable });
  }
}
