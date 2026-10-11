/**
 * 浏览器运行时的诊断工具：错误文本的公开化清洗与日志出口。
 * 与 `worker/api/http.ts` 的同名工具保持同一语义；此处不依赖 HTTP 层。
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
