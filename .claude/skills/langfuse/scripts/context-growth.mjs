#!/usr/bin/env bun
/**
 * Context Growth Analyzer — 分析 session 上下文膨胀轨迹
 *
 * 用法: bun run context-growth.mjs --dir <data-dir> --session <id>
 */

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

let dataDir = "", sessionPrefix = "";

for (let i = 2; i < process.argv.length; i++) {
  if (process.argv[i] === "--dir" && i + 1 < process.argv.length) dataDir = process.argv[++i];
  else if (process.argv[i] === "--session" && i + 1 < process.argv.length) sessionPrefix = process.argv[++i];
}
if (!dataDir || !sessionPrefix) { console.error("用法: bun run context-growth.mjs --dir <dir> --session <id>"); process.exit(1); }

function readJson(fp) { try { return JSON.parse(readFileSync(fp, "utf-8")); } catch { return null; } }
function readText(fp) { try { return readFileSync(fp, "utf-8"); } catch { return null; } }

// ─── 加载 ────────────────────────────────────────────────────────────

const entries = readdirSync(dataDir).sort().map(name => {
  const req = readJson(join(dataDir, name, "request.json"));
  if (!req?.headers?.["x-session-id"]) return null;
  if (!String(req.headers["x-session-id"]).startsWith(sessionPrefix)) return null;
  const streamLog = readText(join(dataDir, name, "stream.log"));
  // usage from stream.log if available
  let usage = {};
  if (streamLog) {
    for (const line of streamLog.split("\n")) {
      if (!line.startsWith("data: ") || line.startsWith("data: [DONE]")) continue;
      try { const ev = JSON.parse(line.slice(6)); if (ev.usage) usage = { ...ev.usage }; } catch {}
    }
  }
  return { name, messages: req.body?.messages || [], usage, streamLog };
}).filter(Boolean);

if (!entries.length) { console.error(`未找到 session "${sessionPrefix}"`); process.exit(1); }

// ─── 估算 token ──────────────────────────────────────────────────────

// 精确按 chars/3.5 估算 (英文代码token化率约3.0-4.0 chars/token)
function estTokens(str) { return Math.round(str.length / 3.5); }

// 分类消息并估算 token
function classify(msg) {
  const raw = JSON.stringify(msg);
  const tokens = estTokens(raw);
  const role = msg.role || "?";
  let type = role, label = "";

  if (role === "assistant") {
    const content = Array.isArray(msg.content) ? msg.content : [];
    const toolUse = content.filter(c => c.type === "tool_use");
    const think = content.find(c => c.type === "thinking" || c.type === "reasoning");
    if (toolUse.length) { type = "tool_use"; label = `${toolUse.length}calls`; }
    else if (think) { type = "thinking"; label = `${tokens}T`; }
    else { type = "text"; label = `${tokens}T`; }
  } else if (role === "user" || role === "tool") {
    const tc = (Array.isArray(msg.content) ? msg.content : []).find(c => c.type === "text" || c.type === "tool_result");
    if (tc?.text) {
      const compacted = tc.text.includes("[compacted:");
      type = compacted ? "compacted" : "tool_result";
      label = compacted ? "[·]" : `${(tc.text.length/1024).toFixed(1)}K`;
    }
  } else if (role === "system") {
    type = "system";
    label = `${tokens}T`;
  }

  return { role, type, tokens, label };
}

// ─── 分析轮次 ────────────────────────────────────────────────────────

const rounds = [];
for (let i = 0; i < entries.length; i++) {
  const e = entries[i];
  const prev = i > 0 ? entries[i - 1] : null;
  const msgs = e.messages;
  const classified = msgs.map(classify);

  // 各类型 token 合计
  const comp = { system: 0, thinking: 0, tool_use: 0, tool_result: 0, compacted: 0, text: 0 };
  for (const c of classified) {
    if (c.type === "system") comp.system += c.tokens;
    else if (c.type === "thinking") comp.thinking += c.tokens;
    else if (c.type === "tool_use") comp.tool_use += c.tokens;
    else if (c.type === "tool_result") comp.tool_result += c.tokens;
    else if (c.type === "compacted") comp.compacted += c.tokens;
    else comp.text += c.tokens; // user messages, system notes etc
  }
  const totalTokens = msgs.length ? estTokens(JSON.stringify(msgs)) : 0;

  // 增长
  let growth = 0, growthMsgs = [];
  if (prev) {
    growth = msgs.length - prev.messages.length;
    growthMsgs = classified.slice(prev.messages.length);
  }

  // 新增消息摘要
  const newSummary = growthMsgs.length ? growthMsgs.map(c => c.label).filter(Boolean).slice(0, 6).join(",") : "";

  // 工具调用(最后一条 assistant)
  let tools = [];
  for (let j = msgs.length - 1; j >= 0; j--) {
    if (msgs[j].role === "assistant") {
      const content = Array.isArray(msgs[j].content) ? msgs[j].content : [];
      tools = content.filter(c => c.type === "tool_use").map(c => c.name);
      break;
    }
  }

  // thinking (最后一条 assistant 的 thinking)
  let thinkingText = "";
  for (let j = msgs.length - 1; j >= 0; j--) {
    if (msgs[j].role === "assistant") {
      const content = Array.isArray(msgs[j].content) ? msgs[j].content : [];
      const th = content.find(c => c.type === "thinking" || c.type === "reasoning");
      if (th) { thinkingText = (th.thinking || "").slice(0, 200); }
      break;
    }
  }

  // compact 事件检测
  let compactEvent = null;
  if (prev && prev.messages.length > 10 && msgs.length < prev.messages.length * 0.55) {
    compactEvent = { type: "full", prevMsg: prev.messages.length, currMsg: msgs.length };
  }

  // LLM usage (from stream.log, roughly match for reference)
  const llmInput = e.usage.input_tokens || 0;
  const llmCache = e.usage.cache_read_input_tokens || 0;

  rounds.push({
    round: i + 1, msgs: msgs.length, totalTokens,
    comp,
    growth, newSummary, tools, thinkingText: thinkingText.slice(0, 100),
    compactEvent,
    llmInput, llmCache,
  });
}

// ─── 输出 ────────────────────────────────────────────────────────────

const R="\x1b[31m",G="\x1b[32m",Y="\x1b[33m",C="\x1b[36m",B="\x1b[1m",D="\x1b[2m",E="\x1b[0m";
function pad(s,n) { return String(s).padEnd(n); }

console.log(`\n${B}══ 上下文增长分析: ${sessionPrefix}  |  ${rounds.length} 轮${E}\n`);

// 紧凑模式：只显示关键变化点
const keyRounds = [];
for (let i = 0; i < rounds.length; i++) {
  const r = rounds[i];
  const isKey = i === 0 || i === rounds.length - 1 ||
    r.compactEvent ||
    Math.abs(r.growth) >= 10 ||
    (r.tools.length && r.growth >= 4) ||  // 有工具调用且有明显增长
    r.llmInput > 0;  // 有 LLM usage 的就是真实调用

  if (isKey) keyRounds.push(r);
  else if (keyRounds.length && keyRounds[keyRounds.length - 1].round !== i - 1) {
    keyRounds.push({ ...r, collapsed: true });
  }
}

console.log(`${B}── 关键轮次 (消息数 & 组成 K tokens) ──${E}`);
console.log(`${D}${pad("轮",4)}${pad("Msg",5)}${pad("Δ",5)}${pad("估算T",8)}${pad("SYS",5)}${pad("THK",5)}${pad("CALL",5)}${pad("TOOL",6)}${pad("CMP",5)}  新增摘要  标记${E}`);

for (const r of keyRounds) {
  const c = r.comp;
  const gs = r.growth > 0 ? `${R}+${r.growth}`.padEnd(4)+E : r.growth < 0 ? `${G}${r.growth}`.padEnd(4)+E : `${D}  - ${E}`;
  const tk = Math.round(r.totalTokens / 1000).toLocaleString();
  const csys = Math.round(c.system / 1000);
  const cthk = Math.round(c.thinking / 1000);
  const ccall = Math.round(c.tool_use / 1000);
  const ctool = Math.round(c.tool_result / 1000);
  const ccmp = Math.round(c.compacted / 1000);

  let flags = "";
  if (r.compactEvent) flags += ` ${Y}[FULL ${r.compactEvent.prevMsg}→${r.compactEvent.currMsg}]${E}`;
  if (r.llmInput > 0) flags += ` ${C}LLM:${Math.round(r.llmInput/1e3)}K hit:${Math.round(r.llmCache/r.llmInput*100)}%${E}`;
  if (r.tools.length) flags += ` 🛠${r.tools.slice(0,4).join(",")}`;
  if (r.thinkingText) flags += ` 💭${r.thinkingText.slice(0,50)}`;

  console.log(`${pad(String(r.round),4)}${pad(String(r.msgs),5)}${gs}${pad(String(tk)+"K",8)}${pad(String(csys),5)}${pad(String(cthk),5)}${pad(String(ccall),5)}${pad(String(ctool),6)}${pad(String(ccmp),5)} ${r.newSummary.slice(0,50)}${flags}`);
}

// 汇总
const first = rounds[0], last = rounds[rounds.length - 1];
const totalMsgGrowth = last.msgs - first.msgs;
const totalTokenGrowth = last.totalTokens - first.totalTokens;

console.log(`\n${B}── 汇总 ──${E}`);
console.log(`  消息: ${first.msgs} → ${last.msgs} (+${totalMsgGrowth})`);
console.log(`  估算 tokens: ${Math.round(first.totalTokens/1e3)}K → ${Math.round(last.totalTokens/1e3)}K (+${Math.round(totalTokenGrowth/1e3)}K)`);
console.log(`  平均每轮: +${(totalMsgGrowth/rounds.length).toFixed(1)} msgs, +${Math.round(totalTokenGrowth/rounds.length/1e3)}K tokens`);
console.log(`  LLM 实际调用: ${rounds.filter(r=>r.llmInput>0).length} 次`);
console.log(`  Full compact: ${rounds.filter(r=>r.compactEvent).length} 次`);

// 上下文组成变化
console.log(`\n${B}── 组成贡献 ──${E}`);
const lastComp = last.comp;
const nonSystem = last.totalTokens - lastComp.system;
console.log(`  系统提示: ${Math.round(lastComp.system/1e3)}K tokens`);
console.log(`  工具结果: ${Math.round(lastComp.tool_result/1e3)}K (${(lastComp.tool_result/nonSystem*100).toFixed(0)}%)`);
console.log(`  已压缩: ${Math.round(lastComp.compacted/1e3)}K (${(lastComp.compacted/nonSystem*100).toFixed(0)}%)`);
console.log(`  思考: ${Math.round(lastComp.thinking/1e3)}K`);
console.log(`  工具调用: ${Math.round(lastComp.tool_use/1e3)}K`);
console.log(`  对话: ${Math.round(lastComp.text/1e3)}K`);
