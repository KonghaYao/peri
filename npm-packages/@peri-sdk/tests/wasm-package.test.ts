import { expect, test } from "bun:test";
import { SESSION_BY_ID_SQL, SESSION_LIST_SQL } from "../dist/portable.js";
import { instantiatePeriWasm } from "../dist/wasm.js";
import { startPeriWasmHost } from "../dist/wasm-host.js";

test("公开 Store 查询常量保留参数化查询契约", () => {
  expect(SESSION_LIST_SQL).toContain("WHERE hidden = 0 AND cwd = ?");
  expect(SESSION_BY_ID_SQL).toContain("WHERE id = ?");
});

test("宿主产物保留 node: 端口说明符且不引入进程控制", async () => {
  const source = await Bun.file(new URL("../dist/wasm-host.js", import.meta.url)).text();
  expect(source).toContain('"node:dns"');
  expect(source).toContain('"node:net"');
  expect(source).not.toMatch(/(?:from\s*|import\s*\()\s*["'](?:dns|net)["']/);
  expect(source).not.toMatch(/child_process|Bun\.spawn|process\.kill|bun:sqlite/);
  expect(startPeriWasmHost).toBeFunction();
});

test("WASM 公共入口支持静态 factory 而不加载本机文件", async () => {
  const module = await instantiatePeriWasm({ moduleFactory: async () => ({
    PeriWasmAcp: { start: async () => { throw new Error("unused"); } },
  }) });
  expect(module.PeriWasmAcp.start).toBeFunction();
  const source = await Bun.file(new URL("../dist/wasm.js", import.meta.url)).text();
  expect(source).not.toMatch(/from\s*["'](?:node:|bun:)|child_process|Bun\.spawn|process\.env/);
});
