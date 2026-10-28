/**
 * OpenUI 围栏切分：只做 O(n) 行扫描，不解析内部 DSL。
 *
 * 契约：
 * - OpenUI 开启围栏必须顶层（0 缩进）；```openui / ```openui-lang / ~~~openui；
 * - 扫描同时跟踪所有普通代码围栏（0-3 空格缩进），普通围栏内的 `openui` 只算内容；
 * - 只接受一个顶层 OpenUI 围栏，多个则整体回退 Markdown；
 * - 流式未闭合时不返回半截程序；
 * - 流结束后的裸 DSL 只在以 `root = Stack(` 开头时识别。
 */
export interface OpenUIDocument {
  /** 围栏之前的 Markdown 原文。 */
  before: string;
  /** 可渲染的完整程序；null 表示不挂载 Renderer。 */
  program: string | null;
  /** 围栏之后的 Markdown 原文。 */
  after: string;
  /** 是否得到稳定、完整的程序。 */
  complete: boolean;
  /** 围栏状态；用于区分“无特征”和“未闭合/多围栏”的回退原因。 */
  status: OpenUIFenceStatus;
}

export type OpenUIFenceStatus = "none" | "open" | "complete" | "multiple";

/** OpenUI 围栏必须顶层（0 缩进），避免列表项/引用块内的示例被当成 UI。 */
const OPENUI_OPENER = /^(`{3,}|~{3,})[ \t]*(openui|openui-lang)[ \t]*$/;
/** 普通代码围栏：0-3 空格缩进，info string 任意。 */
const ANY_OPENER = /^ {0,3}(`{3,}|~{3,})[ \t]*(.*)$/;
/** 关闭围栏：只允许围栏字符与空白。 */
const FENCE_CLOSER = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
const BARE_PROGRAM = /^\s*root\s*=\s*Stack\s*\(/;

/** 裸 DSL 识别：仅用于流结束后的整段程序。 */
export function looksLikeOpenUIProgram(text: string): boolean {
  return BARE_PROGRAM.test(text);
}

type ScanMode =
  | { kind: "idle" }
  | { kind: "generic"; char: string; length: number }
  | {
      kind: "openui";
      char: string;
      length: number;
      lineStart: number;
      contentStart: number;
    };

function lineText(raw: string): string {
  return raw.endsWith("\r") ? raw.slice(0, -1) : raw;
}

function closesFence(line: string, char: string, minLength: number): boolean {
  const close = FENCE_CLOSER.exec(line);
  return close !== null && close[1][0] === char && close[1].length >= minLength;
}

export function splitOpenUI(text: string, streaming: boolean): OpenUIDocument {
  const lines = text.split("\n");
  let offset = 0;
  let mode: ScanMode = { kind: "idle" };
  let block: Pick<OpenUIDocument, "before" | "program" | "after"> | null = null;
  let multiple = false;

  for (const raw of lines) {
    const line = lineText(raw);
    const lineStart = offset;
    offset += raw.length + 1;

    if (mode.kind === "generic") {
      if (closesFence(line, mode.char, mode.length)) mode = { kind: "idle" };
      continue;
    }

    if (mode.kind === "openui") {
      if (closesFence(line, mode.char, mode.length)) {
        block = {
          before: text.slice(0, mode.lineStart),
          program: text.slice(mode.contentStart, lineStart).trim(),
          after: text.slice(offset),
        };
        mode = { kind: "idle" };
      }
      continue;
    }

    const openui = OPENUI_OPENER.exec(line);
    if (openui) {
      if (block) {
        multiple = true;
        break;
      }
      mode = {
        kind: "openui",
        char: openui[1][0],
        length: openui[1].length,
        lineStart,
        contentStart: offset,
      };
      continue;
    }

    const any = ANY_OPENER.exec(line);
    if (any) {
      mode = { kind: "generic", char: any[1][0], length: any[1].length };
    }
  }

  if (multiple) {
    return { before: text, program: null, after: "", complete: false, status: "multiple" };
  }
  if (mode.kind === "openui") {
    return {
      before: text.slice(0, mode.lineStart),
      program: null,
      after: "",
      complete: false,
      status: "open",
    };
  }
  if (block) {
    return { ...block, complete: true, status: "complete" };
  }
  if (!streaming && looksLikeOpenUIProgram(text)) {
    return { before: "", program: text.trim(), after: "", complete: true, status: "none" };
  }
  return { before: text, program: null, after: "", complete: false, status: "none" };
}
