import { HTTPException } from "hono/http-exception";
import { logError, publicError } from "../api/http";
import type { AcpTransport, ChatRepository, Env, StartTransport } from "../types";
import { DeadlineError, PeriWasmHostStartupError, withDeadline, type PeriWasmHostCleanupOutcome } from "../sdk";
import { ChatRecordStore, CHECKPOINT_INDEX_MAX_BYTES, INDEX_ADMISSION_HEADROOM_BYTES, RecordCapacityError,
  emptyStoredRecord, recordBytes, type StoredRecord } from "./record";
import { ChatInteractionQueue } from "./interactions";
import { initializeTransport, WORKSPACE_CWD } from "./bootstrap";
import { createChatRoutes } from "./routes";
import { ChatProjection } from "./projection";
import { ChatSockets, upgradeSocket, type UpgradeSocket, type SyncSocket, type HibernatingSessionState } from "./sync";
import { WasmResources } from "../wasm/resources";
import { CF_WASM_CLEANUP_TIMEOUT_MS } from "../../shared/runtime-limits";
import type { AgentResources } from "../../shared/resources";

export const MAX_REPLY_BYTES = 524_288;
/** 浏览器单次载荷下载上限（与 SDK 默认展示预算一致）；更大的结果需要流式下载（未实现）。 */
export const MAX_PAYLOAD_RESPONSE_BYTES = 1_048_576;
/** 索引接纳上限（已批准 1 MiB）：接近上限时拒绝新 turn。 */
export const MAX_INDEX_BYTES = CHECKPOINT_INDEX_MAX_BYTES;
/** ACP control requests and cancellation keep their own budgets; neither proves a stop on timeout. */
export const ACP_CONTROL_TIMEOUT_MS = 20_000;
export const CANCELLATION_TIMEOUT_MS = 65_000;

interface ActiveRun {
  transport?: AcpTransport;
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

export class ChatSessionCore {
  private record?: StoredRecord;
  private ready?: Promise<void>;
  private readonly projection = new ChatProjection();
  private readonly records: ChatRecordStore;
  private readonly sockets: ChatSockets;
  private startupBlock?: ActiveRun;
  private active?: ActiveRun;
  private resources?: WasmResources;
  private unavailable = false;
  private readonly routes: ReturnType<typeof createChatRoutes>;

  constructor(private readonly state: HibernatingSessionState, private readonly env: Env, private readonly startTransport: StartTransport,
    private readonly repository: Pick<ChatRepository, "get">,
    private readonly upgrade: UpgradeSocket = upgradeSocket) {
    this.records = new ChatRecordStore(state.storage);
    this.sockets = new ChatSockets(env, state, async (id) => {
      await this.ensureSession(id);
      await this.projection.flush();
      return this.projection.sync;
    }, upgrade);
    this.routes = createChatRoutes({
      read: (id) => this.readChat(id),
      resources: (id) => this.readResources(id),
      payload: (id, refId) => this.readPayload(id, refId),
      interaction: (id, interactionId, body) => this.answerInteraction(id, interactionId, body),
      send: (id, content, signal) => this.sendMessage(id, content, signal),
      cancel: (id) => this.cancelMessage(id),
      sync: (id, request) => this.sockets.open(id, request),
    });
  }

  private async restore(): Promise<void> {
    const loaded = await this.records.load();
    if (!loaded.record) return;
    this.record = loaded.record;
    this.unavailable = loaded.record.executionBlocked === true;
    await this.projection.restoreCheckpoint(await this.records.hydrate(loaded.record));
    // 重启打断属于宿主生命周期事实：展示为应用级 error（生成结果本身不改写；记录已在转换时收敛）。
    if (loaded.source === "converted" && loaded.interrupted) this.projection.setRestartInterrupted();
    await this.records.sweep();
  }

  async fetch(request: Request): Promise<Response> {
    await this.sockets.restoreSubscriptions();
    return this.routes.fetch(request, this.env);
  }

  async webSocketMessage(socket: SyncSocket, message: string | ArrayBuffer): Promise<void> {
    await this.sockets.message(socket, message);
  }

  webSocketClose(socket: SyncSocket, _code: number, _reason: string, _wasClean: boolean): void {
    this.sockets.closed(socket);
  }

  webSocketError(socket: SyncSocket, error: unknown): void { this.sockets.error(socket, error); }

  private async ensureSession(id: string, refresh = false): Promise<StoredRecord> {
    await (this.ready ??= this.restore());
    if (this.record && this.record.chat.id !== id) throw new HTTPException(404, { message: "Chat not found" });
    if (!this.record || refresh) {
      const chat = await this.repository.get(id);
      if (!chat || chat.id !== id) throw new HTTPException(404, { message: "Chat not found" });
      if (this.record && this.record.chat.id !== id) throw new HTTPException(404, { message: "Chat not found" });
      if (!this.record) this.record = emptyStoredRecord(chat);
      else this.record.chat = chat;
    }
    this.projection.setRunState({ chat: this.record.chat, running: this.record.running, executionBlocked: this.unavailable });
    return this.record;
  }

  /** 交互回答：浏览器经鉴权命令回答 pending 交互，直接结算 live reverse request；不写 Y.Doc。 */
  private async answerInteraction(id: string, interactionId: string, body: unknown): Promise<Response> {
    const record = await this.ensureSession(id);
    const headers = { "Cache-Control": "no-store" };
    const result = this.active?.interactions.answer(record.chat.id, interactionId, body) ?? "unknown";
    if (result === "accepted") return Response.json({ accepted: true }, { status: 202, headers });
    if (result === "invalid") throw new HTTPException(400, { message: "Invalid interaction response" });
    if (result === "kind_mismatch") throw new HTTPException(400, { message: "Interaction kind does not match" });
    // 迟到/重复/跨会话/已失效统一按不可回答处理。
    throw new HTTPException(409, { message: "Interaction is not pending" });
  }

  /** 载荷全文读取（浏览器经鉴权端点）：失败码沿用 SDK 契约，未知引用不泄露其它会话存在性。 */
  private async readPayload(id: string, refId: string): Promise<Response> {
    await this.ensureSession(id);
    const headers = { "Cache-Control": "no-store" };
    const result = await this.records.readPayload(refId);
    if (result.status === "unknown") {
      return Response.json({ error: { code: "unavailable", message: "载荷引用不存在或已被回收" } }, { status: 404, headers });
    }
    if (result.status === "missing" || result.status === "mismatch") {
      return Response.json({ error: { code: "integrity_error", message: "载荷分段缺失或字节数不符" } }, { status: 500, headers });
    }
    if (result.bytes > MAX_PAYLOAD_RESPONSE_BYTES) {
      return Response.json({ error: { code: "too_large",
        message: `载荷 ${result.bytes} 字节超过单次下载上限 ${MAX_PAYLOAD_RESPONSE_BYTES}，需要流式下载（未实现）` } },
        { status: 413, headers });
    }
    return Response.json({ id: refId, bytes: result.bytes, json: result.json }, { headers });
  }

  private async readChat(id: string): Promise<Response> {
    await this.ensureSession(id, true);
    await this.projection.flush();
    // HTTP 首屏与 WS 推送共用同一 DTO 与身份（SDK view 物化）。
    return Response.json(this.projection.state(), { headers: { "Cache-Control": "no-store" } });
  }

  private async readResources(id: string): Promise<Response> {
    await this.ensureSession(id);
    const resources: AgentResources = {
      sessionId: id, running: this.record!.running === true,
      executionBlocked: this.unavailable, instance: this.resources?.snapshot() ?? null,
    };
    return Response.json(resources, { headers: { "Cache-Control": "no-store" } });
  }

  private async sendMessage(id: string, content: string, signal: AbortSignal): Promise<Response> {
    await this.ensureSession(id);
    return this.startMessage(content, signal);
  }

  private async cancelMessage(id: string): Promise<Response> {
    await this.ensureSession(id);
    const run = this.active;
    if (!run && this.unavailable) throw new HTTPException(503, { message: "ACP host stop is not confirmed" });
    if (run) {
      await this.cancel(run);
      try { await withDeadline(run.settlement, 45_000, "Cancel settlement"); } catch (error) {
        throw new HTTPException(error instanceof DeadlineError ? 504 : 500, { message: publicError(error, this.env) });
      }
    }
    return Response.json({ cancelled: !!run });
  }

  private async startMessage(content: string, _signal: AbortSignal): Promise<Response> {
    if (this.active) throw new HTTPException(409, { message: "Chat is busy" });
    if (this.unavailable) throw new HTTPException(503, { message: "Previous ACP host did not close; this instance cannot run again" });
    const run = activeRun();
    this.active = run;
    try {
      this.assertIndexHeadroom();
      this.record!.running = true;
      // 先发布运行元数据再投影用户输入：否则副本会先看到条目而 running 仍为 false（误判轮次结束）。
      this.projection.setRunState({ chat: this.record!.chat, running: true, executionBlocked: this.unavailable });
      await this.projection.start(content);
      await this.save();
      this.state.waitUntil(this.execute(run, content));
      return Response.json({ accepted: true }, { status: 202 });
    } catch (error) {
      this.active = undefined;
      run.interactions.close();
      clearTimeout(run.cancelTimer);
      run.settle(new Error(publicError(error, this.env)));
      if (this.record?.running) {
        this.record.running = false;
        await this.projection.complete("error", publicError(error, this.env));
        await this.save();
      }
      throw error;
    }
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

  private async execute(run: ActiveRun, content: string): Promise<void> {
    let unsubscribe: (() => void) | undefined;
    let failure: unknown;
    let protocolFailure: Error | undefined;
    let replyBytes = 0;
    let status: "completed" | "cancelled" | "error" = "completed";
    try {
      const resources = new WasmResources();
      this.resources = resources;
      run.transport = await this.startHost(run, resources);
      const transport = run.transport;
      // 反向交互交由 SDK 投影处理：记录在 Yjs、回答经鉴权命令回灌；未接线的方法保持默认拒绝。
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
      run.sessionId = this.record!.chat.id;
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
          if (protocolFailure) return;
          const params = notification.params as {
            sessionId?: unknown;
            _meta?: { "peri.sourceAgentId"?: unknown; peri?: { sourceAgentId?: unknown } };
            update?: { sessionUpdate?: unknown; content?: { type?: unknown; text?: unknown } };
          } | undefined;
          if (!params || params.sessionId !== run.sessionId) return;
          const subagent = params._meta?.["peri.sourceAgentId"] || params._meta?.peri?.sourceAgentId;
          if (subagent && ["agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update"]
            .includes(String(params.update?.sessionUpdate))) return;
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
          const chunkBytes = new TextEncoder().encode(JSON.stringify(chunk.text)).byteLength - 2;
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
      logError("Peri chat execution failed", error, { sessionId: this.record!.chat.id, generationId: run.transport?.generationId });
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
          this.unavailable = true;
          this.record!.executionBlocked = true;
          const closeError = error ?? new Error("Host close rejected without an error value", { cause: error });
          failure = failure === undefined ? closeError : new AggregateError([failure, closeError], "Execution failed and host shutdown is unconfirmed");
        }
      }
      this.record!.running = false;
      try {
        // 先让投影结算：abortAll 把未决交互置 expired 并中止信号，再兜底撤销 live 队列，
        // 这样宿主关闭导致的挂起不会被记成用户拒绝。
        await this.projection.complete(failure !== undefined ? "error" : status, failure !== undefined ? publicError(failure, this.env) : undefined);
        run.interactions.close();
        await this.save();
      } catch (error) {
        logError("Peri chat persistence failed", error);
        failure ??= error ?? new Error("Persistence rejected without an error value", { cause: error });
        try {
          await this.projection.complete("error", publicError(failure, this.env));
          await this.save();
        }
        catch (retryError) { logError("Peri chat error-state persistence failed", retryError); failure = new AggregateError([failure, retryError], "Chat persistence remains unavailable"); }
      }
      try { await this.projection.flush(); }
      catch (flushError) {
        logError("Peri terminal chat projection failed", flushError);
        failure = new AggregateError([...(failure !== undefined ? [failure] : []), flushError], "Chat terminal projection failed");
      }
      this.active = undefined;
      clearTimeout(run.cancelTimer);
      run.settle(failure);
    }
  }

  /** 持久化展示材料：先写全部分段，再提交索引（写入顺序由记录适配器保证）。 */
  private async save(): Promise<void> {
    await this.projection.flush();
    this.projection.setRunState({ chat: this.record!.chat, running: this.record!.running, executionBlocked: this.unavailable });
    await this.records.commit({
      chat: this.record!.chat, running: this.record!.running === true,
      executionBlocked: this.unavailable, checkpoint: this.projection.checkpoint(),
    });
  }

  /** 接纳预检：索引接近 1 MiB 上限时拒绝新 turn，避免写入被截断。 */
  private assertIndexHeadroom(): void {
    const indexBytes = recordBytes({ ...this.record!, checkpoint: this.projection.checkpoint() });
    if (indexBytes + INDEX_ADMISSION_HEADROOM_BYTES > MAX_INDEX_BYTES) {
      throw new HTTPException(413, { message: "Chat display index exceeds the 1 MiB admission limit; create a new chat" });
    }
  }

  private async startHost(run: ActiveRun, resources: WasmResources): Promise<AcpTransport> {
    const starting = Promise.resolve().then(() => this.startTransport(this.env, resources, run.startup.signal));
    try { return await withDeadline(starting, 20_000, "ACP host startup"); }
    catch (error) {
      run.startup.abort(error);
      this.startupBlock = run;
      this.unavailable = true;
      this.record!.executionBlocked = true;
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
          this.observeLateStartupCleanup(run, eventualCleanup);
        }
      } catch (cleanupError) {
        logError("Peri startup cleanup observation failed", cleanupError);
        this.observeLateStartupCleanup(run, eventualCleanup);
      }
      throw error;
    }
  }

  private observeLateStartupCleanup(run: ActiveRun, cleanup: Promise<PeriWasmHostCleanupOutcome>): void {
    const observation = withDeadline(cleanup, CF_WASM_CLEANUP_TIMEOUT_MS, "Late ACP startup cleanup observation")
      .then(async (outcome) => {
        if (!outcome.confirmed) { logError("Peri late startup cleanup unconfirmed", outcome.error); return; }
        this.confirmStartupCleanup(run);
        await this.save();
      }).catch((error) => logError("Peri late startup cleanup observation failed", error));
    this.state.waitUntil(observation);
  }

  private confirmStartupCleanup(run: ActiveRun): void {
    if (this.startupBlock !== run || run.transport) return;
    run.startupCleanupConfirmed = true;
    this.startupBlock = undefined;
    this.unavailable = false;
    this.record!.executionBlocked = false;
  }

}
