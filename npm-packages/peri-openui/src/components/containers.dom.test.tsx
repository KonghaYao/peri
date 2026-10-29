import { afterEach, describe, expect, test } from "bun:test";
import type { ComponentType } from "react";
import { installDom } from "../../tests/dom";

installDom();

const { cleanup, render, screen } = await import("@testing-library/react");
const { createElement } = await import("react");
const { ChangedFilesView, GaugeView, SourcesView } = await import("./containers");

afterEach(() => cleanup());

const renderNode = () => null;

function renderView<P>(component: ComponentType<{ props: P; renderNode: typeof renderNode }>, props: P) {
  return render(createElement(component, { props, renderNode }));
}

function ringWidth(meter: HTMLElement): string {
  return (meter.firstElementChild as HTMLElement).style.width;
}

describe("GaugeView", () => {
  test("正常值暴露 meter 语义与中心文本", () => {
    renderView(GaugeView, {
      value: 42,
      label: "Context used",
      max: 200,
      unit: "k",
      detail: "84k / 200k tokens",
      tone: "info",
    });
    const meter = screen.getByRole("meter");
    expect(meter.getAttribute("aria-label")).toBe("Context used");
    expect(meter.getAttribute("aria-valuemin")).toBe("0");
    expect(meter.getAttribute("aria-valuemax")).toBe("200");
    expect(meter.getAttribute("aria-valuenow")).toBe("42");
    expect(meter.getAttribute("aria-valuetext")).toBe("42k of 200k");
    expect(screen.getByText("42")).toBeTruthy();
    expect(screen.getByText("Context used")).toBeTruthy();
    expect(screen.getByText("84k / 200k tokens")).toBeTruthy();
  });

  test("value 超过 max 时裁剪 aria-valuenow，但保留真实值", () => {
    renderView(GaugeView, { value: 150, label: "Overflow", unit: "%" });
    const meter = screen.getByRole("meter");
    expect(meter.getAttribute("aria-valuenow")).toBe("100");
    expect(meter.getAttribute("aria-valuetext")).toBe("150% of 100%");
    expect(screen.getByText("150")).toBeTruthy();
  });

  test("value 小于 0 时裁剪 aria-valuenow，但保留真实值", () => {
    renderView(GaugeView, { value: -5, label: "Negative", unit: "%" });
    const meter = screen.getByRole("meter");
    expect(meter.getAttribute("aria-valuenow")).toBe("0");
    expect(meter.getAttribute("aria-valuetext")).toBe("-5% of 100%");
    expect(screen.getByText("-5")).toBeTruthy();
  });

  test("max=0 显示可见警告而不是伪造 0", () => {
    renderView(GaugeView, { value: 10, label: "Broken", max: 0 });
    expect(screen.queryByRole("meter")).toBeNull();
    expect(screen.getByRole("status")).toBeTruthy();
    expect(screen.getByText("Gauge unavailable")).toBeTruthy();
    expect(screen.getByText(/max 0 must be greater than 0/)).toBeTruthy();
  });

  test("NaN value 显示可见警告", () => {
    renderView(GaugeView, { value: Number.NaN, label: "NaN gauge" });
    expect(screen.queryByRole("meter")).toBeNull();
    expect(screen.getByText(/must be a finite number/)).toBeTruthy();
  });

  test("长 label 不破坏 meter 语义", () => {
    const label = "非常长的标签".repeat(20);
    renderView(GaugeView, { value: 50, label });
    expect(screen.getByRole("meter").getAttribute("aria-label")).toBe(label);
    expect(screen.getByText(label)).toBeTruthy();
  });

  test("size 收敛到 72–160", () => {
    const first = renderView(GaugeView, { value: 10, label: "Tiny", size: 40 });
    expect(ringWidth(screen.getByRole("meter"))).toBe("72px");
    first.unmount();

    renderView(GaugeView, { value: 10, label: "Huge", size: 999 });
    expect(ringWidth(screen.getByRole("meter"))).toBe("160px");
  });
});

describe("ChangedFilesView", () => {
  test("空数组显示 No file changes", () => {
    renderView(ChangedFilesView, { files: [], title: "本次改动" });
    expect(screen.getByText("本次改动")).toBeTruthy();
    expect(screen.getByText("0 changed files")).toBeTruthy();
    expect(screen.getByText("No file changes")).toBeTruthy();
  });

  test("单文件显示头部计数、状态与 DiffStats", () => {
    const { container } = renderView(ChangedFilesView, {
      files: [{ path: "README.md", additions: 5, deletions: 2, status: "modified" }],
    });
    expect(screen.getByText("1 changed file")).toBeTruthy();
    expect(screen.getByText("/")).toBeTruthy();
    expect(screen.getByText("README.md")).toBeTruthy();
    expect(screen.getByText("modified")).toBeTruthy();
    const groups = screen.getAllByRole("group");
    expect(groups).toHaveLength(2);
    expect(groups[0]?.getAttribute("aria-label")).toBe("5 additions, 2 deletions");
    expect(screen.getAllByText("+5")).toHaveLength(2);
    expect(screen.getAllByText("-2")).toHaveLength(2);
    expect(container.textContent).not.toContain("1 changed files");
  });

  test("按目录分组并保留首次出现顺序", () => {
    renderView(ChangedFilesView, {
      files: [
        { path: "src/lib/a.ts", additions: 1, status: "added" },
        { path: "src/lib/b.ts", deletions: 2 },
        { path: "docs/readme.md", additions: 3 },
      ],
    });
    expect(screen.getByText("3 changed files")).toBeTruthy();
    expect(screen.getByText("src/lib/")).toBeTruthy();
    expect(screen.getByText("docs/")).toBeTruthy();
    expect(screen.getByText("src/lib/a.ts")).toBeTruthy();
    expect(screen.getByText("docs/readme.md")).toBeTruthy();
    expect(screen.getByText("+4")).toBeTruthy();
    expect(screen.getAllByText("-2")).toHaveLength(2);
  });

  test("Windows 分隔符归一化为 /", () => {
    const { container } = renderView(ChangedFilesView, {
      files: [{ path: "src\\canvas\\gauge.tsx", additions: 7, status: "added" }],
    });
    expect(screen.getByText("src/canvas/gauge.tsx")).toBeTruthy();
    expect(screen.getByText("src/canvas/")).toBeTruthy();
    expect(container.textContent).not.toContain("\\");
  });

  test("超过 maxRows 显示 +N more files，不渲染被截断行", () => {
    const files = Array.from({ length: 5 }, (_, index) => ({
      path: `src/f${index + 1}.ts`,
      additions: index + 1,
    }));
    renderView(ChangedFilesView, { files, maxRows: 2 });
    expect(screen.getByText("+3 more files")).toBeTruthy();
    expect(screen.getByText("src/f1.ts")).toBeTruthy();
    expect(screen.getByText("src/f2.ts")).toBeTruthy();
    expect(screen.queryByText("src/f3.ts")).toBeNull();
  });

  test("maxRows 默认 50，硬上限 200", () => {
    const files = Array.from({ length: 205 }, (_, index) => ({
      path: `src/file-${index + 1}.ts`,
    }));
    const first = renderView(ChangedFilesView, { files });
    expect(screen.getByText("+155 more files")).toBeTruthy();
    expect(screen.queryByText("src/file-51.ts")).toBeNull();
    first.unmount();

    renderView(ChangedFilesView, { files, maxRows: 999 });
    expect(screen.getByText("+5 more files")).toBeTruthy();
    expect(screen.getByText("src/file-200.ts")).toBeTruthy();
    expect(screen.queryByText("src/file-201.ts")).toBeNull();
  });

  test("0 增删不渲染 +0/-0", () => {
    renderView(ChangedFilesView, {
      files: [{ path: "unchanged-counts.ts" }],
    });
    expect(screen.getByText("unchanged-counts.ts")).toBeTruthy();
    expect(screen.queryByText("+0")).toBeNull();
    expect(screen.queryByText("-0")).toBeNull();
    expect(screen.queryAllByRole("group")).toHaveLength(0);
  });

  test("超长路径保留完整文本并用 title 提示", () => {
    const path = `src/${"nested/".repeat(40)}${"a".repeat(200)}.ts`;
    renderView(ChangedFilesView, { files: [{ path, additions: 1 }] });
    const row = screen.getByText(path);
    expect(row.getAttribute("title")).toBe(path);
  });
});

describe("SourcesView", () => {
  test("合法 URL 渲染链接、hostname、序号与 snippet", () => {
    renderView(SourcesView, {
      items: [
        {
          title: "Peri docs",
          url: "https://example.com/peri",
          snippet: "Agent loop overview",
        },
      ],
    });
    expect(screen.getByText("Sources")).toBeTruthy();
    expect(screen.getByText("1.")).toBeTruthy();
    const link = screen.getByRole("link", { name: "Peri docs" });
    expect(link.getAttribute("href")).toBe("https://example.com/peri");
    expect(link.getAttribute("target")).toBe("_blank");
    expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    expect(screen.getByText("example.com")).toBeTruthy();
    expect(screen.getByText("Agent loop overview")).toBeTruthy();
  });

  test("非法协议渲染为纯文本并带 title 提示", () => {
    renderView(SourcesView, { items: [{ title: "Bad source", url: "javascript:alert(1)" }] });
    expect(screen.queryByRole("link")).toBeNull();
    const blocked = screen.getByText("Bad source");
    expect(blocked.getAttribute("title")).toContain("Blocked URL");
    expect(blocked.getAttribute("data-blocked-url")).toBe("true");
    expect(screen.getByText("invalid URL")).toBeTruthy();
  });

  test("userinfo 被拒绝且不回显凭据", () => {
    renderView(SourcesView, {
      items: [{ title: "Credential source", url: "https://user:sekret@example.com/path" }],
    });
    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.getByText("Credential source")).toBeTruthy();
    expect(document.body.textContent).not.toContain("sekret");
  });

  test("超过 2048 字符的 URL 被拒绝", () => {
    const url = `https://example.com/${"a".repeat(2048)}`;
    renderView(SourcesView, { items: [{ title: "Long URL", url }] });
    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.getByText("Long URL")).toBeTruthy();
  });

  test("重复 URL 不去重，按条目渲染", () => {
    renderView(SourcesView, {
      items: [
        { title: "First", url: "https://same.example/x" },
        { title: "Second", url: "https://same.example/x" },
      ],
    });
    expect(screen.getAllByRole("link")).toHaveLength(2);
  });

  test("空标题回退到 hostname", () => {
    renderView(SourcesView, {
      items: [{ title: "", url: "https://example.com/path" }],
    });
    expect(screen.getByRole("link", { name: "example.com" })).toBeTruthy();
  });

  test("空数组显示 No sources 并使用自定义标题", () => {
    renderView(SourcesView, { items: [], title: "参考资料" });
    expect(screen.getByText("参考资料")).toBeTruthy();
    expect(screen.getByText("No sources")).toBeTruthy();
  });
});
