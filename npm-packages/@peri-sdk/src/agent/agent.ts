import { AgentClaims } from "../kv/agent-claims";
import type { AtomicManagedAgentKv } from "../kv/types";
import { Session } from "./session";
import type { AgentOptions } from "./types";
import type { SessionSummary } from "../storage/session-summary";

export class Agent {
    readonly id: string;
    readonly session: Session;
    readonly claims: AgentClaims;

    constructor(
        readonly options: AgentOptions,
        kv: AtomicManagedAgentKv,
    ) {
        this.id = options.id;
        this.claims = new AgentClaims(kv, options.sandbox.id, options.id);
        this.session = new Session(this);
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
        return this.options.sandbox.getSessions();
    }

    close(): Promise<void> {
        return this.session.close();
    }
}
