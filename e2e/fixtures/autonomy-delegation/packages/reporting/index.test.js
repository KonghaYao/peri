import test from "node:test";
import assert from "node:assert/strict";
import { mean } from "./index.js";

test("mean divides by the number of values", () => {
  assert.equal(mean([2, 4]), 3);
});

test("mean of an empty list is zero", () => {
  assert.equal(mean([]), 0);
});
