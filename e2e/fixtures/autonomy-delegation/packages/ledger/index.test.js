import test from "node:test";
import assert from "node:assert/strict";
import { netBalance } from "./index.js";

test("net balance keeps the sign of each entry", () => {
  assert.equal(netBalance([{ amount: 10 }, { amount: -4 }]), 6);
});

test("net balance can be negative", () => {
  assert.equal(netBalance([{ amount: -3 }, { amount: 2 }]), -1);
});
