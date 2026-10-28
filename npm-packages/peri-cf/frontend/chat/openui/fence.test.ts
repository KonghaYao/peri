import { describe, expect, test } from "bun:test";
import { looksLikeOpenUIProgram, splitOpenUI } from "./fence";

const PROGRAM = 'root = Stack([title])\ntitle = Text("你好")';

describe("splitOpenUI", () => {
  test("闭合的 openui 围栏切出前后 Markdown", () => {
    const doc = splitOpenUI(`前文\n\`\`\`openui\n${PROGRAM}\n\`\`\`\n后文`, false);
    expect(doc.status).toBe("complete");
    expect(doc.complete).toBe(true);
    expect(doc.before.trim()).toBe("前文");
    expect(doc.program).toBe(PROGRAM);
    expect(doc.after.trim()).toBe("后文");
  });

  test("接受 openui-lang 与 ~~~openui", () => {
    expect(splitOpenUI(`\`\`\`openui-lang\n${PROGRAM}\n\`\`\``, false).program).toBe(PROGRAM);
    expect(splitOpenUI(`~~~openui\n${PROGRAM}\n~~~`, false).program).toBe(PROGRAM);
  });

  test("未闭合围栏返回 open，不泄露半截程序", () => {
    const doc = splitOpenUI(`前言\n\`\`\`openui\n${PROGRAM}`, true);
    expect(doc.status).toBe("open");
    expect(doc.program).toBeNull();
    expect(doc.before.trim()).toBe("前言");
  });

  test("闭合长度短于开启时不关闭", () => {
    const doc = splitOpenUI(`\`\`\`\`openui\n${PROGRAM}\n\`\`\``, false);
    expect(doc.status).toBe("open");
    expect(doc.program).toBeNull();
  });

  test("多个 OpenUI 围栏整体回退 Markdown", () => {
    const text = `\`\`\`openui\n${PROGRAM}\n\`\`\`\n\n\`\`\`openui\n${PROGRAM}\n\`\`\``;
    const doc = splitOpenUI(text, false);
    expect(doc.status).toBe("multiple");
    expect(doc.program).toBeNull();
    expect(doc.before).toBe(text);
  });

  test("普通 text 围栏不触发", () => {
    const text = "```text\nroot = Stack([t])\n```";
    expect(splitOpenUI(text, false)).toMatchObject({ status: "none", program: null, before: text });
  });

  test("外层 text 代码块内的 openui 源码保持可见", () => {
    const text = [
      "````text",
      "```openui",
      PROGRAM,
      "```",
      "````",
    ].join("\n");
    const doc = splitOpenUI(text, false);
    expect(doc.status).toBe("none");
    expect(doc.program).toBeNull();
    expect(doc.before).toBe(text);
  });

  test("外层三反引号 text 代码块内的 openui 不挂载 UI", () => {
    const text = ["```text", "```openui", PROGRAM, "```"].join("\n");
    const doc = splitOpenUI(text, false);
    expect(doc.status).toBe("none");
    expect(doc.program).toBeNull();
    expect(doc.before).toBe(text);
  });

  test("普通代码块内外的 OpenUI 围栏边界保持不变", () => {
    const prefix = [
      "````markdown",
      "```openui",
      PROGRAM,
      "```",
      "````",
      "中间",
      "",
    ].join("\n");
    const suffix = ["````text", "```openui", "```", "````"].join("\n");
    const text = `${prefix}\`\`\`openui\n${PROGRAM}\n\`\`\`\n${suffix}`;
    const doc = splitOpenUI(text, false);
    expect(doc.status).toBe("complete");
    expect(doc.before).toBe(prefix);
    expect(doc.after).toBe(suffix);
  });

  test("列表项缩进的 openui 围栏不挂载 UI", () => {
    const text = `- 示例：\n  \`\`\`openui\n${PROGRAM}\n  \`\`\`\n- 结束`;
    expect(splitOpenUI(text, false)).toMatchObject({ status: "none", program: null, before: text });
  });

  test("普通代码块内的 openui 不算多个围栏", () => {
    const text = [
      "````text",
      "```openui",
      "```",
      "````",
      "```openui",
      PROGRAM,
      "```",
    ].join("\n");
    expect(splitOpenUI(text, false)).toMatchObject({ status: "complete", program: PROGRAM });
  });

  test("流结束后的裸 DSL 作为整段程序", () => {
    expect(splitOpenUI(PROGRAM, false)).toMatchObject({
      status: "none", program: PROGRAM, complete: true, before: "",
    });
  });

  test("流式中的裸 DSL 保持 Markdown", () => {
    expect(splitOpenUI(PROGRAM, true)).toMatchObject({ status: "none", program: null, before: PROGRAM });
  });

  test("普通 Markdown 以 root = 开头但不匹配 Stack 时不识别", () => {
    const text = "root = computeSomething(value)";
    expect(looksLikeOpenUIProgram(text)).toBe(false);
    expect(splitOpenUI(text, false).program).toBeNull();
  });

  test("CRLF 围栏同样可切分", () => {
    const text = `前文\r\n\`\`\`openui\r\n${PROGRAM.replace(/\n/g, "\r\n")}\r\n\`\`\`\r\n后文`;
    const doc = splitOpenUI(text, false);
    expect(doc.status).toBe("complete");
    expect(doc.program?.replace(/\r\n/g, "\n")).toBe(PROGRAM);
  });
});
