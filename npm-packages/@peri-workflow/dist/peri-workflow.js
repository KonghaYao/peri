#!/usr/bin/env node

// src/cli.ts
import { readFileSync as readFileSync2 } from "node:fs";

// src/reader.ts
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
function findRunsRoot(startDir = process.cwd()) {
  let dir = startDir;
  for (;; ) {
    const candidate = join(dir, ".claude", "workflow-runs");
    if (existsSync(candidate))
      return candidate;
    const parent = dirname(dir);
    if (parent === dir)
      return null;
    dir = parent;
  }
}
function loadState(runDir) {
  const raw = readFileSync(join(runDir, "state.json"), "utf8");
  try {
    return JSON.parse(raw);
  } catch (e) {
    throw new Error(`state.json 解析失败: ${e.message}`);
  }
}
function loadOutputs(runDir) {
  const out = new Map;
  const dir = join(runDir, "outputs");
  if (!existsSync(dir))
    return out;
  for (const f of readdirSync(dir)) {
    if (!f.endsWith(".txt"))
      continue;
    const label = f.slice(0, -".txt".length);
    out.set(label, readFileSync(join(dir, f), "utf8"));
  }
  return out;
}
function replacePlaceholders(value, outputs) {
  if (typeof value === "string") {
    const m = /^\$\{([^}]+)\}$/.exec(value);
    if (m && outputs.has(m[1]))
      return outputs.get(m[1]);
    return value;
  }
  if (Array.isArray(value))
    return value.map((v) => replacePlaceholders(v, outputs));
  if (value && typeof value === "object") {
    const obj = {};
    for (const [k, v] of Object.entries(value)) {
      obj[k] = replacePlaceholders(v, outputs);
    }
    return obj;
  }
  return value;
}
function loadJournal(runDir) {
  const path = join(runDir, "journal.jsonl");
  if (!existsSync(path))
    return [];
  const results = [];
  for (const line of readFileSync(path, "utf8").split(`
`)) {
    const t = line.trim();
    if (!t)
      continue;
    try {
      const entry = JSON.parse(t);
      const r = entry.result;
      if (!r)
        continue;
      const kind = r.kind ?? "ok";
      results.push({
        seq: entry.seq ?? results.length + 1,
        kind,
        output: r.output,
        tokens: r.tokenCount ?? r.usage?.outputTokens,
        tools: r.toolCount,
        durationMs: r.durationMs,
        phase: r.phase,
        reason: r.reason,
        detail: r.detail
      });
    } catch {}
  }
  results.sort((a, b) => a.seq - b.seq);
  return results;
}
function fmtDuration(start, end) {
  if (!start || !end)
    return "-";
  const ms = Date.parse(end) - Date.parse(start);
  if (Number.isNaN(ms))
    return "-";
  if (ms < 1000)
    return `${ms}ms`;
  if (ms < 60000)
    return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60000);
  const s = Math.round(ms % 60000 / 1000);
  return `${m}m${String(s).padStart(2, "0")}s`;
}
function fmtNum(n) {
  return n === undefined ? "-" : n.toLocaleString();
}
function fmtVal(v) {
  if (v === undefined || v === null)
    return "-";
  if (typeof v === "string")
    return v;
  return JSON.stringify(v);
}
function agentSummary(a) {
  const out = fmtVal(a.output);
  const first = out.split(`
`).find((l) => l.trim().length > 0) ?? "";
  return first.slice(0, 80) || out.slice(0, 80);
}
function renderReturnValue(rv, outputs) {
  const replaced = replacePlaceholders(rv, outputs);
  if (replaced === undefined || replaced === null) {
    console.log("  (无 return value)");
  } else if (typeof replaced === "string") {
    console.log(replaced);
  } else {
    for (const [k, v] of Object.entries(replaced)) {
      console.log(`### ${k}
`);
      if (typeof v === "string") {
        console.log(v.length > 0 ? v : "  (空)");
      } else {
        console.log(JSON.stringify(v, null, 2));
      }
      console.log();
    }
  }
  return replaced;
}
function resolveRunDir(runId) {
  const root = findRunsRoot();
  if (!root) {
    throw new Error("未找到 .claude/workflow-runs 目录（当前目录及其父目录均无）。请在仓库内运行。");
  }
  if (runId.includes("..") || runId.includes("/") || runId.includes("\\")) {
    throw new Error(`非法 runId（含路径字符）: ${runId}`);
  }
  const runDir = join(root, runId);
  if (!existsSync(join(runDir, "state.json"))) {
    throw new Error(`未找到运行 ${runId}：${runDir}（可用 peri-workflow list 查看已有 run）`);
  }
  return runDir;
}
function reportRun(runId, short, json) {
  let runDir;
  try {
    runDir = resolveRunDir(runId);
  } catch (e) {
    console.error(e.message);
    process.exit(1);
  }
  let state;
  try {
    state = loadState(runDir);
  } catch (e) {
    console.error(`读取运行 ${runId} 失败: ${e.message}`);
    process.exit(1);
  }
  const outputs = loadOutputs(runDir);
  const agents = loadJournal(runDir);
  if (json) {
    const result = {
      run_id: state.run_id,
      workflow_name: state.workflow_name,
      status: state.status,
      error: state.error ?? null,
      started_at: state.started_at ?? null,
      finished_at: state.finished_at ?? null,
      duration: fmtDuration(state.started_at, state.finished_at),
      return_value: replacePlaceholders(state.return_value, outputs),
      outputs: Object.fromEntries(outputs),
      agents,
      run_dir: runDir
    };
    console.log(JSON.stringify(result, null, 2));
    return;
  }
  console.log(`# Workflow Run ${state.run_id} — ${state.workflow_name}`);
  console.log(`status: ${state.status}${state.error ? ` | error: ${state.error}` : ""}`);
  console.log(`duration: ${fmtDuration(state.started_at, state.finished_at)}`);
  console.log(`run 目录: .claude/workflow-runs/${state.run_id}/
`);
  if (state.error) {
    console.log(`## Error

${state.error}
`);
  }
  console.log(`## Return value
`);
  if (state.return_value !== undefined && state.return_value !== null) {
    renderReturnValue(state.return_value, outputs);
  } else {
    console.log("  (无 return value)");
  }
  if (agents.length > 0) {
    console.log(`## Agents (${agents.length})
`);
    console.log("| # | phase | status | tokens | tools | 耗时 | 摘要 |");
    console.log("|---|-------|--------|-------:|------:|-----:|-------|");
    for (const a of agents) {
      const phase = a.phase ?? "-";
      const status = a.kind === "ok" ? "ok" : a.kind === "dead" ? `dead${a.reason ? ` (${a.reason})` : ""}` : "skipped";
      const dur = a.durationMs === undefined ? "-" : `${(a.durationMs / 1000).toFixed(1)}s`;
      console.log(`| ${a.seq} | ${phase} | ${status} | ${fmtNum(a.tokens)} | ${fmtNum(a.tools)} | ${dur} | ${agentSummary(a).replace(/\|/g, "\\|")} |`);
    }
    if (!short) {
      console.log();
      for (const a of agents) {
        console.log(`--- Agent ${a.seq}${a.phase ? ` (${a.phase})` : ""} [${a.kind}] ---`);
        if (a.kind === "dead") {
          console.log(`reason: ${a.reason ?? "-"}${a.detail ? `
detail: ${a.detail}` : ""}`);
        } else if (a.kind === "skipped") {
          console.log("(skipped)");
        } else {
          console.log(fmtVal(a.output));
        }
        console.log();
      }
    }
  } else {
    console.log("(journal 为空——无 agent 调用或运行过早失败)");
  }
}
function listRuns(json) {
  const root = findRunsRoot();
  if (!root) {
    console.error("未找到 .claude/workflow-runs 目录");
    process.exit(1);
  }
  const runs = [];
  for (const d of readdirSync(root)) {
    const statePath = join(root, d, "state.json");
    if (!existsSync(statePath))
      continue;
    try {
      const st = loadState(join(root, d));
      runs.push({
        ...st,
        duration: fmtDuration(st.started_at, st.finished_at),
        dir: d
      });
    } catch {}
  }
  runs.sort((a, b) => (a.finished_at ?? "").localeCompare(b.finished_at ?? ""));
  if (json) {
    console.log(JSON.stringify(runs, null, 2));
    return;
  }
  console.log(`# Workflow runs (${runs.length})
`);
  console.log("| run_id | workflow | status | 时长 | finished_at |");
  console.log("|--------|----------|--------|------|-------------|");
  for (const r of runs) {
    console.log(`| ${r.run_id} | ${r.workflow_name} | ${r.status} | ${r.duration} | ${r.finished_at ?? "-"} |`);
  }
  console.log(`
读取单个 run：peri-workflow read <run_id>`);
}

// node_modules/@claude-code-best/workflow-engine/dist/constants.js
var WORKFLOW_DIR_NAME = ".claude/workflows";
var WORKFLOW_SCRIPT_EXTENSIONS = [".ts", ".js", ".mjs"];
var DEFAULT_MAX_CONCURRENCY = 3;
var MAX_CONCURRENCY_CAP = 16;
var MAX_TOTAL_AGENTS = 1000;
var MAX_ITEMS_PER_CALL = 4096;

// node_modules/@claude-code-best/workflow-engine/dist/ports.js
var HOST_HANDLE = Symbol("workflow.hostHandle");
function createHostHandle(bundle) {
  return { [HOST_HANDLE]: bundle };
}

// node_modules/@claude-code-best/workflow-engine/dist/agentAdapter.js
class AdapterNotFoundError extends Error {
  constructor(message) {
    super(message);
    this.name = "AdapterNotFoundError";
  }
}

class AgentAdapterRegistry {
  adapters = new Map;
  rules = [];
  defaultId = null;
  register(adapter) {
    this.adapters.set(adapter.id, adapter);
    return this;
  }
  default(adapterId) {
    this.defaultId = adapterId;
    return this;
  }
  route(rule) {
    this.rules.push(rule);
    return this;
  }
  has(id) {
    return this.adapters.has(id);
  }
  get(id) {
    return this.adapters.get(id);
  }
  resolve(params) {
    for (const rule of this.rules) {
      if (matchRule(rule, params)) {
        const hit = this.adapters.get(rule.adapter);
        if (hit)
          return hit;
      }
    }
    if (this.defaultId) {
      const fallback = this.adapters.get(this.defaultId);
      if (fallback)
        return fallback;
    }
    throw new AdapterNotFoundError(`No adapter matched (rules=${this.rules.length}, default=${this.defaultId ?? "none"})`);
  }
  async initializeAll() {
    for (const a of this.adapters.values()) {
      await a.initialize?.();
    }
  }
  async disposeAll() {
    for (const a of this.adapters.values()) {
      await a.dispose?.();
    }
  }
}
function matchRule(rule, params) {
  if (rule.kind === "agentType")
    return params.agentType === rule.agentType;
  if (rule.kind === "model") {
    return typeof params.model === "string" && params.model.startsWith(rule.pattern);
  }
  return rule.match(params);
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/concurrency.js
class Semaphore {
  available;
  waiters = [];
  constructor(permits) {
    this.available = Math.max(1, Math.floor(permits));
  }
  async acquire(signal) {
    if (signal?.aborted) {
      throw new Error("Semaphore.acquire aborted (signal already aborted)");
    }
    if (this.available > 0) {
      this.available -= 1;
      return () => this.release();
    }
    return new Promise((resolve, reject) => {
      const onAbort = () => {
        const idx = this.waiters.indexOf(entry);
        if (idx >= 0)
          this.waiters.splice(idx, 1);
        reject(new Error("Semaphore.acquire aborted"));
      };
      const wake = () => {
        signal?.removeEventListener("abort", onAbort);
        resolve(() => this.release());
      };
      const entry = {
        wake,
        cleanup: () => signal?.removeEventListener("abort", onAbort)
      };
      signal?.addEventListener("abort", onAbort, { once: true });
      this.waiters.push(entry);
    });
  }
  release() {
    const next = this.waiters.shift();
    if (next) {
      next.wake();
    } else {
      this.available += 1;
    }
  }
}
function clampMaxConcurrency(n) {
  if (n === undefined || Number.isNaN(n))
    return DEFAULT_MAX_CONCURRENCY;
  return Math.max(1, Math.min(Math.trunc(n), MAX_CONCURRENCY_CAP));
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/script.js
class ScriptError extends Error {
  constructor(message) {
    super(message);
    this.name = "ScriptError";
  }
}
var META_RE = /export\s+const\s+meta\s*=\s*/;
function extractMeta(source) {
  const match = META_RE.exec(source);
  if (!match)
    return { meta: null, body: source };
  let i = match.index + match[0].length;
  while (i < source.length && /\s/.test(source[i]))
    i++;
  if (source[i] !== "{") {
    throw new ScriptError("meta must be an object literal `{ ... }`");
  }
  let depth = 0;
  const start = i;
  let inStr = null;
  for (;i < source.length; i++) {
    const ch = source[i];
    if (inStr) {
      if (ch === "\\") {
        i++;
        continue;
      }
      if (ch === inStr)
        inStr = null;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") {
      inStr = ch;
      continue;
    }
    if (ch === "{")
      depth++;
    else if (ch === "}") {
      depth--;
      if (depth === 0) {
        i++;
        break;
      }
    }
  }
  if (depth !== 0)
    throw new ScriptError("meta literal braces are not closed");
  const literal = source.slice(start, i);
  let metaObj;
  try {
    metaObj = new Function(`return (${literal})`)();
  } catch (e) {
    throw new ScriptError(`meta must be a plain literal (no variable/function calls/interpolation): ${e.message}`);
  }
  const meta = validateMeta(metaObj);
  const body = source.slice(0, match.index) + source.slice(i).replace(/^[ \t]*;[ \t]*\n/, `
`);
  return { meta, body };
}
function validateMeta(v) {
  if (typeof v !== "object" || v === null || Array.isArray(v)) {
    throw new ScriptError("meta must be an object");
  }
  const o = v;
  if (typeof o.name !== "string" || typeof o.description !== "string") {
    throw new ScriptError("meta must include string name and description");
  }
  return o;
}

class NonDeterministicError extends Error {
  constructor(fn) {
    super(`${fn} is not available in workflow scripts (would break resume determinism). Pass timestamps/random seeds via args.`);
    this.name = "NonDeterministicError";
  }
}
function sandboxDate() {
  const fn = function(...args) {
    if (args.length === 0)
      throw new NonDeterministicError("Date.now()/new Date()");
    return new Date(...args);
  };
  fn.now = () => {
    throw new NonDeterministicError("Date.now()");
  };
  fn.parse = Date.parse;
  fn.UTC = Date.UTC;
  return fn;
}
function sandboxMath() {
  return new Proxy(Math, {
    get(target, prop, receiver) {
      if (prop === "random") {
        return () => {
          throw new NonDeterministicError("Math.random()");
        };
      }
      return Reflect.get(target, prop, receiver);
    }
  });
}
var AsyncFunction = Object.getPrototypeOf(async function() {}).constructor;
function assertScriptBody(body) {
  if (/^\s*import\b/m.test(body)) {
    throw new ScriptError("workflow scripts are the body of new AsyncFunction (not ESM modules); import is not supported. " + "agent / parallel / pipeline / phase / log / workflow / args / budget are injected as parameters — use them directly.");
  }
  if (/\bimport\s*\(/m.test(body)) {
    throw new ScriptError("dynamic import(...) is forbidden in workflow scripts: it bypasses the Date/Math sandbox and breaks resume determinism. " + "The sandbox does not guarantee security (same trust level as the LLM), but explicit escapes are prohibited. Inject external dependencies via args.");
  }
  if (/^\s*export\b/m.test(body)) {
    throw new ScriptError("workflow scripts allow only one export const meta = {...} (already extracted by the engine). " + "Remove other export / export default statements; use top-level return for the result.");
  }
}
function parseScript(source) {
  const { meta, body } = extractMeta(source);
  assertScriptBody(body);
  let fn;
  try {
    fn = new AsyncFunction("agent", "parallel", "pipeline", "phase", "log", "workflow", "args", "budget", "Date", "Math", body);
  } catch (e) {
    throw new ScriptError(`Script syntax error: ${e.message}`);
  }
  const sandboxedDate = sandboxDate();
  const sandboxedMath = sandboxMath();
  return {
    meta,
    async execute(hooks, args, budget) {
      return fn(hooks.agent, hooks.parallel, hooks.pipeline, hooks.phase, hooks.log, hooks.workflow, args, budget, sandboxedDate, sandboxedMath);
    }
  };
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/journal.js
import { createHash } from "node:crypto";
function canonicalParams(params) {
  const { label: _label, phase: _phase, ...rest } = params;
  const keys = Object.keys(rest).sort();
  const sorted = {};
  for (const k of keys)
    sorted[k] = rest[k];
  return JSON.stringify(sorted);
}
function agentCallKey(prompt, params) {
  return createHash("sha256").update(prompt + `
` + canonicalParams(params)).digest("hex");
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/budget.js
class BudgetExhaustedError extends Error {
  constructor() {
    super("workflow token budget exhausted (budget.total reached the cap)");
    this.name = "BudgetExhaustedError";
  }
}

class Budget {
  total;
  spentTokens = 0;
  constructor(total) {
    this.total = total;
  }
  spent() {
    return this.spentTokens;
  }
  remaining() {
    return this.total == null ? Infinity : Math.max(0, this.total - this.spentTokens);
  }
  addOutputTokens(n) {
    if (n > 0)
      this.spentTokens += n;
  }
  assertCanSpend() {
    if (this.total != null && this.spentTokens >= this.total) {
      throw new BudgetExhaustedError;
    }
  }
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/namedWorkflows.js
import { readFile, readdir } from "node:fs/promises";
import { parse, resolve as resolve2 } from "node:path";

// node_modules/@claude-code-best/workflow-engine/dist/engine/paths.js
import { resolve, sep } from "node:path";
function containsPath(base, target) {
  const resolvedBase = resolve(base);
  const resolvedTarget = resolve(resolvedBase, target);
  if (resolvedTarget === resolvedBase)
    return true;
  return resolvedTarget.startsWith(resolvedBase + sep);
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/namedWorkflows.js
async function resolveNamedWorkflow(workflowDir, name) {
  for (const ext of WORKFLOW_SCRIPT_EXTENSIONS) {
    const p = resolve2(workflowDir, name + ext);
    if (!containsPath(workflowDir, p))
      return null;
    try {
      return { path: p, content: await readFile(p, "utf-8") };
    } catch {}
  }
  return null;
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/errors.js
class WorkflowError extends Error {
  constructor(message) {
    super(message);
    this.name = "WorkflowError";
  }
}

class WorkflowAbortedError extends Error {
  constructor() {
    super("workflow has been aborted");
    this.name = "WorkflowAbortedError";
  }
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/context.js
function createSharedResources(budgetTotal, maxConcurrency) {
  return {
    semaphore: new Semaphore(clampMaxConcurrency(maxConcurrency)),
    budget: new Budget(budgetTotal),
    agentCountBox: { value: 0 },
    agentIdSeq: { value: 0 },
    depth: 0
  };
}
function createEngineContext(opts) {
  const resources = createSharedResources(opts.budgetTotal, opts.maxConcurrency);
  return {
    ports: opts.ports,
    host: opts.host,
    signal: opts.signal,
    runId: opts.runId,
    workflowName: opts.workflowName,
    cwd: opts.cwd,
    resources,
    journal: opts.journal ? [...opts.journal] : [],
    journalIndex: 0,
    journalInvalidated: false,
    currentPhase: null
  };
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/hooks.js
function makeHooks(ctx, runSubWorkflow) {
  const emit = (init) => {
    ctx.ports.progressEmitter.emit({
      runId: ctx.runId,
      ...init
    });
  };
  const agent = async (prompt, opts = {}) => {
    const r = ctx.resources;
    if (r.agentCountBox.value >= MAX_TOTAL_AGENTS) {
      throw new WorkflowError(`workflow exceeds total agent cap (${MAX_TOTAL_AGENTS})`);
    }
    const agentId = r.agentIdSeq.value++;
    const params = { prompt, ...opts };
    const key = agentCallKey(prompt, params);
    const label = opts.label;
    const phase2 = opts.phase ?? ctx.currentPhase ?? undefined;
    if (!ctx.journalInvalidated && ctx.journalIndex < ctx.journal.length) {
      const entry = ctx.journal[ctx.journalIndex];
      if (entry.key === key) {
        ctx.journalIndex++;
        emit({
          type: "agent_done",
          agentId,
          label,
          phase: phase2,
          result: entry.result
        });
        return resultToOutput(entry.result);
      }
      ctx.journalInvalidated = true;
      ctx.journal = ctx.journal.slice(0, ctx.journalIndex);
      await ctx.ports.journalStore.truncate(ctx.runId);
    }
    let release;
    try {
      release = await ctx.resources.semaphore.acquire(ctx.signal);
    } catch {
      throw new WorkflowAbortedError;
    }
    try {
      if (ctx.signal.aborted)
        throw new WorkflowAbortedError;
      r.budget.assertCanSpend();
      const pending = ctx.ports.taskRegistrar.pendingAction(ctx.runId);
      if (pending?.kind === "skip") {
        const result2 = { kind: "skipped" };
        emit({ type: "agent_done", agentId, label, phase: phase2, result: result2 });
        return null;
      }
      ctx.resources.agentCountBox.value++;
      emit({ type: "agent_started", agentId, label, phase: phase2 });
      const registry = ctx.ports.agentAdapterRegistry;
      const onProgress = (update) => {
        emit({ type: "agent_progress", agentId, label, phase: phase2, ...update });
      };
      const adapterCtx = registry ? {
        host: ctx.host,
        signal: ctx.signal,
        runId: ctx.runId,
        agentId,
        onProgress,
        ...ctx.ports.taskRegistrar.registerAgentAbort ? {
          registerAgentAbort: (id, ac) => {
            ctx.ports.taskRegistrar.registerAgentAbort?.(ctx.runId, id, ac);
          }
        } : {},
        ...ctx.ports.taskRegistrar.unregisterAgentAbort ? {
          unregisterAgentAbort: (id) => {
            ctx.ports.taskRegistrar.unregisterAgentAbort?.(ctx.runId, id);
          }
        } : {}
      } : null;
      const adapter = registry ? registry.resolve(params) : null;
      const invokeBackend = () => adapter ? adapter.run(params, adapterCtx) : ctx.ports.agentRunner.runAgentToResult(params, ctx.host);
      let result;
      try {
        result = await invokeBackend();
        if (result.kind === "dead") {
          const detailStr = typeof result.detail === "string" ? result.detail : "";
          ctx.ports.logger.warn?.(`agent "${label ?? `#${agentId}`}" returned dead` + (result.reason ? ` (${result.reason})` : "") + (detailStr ? `: ${detailStr.slice(0, 150)}` : "") + "; retrying once");
          result = await invokeBackend();
        }
      } catch (e) {
        if (e instanceof WorkflowAbortedError)
          throw e;
        const eMsg = e instanceof Error ? e.message : String(e);
        ctx.ports.logger.warn?.(`agent "${label ?? `#${agentId}`}" threw (${eMsg}); retrying once`);
        try {
          result = await invokeBackend();
        } catch (e2) {
          if (e2 instanceof WorkflowAbortedError)
            throw e2;
          result = {
            kind: "dead",
            reason: "runagent-threw",
            detail: e2 instanceof Error ? e2.message : String(e2)
          };
        }
      }
      if (result.kind === "ok") {
        ctx.resources.budget.addOutputTokens(result.usage.outputTokens);
      }
      emit({ type: "agent_done", agentId, label, phase: phase2, result });
      const entry = { key, seq: agentId, result };
      ctx.journal.push(entry);
      ctx.journalIndex++;
      await ctx.ports.journalStore.append(ctx.runId, entry);
      return resultToOutput(result);
    } finally {
      release();
    }
  };
  const parallel = async (thunks) => {
    if (thunks.length > MAX_ITEMS_PER_CALL) {
      throw new WorkflowError(`parallel exceeds the per-call items cap (${MAX_ITEMS_PER_CALL})`);
    }
    return Promise.all(thunks.map(async (t, i) => {
      try {
        return await t();
      } catch (e) {
        ctx.ports.logger.warn?.(`parallel thunk #${i} failed: ${e.message}`);
        return null;
      }
    }));
  };
  const pipeline = async (items, ...stages) => {
    if (items.length > MAX_ITEMS_PER_CALL) {
      throw new WorkflowError(`pipeline exceeds the per-call items cap (${MAX_ITEMS_PER_CALL})`);
    }
    return Promise.all(items.map(async (item, index) => {
      try {
        let prev = item;
        for (const stage of stages) {
          prev = await stage(prev, item, index);
        }
        return prev;
      } catch (e) {
        ctx.ports.logger.warn?.(`pipeline item #${index} failed: ${e.message}`);
        return null;
      }
    }));
  };
  const phase = (title) => {
    if (ctx.currentPhase) {
      emit({ type: "phase_done", phase: ctx.currentPhase });
    }
    ctx.currentPhase = title;
    emit({ type: "phase_started", phase: title });
  };
  const log = (message) => {
    emit({ type: "log", message });
  };
  const workflow = async (nameOrRef, args) => {
    if (ctx.resources.depth >= 1) {
      throw new WorkflowError("workflow() nesting allows only one level");
    }
    const sub = typeof nameOrRef === "string" ? { name: nameOrRef } : { scriptPath: nameOrRef.scriptPath };
    return runSubWorkflow({ ...sub, args });
  };
  return { agent, parallel, pipeline, phase, log, workflow };
}
function resultToOutput(result) {
  return result.kind === "ok" ? result.output : null;
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/runWorkflow.js
import { readFile as readFile2 } from "node:fs/promises";
import { join as join2 } from "node:path";
async function runWorkflow(opts) {
  const { ports } = opts;
  let parsed;
  try {
    parsed = parseScript(opts.script);
  } catch (e) {
    const error = e.message;
    ports.progressEmitter.emit({
      type: "run_done",
      runId: opts.runId,
      status: "failed",
      error
    });
    return { status: "failed", error };
  }
  const workflowName = opts.workflowName ?? parsed.meta?.name ?? "workflow";
  let journal = [];
  let journalInvalidated = false;
  if (opts.resume && !opts.scriptChanged) {
    journal = await ports.journalStore.read(opts.runId);
  } else if (opts.scriptChanged) {
    await ports.journalStore.truncate(opts.runId);
    journalInvalidated = true;
  }
  const ctx = createEngineContext({
    ports,
    host: opts.host,
    signal: opts.signal,
    runId: opts.runId,
    workflowName,
    cwd: opts.cwd,
    budgetTotal: opts.budgetTotal,
    maxConcurrency: opts.maxConcurrency,
    journal
  });
  if (journalInvalidated)
    ctx.journalInvalidated = true;
  ports.progressEmitter.emit({
    type: "run_started",
    runId: opts.runId,
    workflowName,
    meta: parsed.meta
  });
  const runSubWorkflow = async (sub) => {
    const script = await resolveSubScript(sub, opts.cwd);
    let subParsed;
    try {
      subParsed = parseScript(script);
    } catch (e) {
      throw new WorkflowError(`Sub-workflow script error: ${e.message}`);
    }
    const prevDepth = ctx.resources.depth;
    ctx.resources.depth += 1;
    try {
      const subHooks = makeHooks(ctx, runSubWorkflow);
      return await subParsed.execute(subHooks, sub.args, ctx.resources.budget);
    } finally {
      ctx.resources.depth = prevDepth;
    }
  };
  const hooks = makeHooks(ctx, runSubWorkflow);
  const emitTerminalPhaseDone = () => {
    if (!ctx.currentPhase)
      return;
    ports.progressEmitter.emit({
      type: "phase_done",
      runId: opts.runId,
      phase: ctx.currentPhase
    });
  };
  let result;
  try {
    const returnValue = await parsed.execute(hooks, opts.args, ctx.resources.budget);
    result = { status: "completed", returnValue };
  } catch (e) {
    if (e instanceof WorkflowAbortedError) {
      result = { status: "killed" };
    } else {
      result = { status: "failed", error: e.message };
    }
  }
  emitTerminalPhaseDone();
  ports.progressEmitter.emit({
    type: "run_done",
    runId: opts.runId,
    ...result
  });
  return result;
}
async function resolveSubScript(sub, cwd) {
  if (sub.script)
    return sub.script;
  if (sub.scriptPath)
    return await readFile2(sub.scriptPath, "utf-8");
  if (sub.name) {
    const found = await resolveNamedWorkflow(join2(cwd, WORKFLOW_DIR_NAME), sub.name);
    if (!found)
      throw new WorkflowError(`Sub-workflow "${sub.name}" not found`);
    return found.content;
  }
  throw new WorkflowError("workflow() requires name or scriptPath");
}

// src/validate.ts
var OLD_API_CALL = /\bworkflow\.(agent|parallel|pipeline|phase|log)\s*\(/g;
var HAS_RETURN = /\breturn\b/;
function validateScript(source) {
  const errors = [];
  const warnings = [];
  let meta = null;
  let body = source;
  try {
    const extracted = extractMeta(source);
    meta = extracted.meta;
    body = extracted.body;
    if (!meta) {
      errors.push({
        severity: "error",
        message: "workflow 脚本必须包含 export const meta = { name, description }（宿主依赖 meta.name 标识 workflow）。请补上 meta 声明。"
      });
    }
  } catch {}
  try {
    parseScript(source);
  } catch (e) {
    errors.push({
      severity: "error",
      message: e instanceof Error ? e.message : String(e)
    });
  }
  for (const m of body.matchAll(OLD_API_CALL)) {
    errors.push({
      severity: "error",
      message: `检测到旧式调用 workflow.${m[1]}(...)：引擎注入的是顶层自由函数，请改为直接调用 ${m[1]}(...)（无需 workflow. 前缀）。`
    });
  }
  if (!HAS_RETURN.test(body)) {
    warnings.push({
      severity: "warning",
      message: "未检测到 return 语句：脚本将返回 undefined。请在顶层用 return 返回结果（引擎只允许 export const meta，结果靠顶层 return 输出）。"
    });
  }
  return { ok: errors.length === 0, meta, errors, warnings };
}

// src/cli.ts
function cliUsage() {
  console.log(`用法（CLI 子命令）:
  peri-workflow read <runId> [--short] [--json]   # 完整报告（state + return_value + agents 全量输出）
  peri-workflow list [--json]                     # 列出所有 run（按结束时间倒序）
  peri-workflow validate <script.mjs> [--json]    # 校验 workflow 脚本语法（引擎检查 + 静态补充）
  peri-workflow --help                            # 本帮助

无参数时以 JSON-RPC 模式运行（宿主集成，见 DESIGN.md）。
read/list 从当前目录向上自动定位 .claude/workflow-runs/。`);
}
function isCliCommand(cmd) {
  return cmd === "read" || cmd === "list" || cmd === "validate" || cmd === "--help" || cmd === "-h" || cmd === "help";
}
function cliMain(args) {
  const cmd = args[0];
  if (cmd === "read") {
    const runId = args.slice(1).find((a) => !a.startsWith("--"));
    if (!runId) {
      console.error("用法：peri-workflow read <runId> [--short] [--json]（--help 查看更多）");
      process.exit(1);
    }
    reportRun(runId, args.includes("--short"), args.includes("--json"));
  } else if (cmd === "list") {
    listRuns(args.includes("--json"));
  } else if (cmd === "validate") {
    validateFile(args.slice(1).find((a) => !a.startsWith("--")), args.includes("--json"));
  } else {
    cliUsage();
    process.exit(0);
  }
}
function validateFile(file, json) {
  if (!file) {
    console.error("用法：peri-workflow validate <script.mjs> [--json]（--help 查看更多）");
    process.exit(1);
  }
  let source;
  try {
    source = readFileSync2(file, "utf8");
  } catch {
    console.error(`无法读取文件: ${file}`);
    process.exit(1);
  }
  const r = validateScript(source);
  if (json) {
    console.log(JSON.stringify({
      file,
      ok: r.ok,
      meta: r.meta,
      errors: r.errors.map((e) => e.message),
      warnings: r.warnings.map((e) => e.message)
    }, null, 2));
    if (!r.ok)
      process.exit(1);
    return;
  }
  if (r.ok && r.warnings.length === 0) {
    const name = r.meta?.name ? ` (${r.meta.name})` : "";
    console.log(`✓ ${file} 校验通过${name}`);
    return;
  }
  if (r.ok) {
    console.log(`✓ ${file} 校验通过（${r.warnings.length} 个警告）：`);
    for (const w of r.warnings)
      console.log(`  ⚠ ${w.message}`);
    return;
  }
  console.log(`✗ ${file} 校验失败（${r.errors.length} 个错误）：`);
  for (const e of r.errors)
    console.log(`  ✗ ${e.message}`);
  for (const w of r.warnings)
    console.log(`  ⚠ ${w.message}`);
  process.exit(1);
}

// src/jsonrpc.ts
import * as readline from "readline";

// src/rpc.ts
var reqId = 100;
var _msgSeq = 0;
var writeOut = (line) => process.stdout.write(line);
var pending = new Map;
function send(msg) {
  _msgSeq++;
  writeOut(JSON.stringify(msg) + `
`);
}
function waitDrain() {
  return new Promise((resolve3) => {
    if (process.stdout.writableNeedDrain) {
      process.stdout.once("drain", resolve3);
    } else {
      resolve3();
    }
  });
}
function rpcRequest(method, params) {
  const id = reqId++;
  return new Promise((resolve3, reject) => {
    pending.set(id, { resolve: resolve3, reject });
    send({ jsonrpc: "2.0", id, method, params });
  });
}
function rpcNotify(method, params) {
  send({ jsonrpc: "2.0", method, params });
}
function handleResponse(msg) {
  const entry = pending.get(msg.id);
  if (!entry)
    return;
  pending.delete(msg.id);
  if (msg.error) {
    entry.reject(msg.error);
  } else {
    entry.resolve(msg.result);
  }
}

// src/adapter.ts
var rpcAdapter = {
  id: "perihelion-rpc",
  capabilities: { structuredOutput: true, tools: true },
  async run(params, ctx) {
    try {
      return await rpcRequest("agent/run", {
        runId: ctx.runId,
        agentId: ctx.agentId,
        prompt: params.prompt,
        schema: params.schema,
        model: params.model,
        maxTokens: params.maxTokens,
        agentType: params.agentType,
        isolation: params.isolation,
        allowedTools: params.allowedTools,
        label: params.label,
        phase: params.phase
      });
    } catch (err) {
      if (typeof err === "object" && err !== null && "code" in err && err.code === -32000) {
        throw new WorkflowAbortedError;
      }
      return { kind: "dead", reason: "runagent-threw", detail: String(err) };
    }
  }
};

// src/types.ts
var WORKFLOW_PROTOCOL_VERSION = 1;
var WORKFLOW_BUILD_ID = "@peri-code/workflow@0.2.0";

// src/server.ts
var currentRunId;
var currentAbortController;
var currentCwd;
var currentBudget;
var currentResumeJournal;
function createPorts() {
  return {
    agentAdapterRegistry: new AgentAdapterRegistry().register(rpcAdapter).default("perihelion-rpc"),
    agentRunner: {
      async runAgentToResult(params, _host) {
        return { kind: "dead", reason: "unknown", detail: "agentRunner fallback — use adapterRegistry" };
      }
    },
    progressEmitter: {
      emit(event) {
        rpcNotify("progress/event", event);
      }
    },
    taskRegistrar: {
      register() {
        return { runId: currentRunId, signal: currentAbortController.signal };
      },
      complete() {},
      fail() {},
      kill() {
        currentAbortController.abort();
      },
      pendingAction() {
        return null;
      }
    },
    journalStore: {
      async read() {
        return currentResumeJournal ?? [];
      },
      async append(runId, entry) {
        rpcNotify("journal/append", { runId, entry });
      },
      async truncate(runId) {
        rpcNotify("journal/truncate", { runId });
      }
    },
    permissionGate: { isAborted: () => false },
    logger: {
      debug(msg) {
        rpcNotify("log", { level: "debug", message: msg });
      },
      event(name, meta) {
        rpcNotify("log", { level: "event", message: name, meta });
      },
      warn(msg) {
        rpcNotify("log", { level: "warn", message: msg });
      },
      error(msg) {
        rpcNotify("log", { level: "error", message: msg });
      }
    },
    hostFactory(args) {
      return {
        handle: createHostHandle(null),
        cwd: currentCwd,
        budgetTotal: currentBudget
      };
    }
  };
}
async function handleRequest(msg) {
  const { id, method, params } = msg;
  switch (method) {
    case "workflow/start": {
      const p = params;
      currentRunId = p.runId;
      currentCwd = p.cwd;
      currentBudget = p.budgetTotal;
      currentResumeJournal = p.resume;
      currentAbortController = new AbortController;
      send({
        jsonrpc: "2.0",
        id,
        result: {
          ok: true,
          protocolVersion: WORKFLOW_PROTOCOL_VERSION,
          buildId: WORKFLOW_BUILD_ID
        }
      });
      runWorkflowAsync(p).catch(async (err) => {
        await waitDrain();
        rpcNotify("workflow/done", {
          runId: p.runId,
          status: "failed",
          error: String(err)
        });
        await waitDrain();
        process.exit(1);
      });
      return;
    }
    case "workflow/kill": {
      currentAbortController?.abort();
      send({
        jsonrpc: "2.0",
        id,
        result: { ok: true }
      });
      return;
    }
    default:
      send({
        jsonrpc: "2.0",
        id,
        error: {
          code: -32601,
          message: `unknown method: ${method}`
        }
      });
  }
}
async function runWorkflowAsync({
  runId,
  script,
  args,
  maxConcurrency
}) {
  parseScript(script);
  const result = await runWorkflow({
    script,
    args,
    runId,
    ports: createPorts(),
    host: createHostHandle(null),
    signal: currentAbortController.signal,
    cwd: currentCwd,
    budgetTotal: currentBudget,
    maxConcurrency,
    resume: !!currentResumeJournal
  });
  await waitDrain();
  rpcNotify("workflow/done", {
    runId,
    status: result.status,
    returnValue: result.returnValue,
    error: result.error
  });
  await waitDrain();
  process.exit(0);
}

// src/jsonrpc.ts
function startJsonRpc() {
  const rl = readline.createInterface({ input: process.stdin });
  rl.on("line", (line) => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    if ("id" in msg && msg.id !== undefined && (("result" in msg) || ("error" in msg))) {
      handleResponse(msg);
      return;
    }
    if ("method" in msg && msg.method) {
      handleRequest(msg);
    }
  });
}

// src/index.ts
var args = process.argv.slice(2);
if (isCliCommand(args[0])) {
  cliMain(args);
} else {
  startJsonRpc();
}
