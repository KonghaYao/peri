import { parseInteractionAnswer, type ElicitationDecision, type ElicitationRequest, type InteractionAnswer, type PermissionRequest } from "../sdk";

/**
 * 每轮执行的 live 交互登记表：只保存 settle 闭包与会话身份，
 * 不保存答案内容、令牌或 resolver 引用（不进 Yjs、不写日志）。
 */
export class ChatInteractionQueue {
  private readonly pending = new Map<string, {
    sessionId: string;
    kind: "permission" | "elicitation";
    settle(answer: InteractionAnswer): void;
    cancel(): void;
  }>();

  size(): number { return this.pending.size; }

  /** 无回答者时保持安全默认：权限拒绝。 */
  requestPermission(request: PermissionRequest): Promise<"allow_once" | "reject_once"> {
    if (request.signal.aborted) return Promise.resolve("reject_once");
    return new Promise((resolve) => {
      const finish = (decision: "allow_once" | "reject_once") => {
        this.pending.delete(request.interactionId);
        request.signal.removeEventListener("abort", cancel);
        resolve(decision);
      };
      const cancel = () => finish("reject_once");
      request.signal.addEventListener("abort", cancel, { once: true });
      this.pending.set(request.interactionId, {
        sessionId: request.sessionId, kind: "permission",
        settle: (answer) => { if (answer.kind === "permission") finish(answer.decision); },
        cancel,
      });
    });
  }

  /** 无回答者时保持安全默认：拒绝（decline），不误发 accept。 */
  requestElicitation(request: ElicitationRequest): Promise<ElicitationDecision> {
    if (request.signal.aborted) return Promise.resolve({ action: "decline" });
    return new Promise((resolve) => {
      const finish = (decision: ElicitationDecision) => {
        this.pending.delete(request.interactionId);
        request.signal.removeEventListener("abort", cancel);
        resolve(decision);
      };
      const cancel = () => finish({ action: "decline" });
      request.signal.addEventListener("abort", cancel, { once: true });
      this.pending.set(request.interactionId, {
        sessionId: request.sessionId, kind: "elicitation",
        settle: (answer) => {
          if (answer.kind !== "elicitation") return;
          // cancel 与 decline 都是用户显式决定；accept 的内容约束由 SDK 表单窗口复核。
          finish(answer.decision);
        },
        cancel,
      });
    });
  }

  /**
   * 浏览器回答入口。跨会话、迟到、重复、已失效一律按未知处理（不泄露其它会话存在性）。
   */
  answer(sessionId: string, interactionId: string, raw: unknown): "accepted" | "unknown" | "kind_mismatch" | "invalid" {
    const answer = parseInteractionAnswer(raw);
    if (!answer) return "invalid";
    const pending = this.pending.get(interactionId);
    if (!pending || pending.sessionId !== sessionId) return "unknown";
    if (pending.kind !== answer.kind) return "kind_mismatch";
    pending.settle(answer);
    return "accepted";
  }

  /** 宿主关闭/执行失败/轮次退出：全部按安全默认结算，投影侧随后置 expired。 */
  close(): void {
    for (const pending of [...this.pending.values()]) pending.cancel();
    this.pending.clear();
  }
}
