import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { openuiLibrary, openuiPromptOptions } from "../src/openui/library";

/**
 * 构建期生成器：`library.toSpec()` + promptOptions -> `src/openui/spec.json`。
 * 服务端只读这个 JSON（经 `@peri-code/sdk/openui/prompt`），不 import 前端库。
 *
 * 失败策略：读取/解析/写出错误直接抛出，不吞；`--check` 不一致时给出可执行命令。
 */
const OUTPUT_PATH = resolve(import.meta.dir, "../src/openui/spec.json");

export function buildOpenUIArtifact() {
  return {
    spec: openuiLibrary.toSpec(),
    promptOptions: openuiPromptOptions,
  };
}

export type OpenUIArtifact = ReturnType<typeof buildOpenUIArtifact>;

export function serializeOpenUIArtifact(artifact = buildOpenUIArtifact()): string {
  return `${JSON.stringify(artifact, null, 2)}\n`;
}

export function writeOpenUISpec(): void {
  mkdirSync(dirname(OUTPUT_PATH), { recursive: true });
  writeFileSync(OUTPUT_PATH, serializeOpenUIArtifact(), "utf8");
}

export function checkOpenUISpec(): void {
  let raw: string;
  try {
    raw = readFileSync(OUTPUT_PATH, "utf8");
  } catch (error) {
    throw new Error(
      `OpenUI spec 不存在或不可读：${OUTPUT_PATH}。先运行 \`bun run openui:spec\`。`,
      { cause: error },
    );
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error(`OpenUI spec 不是合法 JSON：${OUTPUT_PATH}。重新运行 \`bun run openui:spec\`。`, {
      cause: error,
    });
  }

  const expected = JSON.parse(serializeOpenUIArtifact()) as unknown;
  if (!isDeepStrictEqual(parsed, expected)) {
    throw new Error(
      `OpenUI spec 已过期：${OUTPUT_PATH} 与 src/openui/library.tsx 不一致。运行 \`bun run openui:spec\` 并提交生成产物。`,
    );
  }
}

if (import.meta.main) {
  if (process.argv.includes("--check")) {
    checkOpenUISpec();
    console.info(`OpenUI spec 已是最新：${OUTPUT_PATH}`);
  } else {
    writeOpenUISpec();
    console.info(`已写入 OpenUI spec：${OUTPUT_PATH}`);
  }
}
