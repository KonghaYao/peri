import { afterEach, describe, expect, test } from "bun:test";
import { installDom } from "../../tests/dom";

installDom();

const { cleanup, render, screen } = await import("@testing-library/react");
const { createElement } = await import("react");
const { DiffStats, formatCompactDiffCount } = await import("./diff-view");

afterEach(() => cleanup());

describe("DiffStats", () => {
  test("默认 inline 布局保留非零项与 aria-label", () => {
    const { container } = render(createElement(DiffStats, { additions: 1500, deletions: 0 }));
    expect(screen.getByText("+1.5k")).toBeTruthy();
    expect(screen.queryByText("-0")).toBeNull();
    const group = screen.getByRole("group");
    expect(group.getAttribute("aria-label")).toBe("1500 additions, 0 deletions");
    expect((group as HTMLElement).style.display).toBe("inline-flex");
    expect(container.textContent).toBe("+1.5k");
  });

  test("aligned 布局补齐 0 值以保证两列对齐", () => {
    render(createElement(DiffStats, { additions: 0, deletions: 12345, layout: "aligned" }));
    expect(screen.getByText("+0")).toBeTruthy();
    expect(screen.getByText("-12k")).toBeTruthy();
    const group = screen.getByRole("group");
    expect(group.getAttribute("aria-label")).toBe("0 additions, 12345 deletions");
    expect((group as HTMLElement).style.display).toBe("inline-grid");
  });

  test("0/0 保持不渲染", () => {
    const { container } = render(createElement(DiffStats, { additions: 0, deletions: 0 }));
    expect(container.textContent).toBe("");
    expect(screen.queryByRole("group")).toBeNull();
  });

  test("showParentheses 在分组外包裹括号", () => {
    const { container } = render(
      createElement(DiffStats, { additions: 5, deletions: 2, showParentheses: true }),
    );
    expect(container.textContent).toBe("(+5-2)");
  });

  test("非有限计数不渲染为 NaN", () => {
    const { container } = render(
      createElement(DiffStats, { additions: Number.NaN, deletions: 3 }),
    );
    expect(container.textContent).toBe("-3");
    expect(screen.getByRole("group").getAttribute("aria-label")).toBe("0 additions, 3 deletions");
  });

  test("紧凑格式与 DiffStatLabel 规则一致", () => {
    expect(formatCompactDiffCount(0)).toBe("0");
    expect(formatCompactDiffCount(999)).toBe("999");
    expect(formatCompactDiffCount(1000)).toBe("1k");
    expect(formatCompactDiffCount(1500)).toBe("1.5k");
    expect(formatCompactDiffCount(12345)).toBe("12k");
    expect(formatCompactDiffCount(1_000_000)).toBe("1m");
    expect(formatCompactDiffCount(2_500_000_000)).toBe("2.5b");
    expect(formatCompactDiffCount(-5)).toBe("0");
    expect(formatCompactDiffCount(Number.NaN)).toBe("0");
  });
});
