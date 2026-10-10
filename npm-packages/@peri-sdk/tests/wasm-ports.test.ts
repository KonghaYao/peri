import { expect, test } from "bun:test";
import { Socket } from "node:net";
import { createNodeNetworkPort, createNodeSchedulerPort } from "../src/wasm/ports";

test("网络端口只销毁自有 socket", async () => {
  const network = createNodeNetworkPort();
  const HostSocket = network.module.Socket as typeof Socket;
  const owned = new HostSocket();
  const external = new Socket();
  try {
    network.close();
    await network.drain();
    expect(owned.destroyed).toBe(true);
    expect(external.destroyed).toBe(false);
  } finally { external.destroy(); }
});

test("已销毁 socket 可幂等清理", async () => {
  const network = createNodeNetworkPort();
  const HostSocket = network.module.Socket as typeof Socket;
  const socket = new HostSocket();
  socket.destroy();
  network.close();
  network.close();
  await network.drain();
  expect(socket.destroyed).toBe(true);
});

test("drain 等待 close 事件且禁止关闭后创建 socket", async () => {
  const network = createNodeNetworkPort();
  const HostSocket = network.module.Socket as typeof Socket;
  const socket = new HostSocket();
  let closed = false;
  socket.once("close", () => { closed = true; });
  await expect(network.drain()).rejects.toThrow("has not started");
  network.close();
  await network.drain();
  expect(closed).toBe(true);
  expect(() => new HostSocket()).toThrow("WASM network is closed");
});

test("socket 销毁失败可观测且可重试", async () => {
  const network = createNodeNetworkPort();
  const HostSocket = network.module.Socket as typeof Socket;
  const socket = new HostSocket();
  const destroy = socket.destroy;
  socket.destroy = () => { throw new Error("destroy failed"); };
  expect(() => network.close()).toThrow("WASM network cleanup failed");
  socket.destroy = destroy;
  network.close();
  await network.drain();
  expect(socket.destroyed).toBe(true);
});

test("宿主关闭取消自有 timer 且拒绝新增回调", async () => {
  const scheduler = createNodeSchedulerPort();
  let calls = 0;
  scheduler.schedule(() => { calls++; });
  scheduler.close();
  await Bun.sleep(5);
  expect(calls).toBe(0);
  expect(() => scheduler.schedule(() => {})).toThrow("WASM scheduler is closed");
});
