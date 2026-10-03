import { expect, test } from "bun:test";
import { createConnection } from "node:net";
import { ProcessSupervisor } from "../src/sandbox/process-supervisor";
import { StdioTransport } from "../src/transport/stdio-transport";

async function query(supervisor: ProcessSupervisor, sessionId: string, generationId: string, token = supervisor.token): Promise<boolean> {
  const socket = createConnection(supervisor.socketPath);
  await new Promise<void>((resolve, reject) => {
    socket.once("connect", resolve);
    socket.once("error", reject);
  });
  socket.write(JSON.stringify({ token, sessionId, generationId }) + "\n");
  let line = "";
  for await (const chunk of socket) line += chunk.toString();
  return JSON.parse(line).proven === true;
}

test.skipIf(process.platform === "win32")("supervisor proves only its registered stopped generation", async () => {
  const supervisor = await ProcessSupervisor.start();
  const transport = await StdioTransport.start({ command: process.execPath, args: ["-e", "setTimeout(() => {}, 30000)"] });
  supervisor.registerGeneration(transport);
  await supervisor.register("session-a", transport);
  expect(await query(supervisor, "session-b", transport.generationId)).toBe(false);
  expect(await query(supervisor, "session-a", transport.generationId, "0".repeat(64))).toBe(false);
  expect(await query(supervisor, "session-a", "0".repeat(32))).toBe(false);
  await transport.close();
  expect(await query(supervisor, "session-a", transport.generationId)).toBe(true);
}, 30_000);

test.skipIf(process.platform === "win32")("concurrent registration cannot replace a live generation", async () => {
  const supervisor = await ProcessSupervisor.start();
  const first = await StdioTransport.start({ command: process.execPath, args: ["-e", "setTimeout(() => {}, 30000)"] });
  const second = await StdioTransport.start({ command: process.execPath, args: ["-e", "setTimeout(() => {}, 30000)"] });
  supervisor.registerGeneration(first);
  supervisor.registerGeneration(second);
  try {
    const [one, two] = await Promise.allSettled([
      supervisor.register("session-race", first),
      supervisor.register("session-race", second),
    ]);
    expect(one.status).toBe("fulfilled");
    expect(two.status).toBe("rejected");
  } finally {
    await Promise.all([first.close(), second.close()]);
  }
}, 30_000);

test.skipIf(process.platform === "win32")("spawn registration proves a crash before session response", async () => {
  const supervisor = await ProcessSupervisor.start();
  const transport = await StdioTransport.start({ command: process.execPath, args: ["-e", "setTimeout(() => {}, 30000)"] });
  supervisor.registerGeneration(transport);
  await transport.close();
  expect(await query(supervisor, "session-before-response", transport.generationId)).toBe(true);
}, 30_000);
