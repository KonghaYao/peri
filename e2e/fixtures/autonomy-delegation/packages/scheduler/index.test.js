import test from "node:test";
import assert from "node:assert/strict";
import { nextRetryDelay } from "./index.js";

test("retry delay grows exponentially", () => {
  assert.equal(nextRetryDelay(0), 1000);
  assert.equal(nextRetryDelay(1), 2000);
  assert.equal(nextRetryDelay(2), 4000);
});
