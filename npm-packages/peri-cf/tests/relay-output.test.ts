import { expect, test } from "bun:test";
import { relayOutput } from "../scripts/relay-output";

function collect(): { chunks: string[]; write(chunk: Uint8Array): void } {
  const chunks: string[] = [];
  return { chunks, write: (chunk) => { chunks.push(new TextDecoder().decode(chunk)); } };
}

function streamOf(...parts: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      for (const part of parts) controller.enqueue(encoder.encode(part));
      controller.close();
    },
  });
}

test("relay writes every chunk to the console and the log target in order", async () => {
  const terminal = collect();
  const logFile = collect();
  await relayOutput(streamOf("first line\n", "second "), terminal, logFile);
  expect(terminal.chunks).toEqual(["first line\n", "second "]);
  expect(logFile.chunks).toEqual(terminal.chunks);
});

test("relay returns without writing when the process has no piped stream", async () => {
  const terminal = collect();
  await relayOutput(undefined, terminal);
  expect(terminal.chunks).toHaveLength(0);
});

test("concurrent relays from stdout and stderr both reach the shared log target", async () => {
  const logFile = collect();
  await Promise.all([
    relayOutput(streamOf("out-1\n", "out-2\n"), collect(), logFile),
    relayOutput(streamOf("err-1\n"), logFile),
  ]);
  expect([...logFile.chunks].sort()).toEqual(["err-1\n", "out-1\n", "out-2\n"]);
});
