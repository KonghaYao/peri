import {
  generateSystemPrompt,
  type LibrarySpec,
  type SystemPromptOptions,
} from "@openuidev/lang-core";
import artifact from "./generated/openui.json";

/**
 * 由前端生成的 spec 产物构建 OpenUI 系统提示词。
 *
 * - 不传 `cloud`，本地纯函数生成，不产生 `]]>openui:config`；
 * - 服务端不 import React、canvas 或前端 library；
 * - 生成失败直接抛错，由启动错误路径暴露，不静默降级。
 */
export const OPENUI_SYSTEM_PROMPT = generateSystemPrompt({
  library: artifact.spec as unknown as LibrarySpec,
  promptOptions: artifact.promptOptions as unknown as SystemPromptOptions,
});
