import { expect, test } from "bun:test";
import { SessionEventLog } from "./session-event-log";

test("event log replays missed events, follows live events, and resumes after disconnect", () => {
  const log = new SessionEventLog();
  const event = (method: string) => ({ jsonrpc: "2.0" as const, method });
  log.append(event("first"));
  log.append(event("second"));

  const firstConnection: number[] = [];
  const disconnect = log.subscribe(0, (entry) => firstConnection.push(entry.id));
  log.append(event("third"));
  disconnect();
  log.append(event("fourth"));
  expect(firstConnection).toEqual([1, 2, 3]);

  const resumed: number[] = [];
  log.subscribe(3, (entry) => resumed.push(entry.id));
  log.append(event("fifth"));
  expect(resumed).toEqual([4, 5]);
});
