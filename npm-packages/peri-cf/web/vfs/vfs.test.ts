/**
 * 浏览器 VFS：写入真的落 IndexedDB（换实例仍读得回）、缺失文件收敛为 null，
 * 以及父目录创建、列目录与删除的行为边界。
 */

import "fake-indexeddb/auto";
import { describe, expect, test } from "bun:test";
import { createVfs } from "./index";

let sequence = 0;
/** 每个用例一棵独立的库，避免用例之间互相看见对方的数据。 */
function nextVfsName(): string {
  sequence += 1;
  return `peri-vfs-test-${sequence}`;
}

describe("浏览器 VFS", () => {
  test("写入后可读回，中文按 UTF-8 往返", async () => {
    const vfs = createVfs(nextVfsName());
    await vfs.writeText("/config.json", '{"machineId":"配置"}');
    expect(await vfs.readText("/config.json")).toBe('{"machineId":"配置"}');
  });

  test("数据落在 IndexedDB：换一个实例读得到，等价于刷新后仍在", async () => {
    const name = nextVfsName();
    await createVfs(name).writeText("/config.json", '{"machineId":"m1"}');
    expect(await createVfs(name).readText("/config.json")).toBe('{"machineId":"m1"}');
  });

  test("父目录不存在时按需创建", async () => {
    const vfs = createVfs(nextVfsName());
    await vfs.writeText("/peri/settings/config.json", "{}");
    expect(await vfs.readText("/peri/settings/config.json")).toBe("{}");
  });

  test("读取不存在的文件返回 null", async () => {
    expect(await createVfs(nextVfsName()).readText("/absent.json")).toBeNull();
  });

  test("list 返回目录下的条目名并按字典序排列", async () => {
    const vfs = createVfs(nextVfsName());
    await vfs.writeText("/peri/b.json", "b");
    await vfs.writeText("/peri/a.json", "a");
    expect(await vfs.list("/peri")).toEqual(["a.json", "b.json"]);
  });

  test("删除后读回 null，新实例也看不到该文件", async () => {
    const name = nextVfsName();
    const vfs = createVfs(name);
    await vfs.writeText("/config.json", "{}");
    await vfs.deleteFile("/config.json");
    expect(await vfs.readText("/config.json")).toBeNull();
    expect(await createVfs(name).readText("/config.json")).toBeNull();
  });

  test("删除不存在的文件照常抛出", async () => {
    await expect(createVfs(nextVfsName()).deleteFile("/absent.json")).rejects.toThrow();
  });
});
