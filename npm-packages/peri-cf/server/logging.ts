/**
 * 诊断日志出口。server 与 frontend 是两个独立进程，不共享运行时；
 * 这层 3 行 `console.error` 包装允许两侧各一份，不引入公共包。
 */
export function logError(label: string, error: unknown, context: Record<string, unknown> = {}): void {
  console.error(label, context, error);
}
