import type { Transport } from "../transport/types";

/** ACP protocol version this SDK client speaks. */
export const ACP_PROTOCOL_VERSION = 1;

export interface AcpInitializeOptions {
    clientName?: string;
    clientVersion?: string;
    /** Peri capability flags declared to the host under `clientCapabilities._meta`. */
    capabilities: Readonly<Record<string, boolean | number>>;
}

/**
 * ACP `initialize` for one client. Capabilities are the caller's own contract: a client that
 * declares fewer Peri capabilities must not receive events reserved for the ones it left out.
 */
export async function initializeAcpClient(transport: Transport, options: AcpInitializeOptions): Promise<void> {
    const initialized = await transport.request<{ protocolVersion?: number }>("initialize", {
        protocolVersion: ACP_PROTOCOL_VERSION,
        clientCapabilities: { _meta: { ...options.capabilities } },
        ...(options.clientName || options.clientVersion
            ? { clientInfo: { name: options.clientName ?? "peri-sdk", version: options.clientVersion ?? "0.0.0" } }
            : {}),
    });
    if (initialized?.protocolVersion !== ACP_PROTOCOL_VERSION)
        throw new Error("Unsupported ACP protocol version");
}
