import { expect, test } from "bun:test";
import { analyzeSleepCommand, extractCommandArgument } from "./shell_command.js";

const cases: Array<{ command: string; shape: string | null; waitKind: string; seconds: number; sleepSegments: number }> = [
  { command: "sleep 30; echo waited", shape: "compound", waitKind: "noop_only", seconds: 30, sleepSegments: 1 },
  { command: "sleep 45; tail -3 build.log", shape: "compound", waitKind: "other_command", seconds: 45, sleepSegments: 1 },
  { command: "sleep 90", shape: "standalone", waitKind: "pure", seconds: 90, sleepSegments: 1 },
  { command: "sleep 0.5", shape: "standalone", waitKind: "pure", seconds: 0.5, sleepSegments: 1 },
  { command: "sleep 15 2>&1", shape: "standalone", waitKind: "pure", seconds: 15, sleepSegments: 1 },
  { command: "time sleep 2", shape: "standalone", waitKind: "pure", seconds: 2, sleepSegments: 1 },
  { command: "sleep 1 && echo ready", shape: "compound", waitKind: "noop_only", seconds: 1, sleepSegments: 1 },
  { command: "sleep $DELAY; true", shape: "variable", waitKind: "noop_only", seconds: 0, sleepSegments: 1 },
  { command: "for i in 1 2; do sleep 1; done", shape: "loop", waitKind: "other_command", seconds: 1, sleepSegments: 1 },
  { command: 'echo "sleep 30"', shape: null, waitKind: "none", seconds: 0, sleepSegments: 0 },
  { command: "bash -c 'sleep 5'", shape: null, waitKind: "none", seconds: 0, sleepSegments: 0 },
  { command: "sleep 2; sleep 3", shape: "compound", waitKind: "pure", seconds: 5, sleepSegments: 2 },
];

for (const entry of cases) {
  test(`parses ${entry.command}`, () => {
    const result = analyzeSleepCommand(entry.command);
    expect(result.shape).toBe(entry.shape as never);
    expect(result.waitKind).toBe(entry.waitKind as never);
    expect(result.seconds).toBe(entry.seconds);
    expect(result.sleepSegments).toBe(entry.sleepSegments);
  });
}

test("extracts command arguments from common shapes", () => {
  expect(extractCommandArgument({ command: "sleep 1" })).toBe("sleep 1");
  expect(extractCommandArgument({ cmd: "sleep 2" })).toBe("sleep 2");
  expect(extractCommandArgument("sleep 3")).toBe("sleep 3");
  expect(extractCommandArgument({ path: "/tmp" })).toBeNull();
  expect(extractCommandArgument(null)).toBeNull();
});
