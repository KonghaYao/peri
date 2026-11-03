import { afterEach, describe, expect, test } from "bun:test";
import { installDom } from "./dom";

installDom();

const { cleanup, render, screen } = await import("@testing-library/react");
const { createElement } = await import("react");
const { Stat, Stack, Text } = await import("../../src/openui/canvas");
const { getHostTheme, setHostTheme } = await import("../../src/openui/canvas/hooks");
const { installOpenUITheme, resolveOpenUIThemeKind } = await import("../../src/openui/theme");
const openuiEntry = await import("../../src/openui/index");

afterEach(() => {
  cleanup();
  document.documentElement.className = "";
});

describe("openui theme", () => {
  test("显式 .light/.dark 优先于系统偏好", () => {
    const root = document.createElement("div");
    root.className = "light";
    expect(resolveOpenUIThemeKind(root, true)).toBe("light");
    root.className = "dark";
    expect(resolveOpenUIThemeKind(root, false)).toBe("dark");
    root.className = "";
    expect(resolveOpenUIThemeKind(root, true)).toBe("dark");
    expect(resolveOpenUIThemeKind(root, false)).toBe("light");
  });

  test("installOpenUITheme 从 CSS 变量回退值构建 canvas token", async () => {
    document.documentElement.className = "light";
    const uninstall = installOpenUITheme();
    expect(getHostTheme().kind).toBe("light");
    expect(getHostTheme().tokens.bg.editor).toBe("#f9fafb");
    expect(getHostTheme().tokens.text.primary).toBe("#0f1115");
    expect(getHostTheme().tokens.accent.primary).toBe("#4176e6");

    document.documentElement.className = "dark";
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(getHostTheme().kind).toBe("dark");
    expect(getHostTheme().tokens.bg.editor).toBe("#1b1b1c");
    uninstall();
  });

  test("入口导出骨架：installOpenUITheme 可从 index 取得", () => {
    expect(openuiEntry.installOpenUITheme).toBe(installOpenUITheme);
    expect(typeof openuiEntry.resolveOpenUIThemeKind).toBe("function");
  });

  test("Card/Text/Stat 使用当前 theme token", () => {
    setHostTheme({
      kind: "light",
      editorBackground: "#f9fafb",
      editorForeground: "#0f1115",
      primary: "#4176e6",
    });
    const { tokens } = getHostTheme();
    render(
      createElement(
        Stack,
        null,
        createElement(Text, null, "hello"),
        createElement(Stat, { value: "42", label: "answer" }),
      ),
    );
    const text = screen.getByText("hello");
    const stat = screen.getByText("42");
    expect(text.style.color).toBe(tokens.text.primary);
    expect(stat.style.color).toBe(tokens.text.primary);
    expect(stat.textContent).toBe("42");
  });
});
