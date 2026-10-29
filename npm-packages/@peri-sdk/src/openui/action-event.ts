/**
 * Renderer 传给宿主的原始 action 事件；与 `@openuidev/lang-core` 的 `ActionEvent`
 * 结构兼容，但由本包定义，避免消费端依赖 OpenUI 内部类型。
 */
export interface OpenUIActionEvent {
  readonly type: string;
  readonly params: Record<string, unknown>;
  readonly humanFriendlyMessage: string;
  readonly formState?: Record<string, unknown>;
  readonly formName?: string;
}
