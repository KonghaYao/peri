import { AgentClaims } from "../kv/agent-claims";
import type { AtomicManagedAgentKv } from "../kv/types";
import { Session } from "./session";
import type { AgentOptions } from "./types";
import type { SessionSummary } from "../storage/session-summary";
import { SessionDocs } from "../state/session-docs";
import type { CloseOptions } from "./session-close";

export class Agent {
    readonly id: string;
    readonly session: Session;
    readonly claims: AgentClaims;
    private currentDocs: SessionDocs;
    /** 当前有效的初始 load 代次；旧代次的通知与响应一律作废（REQ-H04、验收 A22）。 */
    private loadGeneration?: string;

    constructor(
        readonly options: AgentOptions,
        kv: AtomicManagedAgentKv,
    ) {
        this.id = options.id;
        this.claims = new AgentClaims(kv, options.sandbox.id, options.id);
        this.currentDocs = new SessionDocs();
        this.session = new Session(this);
    }

    get docs(): SessionDocs {
        return this.currentDocs;
    }

    /** 已建立会话的持久 cwd 优先；调用方传入的 path 原样使用，不做本机解析。 */
    get path(): string | undefined {
        return this.session.path ?? this.options.path;
    }

    /** Discard a partial history replay before Session.start can be retried. */
    discardFailedSessionDocs(): void {
        this.currentDocs.destroy();
        this.currentDocs = new SessionDocs();
    }

    /** 开始一次初始 load：返回本次代次，并让此前未完成的代次作废。 */
    beginLoadGeneration(generation: string): string {
        this.loadGeneration = generation;
        return generation;
    }

    /** 该代次是否仍是当前这次 load（被新代次取代后为假：暂存丢弃、迟到回放不安装）。 */
    isCurrentLoadGeneration(generation: string | undefined): boolean {
        return generation !== undefined && this.loadGeneration === generation;
    }

    mcpServers(): unknown[] {
        const servers: unknown[] = Object.entries(
            this.options.mcpServers ?? {},
        ).map(([name, server]) => ({
            name,
            ...server,
            ...(server.type === "http"
                ? {
                      headers: Object.entries(server.headers ?? {}).map(
                          ([header, value]) => ({ name: header, value }),
                      ),
                  }
                : {
                      env: Object.entries(server.env ?? {}).map(
                          ([name, value]) => ({ name, value }),
                      ),
                  }),
        }));
        const workspace = this.options.sandbox.optionalWorkspace;
        if (workspace) {
            if (this.options.mcpServers?.workspace) {
                throw new Error(
                    "Extra MCP server cannot override Sandbox workspace",
                );
            }
            servers.unshift({
                name: "workspace",
                type: "http",
                url: workspace.url,
                headers: Object.entries(workspace.headers ?? {}).map(
                    ([name, value]) => ({ name, value }),
                ),
            });
        }
        return servers;
    }

    getSessions(): Promise<SessionSummary[]> {
        const path = this.path;
        if (!path) throw new Error("Agent has no Session path");
        return this.options.sandbox.getSessions(path);
    }

    close(options?: CloseOptions): Promise<void> {
        return this.session.close(options);
    }
}
