/**
 * 运行时诊断工具：错误文本的公开化清洗与日志出口。
 * 错误文本只公开稳定语义，不泄露内部路径或凭据；此处不依赖 HTTP 层。
 */

export function logError(label: string, error: unknown, context: Record<string, unknown> = {}): void {
  console.error(label, context, error);
}

/** 错误文案单一出口：宿主字符串与 Error 用同一条清洗规则，避免控制字符进入视图。 */
export function errorText(error: unknown): string {
  const text = typeof error === "string" ? error
    : error instanceof Error ? error.message : "本地宿主调用失败，请稍后重试。";
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "").slice(0, 512);
}
