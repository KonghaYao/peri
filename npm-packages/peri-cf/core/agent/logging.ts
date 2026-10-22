/**
 * 运行时诊断工具：错误文本的公开化清洗与日志出口。
 * 错误文本只公开稳定语义，不泄露内部路径或凭据；此处不依赖 HTTP 层。
 */

export function diagnosticMessage(error: unknown): string {
  const ancestors = new Set<object>();
  function describe(value: unknown): string {
    if (typeof value === "string") return value;
    if (!value || typeof value !== "object") return String(value);
    if (ancestors.has(value)) return "[Circular error cause]";
    ancestors.add(value);
    let message: string;
    if ("message" in value && typeof value.message === "string") {
      message = value.message;
    } else {
      try {
        message = JSON.stringify(value) ?? String(value);
      } catch {
        message = String(value);
      }
    }
    if ("cause" in value && value.cause !== undefined)
      message += `\nCaused by: ${describe(value.cause)}`;
    if (value instanceof AggregateError)
      for (const failure of value.errors) message += `\nAggregated error: ${describe(failure)}`;
    ancestors.delete(value);
    return message;
  }
  return describe(error);
}

export function logError(label: string, error: unknown, context: Record<string, unknown> = {}): void {
  console.error(label, context, error);
}

export function publicError(error: unknown): string {
  return diagnosticMessage(error).replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "").slice(0, 512);
}

/** 错误文案单一出口：宿主字符串与 Error 用同一条清洗规则，避免控制字符进入视图。 */
export function errorText(error: unknown): string {
  const text = typeof error === "string" ? error
    : error instanceof Error ? error.message : "本地宿主调用失败，请稍后重试。";
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "").slice(0, 512);
}
