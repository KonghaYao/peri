#!/usr/bin/env bun
/**
 * 审计单条 trace 的 observation 父链。
 * 只输出 observation 类型、ID 与结构状态，不打印 input/output/error 正文。
 */
import { auditObservationTree, fetchObservations } from "./lib.ts";

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  console.log("用法: bun trace-tree.ts <traceId>");
  process.exit(0);
}

const traceId = args.find((arg) => !arg.startsWith("--"));
if (!traceId) {
  console.error("Usage: bun trace-tree.ts <traceId>");
  process.exit(1);
}

const observations = await fetchObservations(traceId);
const audit = auditObservationTree(observations, traceId);
const byId = new Map<string, any>();
for (const observation of observations) {
  if (typeof observation?.id === "string" && !byId.has(observation.id)) byId.set(observation.id, observation);
}

const children = new Map<string, string[]>();
const roots: string[] = [];
for (const [id, observation] of byId) {
  const parentId = typeof observation.parentObservationId === "string" ? observation.parentObservationId : undefined;
  if (!parentId || parentId === traceId || !byId.has(parentId)) {
    roots.push(id);
    continue;
  }
  const siblings = children.get(parentId) || [];
  siblings.push(id);
  children.set(parentId, siblings);
}

const sortIds = (ids: string[]) => ids.sort((left, right) => {
  const leftTime = String(byId.get(left)?.startTime || "");
  const rightTime = String(byId.get(right)?.startTime || "");
  return leftTime.localeCompare(rightTime) || left.localeCompare(right);
});

const visited = new Set<string>();
function printTree(id: string, depth: number) {
  if (visited.has(id)) {
    console.log(`${"  ".repeat(depth)}- CYCLE/REVISIT ${id}`);
    return;
  }
  visited.add(id);
  const observation = byId.get(id);
  console.log(`${"  ".repeat(depth)}- ${String(observation?.type || "UNKNOWN")} ${id}`);
  for (const childId of sortIds(children.get(id) || [])) printTree(childId, depth + 1);
}

console.log(`## Trace tree: ${traceId}`);
console.log(`Observations: ${observations.length} (${byId.size} unique IDs)\n`);
for (const rootId of sortIds(roots)) printTree(rootId, 0);
for (const remainingId of sortIds([...byId.keys()].filter((id) => !visited.has(id)))) printTree(remainingId, 0);

console.log("\n## Integrity audit");
console.log(`Duplicate IDs: ${audit.duplicateIds.length}`);
console.log(`Missing parents: ${audit.missingParents.length}`);
console.log(`Cycles: ${audit.cycles.length}`);
for (const item of audit.missingParents) console.log(`- MISSING_PARENT ${item.id} -> ${item.parentObservationId}`);
for (const cycle of audit.cycles) console.log(`- CYCLE ${cycle.join(" -> ")}`);

if (audit.duplicateIds.length || audit.missingParents.length || audit.cycles.length) process.exit(1);
