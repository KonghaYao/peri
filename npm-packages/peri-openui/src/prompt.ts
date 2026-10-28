import {
  generateSystemPrompt,
  type LibrarySpec,
  type SystemPromptOptions,
} from "@openuidev/lang-core";
import artifact from "../spec.json";

/**
 * 服务端出口：只读提交入库的 `spec.json`，不 import React、canvas 或 library。
 *
 * - 不传 `cloud`，本地纯函数生成，不产生 `]]>openui:config`；
 * - 生成失败直接抛错，由启动错误路径暴露，不静默降级。
 */
export const OPENUI_ARTIFACT = artifact;

export function generateOpenUISystemPrompt(): string {
  return generateSystemPrompt({
    library: artifact.spec as unknown as LibrarySpec,
    promptOptions: artifact.promptOptions as unknown as SystemPromptOptions,
  });
}
