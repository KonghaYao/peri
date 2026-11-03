import { afterEach, describe, expect, test } from "bun:test";
import { installDom } from "./dom";

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

const { act, cleanup, fireEvent, render, screen, waitFor } = await import("@testing-library/react");
const { createElement } = await import("react");
const { OpenUIProgram, openuiParseStats } = await import("../../src/openui/renderer");

afterEach(() => {
  cleanup();
  openuiParseStats.count = 0;
  openuiParseStats.maxDurationMs = 0;
});

const renderText = (value: string) => createElement("div", { "data-slot": "peri-markdown" }, value);

interface MountOptions {
  isStreaming?: boolean;
  actionsEnabled?: boolean;
  onAction?: (event: unknown) => void;
}

function programElement(program: string, options: MountOptions = {}) {
  return createElement(OpenUIProgram, {
    program,
    isStreaming: options.isStreaming ?? false,
    actionsEnabled: options.actionsEnabled ?? true,
    fallbackText: program,
    renderText,
    onAction: options.onAction,
  });
}

function mount(program: string, options: MountOptions = {}) {
  return render(programElement(program, options));
}

function update(view: ReturnType<typeof mount>, program: string, options: MountOptions = {}) {
  view.rerender(programElement(program, options));
}

/** 等待超过一个节流窗口，确保 trailing 更新落地。 */
function waitStreamWindow(): Promise<void> {
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 160));
  });
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

  test("渲染 Gauge、ChangedFiles 与 Sources 叶子组件", () => {
    mount(`root = Stack([gauge, changes, sources])
gauge = Gauge(72, "上下文用量", 100, "%")
changes = ChangedFiles([{ path: "src/a.ts", additions: 2, deletions: 1, status: "modified" }])
sources = Sources([{ title: "Peri", url: "https://example.com/peri" }])`);
    expect(screen.getByRole("meter").getAttribute("aria-valuenow")).toBe("72");
    expect(screen.getByText("1 changed file")).toBeTruthy();
    expect(screen.getByText("src/a.ts")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Peri" })).toBeTruthy();
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

  test("流式 partial 在闭合前挂载 Renderer，并随新定义更新", async () => {
    const view = mount("root = Stack([", { isStreaming: true });
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui-skeleton"]')).toBeNull();
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();

    update(view, 'root = Stack([title])\ntitle = Text("标题")', { isStreaming: true });
    await waitFor(() => expect(screen.getByText("标题")).toBeTruthy(), { timeout: 1000 });
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeTruthy();
  });

  test("瞬时 errors/unresolved 不回退原文，保留上一版可渲染程序", async () => {
    const view = mount('root = Stack([title])\ntitle = Text("标题")', { isStreaming: true });
    expect(screen.getByText("标题")).toBeTruthy();

    // table 未定义 + Text 未闭合：瞬时 missing-required，不能回退 Markdown。
    update(view, 'root = Stack([title, table])\ntitle = Text(', { isStreaming: true });
    await waitStreamWindow();
    expect(screen.getByText("标题")).toBeTruthy();
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();

    update(
      view,
      'root = Stack([title, table])\ntitle = Text("标题")\ntable = Table(["模块", "状态"], [["渲染", "完成"]])',
      { isStreaming: true },
    );
    await waitFor(() => expect(screen.getByText("模块")).toBeTruthy(), { timeout: 1000 });
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();
  });

  test("流式解析错误且无上一版时显示内联骨架", async () => {
    mount('root = Stack([t])\nt = Text(', { isStreaming: true });
    await waitStreamWindow();
    expect(document.querySelector('[data-slot="peri-openui-skeleton"]')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();
    expect(document.body.textContent).not.toContain("root = Stack");
  });

  test("流结束仍非法时走严格回退", () => {
    const program = 'root = Stack([t])\nt = Text("hi"';
    const view = mount(program, { isStreaming: true });
    expect(document.querySelector('[data-openui-fallback]')).toBeNull();

    update(view, program, { isStreaming: false });
    expect(document.querySelector('[data-openui-fallback="incomplete-program"]')).toBeTruthy();
    expect(document.querySelector('[data-slot="peri-openui"]')).toBeNull();
  });

  test("流式高频更新按 120ms 合并，不逐字符解析", async () => {
    const view = mount('root = Stack([t])\nt = Text("v1")', { isStreaming: true });
    // 先推进一次，确保后续更新落在同一个节流窗口内。
    update(view, 'root = Stack([t])\nt = Text("v1a")', { isStreaming: true });
    const base = openuiParseStats.count;

    update(view, 'root = Stack([t])\nt = Text("v2")', { isStreaming: true });
    update(view, 'root = Stack([t])\nt = Text("v3")', { isStreaming: true });
    update(view, 'root = Stack([t])\nt = Text("v4")', { isStreaming: true });

    expect(openuiParseStats.count).toBe(base);
    expect(screen.queryByText("v2")).toBeNull();
    expect(screen.queryByText("v3")).toBeNull();

    await waitFor(() => expect(screen.getByText("v4")).toBeTruthy(), { timeout: 1000 });
    expect(openuiParseStats.count).toBe(base + 1);
    expect(screen.queryByText("v2")).toBeNull();
    expect(screen.queryByText("v3")).toBeNull();
  });
});
