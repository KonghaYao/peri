import { withDeadline } from "./deadline";
import type { AcpTransport } from "../types";

export const WORKSPACE_CWD = "/workspace";

export async function initializeTransport(transport: AcpTransport): Promise<void> {
  const initialized = await withDeadline(transport.request<{ protocolVersion: number }>("initialize", {
    protocolVersion: 1, clientCapabilities: { _meta: {
      "peri.executionProtocol": 1,
      "peri.userInputQueue": true,
    } },
    clientInfo: { name: "peri-cf", version: "0.1.0" },
  }), 20_000, "ACP initialize");
  if (initialized?.protocolVersion !== 1) throw new Error("Unsupported ACP protocol version");
}
