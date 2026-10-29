import { realpathSync } from "node:fs";
import { mock } from "bun:test";
import * as React from "react";

/**
 * 测试期 React 去重。
 *
 * `@peri-code/sdk` 以 `file:` 依赖链接进 node_modules，其 OpenUI 源码自带 devDependencies React。
 * Bun 测试没有 Vite 的 `resolve.dedupe`；两份 React 会让 Renderer 的 hooks 失效。
 *
 * 作为 test preload 在导入任何被测模块前执行：既按裸名注册，也按 OpenUI 源码
 * 实际解析到的绝对路径注册，覆盖 SDK 内部的 `import ... from "react"`。
 * 浏览器构建由 `vite.config.ts` 的 `resolve.dedupe` 处理。
 */
const openuiSrc = realpathSync(
  new URL("../../node_modules/@peri-code/sdk/src/openui", import.meta.url).pathname,
);
const openuiReact = Bun.resolveSync("react", openuiSrc);

mock.module("react", () => React);
mock.module(openuiReact, () => React);
