// 聊天适配层出口：库能力从 `@peri-code/sdk/openui` 转发，本地只保留消息门面与 action 解析。
// OpenUIProgram / library / Renderer 不在 index 静态导出，由 OpenUIMessage 懒加载，
// 避免主 chunk 被 react-lang 与 canvas 图表拖大。
export {
  installOpenUITheme,
  resolveOpenUIThemeKind,
  splitOpenUI,
} from "@peri-code/sdk/openui";
export type {
  OpenUIActionEvent,
  OpenUIDocument,
  OpenUIFenceStatus,
} from "@peri-code/sdk/openui";
export { OpenUIMessage } from "./OpenUIMessage";
export type { OpenUIMessageProps } from "./OpenUIMessage";
export { parseOpenUIAction } from "./actions";
export type {
  CompactFormState,
  FormPrimitive,
  ParsedOpenUIAction,
} from "./actions";
