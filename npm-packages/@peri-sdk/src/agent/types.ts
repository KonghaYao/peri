import type { Sandbox } from "../sandbox/sandbox";

export type AcpMcpServer =
    | { type: "http"; url: string; headers?: Record<string, string> }
    | {
          type: "stdio";
          command: string;
          args?: string[];
          env?: Record<string, string>;
      };

export interface AgentOptions {
    id: string;
    sandbox: Sandbox;
    instructions?: string;
    mcpServers?: Record<string, AcpMcpServer>;
}
