// 必须先关掉 OpenUI devtools，再加载任何 @openuidev/react-lang 模块。
import "./devtools-off";

// 轻量出口：宿主只需要门面、action 解析、主题与类型。
// OpenUIProgram / library / Renderer 不在 index 静态导出，由 OpenUIMessage 懒加载，
// 避免主 chunk 被 react-lang 与 canvas 图表拖大。
export { installOpenUITheme, resolveOpenUIThemeKind } from "./theme";
export { OpenUIMessage } from "./OpenUIMessage";
export type { OpenUIMessageProps } from "./OpenUIMessage";
export { parseOpenUIAction } from "./actions";
export type {
  CompactFormState,
  FormPrimitive,
  OpenUIActionEvent,
  ParsedOpenUIAction,
} from "./actions";
export type { OpenUIArtifact } from "./generate";
