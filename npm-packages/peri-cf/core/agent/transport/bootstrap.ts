import { initializeAcpClient, type Transport } from "@peri-code/sdk/portable";
import { withDeadline } from "@peri-code/sdk/wasm";

/** ACP capabilities this client declares; the host must not send events reserved to omitted ones. */
const CLIENT_CAPABILITIES = {
  "peri.userInputQueue": true,
  // 后台任务事件通道（Rust 侧门控见 event_sink.rs）。
  "peri.unstableEvent": true,
  // 用于区分历史重放与实时事件；不声明时历史 turn 会被当作实时事件投影。
  "peri.replay": true,
  // 不声明会退回完整历史逐段读取，冷启动的请求量会超出存储配额。
  "peri.historyPagination": true,
} as const;

export async function initializeTransport(transport: Transport): Promise<void> {
  await withDeadline(initializeAcpClient(transport, {
    clientName: "peri-cf", clientVersion: "0.1.0", capabilities: CLIENT_CAPABILITIES,
  }), 20_000, "ACP initialize");
}
