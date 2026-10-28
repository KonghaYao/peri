import { afterEach, describe, expect, test } from "bun:test";
import { installDom } from "../test/dom";
import type { MonitorRepository, MonitorSnapshot } from "../remote";

installDom();

const { act, cleanup, fireEvent, render, screen, waitFor, within } =
  await import("@testing-library/react");
const { createElement, useState } = await import("react");
const { MonitorDialog } = await import("./MonitorDialog");
const { formatBytes } = await import("./monitor/format");

afterEach(() => cleanup());

const ID = "sbx_0123456789abcdef0123456789abcdef";

function snapshot(): MonitorSnapshot {
  return {
    ok: true,
    generatedAt: "2026-10-27T10:00:00.000Z",
    uptimeSeconds: 3725,
    llm: {
      provider: "openai",
      activeAlias: "sonnet",
      baseUrl: "https://api.example.com/v1",
      tiers: { fable: "gpt-fable", opus: "gpt-opus", sonnet: "gpt-sonnet", haiku: "gpt-haiku" },
    },
    mcp: {
      sandboxEnabled: true,
      remote: [{
        name: "remote",
        url: "https://mcp.example.com/mcp",
        headerNames: ["Authorization", "X-Test"],
      }],
      sandbox: [{ sandboxId: ID, name: "workspace", url: "http://127.0.0.1:9000/mcp", state: "ready" }],
    },
    wasm: {
      moduleUrl: "/opt/peri/peri-wasm.js",
      process: {
        rssBytes: 128 * 1024 * 1024,
        heapTotalBytes: 30 * 1024 * 1024,
        heapUsedBytes: 10 * 1024 * 1024,
        arrayBufferBytes: 64 * 1024 * 1024,
      },
      activeChats: 2,
    },
    sandbox: {
      enabled: true,
      root: "/sandboxes",
      settings: {
        path: "/etc/peri/sandbox.json",
        source: "file",
        profileName: "default",
        profile: {
          memoryMax: "1G",
          tasksMax: 128,
          cpuQuota: "100%",
          allowedDomains: ["github.com", "registry.npmjs.org"],
          allowedDomainsTotal: 2,
          runtimeReadPathsCount: 3,
        },
      },
      workspacesOnDisk: 4,
      active: [{
        sandboxId: ID,
        state: "ready",
        refs: 2,
        startedAt: "2026-10-27T09:00:00.000Z",
        workspacePath: `/sandboxes/${ID}/workspace`,
        mcpUrl: "http://127.0.0.1:9000/mcp",
        mcpServerName: "workspace",
        lastError: null,
      }],
      starting: 1,
    },
  };
}

function renderDialog(options: {
  repository: MonitorRepository;
  onClose?: () => void;
  intervalMs?: number;
}) {
  return render(createElement(MonitorDialog, {
    onClose: options.onClose ?? (() => {}),
    repository: options.repository,
    intervalMs: options.intervalMs ?? 60_000,
  }));
}

function resolved(): MonitorRepository {
  return { read: async () => snapshot() };
}

describe("MonitorDialog", () => {
  test("四个 tab 各自渲染已去敏的数据", async () => {
    renderDialog({ repository: resolved() });
    await screen.findByText("gpt-sonnet");

    const tab = (name: string) => screen.getByRole("tab", { name });
    expect(screen.getAllByRole("tab").map((item) => item.textContent))
      .toEqual(["LLM 配置", "MCP 配置", "WASM 内存", "Sandbox 情况"]);
    expect(tab("LLM 配置").getAttribute("aria-selected")).toBe("true");
    expect(screen.getByText("openai")).toBeTruthy();
    expect(screen.getByText("https://api.example.com/v1")).toBeTruthy();
    expect(screen.queryByText(/model-secret/)).toBeNull();

    fireEvent.click(tab("MCP 配置"));
    expect(screen.getByText("remote")).toBeTruthy();
    expect(screen.getByText("Authorization, X-Test")).toBeTruthy();
    expect(screen.getByText("http://127.0.0.1:9000/mcp")).toBeTruthy();
    expect(screen.queryByText(/Bearer/)).toBeNull();
    expect(screen.getAllByText(/未做连通性探测/)).toHaveLength(2);

    fireEvent.click(tab("WASM 内存"));
    expect(screen.getByText(formatBytes(128 * 1024 * 1024))).toBeTruthy();
    expect(screen.getByText(formatBytes(64 * 1024 * 1024))).toBeTruthy();
    expect(screen.getByText("/opt/peri/peri-wasm.js")).toBeTruthy();
    expect(screen.getByText(/SDK 未暴露每个 WASM 运行后端的堆占用/)).toBeTruthy();
    expect(screen.getByText(/2 个/)).toBeTruthy();

    fireEvent.click(tab("Sandbox 情况"));
    expect(screen.getByText("/sandboxes")).toBeTruthy();
    expect(screen.getByText("default")).toBeTruthy();
    expect(screen.getByText("1G")).toBeTruthy();
    expect(screen.getByText("4 个")).toBeTruthy();
    expect(screen.getByText(`/sandboxes/${ID}/workspace`)).toBeTruthy();
    expect(screen.getByText(/1 小时/)).toBeTruthy();
    expect(screen.getByText("2")).toBeTruthy();
  });

  test("Sandbox 设置读取失败时展示错误态而不是空 profile", async () => {
    const broken = snapshot();
    broken.sandbox.settings = {
      path: "/etc/peri/sandbox.json",
      source: "error",
      profileName: null,
      profile: null,
      error: "Sandbox settings not found: /etc/peri/sandbox.json",
    };
    renderDialog({ repository: { read: async () => broken } });
    await screen.findByText("gpt-sonnet");
    fireEvent.click(screen.getByRole("tab", { name: "Sandbox 情况" }));

    expect(screen.getByText("设置读取失败")).toBeTruthy();
    expect(screen.getByText("设置读取失败，无法确定生效 profile。")).toBeTruthy();
    expect(screen.getByText(/Sandbox settings not found/)).toBeTruthy();
  });

  test("首屏加载态与读取失败后的重试", async () => {
    let resolve: ((value: MonitorSnapshot) => void) | undefined;
    const pending: MonitorRepository = {
      read: () => new Promise<MonitorSnapshot>((done) => { resolve = done; }),
    };
    renderDialog({ repository: pending });
    expect(screen.getByText("正在读取监控数据…")).toBeTruthy();
    await act(async () => { resolve!(snapshot()); });
    expect(await screen.findByText("gpt-sonnet")).toBeTruthy();

    cleanup();

    let calls = 0;
    const flaky: MonitorRepository = {
      read: async () => {
        calls += 1;
        if (calls === 1) throw new Error("网络中断");
        return snapshot();
      },
    };
    renderDialog({ repository: flaky });
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/网络中断/)).toBeTruthy();
    expect(screen.getByText("暂无监控数据，请重试。")).toBeTruthy();

    fireEvent.click(within(alert).getByText("重试"));
    expect(await screen.findByText("gpt-sonnet")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  test("更新失败保留上次数据并可重试", async () => {
    let calls = 0;
    const repository: MonitorRepository = {
      read: async () => {
        calls += 1;
        if (calls === 1) return snapshot();
        throw new Error("再次失败");
      },
    };
    renderDialog({ repository });
    await screen.findByText("gpt-sonnet");

    fireEvent.click(screen.getByLabelText("刷新监控数据"));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("更新失败");
    expect(alert.textContent).toContain("再次失败");
    expect(alert.textContent).toContain("显示上次数据");
    expect(screen.getByText("gpt-sonnet")).toBeTruthy();
  });

  test("打开期间轮询，卸载后停止请求", async () => {
    let calls = 0;
    const repository: MonitorRepository = {
      read: async () => {
        calls += 1;
        return snapshot();
      },
    };
    const view = renderDialog({ repository, intervalMs: 10 });
    await waitFor(() => expect(calls).toBeGreaterThanOrEqual(2));

    view.unmount();
    const stoppedAt = calls;
    await act(async () => { await new Promise((done) => setTimeout(done, 50)); });
    expect(calls).toBe(stoppedAt);
  });

  test("tablist 键盘切换、ARIA 关联与焦点管理", async () => {
    renderDialog({ repository: resolved() });
    await screen.findByText("gpt-sonnet");

    const tablist = screen.getByRole("tablist", { name: "监控分类" });
    const orientation = tablist.getAttribute("aria-orientation");
    expect(orientation === "vertical" || orientation === "horizontal").toBe(true);
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("monitor-tab-llm");
    expect(document.activeElement).toBe(screen.getByRole("tab", { name: "LLM 配置" }));

    fireEvent.keyDown(tablist, { key: "ArrowDown" });
    const mcpTab = screen.getByRole("tab", { name: "MCP 配置" });
    expect(mcpTab.getAttribute("aria-selected")).toBe("true");
    expect(mcpTab.getAttribute("aria-controls")).toBe("monitor-panel-mcp");
    expect(document.activeElement).toBe(mcpTab);

    fireEvent.keyDown(tablist, { key: "ArrowLeft" });
    expect(screen.getByRole("tab", { name: "LLM 配置" }).getAttribute("aria-selected")).toBe("true");

    fireEvent.keyDown(tablist, { key: "End" });
    expect(screen.getByRole("tab", { name: "Sandbox 情况" }).getAttribute("aria-selected")).toBe("true");

    fireEvent.keyDown(tablist, { key: "Home" });
    expect(screen.getByRole("tab", { name: "LLM 配置" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("monitor-tab-llm");
  });

  test("窄屏与宽屏的 tab 方向跟随媒体查询", async () => {
    const original = window.matchMedia;
    const stub = (matches: boolean) => ((query: string) => ({
      matches,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => true,
    })) as typeof window.matchMedia;

    try {
      window.matchMedia = stub(false);
      const narrow = renderDialog({ repository: resolved() });
      await screen.findByText("gpt-sonnet");
      expect(screen.getByRole("tablist").getAttribute("aria-orientation")).toBe("horizontal");
      narrow.unmount();

      window.matchMedia = stub(true);
      renderDialog({ repository: resolved() });
      await screen.findByText("gpt-sonnet");
      expect(screen.getByRole("tablist").getAttribute("aria-orientation")).toBe("vertical");
    } finally {
      window.matchMedia = original;
    }
  });

  test("焦点困在对话框内，Escape 关闭并归还焦点", async () => {
    function Harness() {
      const [open, setOpen] = useState(false);
      return createElement("div", null,
        createElement("button", {
          type: "button",
          onClick: () => setOpen(true),
        }, "打开监控"),
        open
          ? createElement(MonitorDialog, { onClose: () => setOpen(false), repository: resolved() })
          : null);
    }

    render(createElement(Harness));
    const trigger = screen.getByText("打开监控");
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole("dialog", { name: "服务端监控" });
    await screen.findByText("gpt-sonnet");
    expect(dialog.contains(document.activeElement)).toBe(true);

    fireEvent.keyDown(document.activeElement!, { key: "Tab", shiftKey: true });
    expect(dialog.contains(document.activeElement)).toBe(true);

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(trigger);
  });
});
