import { ChatCommandError, publicError, type ChatCommandPayload } from "@core/agent";

/**
 * 客户端对命令处置的完整分类：wire 只携带 `rejected`/`unknown`；
 * `not_sent` 表示预检失败、命令没有离开客户端，可安全重试。
 */
export type ClientCommandDisposition = "not_sent" | "rejected" | "unknown";

export type CommandFailure = {
  ok: false;
  code: string;
  message: string;
  disposition: ClientCommandDisposition;
};
export type CommandOutcome = { ok: true; payload?: ChatCommandPayload } | CommandFailure;

/** 客户端预检失败：命令未发送，可安全重试。 */
export function clientCommandFailure(code: string, message: string): CommandFailure {
  return { ok: false, code, message, disposition: "not_sent" };
}

/** 运行期异常收敛：已知命令失败保留服务端处置；其他异常视为传输失败、执行状态未知。 */
export function fromCommandError(error: unknown): CommandFailure {
  if (error instanceof ChatCommandError)
    return { ok: false, code: error.code, message: error.message, disposition: error.disposition };
  return { ok: false, code: "transport_error", message: publicError(error), disposition: "unknown" };
}
