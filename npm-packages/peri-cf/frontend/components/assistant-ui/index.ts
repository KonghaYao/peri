/**
 * vendored 审批/问答卡片的公共出口。目录名与源码形状沿用上游 registry，
 * 但不再依赖 `@assistant-ui/*` 运行时；应用代码只经本入口引用。
 */
export { ApprovalCard, type ApprovalState } from "./elements/approval-card";
export {
  ElicitationForm, type ElicitationField, type ElicitationState,
} from "./elements/elicitation-form";
export * from "./elements/surfaces";
