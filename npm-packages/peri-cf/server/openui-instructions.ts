import { generateOpenUISystemPrompt } from "@peri-code/sdk/openui/prompt";

/**
 * 服务端系统提示词：只经 `@peri-code/sdk/openui/prompt` 读取生成的 spec。
 *
 * - 不传 `cloud`，本地纯函数生成，不产生 `]]>openui:config`；
 * - 服务端不 import React、canvas 或前端 library；
 * - 生成失败直接抛错，由启动错误路径暴露，不静默降级。
 */
export const OPENUI_SYSTEM_PROMPT = generateOpenUISystemPrompt();
