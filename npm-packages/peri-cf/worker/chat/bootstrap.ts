import { initializeAcpClient, withDeadline } from "../sdk";
import type { AcpTransport } from "../types";

export const WORKSPACE_CWD = "/workspace";

/** ACP capabilities this client declares; the host must not send events reserved to omitted ones. */
const CLIENT_CAPABILITIES = {
  "peri.userInputQueue": true,
  // 任务快照与后台任务事件通道（P4 已接好能力源；Rust 侧门控见 event_sink.rs 的 caps.unstable_event）。
  "peri.unstableEvent": true,
  // 重放复原依赖 `_meta.periReplay` 区分历史与实时（Rust 侧门控见 session_replay.rs：
  // 只有声明该能力才会给 session/update 打标，未声明时历史 turn 会被当作实时事件投影）。
  "peri.replay": true,
} as const;

export async function initializeTransport(transport: AcpTransport): Promise<void> {
  await withDeadline(initializeAcpClient(transport, {
    clientName: "peri-cf", clientVersion: "0.1.0", capabilities: CLIENT_CAPABILITIES,
  }), 20_000, "ACP initialize");
}
