/**
 * 轻量出口：围栏切分、主题桥与共享类型。
 *
 * 不静态导出 library / canvas / Renderer：宿主 import `@peri-code/sdk/openui` 不会把
 * `@openuidev/react-lang` 与图表打进主 chunk。渲染入口见 `@peri-code/sdk/openui/renderer`，
 * 服务端提示词见 `@peri-code/sdk/openui/prompt`。
 */
export type { OpenUIActionEvent } from "./action-event";
export { looksLikeOpenUIProgram, splitOpenUI } from "./fence";
export type { OpenUIDocument, OpenUIFenceStatus } from "./fence";
export { installOpenUITheme, resolveOpenUIThemeKind } from "./theme";
