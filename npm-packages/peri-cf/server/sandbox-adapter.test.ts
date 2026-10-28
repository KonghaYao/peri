import { describe, expect, test } from "bun:test";
import { SandboxManager, type SandboxAdapter } from "@peri-code/sdk/sandbox";
import { createPeriSandboxAdapter, type PeriSandboxStartOptions } from "peri-sandbox";

/**
 * 生产测试场的最小契约检查：已发布 SDK 的 `/sandbox` 入口必须能接受
 * 本地 `peri-sandbox` 的官方 adapter。不启动实例，因此不依赖 Peri 可执行文件、
 * SRT 或 systemd。
 */
describe("published SDK sandbox contract", () => {
  test("peri-sandbox adapter satisfies the published SandboxAdapter contract", () => {
    const adapter: SandboxAdapter<PeriSandboxStartOptions> = createPeriSandboxAdapter();

    expect(typeof adapter.start).toBe("function");
    expect(typeof adapter.deleteData).toBe("function");

    const manager = new SandboxManager({ adapter });
    expect(manager).toBeInstanceOf(SandboxManager);
  });
});
