import { initializeAcpClient, withDeadline } from "../sdk";
import type { AcpTransport } from "../types";

export const WORKSPACE_CWD = "/workspace";

/** ACP capabilities this client declares; the host must not send events reserved to omitted ones. */
const CLIENT_CAPABILITIES = { "peri.userInputQueue": true } as const;

export async function initializeTransport(transport: AcpTransport): Promise<void> {
  await withDeadline(initializeAcpClient(transport, {
    clientName: "peri-cf", clientVersion: "0.1.0", capabilities: CLIENT_CAPABILITIES,
  }), 20_000, "ACP initialize");
}
