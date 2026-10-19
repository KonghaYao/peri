import assert from "node:assert/strict";
import { mean } from "./packages/reporting/index.js";
import { netBalance } from "./packages/ledger/index.js";
import { nextRetryDelay } from "./packages/scheduler/index.js";

assert.equal(mean([2, 4]), 3);
assert.equal(mean([]), 0);
assert.equal(netBalance([{ amount: 10 }, { amount: -4 }]), 6);
assert.equal(netBalance([{ amount: -3 }, { amount: 2 }]), -1);
assert.equal(nextRetryDelay(0), 1000);
assert.equal(nextRetryDelay(1), 2000);
assert.equal(nextRetryDelay(2), 4000);

console.log("AUTONOMY_ACCEPTANCE_OK");
