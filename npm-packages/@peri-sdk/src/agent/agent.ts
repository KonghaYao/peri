import { AgentClaims } from "../kv/agent-claims";
import type { AtomicManagedAgentKv } from "../kv/types";
import { Session } from "./session";
import type { AgentOptions } from "./types";
import type { SessionSummary } from "../storage/session-summary";
import { SessionDocs } from "../state/session-docs";
import { realpathSync } from "node:fs";

export class Agent {
    readonly id: string;
    readonly session: Session;
    readonly claims: AgentClaims;
    private readonly requestedPath?: string;
    private currentDocs: SessionDocs;

    constructor(
        readonly options: AgentOptions,
        kv: AtomicManagedAgentKv,
    ) {
        this.id = options.id;
        if (options.path) {
            // Keep Store list filtering and ACP setup on the same canonical cwd.
            try { this.requestedPath = realpathSync(options.path); }
            catch { this.requestedPath = options.path; } // A remote path need not exist on this host.
        }
        this.claims = new AgentClaims(kv, options.sandbox.id, options.id);
        this.currentDocs = new SessionDocs();
        this.session = new Session(this);
    }

    get docs(): SessionDocs {
        return this.currentDocs;
    }

    get path(): string | undefined {
        return this.session.path ?? this.requestedPath;
    }

    /** Discard a partial history replay before Session.start can be retried. */
    discardFailedSessionDocs(): void {
        this.currentDocs.destroy();
        this.currentDocs = new SessionDocs();
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

    close(): Promise<void> {
        return this.session.close();
    }
}
