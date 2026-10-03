import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { createConnection, type Socket } from "node:net";
import { ProcessBroker } from "../src/transport/process-broker";

function readAck(socket: Socket): Promise<void> {
  return new Promise((resolve, reject) => {
    socket.once("data", (bytes: Buffer) => bytes[0] === 0x06
      ? resolve()
      : reject(new Error("broker rejected registration")));
    socket.once("error", reject);
  });
}

test.skipIf(process.platform === "win32")("broker kills and reaps a registered process group after Agent exit", async () => {
  const broker = await ProcessBroker.start();
  const socket = createConnection(broker.socketPath);
  const child = spawn("sleep", ["30"], { detached: true, stdio: "ignore" });
  try {
    await new Promise<void>((resolve, reject) => {
      socket.once("connect", resolve);
      socket.once("error", reject);
    });
    const hello = readAck(socket);
    socket.write(Buffer.from(`B${broker.token}`));
    await hello;
    const register = readAck(socket);
    const message = Buffer.alloc(5);
    message[0] = 0x50;
    message.writeUInt32LE(child.pid!, 1);
    socket.write(message);
    await register;
    const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
    expect(await broker.settleAfterAgentExit()).toBe(true);
    socket.end();
    await exited;
  } finally {
    socket.destroy();
    try { process.kill(-child.pid!, "SIGKILL"); } catch { /* already stopped */ }
    await broker.settleAfterAgentExit();
  }
});

test.skipIf(process.platform === "win32")("normal anchored E/ACK release removes the group before its PID is reusable", async () => {
  const broker = await ProcessBroker.start();
  const anchor = spawn(process.execPath, ["-e", `
    const net = require("node:net");
    const socket = net.createConnection(process.env.BROKER_SOCKET);
    let phase = 0;
    socket.on("data", (chunk) => {
      if (chunk[0] !== 6) process.exit(2);
      if (phase++ === 0) {
        const registration = Buffer.alloc(5);
        registration[0] = 0x50;
        registration.writeUInt32LE(process.pid, 1);
        socket.write(registration);
      } else if (phase === 2) socket.write(Buffer.from([0x45]));
      else { socket.end(); process.exit(0); }
    });
    socket.on("connect", () => socket.write(Buffer.from("B" + process.env.BROKER_TOKEN)));
  `], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env, BROKER_SOCKET: broker.socketPath, BROKER_TOKEN: broker.token },
  });
  try {
    const code = await new Promise<number | null>((resolve) => anchor.once("exit", resolve));
    expect(code).toBe(0);
    expect(await broker.settleAfterAgentExit()).toBe(true);
  } finally {
    try { process.kill(-anchor.pid!, "SIGKILL"); } catch { /* already stopped */ }
    await broker.settleAfterAgentExit();
  }
});
