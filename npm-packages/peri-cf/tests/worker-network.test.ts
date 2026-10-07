import { expect, test } from "bun:test";
import { workerNetwork } from "../worker/wasm/network";

test("WASM Host shutdown destroys only its own remaining TCP sockets", () => {
  const first = workerNetwork();
  const second = workerNetwork();
  const owned = new first.module.Socket();
  const other = new second.module.Socket();
  first.close();
  expect(owned.destroyed).toBe(true);
  expect(other.destroyed).toBe(false);
  first.close();
  second.close();
  expect(other.destroyed).toBe(true);
});

test("WASM Host cleanup also handles sockets destroyed before its shutdown", () => {
  const network = workerNetwork();
  const socket = new network.module.Socket();
  socket.destroy();
  expect(() => network.close()).not.toThrow();
  expect(socket.destroyed).toBe(true);
});

test("WASM cleanup confirms socket close events and forbids new sockets", async () => {
  const network = workerNetwork();
  const socket = new network.module.Socket();
  let confirmed = false;
  socket.once("close", () => { confirmed = true; });
  network.close();
  await network.drain();
  expect(confirmed).toBe(true);
  expect(() => new network.module.Socket()).toThrow("WASM network is closed");
});

test("failed destruction is observable and can only confirm after actual socket close", async () => {
  const network = workerNetwork();
  const socket = new network.module.Socket();
  const destroy = socket.destroy.bind(socket);
  socket.destroy = () => { throw new Error("destruction failed"); };
  expect(() => network.close()).toThrow("WASM network cleanup failed");
  socket.destroy = destroy;
  network.close();
  await network.drain();
  expect(socket.destroyed).toBe(true);
});
