import { HTTPException } from "hono/http-exception";
import { logError, publicError } from "../api/http";
import type { AcpTransport, ChatRepository, Env, ExecutionDispatcher, SessionRecord, StartExecution, StartTransport } from "../types";
import { ACP_CONTROL_TIMEOUT_MS, CANCELLATION_TIMEOUT_MS, DeadlineError, withDeadline } from "./deadline";
import { initializeTransport, WORKSPACE_CWD } from "./bootstrap";
import { createChatRoutes } from "./routes";
import { ChatProjection } from "./projection";
import { ChatSockets, upgradeSocket, type UpgradeSocket, type SyncSocket, type HibernatingSessionState } from "./sync";
import { WasmResources } from "../wasm/resources";
import { WasmStartupError, type StartupCleanupOutcome } from "../wasm/lifecycle";
import { CF_WASM_CLEANUP_TIMEOUT_MS } from "../../shared/runtime-limits";
import type { AgentResources } from "../../shared/resources";

export const MAX_TRANSCRIPT_BYTES = 1_048_576;
export const MAX_REPLY_BYTES = 524_288;

interface ActiveRun {
  transport?: AcpTransport;
  execution?: ExecutionDispatcher;
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
  return { promptWritten: false, cancelRequested: false, settlement, settle, cancelExpired, expireCancel, startup: new AbortController() };
}

export class ChatSessionCore {
  private record?: SessionRecord;
  private ready?: Promise<void>;
  private readonly projection = new ChatProjection();
  private readonly sockets: ChatSockets;
  private startupBlock?: ActiveRun;
  private active?: ActiveRun;
  private resources?: WasmResources;
  private unavailable = false;
  private readonly routes: ReturnType<typeof createChatRoutes>;

  constructor(private readonly state: HibernatingSessionState, private readonly env: Env, private readonly startTransport: StartTransport,
    private readonly startExecution: StartExecution, private readonly repository: Pick<ChatRepository, "get">,
    private readonly upgrade: UpgradeSocket = upgradeSocket) {
    this.sockets = new ChatSockets(env, state, async (id) => {
      await this.ensureSession(id);
      await this.projection.flush();
      return this.projection.sync;
    }, upgrade);
    this.routes = createChatRoutes({
      read: (id) => this.readChat(id),
      resources: (id) => this.readResources(id),
      send: (id, content, signal) => this.sendMessage(id, content, signal),
      cancel: (id) => this.cancelMessage(id),
      sync: (id, request) => this.sockets.open(id, request),
    });
  }

  private async restore(): Promise<void> {
    this.record = await this.state.storage.get<SessionRecord>("session");
    this.unavailable = this.record?.executionBlocked === true;
    if (!this.record) return;
    if (!this.record.running) { await this.projection.restore(this.record); return; }
    this.unavailable = true;
    this.record.executionBlocked = true;
    for (const message of this.record.messages) {
      if (message.status === "running") {
        message.status = "error";
        message.error = "Execution interrupted by host restart; not automatically resumed";
      }
    }
    this.record.running = false;
    await this.projection.restore(this.record);
    await this.save();
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

  private async ensureSession(id: string, refresh = false): Promise<SessionRecord> {
    await (this.ready ??= this.restore());
    if (this.record && this.record.chat.id !== id) throw new HTTPException(404, { message: "Chat not found" });
    if (!this.record || refresh) {
      const chat = await this.repository.get(id);
      if (!chat || chat.id !== id) throw new HTTPException(404, { message: "Chat not found" });
      if (this.record && this.record.chat.id !== id) throw new HTTPException(404, { message: "Chat not found" });
      if (!this.record) {
        this.record = { chat, messages: [] };
        await this.projection.restore(this.record);
      }
      else this.record.chat = chat;
    }
    return this.record;
  }

  private async readChat(id: string): Promise<Response> {
    const record = await this.ensureSession(id, true);
    await this.projection.flush();
    return Response.json({ chat: record.chat, messages: record.messages });
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
      const nextRecord = { ...this.record!, messages: [...this.record!.messages,
        { id: crypto.randomUUID(), role: "user", content, createdAt: new Date().toISOString(), status: "completed" },
        { id: crypto.randomUUID(), role: "assistant", content: "", createdAt: new Date().toISOString(), status: "running" },
      ] };
      if (new TextEncoder().encode(JSON.stringify(nextRecord)).byteLength > MAX_TRANSCRIPT_BYTES)
        throw new HTTPException(413, { message: "Chat history exceeds the 1 MiB admission limit; create a new chat" });
      this.record!.running = true;
      await this.projection.start(content);
      await this.save();
      this.state.waitUntil(this.execute(run, content));
      return Response.json({ accepted: true }, { status: 202 });
    } catch (error) {
      this.active = undefined;
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
    run.cancelDelivery ??= withDeadline(run.execution!.stop(run.sessionId), ACP_CONTROL_TIMEOUT_MS, "ACP exact-target stop").then(async (paused) => {
      if (paused) {
        this.record!.pausedByStop = paused;
        await this.save();
      }
    }).catch((error) => {
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
      run.execution = this.startExecution(transport);
      transport.setRequestHandler((method, params) => {
        const admission = run.execution!.handle(method, params);
        if (admission !== undefined) return admission;
        if (method === "session/request_permission") return { outcome: { outcome: "cancelled" } };
        throw new Error("ACP client capability is not authorized");
      });
      await initializeTransport(transport);
      run.sessionId = this.record!.chat.id;
      const loaded = await withDeadline(transport.request("session/load", {
        cwd: WORKSPACE_CWD, mcpServers: [], sessionId: run.sessionId,
      }), 20_000, "ACP session/load");
      if (!loaded || typeof loaded !== "object") throw new Error("Invalid ACP session/load response");
      if (run.cancelRequested) {
        status = "cancelled";
      } else {
        if (this.record!.pausedByStop) {
          await withDeadline(run.execution.resume(run.sessionId, this.record!.pausedByStop), ACP_CONTROL_TIMEOUT_MS, "ACP resume after own stop");
          delete this.record!.pausedByStop;
          await this.save();
        }
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
      if (run.transport) {
        run.execution?.seal();
        try {
          await withDeadline(run.transport.close(), 20_000, "ACP host close");
          if (run.sessionId && run.execution)
            await withDeadline(run.execution.stopAfterHostClose(run.sessionId), 20_000, "SDK execution settlement");
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
        await this.projection.complete(failure !== undefined ? "error" : status, failure !== undefined ? publicError(failure, this.env) : undefined);
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

  private async save(): Promise<void> {
    await this.projection.flush();
    await this.state.storage.put("session", this.record!);
  }

  private async startHost(run: ActiveRun, resources: WasmResources): Promise<AcpTransport> {
    const starting = Promise.resolve().then(() => this.startTransport(this.env, resources, run.startup.signal));
    try { return await withDeadline(starting, 20_000, "ACP host startup"); }
    catch (error) {
      run.startup.abort(error);
      this.startupBlock = run;
      this.unavailable = true;
      this.record!.executionBlocked = true;
      const eventualCleanup: Promise<StartupCleanupOutcome> = error instanceof WasmStartupError ? error.eventualCleanup :
        starting.then(async (lateTransport) => {
          await lateTransport.close();
          return { confirmed: true };
        }, (startupError) => startupError instanceof WasmStartupError ? startupError.eventualCleanup :
          { confirmed: false, error: startupError });
      const cleanup = error instanceof WasmStartupError ? error.cleanup : eventualCleanup;
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

  private observeLateStartupCleanup(run: ActiveRun, cleanup: Promise<StartupCleanupOutcome>): void {
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
