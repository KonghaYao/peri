import { PeriWasmHostStartupError, withDeadline, type PeriWasmHostLease, type WasmAcpTransport } from "@peri-code/sdk/wasm";
import { chatIdSchema } from "../../shared";
import { defaultDefer } from "./defer";
import type { AcquireHost, AgentEnvironment, Defer } from "../types";

/**
 * 创建会话：一次性宿主完成 `session/new` 与 `session/rename` 后关闭。
 * 系统指令与 MCP 配置随会话快照冻结；创建阶段没有 prompt，反向请求保持默认拒绝。
 */
export class SessionCreator {
  constructor(private readonly acquireHost: AcquireHost,
    private readonly environment: AgentEnvironment,
    private readonly defer: Defer = defaultDefer) {}

  async create(title: string): Promise<string> {
    let lease: PeriWasmHostLease | undefined;
    let transport: WasmAcpTransport | undefined;
    const mcp = this.environment.openMcp("create");
    let failure: unknown;
    const observeCleanup = (promise: Promise<unknown>): void => {
      this.defer(promise.catch((error) => {
        this.environment.logError("Peri session creation cleanup failed", error, { generationId: lease?.generationId });
        throw error;
      }));
    };
    const cleanupStartup = async (error: unknown): Promise<void> => {
      if (!(error instanceof PeriWasmHostStartupError)) throw error;
      const outcome = await error.cleanup;
      if (!outcome.confirmed)
        throw new AggregateError([error, outcome.error], "Creation startup cleanup is unconfirmed");
    };
    try {
      try {
        lease = await this.acquireHost({ signal: new AbortController().signal });
      } catch (error) {
        if (error instanceof PeriWasmHostStartupError) observeCleanup(cleanupStartup(error));
        throw error;
      }
      transport = lease.transport;
      transport.setRequestHandler((method, params) => {
        if (method === "mcp/message") return mcp.handleMessage(params);
        if (method === "session/request_permission") return { outcome: { outcome: "cancelled" } };
        throw new Error("ACP client capability is not authorized");
      });
      await this.environment.initialize(transport);
      const created = await withDeadline(transport.request<{ sessionId: string }>("session/new", {
        cwd: this.environment.cwd, mcpServers: mcp.servers,
        _meta: { "peri.instructions": this.environment.instructions },
      }), 20_000, "ACP session/new");
      if (!chatIdSchema.safeParse(created?.sessionId).success)
        throw new Error("Invalid ACP session/new identity");
      // 在 rename 之前冻结：即使命名失败，已创建的会话仍能按创建时的配置恢复 MCP。
      mcp.freeze?.(created.sessionId);
      const renamed = await withDeadline(transport.request<{ sessionId: string; title: string }>("session/rename", {
        sessionId: created.sessionId, title,
      }), 20_000, "ACP session/rename");
      if (renamed?.sessionId !== created.sessionId || renamed.title !== title)
        throw new Error("Invalid ACP session/rename response");
      return created.sessionId;
    } catch (error) {
      failure = error;
      this.environment.logError("Peri session creation failed", error, { generationId: transport?.generationId });
      throw error;
    } finally {
      try { await mcp.close(); }
      catch (error) { this.environment.logError("Peri MCP bridge close failed", error, { generationId: transport?.generationId }); }
      if (lease) {
        const report = await lease.close();
        if (!report.confirmed) {
          this.environment.logError("Peri session creation host close failed", report.error, { generationId: report.generationId });
          if (failure) throw new AggregateError([failure, report.error], "Creation failed and host close is unconfirmed");
          throw report.error ?? new Error("Creation host close is unconfirmed");
        }
      }
    }
  }
}
