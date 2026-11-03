import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { installDom } from "../../test/dom";

installDom();

const { cleanup, render, screen, waitFor } = await import("@testing-library/react");
const { createElement } = await import("react");
const { OpenUIMessage } = await import("./OpenUIMessage");
const { OpenUIErrorBoundary } = await import("@peri-code/sdk/openui/error-boundary");

afterEach(() => cleanup());

// 不依赖 timeline 模块：用最小 Markdown 替身即可验证回退与切分。
const renderMarkdown = (value: string) => createElement("div", { "data-slot": "peri-markdown" }, value);

function renderMessage(text: string, streaming = false) {
  return render(createElement(OpenUIMessage, { text, streaming, renderMarkdown }));
}

const PROGRAM = 'root = Stack([title])\ntitle = Text("OpenUI 内容")';

describe("OpenUIMessage", () => {
  test("普通 Markdown 原样走 renderMarkdown", () => {
    renderMessage("普通回答");
    expect(document.querySelector('[data-slot="peri-markdown"]')?.textContent).toContain("普通回答");
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
  });

  test("外层 text 代码块内的 openui 源码可见且不挂载 UI", () => {
    renderMessage(["````text", "```openui", PROGRAM, "```", "````"].join("\n"));
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.body.textContent).toContain("root = Stack");
    expect(document.body.textContent).toContain("```openui");
  });

  test("完整围栏挂载 Renderer", async () => {
    renderMessage(`前文\n\`\`\`openui\n${PROGRAM}\n\`\`\`\n后文`);
    expect(await screen.findByText("OpenUI 内容")).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
    expect(document.body.textContent).toContain("前文");
    expect(document.body.textContent).toContain("后文");
  });

  test("流式中未闭合围栏在闭合前就挂载 Renderer", async () => {
    renderMessage(`\`\`\`openui\n${PROGRAM}`, true);
    expect(await screen.findByText("OpenUI 内容")).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();
    expect(document.body.textContent).not.toContain("root = Stack");
  });

  test("流式未闭合围栏保留 before，且只渲染一次", async () => {
    renderMessage(`前文\n\`\`\`openui\n${PROGRAM}`, true);
    expect(await screen.findByText("OpenUI 内容")).toBeTruthy();
    expect(screen.getAllByText("前文")).toHaveLength(1);
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
  });

  test("流式逐步更新：闭合前出现 UI，闭合后不回退不重复", async () => {
    const view = renderMessage("```openui\n", true);
    expect(screen.getByText("正在生成界面…")).toBeTruthy();

    const partial = `\`\`\`openui\n${PROGRAM}`;
    view.rerender(createElement(OpenUIMessage, { text: partial, streaming: true, renderMarkdown }));
    expect(await screen.findByText("OpenUI 内容")).toBeTruthy();
    expect(document.querySelectorAll('[data-slot="peri-openui"]')).toHaveLength(1);

    const closed = `\`\`\`openui\n${PROGRAM}\n\`\`\``;
    view.rerender(createElement(OpenUIMessage, { text: closed, streaming: false, renderMarkdown }));
    await waitFor(() => expect(document.querySelectorAll('[data-slot="peri-openui"]')).toHaveLength(1));
    expect(screen.getAllByText("OpenUI 内容")).toHaveLength(1);
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();
    expect(document.querySelector('[data-slot="peri-openui-skeleton"]')).toBeNull();
  });

  test("流结束仍未闭合时回退原始 Markdown", () => {
    renderMessage(`\`\`\`openui\n${PROGRAM}`, false);
    expect(document.querySelector('[data-openui-fallback="incomplete-fence"]')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.body.textContent).toContain("root = Stack");
    expect(document.body.textContent).toContain("OpenUI 内容");
  });

  test("解析失败回退 Markdown 并标记 parse-failed", async () => {
    renderMessage('```openui\nroot = Nope("x")\n```');
    await waitFor(() => expect(document.querySelector('[data-openui-fallback="parse-failed"]')).toBeTruthy());
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.querySelector('[data-openui-fallback="parse-failed"]')?.textContent).toContain("root = Nope");
  });

  test("空围栏回退 parse-failed 且原文可见", async () => {
    renderMessage("```openui\n\n```");
    await waitFor(() => expect(document.querySelector('[data-openui-fallback="parse-failed"]')).toBeTruthy());
    expect(document.body.textContent).toContain("openui");
  });

  test("闭合围栏内程序不完整时标记 incomplete-program", async () => {
    renderMessage('```openui\nroot = Stack([t])\nt = Text("hi"\n```');
    await waitFor(() => expect(document.querySelector('[data-openui-fallback="incomplete-program"]')).toBeTruthy());
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
  });

  test("多个围栏整体回退 Markdown", () => {
    renderMessage(`\`\`\`openui\n${PROGRAM}\n\`\`\`\n\n\`\`\`openui\n${PROGRAM}\n\`\`\``);
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();
  });

  test("流式中完整围栏走严格路径，按钮按完整程序启用", async () => {
    renderMessage('```openui\nroot = Stack([submit])\nsubmit = Button("提交", @ToAssistant("提交"))\n```', true);
    const button = await screen.findByRole("button", { name: "提交" });
    expect((button as HTMLButtonElement).disabled).toBe(false);
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
  });

  test("error boundary 捕获渲染异常并回退原文", async () => {
    const error = spyOn(console, "error").mockImplementation(() => {});
    const Thrower = () => {
      throw new Error("render boom");
    };
    render(
      createElement(OpenUIErrorBoundary, {
        fallback: renderMarkdown("原始 Markdown"),
        children: createElement(Thrower),
      }),
    );
    await waitFor(() => expect(document.querySelector('[data-openui-fallback="render-error"]')).toBeTruthy());
    expect(document.body.textContent).toContain("原始 Markdown");
    error.mockRestore();
  });
});
