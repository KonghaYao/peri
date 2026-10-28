import { afterEach, describe, expect, test } from "bun:test";
import { installDom } from "../tests/dom";

installDom();

// 最小 ResizeObserver：图表在 happy-dom 中保持 fallback 宽度即可。
class NoopResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
Object.defineProperty(globalThis, "ResizeObserver", {
  value: NoopResizeObserver,
  configurable: true,
  writable: true,
});

const { cleanup, fireEvent, render, screen, waitFor } = await import("@testing-library/react");
const { createElement } = await import("react");
const { OpenUIProgram } = await import("./renderer");

afterEach(() => cleanup());

const renderText = (value: string) => createElement("div", { "data-slot": "peri-markdown" }, value);

function mount(program: string, options: {
  isStreaming?: boolean;
  actionsEnabled?: boolean;
  onAction?: (event: unknown) => void;
} = {}) {
  return render(createElement(OpenUIProgram, {
    program,
    isStreaming: options.isStreaming ?? false,
    actionsEnabled: options.actionsEnabled ?? true,
    fallbackText: program,
    renderText,
    onAction: options.onAction,
  }));
}

describe("OpenUIProgram", () => {
  test("渲染 Stat、Table 与标题", () => {
    mount(`root = Stack([title, stats, table])
title = H3("指标")
stats = Grid(2, [s1, s2])
s1 = Stat("12", "任务")
s2 = Stat("3", "阻塞", "danger")
table = Table(["模块", "状态"], [["渲染", "完成"]])`);
    expect(screen.getByText("指标")).toBeTruthy();
    expect(screen.getByText("12")).toBeTruthy();
    expect(screen.getByText("渲染")).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
  });

  test("未解析引用时回退 Markdown 原文", () => {
    mount('root = Stack([missing])');
    expect(document.querySelector('[data-openui-fallback="validation-failed"]')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.body.textContent).toContain("root = Stack([missing])");
  });

  test("缺失必填 prop 时回退 Markdown 原文", () => {
    mount('root = Stack([t])\nt = Table(["H"])');
    expect(document.querySelector('[data-openui-fallback="validation-failed"]')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.body.textContent).toContain('Table(["H"])');
  });

  test("类型校验失败时回退 Markdown 原文", () => {
    mount('root = Stack([t])\nt = Text(true)');
    expect(document.querySelector('[data-openui-fallback="validation-failed"]')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.body.textContent).toContain("Text(true)");
  });

  test("Button 缺少 action 时按校验失败回退，不发送 label", () => {
    mount('root = Stack([b])\nb = Button("无动作")');
    expect(document.querySelector('[data-openui-fallback="validation-failed"]')).toBeTruthy();
    expect(screen.queryByRole("button", { name: "无动作" })).toBeNull();
    expect(document.body.textContent).toContain('Button("无动作")');
  });

  test("orphaned 语句不影响渲染，不触发回退", () => {
    mount(`root = Stack([t])
t = Text("VISIBLE")
unused = Text("ORPHAN")`);
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
    expect(screen.getByText("VISIBLE")).toBeTruthy();
  });

  test("图表 series 与 categories 长度不一致时显示 warning，不崩溃", () => {
    mount(`root = Stack([chart])
chart = BarChart(["A", "B"], [{ name: "数量", data: [1] }])`);
    expect(screen.getByText("图表数据不完整")).toBeTruthy();
    expect(screen.getByText("分类数与数值数量不一致，已省略该图。")).toBeTruthy();
  });

  test("Table 行长度不一致时按表头截断/补空", () => {
    mount('root = Stack([table])\ntable = Table(["A", "B"], [["only-a"]])');
    const cells = document.querySelectorAll("tbody td");
    expect(cells).toHaveLength(2);
    expect(cells[0]?.textContent).toBe("only-a");
    expect(cells[1]?.textContent).toBe("");
  });

  test("Button 触发 onAction，并携带精简表单值", async () => {
    const events: unknown[] = [];
    mount(`root = Stack([card])
card = Card("表单", [name, submit])
name = TextInput("name", "姓名", "请输入")
submit = Button("提交", @ToAssistant("提交表单"))`, {
      onAction: (event) => events.push(event),
    });

    fireEvent.change(screen.getByPlaceholderText("请输入"), { target: { value: "Ada" } });
    fireEvent.click(screen.getByRole("button", { name: "提交" }));

    await waitFor(() => expect(events).toHaveLength(1));
    expect(events[0]).toMatchObject({
      type: "continue_conversation",
      humanFriendlyMessage: "提交表单",
      formState: { name: { value: "Ada" } },
    });
  });

  test("actionsEnabled=false 时 Button 禁用且不触发 action", () => {
    const events: unknown[] = [];
    mount('root = Stack([submit])\nsubmit = Button("提交", @ToAssistant("提交"))', {
      actionsEnabled: false,
      onAction: (event) => events.push(event),
    });
    const button = screen.getByRole("button", { name: "提交" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    expect(events).toHaveLength(0);
  });

  test("isStreaming=true 时表单控件与按钮禁用", () => {
    mount(`root = Stack([input, submit])
input = TextInput("name", "姓名", "请输入")
submit = Button("提交", @ToAssistant("提交"))`, { isStreaming: true });
    expect((screen.getByPlaceholderText("请输入") as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "提交" }) as HTMLButtonElement).disabled).toBe(true);
  });

  test("Text 转义 HTML，Link 拒绝 javascript:", () => {
    mount(`root = Stack([text, link])
text = Text("<img src=x onerror=alert(1)>")
link = Link("坏链接", "javascript:alert(1)")`);
    expect(document.querySelector("img")).toBeNull();
    expect(screen.getByText("<img src=x onerror=alert(1)>")).toBeTruthy();
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(screen.getByText("坏链接").getAttribute("title")).toContain("Blocked non-http(s) link");
  });
});
