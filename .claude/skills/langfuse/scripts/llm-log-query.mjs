#!/usr/bin/env bun
/**
 * LLM Log Query Tool — 分析 llm-gateway 日志目录
 *
 * 用法: bun run llm-log-query.mjs <command> [options]
 * 命令: list | show | session | diff | stats | cache | cache-debug | cache-control
 */

import { readdirSync, readFileSync, existsSync, statSync } from "node:fs";
import { join, basename } from "node:path";

// ─── 工具函数 ─────────────────────────────────────────────────────

function resolveDataDir(dir) {
  if (!dir) {
    // 默认相对于 side-projects/llm-gateway/data
    const candidates = [
      "side-projects/llm-gateway/data",
      "data",
    ];
    for (const c of candidates) {
      if (existsSync(c)) return c;
    }
    console.error("找不到数据目录，请用 --dir 指定");
    process.exit(1);
  }
  return dir;
}

function readJson(filePath) {
  try {
    return JSON.parse(readFileSync(filePath, "utf-8"));
  } catch {
    return null;
  }
}

function readText(filePath) {
  try {
    return readFileSync(filePath, "utf-8");
  } catch {
    return null;
  }
}

function truncStr(s, max = 100) {
  if (!s || typeof s !== "string") return "";
  const flat = s.replace(/\n/g, "\\n");
  return flat.length > max ? flat.slice(0, max) + "…" : flat;
}

function parseEntryName(name) {
  // 2026-05-14_06-15-49-901_0001
  const m = name.match(/^(\d{4}-\d{2}-\d{2})_(\d{2})-(\d{2})-(\d{2})-(\d{3})_(\d+)$/);
  if (!m) return null;
  return {
    date: m[1],
    time: `${m[2]}:${m[3]}:${m[4]}.${m[5]}`,
    seq: parseInt(m[6]),
    iso: `${m[1]}T${m[2]}:${m[3]}:${m[4]}.${m[5]}Z`,
    sortKey: `${m[1]}${m[2]}${m[3]}${m[4]}${m[5]}_${m[6].padStart(6, "0")}`,
  };
}

// ─── 从 stream.log 提取数据 ──────────────────────────────────────

function extractUsageFromStream(streamLog) {
  if (!streamLog) return null;
  let usage = null;
  for (const line of streamLog.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("data: ")) continue;
    const dataStr = trimmed.slice(6);
    if (dataStr === "[DONE]") continue;
    try {
      const data = JSON.parse(dataStr);
      // message_delta carries final usage with output_tokens
      if (data.type === "message_delta" && data.usage) {
        const u = data.usage;
        const rawInput = u.input_tokens || 0;
        const cacheCreation = u.cache_creation_input_tokens || 0;
        const cacheRead = u.cache_read_input_tokens || 0;
        usage = {
          input: rawInput + cacheCreation + cacheRead,
          output: u.output_tokens || 0,
          cacheCreation,
          cacheRead,
        };
      }
      // Fallback: message_start also has usage (output_tokens=0 initially)
      if (data.type === "message_start" && data.message?.usage && !usage) {
        const u = data.message.usage;
        const rawInput = u.input_tokens || 0;
        const cacheCreation = u.cache_creation_input_tokens || 0;
        const cacheRead = u.cache_read_input_tokens || 0;
        usage = {
          input: rawInput + cacheCreation + cacheRead,
          output: u.output_tokens || 0,
          cacheCreation,
          cacheRead,
        };
      }
    } catch { /* skip */ }
  }
  return usage;
}

function extractStreamMetadata(streamLog, headers) {
  // Extract route from headers.host
  let route = "";
  const host = headers["host"] || "";
  if (host.includes("anthropic")) route = "anthropic";
  else if (host.includes("openai")) route = "openai";
  else if (host) route = host.replace(/^api\./, "").split(".")[0];

  // Determine status from stream.log presence and content
  let status = "";
  let latency = "";
  if (streamLog) {
    if (streamLog.includes("message_stop")) {
      status = "200";
    } else if (streamLog.includes('"error"')) {
      status = "error";
    }
    // Extract latency from content-length + timing heuristic (not available in stream-only logs)
  }

  return { route, status, latency };
}

function extractStreamResponse(streamLog) {
  if (!streamLog) return null;
  const content = [];  // text blocks
  const thinking = []; // thinking blocks
  const toolCalls = []; // tool_use blocks
  let stopReason = "";
  let model = "";

  for (const line of streamLog.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("data: ")) continue;
    const dataStr = trimmed.slice(6);
    if (dataStr === "[DONE]") continue;
    try {
      const data = JSON.parse(dataStr);
      if (data.type === "message_start" && data.message) {
        model = data.message.model || "";
        stopReason = data.message.stop_reason || "";
      }
      if (data.type === "content_block_start") {
        const block = data.content_block;
        if (block?.type === "text") content.push("");
        else if (block?.type === "thinking") thinking.push("");
        else if (block?.type === "tool_use") toolCalls.push({ id: block.id, name: block.name, input: "" });
      }
      if (data.type === "content_block_delta") {
        const delta = data.delta;
        if (delta?.type === "text_delta" && content.length > 0) {
          content[content.length - 1] += delta.text;
        } else if (delta?.type === "thinking_delta" && thinking.length > 0) {
          thinking[thinking.length - 1] += delta.thinking;
        } else if (delta?.type === "input_json_delta" && toolCalls.length > 0) {
          toolCalls[toolCalls.length - 1].input += delta.partial_json;
        }
      }
      if (data.type === "message_delta") {
        if (data.delta?.stop_reason) stopReason = data.delta.stop_reason;
      }
    } catch { /* skip */ }
  }

  return { content, thinking, toolCalls, stopReason, model };
}


function loadEntries(dataDir) {
  let dirs;
  try {
    dirs = readdirSync(dataDir).filter((d) => {
      const p = join(dataDir, d);
      return statSync(p).isDirectory() && parseEntryName(d);
    });
  } catch {
    console.error(`无法读取目录: ${dataDir}`);
    process.exit(1);
  }

  return dirs.map((d) => {
    const parsed = parseEntryName(d);
    const dir = join(dataDir, d);
    const reqRaw = readJson(join(dir, "request.json"));
    const resRaw = readJson(join(dir, "response.json"));
    const streamLog = readText(join(dir, "stream.log"));
    const logTxt = readText(join(dir, "log.txt"));

    // 兼容两种格式: { headers, body } 或裸 body
    let headers = {};
    let body = reqRaw;
    if (reqRaw && reqRaw.body !== undefined && typeof reqRaw.body === "object") {
      headers = reqRaw.headers || {};
      body = reqRaw.body;
    }

    // 从 log.txt 提取元信息（旧格式），或从 headers/stream.log 推导（新格式）
    let route = "";
    let latency = "";
    let status = "";
    if (logTxt) {
      const routeM = logTxt.match(/ROUTE:\s*(.+)/);
      route = routeM ? routeM[1].trim() : "";
      const latencyM = logTxt.match(/LATENCY:\s*(\d+)ms/);
      latency = latencyM ? latencyM[1] : "";
      const statusM = logTxt.match(/STATUS:\s*(\d+)/);
      status = statusM ? statusM[1] : "";
      if (!headers["x-session-id"] && !headers["x-litellm-session-id"]) {
        const sessionM = logTxt.match(/^\s*x-session-id:\s*(.+)$/mi);
        if (sessionM) headers["x-session-id"] = sessionM[1].trim();
      }
    }
    if (!route || !status) {
      const meta = extractStreamMetadata(streamLog, headers);
      if (!route) route = meta.route;
      if (!status) status = meta.status;
      if (!latency) latency = meta.latency;
    }

    const sessionId = headers["x-session-id"] || headers["x-litellm-session-id"] || "";
    const model = body?.model || "";
    const msgCount = body?.messages?.length || 0;
    const hasTools = !!(body?.tools?.length);
    const hasStream = !!streamLog;
    const isStream = body?.stream === true;

    // token usage — 优先从 response.json，fallback 到 stream.log
    let usage = null;
    if (resRaw?.usage) {
      const u = resRaw.usage;
      if (u.prompt_tokens !== undefined) {
        usage = {
          input: u.prompt_tokens || 0,
          output: u.completion_tokens || 0,
          cacheCreation: 0,
          cacheRead: u.prompt_tokens_details?.cached_tokens || 0,
        };
      } else {
        const rawInput = u.input_tokens || 0;
        const cacheCreation = u.cache_creation_input_tokens || 0;
        const cacheRead = u.cache_read_input_tokens || 0;
        usage = {
          input: rawInput + cacheCreation + cacheRead,
          output: u.output_tokens || 0,
          cacheCreation,
          cacheRead,
        };
      }
    }
    if (!usage && streamLog) {
      usage = extractUsageFromStream(streamLog);
    }

    return {
      id: d,
      ...parsed,
      dir,
      headers,
      body,
      response: resRaw,
      streamLog,
      logTxt,
      route,
      latency,
      status,
      sessionId,
      model,
      msgCount,
      hasTools,
      hasStream,
      isStream,
      usage,
    };
  }).sort((a, b) => a.sortKey.localeCompare(b.sortKey));
}

// ─── list 命令 ────────────────────────────────────────────────────

function cmdList(args) {
  const dataDir = resolveDataDir(args.dir);
  let entries = loadEntries(dataDir);

  // 过滤
  if (args.model) entries = entries.filter((e) => e.model.toLowerCase().includes(args.model.toLowerCase()));
  if (args.session) entries = entries.filter((e) => e.sessionId === args.session);
  if (args.route) entries = entries.filter((e) => e.route.toLowerCase().includes(args.route.toLowerCase()));
  if (args.after) entries = entries.filter((e) => e.date >= args.after);
  if (args.before) entries = entries.filter((e) => e.date <= args.before);
  if (args.errors) entries = entries.filter((e) => e.status && parseInt(e.status) >= 400);

  const limit = parseInt(args.limit) || 50;
  const show = entries.slice(-limit);

  if (show.length === 0) {
    console.log("没有匹配的请求记录");
    return;
  }

  console.log(`共 ${entries.length} 条记录，显示最近 ${show.length} 条\n`);
  console.log("序号 | 请求ID                               | 时间              | 路由          | 模型                         | Session                         | 消息 | 状态 | 延迟");
  console.log("-".repeat(160));
  show.forEach((e, i) => {
    const id = e.id.padEnd(38);
    const time = `${e.date} ${e.time}`.padEnd(18);
    const route = e.route.padEnd(14);
    const model = truncStr(e.model, 28).padEnd(30);
    const sid = truncStr(e.sessionId, 30).padEnd(32);
    const msg = String(e.msgCount).padEnd(4);
    const status = (e.status || "?").padEnd(5);
    const lat = (e.latency ? e.latency + "ms" : "").padEnd(6);
    console.log(`${String(i + 1).padStart(3)} | ${id} | ${time} | ${route} | ${model} | ${sid} | ${msg} | ${status} | ${lat}`);
  });
}

// ─── show 命令 ────────────────────────────────────────────────────

function cmdShow(args) {
  const dataDir = resolveDataDir(args.dir);
  const entries = loadEntries(dataDir);
  const target = args._[0];
  if (!target) { console.error("用法: show <request-id>"); process.exit(1); }

  const entry = entries.find((e) => e.id === target || e.id.startsWith(target));
  if (!entry) { console.error(`找不到请求: ${target}`); process.exit(1); }

  const e = entry;

  console.log("═".repeat(72));
  console.log(`请求: ${e.id}`);
  console.log(`时间: ${e.date} ${e.time}`);
  console.log(`路由: ${e.route}`);
  console.log(`模型: ${e.model}`);
  console.log(`Session: ${e.sessionId || "(无)"}`);
  console.log(`状态: ${e.status}  延迟: ${e.latency}ms`);
  if (e.usage) {
    console.log(`Token: input=${e.usage.input} output=${e.usage.output} cache_read=${e.usage.cacheRead} cache_creation=${e.usage.cacheCreation || 0}`);
  }
  if (e.body?.thinking) {
    console.log(`Thinking: ${e.body.thinking.type} budget=${e.body.thinking.budget_tokens || "N/A"}`);
  }
  if (e.body?.output_config) {
    console.log(`Output Config: ${JSON.stringify(e.body.output_config)}`);
  }
  console.log("═".repeat(72));

  if (args.headers) {
    console.log("\n── Headers ──");
    console.log(JSON.stringify(e.headers, null, 2));
  }

  if (args.messages) {
    console.log("\n── Messages ──");
    if (e.body?.system) {
      const sys = e.body.system;
      if (Array.isArray(sys)) {
        sys.forEach((block, i) => {
          console.log(`  S${i}. [system] ${truncStr(block.text || JSON.stringify(block), 120)}${block.cache_control ? " [cached]" : ""}`);
        });
      } else if (typeof sys === "string") {
        console.log(`  S0. [system] ${truncStr(sys, 120)}`);
      }
    }
    if (e.body?.messages) {
      e.body.messages.forEach((m, i) => {
        const role = m.role || "?";
        const content = truncStr(typeof m.content === "string" ? m.content : JSON.stringify(m.content), 120);
        const toolCalls = m.tool_calls?.length ? ` [${m.tool_calls.length} tool_calls]` : "";
        console.log(`  ${i}. [${role}]${toolCalls} ${content}`);
      });
    }
    if (!e.body?.messages && !e.body?.system) {
      console.log("  (无消息)");
    }
  }

  if (args.tools) {
    console.log("\n── Tools ──");
    if (e.body?.tools) {
      e.body.tools.forEach((t, i) => {
        const name = t.function?.name || t.name || "?";
        const cc = t.cache_control ? " [cached]" : "";
        console.log(`  ${i}. ${name}${cc}`);
      });
      console.log(`  共 ${e.body.tools.length} 个工具`);
    } else {
      console.log("  (无工具)");
    }
  }

  if (args.body) {
    console.log("\n── Request Body ──");
    console.log(JSON.stringify(e.body, null, 2));
  }

  if (args.stream && e.streamLog) {
    console.log("\n── Stream Events ──");
    parseStreamLog(e.streamLog).forEach((evt) => {
      console.log(`  [${evt.type}] ${truncStr(JSON.stringify(evt.data), 120)}`);
    });
  }

  if (!args.stream && !args.body && !args.messages && !args.tools && !args.headers) {
    // 默认显示响应摘要
    console.log("\n── Response ──");
    if (e.response) {
      if (e.response.choices?.[0]) {
        const choice = e.response.choices[0];
        console.log(`  finish_reason: ${choice.finish_reason}`);
        if (choice.message?.content) console.log(`  content: ${truncStr(choice.message.content, 200)}`);
        if (choice.message?.tool_calls) console.log(`  tool_calls: ${choice.message.tool_calls.length} 个`);
      } else if (e.response.content) {
        const textParts = e.response.content.filter((c) => c.type === "text").map((c) => c.text);
        console.log(`  content: ${truncStr(textParts.join(""), 200)}`);
        console.log(`  stop_reason: ${e.response.stop_reason}`);
      }
      if (e.response.error) console.log(`  error: ${JSON.stringify(e.response.error)}`);
    } else if (e.streamLog) {
      // 新格式：从 stream.log 提取响应
      const streamResp = extractStreamResponse(e.streamLog);
      if (streamResp) {
        console.log(`  stop_reason: ${streamResp.stopReason}`);
        if (streamResp.model) console.log(`  stream_model: ${streamResp.model}`);
        if (streamResp.thinking.length > 0) {
          const t = streamResp.thinking.join("\n");
          console.log(`  thinking: ${truncStr(t, 200)}`);
        }
        if (streamResp.content.length > 0) {
          const t = streamResp.content.join("\n");
          console.log(`  content: ${truncStr(t, 200)}`);
        }
        if (streamResp.toolCalls.length > 0) {
          console.log(`  tool_calls: ${streamResp.toolCalls.length} 个`);
          streamResp.toolCalls.forEach((tc, i) => {
            console.log(`    ${i}. ${tc.name} (${tc.id})`);
          });
        }
      }
    }
  }
}

function parseStreamLog(text) {
  const events = [];
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("data: ")) continue;
    const dataStr = trimmed.slice(6);
    if (dataStr === "[DONE]") {
      events.push({ type: "done", data: null });
      continue;
    }
    try {
      const data = JSON.parse(dataStr);
      // OpenAI stream format
      if (data.choices?.[0]?.delta) {
        const delta = data.choices[0].delta;
        if (delta.content) events.push({ type: "text", data: delta.content });
        else if (delta.tool_calls) events.push({ type: "tool_call", data: delta.tool_calls });
        else if (delta.role) events.push({ type: "role", data: delta.role });
        else events.push({ type: "delta", data });
      }
      // Anthropic stream format
      else if (data.type === "content_block_delta") {
        if (data.delta?.text) events.push({ type: "text", data: data.delta.text });
        else if (data.delta?.thinking) events.push({ type: "thinking", data: data.delta.thinking });
        else if (data.delta?.partial_json) events.push({ type: "tool_input", data: data.delta.partial_json });
        else events.push({ type: data.type, data: data.delta });
      } else if (data.type === "message_start" || data.type === "message_delta") {
        events.push({ type: data.type, data: data });
      } else {
        events.push({ type: data.type || "unknown", data });
      }
    } catch {
      events.push({ type: "raw", data: dataStr.slice(0, 80) });
    }
  }
  return events;
}

// ─── session 命令 ─────────────────────────────────────────────────

function cmdSession(args) {
  const dataDir = resolveDataDir(args.dir);
  const entries = loadEntries(dataDir);
  const sessionId = args._[0];
  if (!sessionId) { console.error("用法: session <session-id>"); process.exit(1); }

  const sessionEntries = entries.filter((e) => e.sessionId === sessionId);
  if (sessionEntries.length === 0) {
    console.error(`找不到 session: ${sessionId}`);
    process.exit(1);
  }

  console.log("═".repeat(72));
  console.log(`Session: ${sessionId}`);
  console.log(`请求数: ${sessionEntries.length}`);
  console.log(`时间范围: ${sessionEntries[0].date} ${sessionEntries[0].time} ~ ${sessionEntries[sessionEntries.length - 1].date} ${sessionEntries[sessionEntries.length - 1].time}`);
  console.log("═".repeat(72));

  // 检查是否有 diff 子命令
  if (args._[1] === "diff") {
    cmdSessionDiff(args, sessionEntries);
    return;
  }

  let totalInput = 0, totalOutput = 0;
  sessionEntries.forEach((e, i) => {
    console.log(`\n── Round ${i + 1} ──`);
    console.log(`  ID: ${e.id}`);
    console.log(`  模型: ${e.model}  路由: ${e.route}  状态: ${e.status}  延迟: ${e.latency}ms`);
    if (e.usage) {
      totalInput += e.usage.input;
      totalOutput += e.usage.output;
      console.log(`  Token: input=${e.usage.input} output=${e.usage.output} cache_read=${e.usage.cacheRead}`);
    }

    if (e.body?.messages) {
      console.log(`  消息 (${e.body.messages.length} 条):`);
      e.body.messages.forEach((m, j) => {
        const role = m.role || "?";
        const content = typeof m.content === "string" ? m.content : JSON.stringify(m.content);
        const toolCalls = m.tool_calls?.length ? ` [+${m.tool_calls.length} tool_calls]` : "";
        if (args.full) {
          console.log(`    ${j}. [${role}]${toolCalls}`);
          console.log(`       ${content}`);
        } else {
          console.log(`    ${j}. [${role}]${toolCalls} ${truncStr(content, 90)}`);
        }
      });
    }

    // 工具调用
    if (e.body?.tools?.length) {
      console.log(`  工具: ${e.body.tools.length} 个`);
    }

    // 响应摘要
    if (e.response) {
      const resp = summarizeResponse(e.response);
      if (resp) console.log(`  响应: ${resp}`);
    }
  });

  console.log(`\n── Session 汇总 ──`);
  console.log(`  总输入 token: ${totalInput}`);
  console.log(`  总输出 token: ${totalOutput}`);
  console.log(`  总请求: ${sessionEntries.length}`);
}

function cmdSessionDiff(args, sessionEntries) {
  const r1 = parseInt(args._[2]);
  const r2 = parseInt(args._[3]);
  if (!r1 || !r2) {
    console.error("用法: session <id> diff <round1> <round2>  (round 从 1 开始)");
    process.exit(1);
  }
  if (r1 < 1 || r2 < 1 || r1 > sessionEntries.length || r2 > sessionEntries.length) {
    console.error(`轮次范围: 1 ~ ${sessionEntries.length}`);
    process.exit(1);
  }

  const e1 = sessionEntries[r1 - 1];
  const e2 = sessionEntries[r2 - 1];
  diffMessages(e1.body?.messages || [], e2.body?.messages || [], r1, r2);
}

function diffMessages(msgs1, msgs2, r1, r2) {
  console.log(`\n── Diff: Round ${r1} vs Round ${r2} ──`);

  const maxLen = Math.max(msgs1.length, msgs2.length);
  let added = 0, removed = 0, changed = 0, same = 0;

  for (let i = 0; i < maxLen; i++) {
    const m1 = msgs1[i];
    const m2 = msgs2[i];

    if (!m1 && m2) {
      added++;
      console.log(`  + [${m2.role}] ${truncStr(contentStr(m2.content), 90)}`);
    } else if (m1 && !m2) {
      removed++;
      console.log(`  - [${m1.role}] ${truncStr(contentStr(m1.content), 90)}`);
    } else if (m1 && m2) {
      const c1 = contentStr(m1.content);
      const c2 = contentStr(m2.content);
      if (c1 === c2 && m1.role === m2.role) {
        same++;
      } else {
        changed++;
        console.log(`  ~ [${m1.role}] → [${m2.role}]`);
        if (c1 !== c2) {
          console.log(`      旧: ${truncStr(c1, 80)}`);
          console.log(`      新: ${truncStr(c2, 80)}`);
        }
      }
    }
  }

  console.log(`\n  ${same} 相同 / ${added} 新增 / ${removed} 删除 / ${changed} 修改`);
}

function contentStr(c) {
  if (typeof c === "string") return c;
  return JSON.stringify(c);
}

function summarizeResponse(res) {
  if (res.error) return `ERROR: ${JSON.stringify(res.error)}`;
  if (res.choices?.[0]) {
    const ch = res.choices[0];
    const parts = [];
    if (ch.message?.content) parts.push(truncStr(ch.message.content, 60));
    if (ch.message?.tool_calls) parts.push(`${ch.message.tool_calls.length} tool_calls`);
    return `${ch.finish_reason} | ${parts.join(", ")}`;
  }
  if (res.content) {
    const texts = res.content.filter((c) => c.type === "text").map((c) => c.text);
    return `${res.stop_reason} | ${truncStr(texts.join(""), 60)}`;
  }
  return null;
}

// ─── diff 命令（直接对比两个请求 ID）──────────────────────────────

function cmdDiff(args) {
  const dataDir = resolveDataDir(args.dir);
  const entries = loadEntries(dataDir);
  const id1 = args._[0];
  const id2 = args._[1];
  if (!id1 || !id2) { console.error("用法: diff <request-id-1> <request-id-2>"); process.exit(1); }

  const e1 = entries.find((e) => e.id === id1 || e.id.startsWith(id1));
  const e2 = entries.find((e) => e.id === id2 || e.id.startsWith(id2));
  if (!e1) { console.error(`找不到请求: ${id1}`); process.exit(1); }
  if (!e2) { console.error(`找不到请求: ${id2}`); process.exit(1); }

  console.log(`\n── Diff: ${e1.id} vs ${e2.id} ──`);

  // 比较基本信息
  const fields = ["model", "route", "status", "latency"];
  for (const f of fields) {
    if (e1[f] !== e2[f]) {
      console.log(`  ${f}: ${e1[f] || "(空)"} → ${e2[f] || "(空)"}`);
    }
  }

  // 比较消息
  diffMessages(e1.body?.messages || [], e2.body?.messages || [], 1, 2);

  // 比较工具
  const tools1 = (e1.body?.tools || []).map((t) => t.function?.name || t.name);
  const tools2 = (e2.body?.tools || []).map((t) => t.function?.name || t.name);
  if (tools1.join(",") !== tools2.join(",")) {
    const addedTools = tools2.filter((t) => !tools1.includes(t));
    const removedTools = tools1.filter((t) => !tools2.includes(t));
    if (addedTools.length) console.log(`  新增工具: ${addedTools.join(", ")}`);
    if (removedTools.length) console.log(`  移除工具: ${removedTools.join(", ")}`);
  }
}

// ─── stats 命令 ───────────────────────────────────────────────────

function cmdStats(args) {
  const dataDir = resolveDataDir(args.dir);
  const entries = loadEntries(dataDir);

  if (entries.length === 0) {
    console.log("没有请求记录");
    return;
  }

  console.log("═".repeat(72));
  console.log(`总请求数: ${entries.length}`);
  console.log(`时间范围: ${entries[0].date} ${entries[0].time} ~ ${entries[entries.length - 1].date} ${entries[entries.length - 1].time}`);

  const errors = entries.filter((e) => e.status && parseInt(e.status) >= 400);
  console.log(`错误: ${errors.length} (${((errors.length / entries.length) * 100).toFixed(1)}%)`);

  // Token 汇总
  const withUsage = entries.filter((e) => e.usage);
  if (withUsage.length > 0) {
    const totalInput = withUsage.reduce((s, e) => s + e.usage.input, 0);
    const totalOutput = withUsage.reduce((s, e) => s + e.usage.output, 0);
    const totalCache = withUsage.reduce((s, e) => s + e.usage.cacheRead, 0);
    console.log(`\nToken 汇总 (${withUsage.length} 条有用量数据):`);
    console.log(`  输入: ${totalInput.toLocaleString()}`);
    console.log(`  输出: ${totalOutput.toLocaleString()}`);
    console.log(`  缓存命中: ${totalCache.toLocaleString()}`);
  }

  const groupBy = args.by || "model";
  const groups = {};
  entries.forEach((e) => {
    let key;
    switch (groupBy) {
      case "model": key = e.model || "(未知)"; break;
      case "session": key = e.sessionId || "(无 session)"; break;
      case "route": key = e.route || "(未知)"; break;
      case "hour": key = `${e.date} ${e.time.slice(0, 2)}:00`; break;
      default: key = e[groupBy] || "(未知)";
    }
    if (!groups[key]) groups[key] = [];
    groups[key].push(e);
  });

  console.log(`\n── 按 ${groupBy} 分组 ──`);
  const sorted = Object.entries(groups).sort((a, b) => b[1].length - a[1].length);
  for (const [key, items] of sorted) {
    const errCount = items.filter((e) => e.status && parseInt(e.status) >= 400).length;
    const errStr = errCount > 0 ? ` (${errCount} 错误)` : "";
    const usageStr = items.some((e) => e.usage)
      ? ` | input=${items.reduce((s, e) => s + (e.usage?.input || 0), 0)} output=${items.reduce((s, e) => s + (e.usage?.output || 0), 0)}`
      : "";
    console.log(`  ${key}: ${items.length} 请求${errStr}${usageStr}`);
  }
  console.log("═".repeat(72));
}

// ─── cache 命令 — 缓存率深度分析 ─────────────────────────────────

function cmdCache(args) {
  const dataDir = resolveDataDir(args.dir);
  let entries = loadEntries(dataDir);

  // 可按 session 过滤
  if (args.session) entries = entries.filter((e) => e.sessionId === args.session);
  if (args.after) entries = entries.filter((e) => e.date >= args.after);
  if (args.before) entries = entries.filter((e) => e.date <= args.before);

  const withUsage = entries.filter((e) => e.usage);
  if (withUsage.length === 0) {
    console.log("没有 token 用量数据，无法分析缓存率");
    return;
  }

  // ── 全局缓存率 ──
  const totalInput = withUsage.reduce((s, e) => s + e.usage.input, 0);
  const totalOutput = withUsage.reduce((s, e) => s + e.usage.output, 0);
  const totalCacheCreation = withUsage.reduce((s, e) => s + (e.usage.cacheCreation || 0), 0);
  const totalCacheRead = withUsage.reduce((s, e) => s + e.usage.cacheRead, 0);

  // 有效输入 = 总 input - cache_read（实际发给模型的新 token）
  const effectiveInput = totalInput - totalCacheRead;
  const cacheHitRate = totalInput > 0 ? (totalCacheRead / totalInput * 100).toFixed(1) : "0.0";
  // 缓存写入率：cache_creation 占总 input 的比例
  const cacheCreationRate = totalInput > 0 ? (totalCacheCreation / totalInput * 100).toFixed(1) : "0.0";
  // 无效输入率：既没命中缓存也没写入缓存的部分（cold miss）
  const coldMissInput = totalInput - totalCacheRead - totalCacheCreation;
  const coldMissRate = totalInput > 0 ? (coldMissInput / totalInput * 100).toFixed(1) : "0.0";

  console.log("═".repeat(72));
  console.log("缓存率分析报告");
  console.log("═".repeat(72));
  console.log(`\n── 全局概况 ──`);
  console.log(`  总请求数:       ${entries.length}（其中 ${withUsage.length} 条有用量）`);
  console.log(`  总输入 token:   ${totalInput.toLocaleString()}`);
  console.log(`  总输出 token:   ${totalOutput.toLocaleString()}`);
  console.log(`  缓存写入:       ${totalCacheCreation.toLocaleString()} (${cacheCreationRate}%)`);
  console.log(`  缓存命中:       ${totalCacheRead.toLocaleString()} (${cacheHitRate}%)`);
  console.log(`  冷miss输入:     ${coldMissInput.toLocaleString()} (${coldMissRate}%)`);
  console.log(`  有效输入:       ${effectiveInput.toLocaleString()}（总输入 - 缓存命中）`);

  // ── 按请求逐条分析 ──
  // 找出缓存命中率为 0 的请求（可能有问题）
  const zeroCache = withUsage.filter((e) => e.usage.cacheRead === 0 && e.usage.cacheCreation === 0);
  const lowCache = withUsage.filter((e) => {
    const rate = e.usage.input > 0 ? e.usage.cacheRead / e.usage.input : 0;
    return rate > 0 && rate < 0.3;
  });
  const goodCache = withUsage.filter((e) => {
    const rate = e.usage.input > 0 ? e.usage.cacheRead / e.usage.input : 0;
    return rate >= 0.3;
  });

  console.log(`\n── 缓存健康度 ──`);
  console.log(`  无缓存活动:     ${zeroCache.length} 条`);
  console.log(`  缓存率 < 30%:   ${lowCache.length} 条`);
  console.log(`  缓存率 >= 30%:  ${goodCache.length} 条`);

  // ── 缓存趋势（按 session 或按时间序列）──
  const bySession = args["by-session"];
  if (bySession || args.session) {
    // 按 session 内的请求顺序展示缓存率变化
    const sessionGroups = {};
    withUsage.forEach((e) => {
      const sid = e.sessionId || "(无 session)";
      if (!sessionGroups[sid]) sessionGroups[sid] = [];
      sessionGroups[sid].push(e);
    });

    console.log(`\n── 按 Session 的缓存率趋势 ──`);
    for (const [sid, items] of Object.entries(sessionGroups)) {
      if (args.session && sid !== args.session) continue;
      console.log(`\n  Session: ${sid} (${items.length} 条)`);
      console.log(`  轮次 | 请求ID                    | 输入   | 缓存读 | 缓存写 | 命中率`);
      console.log("  " + "-".repeat(70));
      items.forEach((e, i) => {
        const rate = e.usage.input > 0 ? (e.usage.cacheRead / e.usage.input * 100).toFixed(1) + "%" : "-";
        const creation = (e.usage.cacheCreation || 0);
        console.log(`  ${String(i + 1).padStart(4)} | ${e.id.slice(0, 26).padEnd(27)} | ${String(e.usage.input).padStart(6)} | ${String(e.usage.cacheRead).padStart(6)} | ${String(creation).padStart(6)} | ${rate}`);
      });
      // Session 汇总
      const sTotalInput = items.reduce((s, e) => s + e.usage.input, 0);
      const sTotalCacheRead = items.reduce((s, e) => s + e.usage.cacheRead, 0);
      const sTotalCacheCreation = items.reduce((s, e) => s + (e.usage.cacheCreation || 0), 0);
      const sRate = sTotalInput > 0 ? (sTotalCacheRead / sTotalInput * 100).toFixed(1) + "%" : "-";
      console.log(`  汇总: 总输入=${sTotalInput.toLocaleString()} 缓存读=${sTotalCacheRead.toLocaleString()} 缓存写=${sTotalCacheCreation.toLocaleString()} 命中率=${sRate}`);
    }
  }

  // ── 诊断建议 ──
  console.log(`\n── 诊断 ──`);
  const issues = [];

  if (zeroCache.length > 0 && withUsage.length > 2) {
    issues.push(`有 ${zeroCache.length}/${withUsage.length} 条请求完全没有缓存活动`);
    if (zeroCache.length === withUsage.length) {
      issues.push("  → 所有请求都未命中缓存，检查是否启用了 prompt caching（Anthropic 需要 anthropic-beta header）");
    } else {
      issues.push("  → 部分请求无缓存，可能原因：首次请求（冷启动）、system prompt 变化、工具列表变化、消息顺序不稳定");
      // 检查是否是每轮第一条请求无缓存（正常的冷启动）
      const firstOfSession = [];
      const seenSessions = new Set();
      for (const e of withUsage) {
        const sid = e.sessionId || e.id;
        if (!seenSessions.has(sid)) {
          seenSessions.add(sid);
          firstOfSession.push(e);
        }
      }
      const firstReqNoCache = firstOfSession.filter((e) => e.usage.cacheRead === 0 && e.usage.cacheCreation === 0);
      if (firstReqNoCache.length > 0) {
        issues.push(`  → 其中 ${firstReqNoCache.length} 条是 session 首次请求（冷启动，属正常）`);
      }
    }
  }

  // 检查缓存率逐轮是否下降（可能意味着 system prompt 不稳定）
  if (args.session) {
    const sessionEntries = withUsage.filter((e) => e.sessionId === args.session);
    if (sessionEntries.length >= 3) {
      const rates = sessionEntries.map((e) => e.usage.input > 0 ? e.usage.cacheRead / e.usage.input : 0);
      let declining = 0;
      for (let i = 1; i < rates.length; i++) {
        if (rates[i] < rates[i - 1]) declining++;
      }
      if (declining > rates.length / 2) {
        issues.push("缓存命中率逐轮下降超过半数");
        issues.push("  → 可能原因：messages 前缀不稳定（prepend 插入）、tools 数组顺序变化、system prompt 动态段过大");
      }
    }
  }

  // 检查缓存写入过多（每轮都在重新写缓存，说明前缀总在变）
  if (totalCacheCreation > totalCacheRead && totalInput > 0) {
    issues.push(`缓存写入(${totalCacheCreation}) > 缓存读取(${totalCacheRead})，投入未回收`);
    issues.push("  → 缓存写入后未在下一次请求中命中，说明缓存前缀在请求间不稳定");
  }

  if (issues.length === 0) {
    console.log("  缓存状态正常，无异常发现");
  } else {
    issues.forEach((line) => console.log(`  ${line}`));
  }
  console.log("═".repeat(72));
}

// ─── cache-debug 命令 — 缓存诊断深度分析 ────────────────────────────

function cmdCacheDebug(args) {
  const dataDir = resolveDataDir(args.dir);
  const entries = loadEntries(dataDir);
  const targetIds = args._;

  if (targetIds.length === 0) {
    console.error("用法: cache-debug <request-id-1> [request-id-2] ... [--dir ./data]");
    process.exit(1);
  }

  const targets = targetIds.map((id) => {
    const entry = entries.find((e) => e.id === id || e.id.startsWith(id));
    if (!entry) {
      console.error(`找不到请求: ${id}`);
      process.exit(1);
    }
    return entry;
  });

  for (let i = 0; i < targets.length; i++) {
    if (i > 0) console.log("\n");
    reportCacheDebug(targets[i], entries);
  }
}

function reportCacheDebug(entry, allEntries) {
  console.log("═".repeat(72));
  console.log(`Cache Debug: ${entry.id}`);
  console.log(`时间: ${entry.date} ${entry.time}  模型: ${entry.model}`);
  console.log(`Session: ${entry.sessionId || "(无)"}`);
  console.log("═".repeat(72));

  // ── 1. 缓存概要表 ──
  sectionHeader("缓存概要");
  if (!entry.usage) {
    console.log("  (无 token 用量数据)");
    return;
  }
  const hitRate = entry.usage.input > 0 ? (entry.usage.cacheRead / entry.usage.input * 100).toFixed(1) : "0.0";
  const creationRate = entry.usage.input > 0 ? ((entry.usage.cacheCreation || 0) / entry.usage.input * 100).toFixed(1) : "0.0";
  console.log(`  input_tokens:                ${entry.usage.input.toLocaleString()}`);
  console.log(`  cache_read_input_tokens:     ${entry.usage.cacheRead.toLocaleString()}  (${hitRate}%)`);
  console.log(`  cache_creation_input_tokens: ${(entry.usage.cacheCreation || 0).toLocaleString()}  (${creationRate}%)`);
  console.log(`  output_tokens:               ${entry.usage.output.toLocaleString()}`);

  // ── 2. 自动查找前一轮请求 ──
  sectionHeader("前一轮请求");
  const prevEntry = findPreviousInSession(entry, allEntries);
  if (!prevEntry) {
    console.log("  未找到同 session 的前一轮请求（可能是首轮冷启动）");
    sectionHeader("Cache Control Breakpoints");
    printCacheControlMapFromData(buildCacheControlMap(entry));
    sectionHeader("诊断结论");
    console.log("  Cold start: first request in session, no prior cache to compare");
    return;
  }
  console.log(`  前一轮 ID: ${prevEntry.id}`);
  console.log(`  前一轮时间: ${prevEntry.date} ${prevEntry.time}`);
  if (prevEntry.usage) {
    const prevRate = prevEntry.usage.input > 0 ? (prevEntry.usage.cacheRead / prevEntry.usage.input * 100).toFixed(1) : "0.0";
    console.log(`  前一轮 input=${prevEntry.usage.input.toLocaleString()} cache_read=${prevEntry.usage.cacheRead.toLocaleString()} (${prevRate}%)`);
  } else {
    console.log("  前一轮无 token 用量数据");
  }

  // ── 3. 判断缓存下降类型 ──
  sectionHeader("缓存变化分析");
  if (!entry.usage || !prevEntry.usage) {
    console.log("  缺少用量数据，无法分析");
    sectionHeader("Cache Control Breakpoints");
    printCacheControlMapFromData(buildCacheControlMap(entry));
    return;
  }
  const inputDelta = entry.usage.input - prevEntry.usage.input;
  const cacheReadDelta = entry.usage.cacheRead - prevEntry.usage.cacheRead;
  console.log(`  input 变化:      ${inputDelta >= 0 ? "+" : ""}${inputDelta.toLocaleString()}`);
  console.log(`  cache_read 变化: ${cacheReadDelta >= 0 ? "+" : ""}${cacheReadDelta.toLocaleString()}`);

  let cacheVerdict;
  if (prevEntry.usage.cacheRead === 0) {
    cacheVerdict = "cold_start";
    console.log("  类型: 首轮冷启动（前一轮 cache_read = 0）");
  } else if (inputDelta < 500 && cacheReadDelta < -5000) {
    cacheVerdict = "invalidation";
    console.log("  类型: 缓存失效 (Cache Invalidation)");
    console.log(`  原因: input 仅增 ${inputDelta}，但 cache_read 大幅下降 ${cacheReadDelta}`);
  } else if (inputDelta > 5000 && Math.abs(cacheReadDelta) < 1000) {
    cacheVerdict = "dilution";
    console.log("  类型: 缓存稀释 (Cache Dilution)");
    console.log(`  原因: input 大增 ${inputDelta}，但 cache_read 基本不变`);
  } else {
    cacheVerdict = "normal";
    console.log("  类型: 正常");
  }

  // ── 4. 解析 cache_control 断点地图 ──
  sectionHeader("Cache Control Breakpoints");
  const currentMap = buildCacheControlMap(entry);
  printCacheControlMapFromData(currentMap);

  // ── 5. 当检测到缓存异常时，定位原因 ──
  if (cacheVerdict === "invalidation") {
    sectionHeader("缓存失效原因分析");
    const prevMap = buildCacheControlMap(prevEntry);
    diagnoseCacheInvalidation(entry, prevEntry, currentMap, prevMap);
  } else if (cacheVerdict === "dilution") {
    sectionHeader("缓存稀释原因分析");
    diagnoseCacheDilution(entry, prevEntry);
  }

  // ── 6. 输出诊断结论 ──
  sectionHeader("诊断结论");
  const conclusion = generateConclusion(
    entry, prevEntry, cacheVerdict, currentMap,
    cacheVerdict === "invalidation" ? buildCacheControlMap(prevEntry) : null,
  );
  console.log(`  ${conclusion}`);
}

function sectionHeader(title) {
  console.log(`\n── ${title} ──`);
}

function findPreviousInSession(entry, allEntries) {
  const sid = entry.sessionId;
  if (!sid) return null;
  const sessionEntries = allEntries.filter((e) => e.sessionId === sid);
  const idx = sessionEntries.findIndex((e) => e.id === entry.id);
  if (idx <= 0) return null;
  return sessionEntries[idx - 1];
}

function buildCacheControlMap(entry) {
  const body = entry.body;
  if (!body) return { system: [], tools: { count: 0, lastHasCC: false, lastName: "" }, messages: [] };

  const system = (body.system || []).map((block, i) => {
    const hasCC = block.cache_control !== undefined;
    const text = block.text || "";
    const len = text.length;
    const hasBoundary = text.includes("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__");
    return { index: i, hasCC, len, hasBoundary };
  });

  const tools = body.tools || [];
  const toolsInfo = {
    count: tools.length,
    lastHasCC: tools.length > 0 && tools[tools.length - 1].cache_control !== undefined,
    lastName: tools.length > 0 ? (tools[tools.length - 1].name || tools[tools.length - 1].function?.name || "?") : "",
  };

  const messages = [];
  (body.messages || []).forEach((msg, i) => {
    const role = msg.role || "?";
    const content = msg.content;
    if (Array.isArray(content)) {
      content.forEach((block, j) => {
        if (block.cache_control !== undefined) {
          messages.push({ msgIndex: i, role, blockIndex: j, blockType: block.type || "unknown" });
        }
      });
    }
  });

  return { system, tools: toolsInfo, messages };
}

function printCacheControlMapFromData(map) {
  let bpNum = 1;
  for (const s of map.system) {
    const num = s.hasCC ? `#${bpNum++}` : "  ";
    const lenStr = s.len > 0 ? `${s.len.toLocaleString()} chars`.padEnd(16) : "".padEnd(16);
    let annotation = "";
    if (s.hasBoundary) annotation = " (contains DYNAMIC_BOUNDARY)";
    else if (s.hasCC) annotation = " (static system prompt)";
    console.log(`  ${num}  system[${s.index}] end          ${lenStr}  ${annotation}`);
  }

  if (map.tools.count > 0) {
    if (map.tools.lastHasCC) {
      console.log(`  #${bpNum++}  tools (${map.tools.count} items)                    last=${map.tools.lastName}`);
    } else {
      console.log(`  ⚠   tools (${map.tools.count} items)                    NO cache_control on last tool (last=${map.tools.lastName})`);
    }
  }

  for (const m of map.messages) {
    const num = `#${bpNum++}`;
    let label = m.msgIndex === 0 ? "first user message" : `${m.role} message`;
    console.log(`  ${num}  messages[${m.msgIndex}] ${m.role.padEnd(8)}  ${m.blockType}  (${label})`);
  }

  if (bpNum === 1) {
    console.log("  (no cache_control breakpoints found)");
  }
}

function diagnoseCacheInvalidation(current, prev, curMap, prevMap) {
  // system blocks 变化
  const curSystemTexts = (current.body?.system || []).map((s) => s.text || "");
  const prevSystemTexts = (prev.body?.system || []).map((s) => s.text || "");

  if (curSystemTexts.length !== prevSystemTexts.length) {
    console.log(`  system blocks 数量变化: ${prevSystemTexts.length} -> ${curSystemTexts.length}`);
  } else {
    for (let i = 0; i < curSystemTexts.length; i++) {
      if (curSystemTexts[i] !== prevSystemTexts[i]) {
        const diff = findFirstDiff(curSystemTexts[i], prevSystemTexts[i]);
        console.log(`  system[${i}] 内容变化 (first diff at char ${diff})`);
      }
    }
  }

  // tools 变化
  const curToolNames = (current.body?.tools || []).map((t) => t.name || t.function?.name || "?");
  const prevToolNames = (prev.body?.tools || []).map((t) => t.name || t.function?.name || "?");
  if (JSON.stringify(curToolNames) !== JSON.stringify(prevToolNames)) {
    if (curToolNames.length !== prevToolNames.length) {
      console.log(`  tools 数量变化: ${prevToolNames.length} -> ${curToolNames.length}`);
    }
    const addedTools = curToolNames.filter((t) => !prevToolNames.includes(t));
    const removedTools = prevToolNames.filter((t) => !curToolNames.includes(t));
    if (addedTools.length > 0) console.log(`  新增工具: ${addedTools.join(", ")}`);
    if (removedTools.length > 0) console.log(`  移除工具: ${removedTools.join(", ")}`);
    if (addedTools.length === 0 && removedTools.length === 0) {
      console.log("  工具顺序发生变化");
    }
  }

  // 消息前缀变化（prepend 检测）
  const curMsgs = current.body?.messages || [];
  const prevMsgs = prev.body?.messages || [];
  const prefixMatchLen = findCommonMessagePrefixLength(curMsgs, prevMsgs);
  if (prefixMatchLen < prevMsgs.length) {
    console.log(`  检测到消息前缀变化: 前 ${prefixMatchLen} 条消息匹配，之后 diverge`);
    if (prefixMatchLen < curMsgs.length) {
      console.log(`  前 ${prefixMatchLen} 条之后的消息内容发生了变化（可能是 compact 或 prepend）`);
    }
  }

  // cache_control 标记迁移
  const prevCCMsgIndices = new Set(prevMap.messages.map((m) => m.msgIndex));
  const curCCMsgIndices = new Set(curMap.messages.map((m) => m.msgIndex));
  const removedCC = [...prevCCMsgIndices].filter((i) => !curCCMsgIndices.has(i));
  const addedCC = [...curCCMsgIndices].filter((i) => !prevCCMsgIndices.has(i));
  if (removedCC.length > 0) {
    console.log(`  cache_control 从 messages[${removedCC.join(", ")}] 移除`);
  }
  if (addedCC.length > 0) {
    console.log(`  cache_control 新增到 messages[${addedCC.join(", ")}]`);
  }

  // system cache_control 变化
  const prevCCSystem = new Set(prevMap.system.filter((s) => s.hasCC).map((s) => s.index));
  const curCCSystem = new Set(curMap.system.filter((s) => s.hasCC).map((s) => s.index));
  const removedSysCC = [...prevCCSystem].filter((i) => !curCCSystem.has(i));
  const addedSysCC = [...curCCSystem].filter((i) => !prevCCSystem.has(i));
  if (removedSysCC.length > 0) {
    console.log(`  cache_control 从 system[${removedSysCC.join(", ")}] 移除`);
  }
  if (addedSysCC.length > 0) {
    console.log(`  cache_control 新增到 system[${addedSysCC.join(", ")}]`);
  }
}

function diagnoseCacheDilution(current, prev) {
  const curMsgs = current.body?.messages || [];
  const prevMsgs = prev.body?.messages || [];

  if (curMsgs.length > prevMsgs.length) {
    const newMsgCount = curMsgs.length - prevMsgs.length;
    const prefixMatchLen = findCommonMessagePrefixLength(curMsgs, prevMsgs);
    if (prefixMatchLen === prevMsgs.length) {
      console.log(`  新增 ${newMsgCount} 条消息（尾部追加，正常）`);
      const newMsgs = curMsgs.slice(prevMsgs.length);
      const totalNewChars = newMsgs.reduce((s, m) => s + contentLength(m.content), 0);
      console.log(`  新增内容约 ${totalNewChars.toLocaleString()} chars`);
    } else {
      console.log(`  消息数量变化: ${prevMsgs.length} -> ${curMsgs.length}，前缀不完全匹配`);
    }
  }

  const curSystemTexts = (current.body?.system || []).map((s) => s.text || "");
  const prevSystemTexts = (prev.body?.system || []).map((s) => s.text || "");
  for (let i = 0; i < Math.max(curSystemTexts.length, prevSystemTexts.length); i++) {
    if (curSystemTexts[i] !== prevSystemTexts[i]) {
      const curLen = curSystemTexts[i]?.length || 0;
      const prevLen = prevSystemTexts[i]?.length || 0;
      console.log(`  system[${i}] 内容变化: ${prevLen.toLocaleString()} -> ${curLen.toLocaleString()} chars (delta: ${curLen - prevLen})`);
    }
  }
}

function findCommonMessagePrefixLength(msgs1, msgs2) {
  let len = 0;
  const max = Math.min(msgs1.length, msgs2.length);
  for (let i = 0; i < max; i++) {
    if (messageContentEqual(msgs1[i], msgs2[i])) {
      len++;
    } else {
      break;
    }
  }
  return len;
}

function messageContentEqual(m1, m2) {
  if (m1.role !== m2.role) return false;
  const c1 = typeof m1.content === "string" ? m1.content : JSON.stringify(m1.content);
  const c2 = typeof m2.content === "string" ? m2.content : JSON.stringify(m2.content);
  return c1 === c2;
}

function findFirstDiff(s1, s2) {
  const max = Math.min(s1.length, s2.length);
  for (let i = 0; i < max; i++) {
    if (s1[i] !== s2[i]) return i;
  }
  return max;
}

function contentLength(content) {
  if (typeof content === "string") return content.length;
  if (Array.isArray(content)) {
    return content.reduce((s, b) => s + (typeof b.text === "string" ? b.text.length : JSON.stringify(b).length), 0);
  }
  return 0;
}

function generateConclusion(current, prev, cacheVerdict, curMap, prevMap) {
  const hitRate = current.usage.input > 0 ? (current.usage.cacheRead / current.usage.input * 100).toFixed(1) : "0.0";

  if (cacheVerdict === "cold_start") {
    const creation = current.usage.cacheCreation || 0;
    if (creation > 0) {
      return `Cold start: ${creation.toLocaleString()} tokens written to cache, subsequent requests should benefit`;
    }
    return "Cold start: no cache activity detected, check if prompt caching is enabled";
  }

  if (cacheVerdict === "invalidation" && prevMap) {
    const prevCCMsgIndices = prevMap.messages.map((m) => m.msgIndex);
    const curCCMsgIndices = curMap.messages.map((m) => m.msgIndex);
    const removedCC = prevCCMsgIndices.filter((i) => !curCCMsgIndices.includes(i));
    const addedCC = curCCMsgIndices.filter((i) => !prevCCMsgIndices.includes(i));

    if (removedCC.length > 0 && addedCC.length > 0) {
      return `Cache invalidation: ephemeral breakpoint migrated from messages[${removedCC}] to messages[${addedCC}], removing cache_control from earlier messages invalidated message-area cache entries`;
    }

    const prevCCSystem = prevMap.system.filter((s) => s.hasCC).map((s) => s.index);
    const curCCSystem = curMap.system.filter((s) => s.hasCC).map((s) => s.index);
    const removedSysCC = prevCCSystem.filter((i) => !curCCSystem.includes(i));
    if (removedSysCC.length > 0) {
      return `Cache invalidation: cache_control removed from system[${removedSysCC}], likely due to system prompt content change`;
    }

    const prevToolCount = prevMap.tools.count;
    const curToolCount = curMap.tools.count;
    if (prevToolCount !== curToolCount) {
      return `Cache invalidation: tools array changed (${prevToolCount} -> ${curToolCount} items), invalidated the entire tools cache segment`;
    }

    const curMsgs = current.body?.messages || [];
    const prevMsgs = prev.body?.messages || [];
    const prefixLen = findCommonMessagePrefixLength(curMsgs, prevMsgs);
    if (prefixLen < prevMsgs.length && prefixLen < curMsgs.length) {
      return `Cache invalidation: message prefix diverged at index ${prefixLen}, content before the cache_control breakpoint changed`;
    }

    return `Cache invalidation: cache_read dropped from ${prev.usage.cacheRead.toLocaleString()} to ${current.usage.cacheRead.toLocaleString()}, cause unclear from request structure alone (possible provider-side cache eviction)`;
  }

  if (cacheVerdict === "dilution") {
    const inputDelta = current.usage.input - prev.usage.input;
    return `Cache dilution: +${inputDelta.toLocaleString()} tokens of new content, existing cache prefix preserved`;
  }

  if (parseFloat(hitRate) >= 80) {
    return `Cache healthy: ${hitRate}% hit rate, no issues detected`;
  }
  return `Cache normal: ${hitRate}% hit rate, moderate cache utilization`;
}

// ─── cache-control 命令 — 断点地图与问题检测 ────────────────────────

function cmdCacheControl(args) {
  const dataDir = resolveDataDir(args.dir);
  const entries = loadEntries(dataDir);
  const targetIds = args._;

  if (targetIds.length === 0) {
    console.error("用法: cache-control <request-id> [--dir ./data]");
    process.exit(1);
  }

  const entry = entries.find((e) => e.id === targetIds[0] || e.id.startsWith(targetIds[0]));
  if (!entry) {
    console.error(`找不到请求: ${targetIds[0]}`);
    process.exit(1);
  }

  console.log("═".repeat(72));
  console.log(`Cache Control Breakpoint Map: ${entry.id}`);
  console.log(`时间: ${entry.date} ${entry.time}  模型: ${entry.model}`);
  console.log(`Session: ${entry.sessionId || "(无)"}`);
  console.log("═".repeat(72));

  const map = buildCacheControlMap(entry);
  const body = entry.body;
  const msgs = body?.messages || [];

  // ── System Blocks ──
  sectionHeader("System Blocks");
  for (const s of map.system) {
    const cc = s.hasCC ? "✅ ephemeral" : "❌ no cache_control";
    let preview = "";
    const text = (body?.system || [])[s.index]?.text || "";
    if (text.length > 0) preview = `"${text.slice(0, 50)}${text.length > 50 ? "..." : ""}"`;
    console.log(`  [${s.index}] ${s.len.toLocaleString().padStart(6)} chars  ${cc}  ${preview}`);
    if (s.hasBoundary) console.log(`    ← __SYSTEM_PROMPT_DYNAMIC_BOUNDARY__ found here`);
  }

  // ── Tools ──
  sectionHeader("Tools");
  console.log(`  ${map.tools.count} tools defined`);
  if (map.tools.count > 0) {
    if (map.tools.lastHasCC) {
      console.log(`  ✅ Last tool (${map.tools.lastName}) has cache_control`);
    } else {
      console.log(`  ⚠️  NO cache_control on last tool (${map.tools.lastName})`);
      console.log("      → Tools only cached implicitly via the next breakpoint that includes them");
      console.log("      → If tools change, that breakpoint and all subsequent ones are invalidated");
    }
  }

  // ── Messages ──
  sectionHeader(`Messages (${msgs.length} total)`);
  const ccMsgSet = new Set(map.messages.map((m) => m.msgIndex));
  const totalUserMsgs = msgs.filter((m) => m.role === "user").length;
  const ccUserMsgs = map.messages.filter((m) => m.role === "user").length;
  console.log(`  User messages with cache_control: ${ccUserMsgs} / ${totalUserMsgs}`);

  for (const m of map.messages) {
    const label = m.msgIndex === 0 ? "first user message" : `user message`;
    console.log(`  [${m.msgIndex}]  ${m.role.padEnd(10)} ✅ ephemeral   (${label})`);
  }
  if (map.messages.length === 0) {
    console.log("  (no cache_control on any message)");
  }

  // ── Breakpoint Coverage Estimate ──
  sectionHeader("Breakpoint Coverage Estimate");
  let bpNum = 1;
  let cumulativeChars = 0;
  const charToToken = (chars) => Math.ceil(chars / 3.5);

  for (const s of map.system) {
    if (s.hasCC) {
      cumulativeChars += s.len;
      console.log(`  BP#${bpNum}  system[${s.index}] end        ~${charToToken(cumulativeChars).toLocaleString().padStart(6)} tokens  (cumulative ${cumulativeChars.toLocaleString()} chars)`);
      bpNum++;
    } else {
      cumulativeChars += s.len;
    }
  }

  // Tools chars (rough estimate)
  const toolsJson = JSON.stringify(body?.tools || []);
  const toolsChars = toolsJson.length;
  if (map.tools.lastHasCC) {
    cumulativeChars += toolsChars;
    console.log(`  BP#${bpNum}  tools end                ~${charToToken(cumulativeChars).toLocaleString().padStart(6)} tokens`);
    bpNum++;
  } else {
    cumulativeChars += toolsChars;
  }

  // Messages up to each cc message
  for (const m of map.messages) {
    let msgChars = 0;
    for (let i = 0; i <= m.msgIndex; i++) {
      msgChars += contentLength(msgs[i]?.content || "");
    }
    const total = cumulativeChars + msgChars;
    const label = m.msgIndex === 0 ? "first user message" : `messages[${m.msgIndex}]`;
    console.log(`  BP#${bpNum}  ${label.padEnd(24)} ~${charToToken(total).toLocaleString().padStart(6)} tokens`);
    bpNum++;
  }

  // ── Issues ──
  const issues = [];

  if (map.tools.count > 0 && !map.tools.lastHasCC) {
    issues.push("Tools array has no cache_control breakpoint");
    issues.push("  → Tools only cached implicitly via the next message breakpoint");
    issues.push("  → If tools change (order/content), message-area cache is invalidated");
    issues.push("  → Recommendation: add cache_control to last tool for independent caching");
  }

  for (const s of map.system) {
    if (s.hasBoundary && s.hasCC) {
      issues.push(`System block [${s.index}] contains DYNAMIC_BOUNDARY and has ephemeral breakpoint`);
      issues.push("  → Content BEFORE boundary is cached, content AFTER is not");
      issues.push("  → This is correct design if the boundary properly separates static/dynamic");
    }
    if (!s.hasBoundary && !s.hasCC) {
      issues.push(`System block [${s.index}] has no cache_control and no boundary`);
      issues.push("  → This block is never independently cached");
    }
  }

  if (ccUserMsgs < 2 && totalUserMsgs >= 3) {
    issues.push(`Only ${ccUserMsgs} user message has cache_control out of ${totalUserMsgs}`);
    issues.push("  → Most user messages are tool_result-only (no text block)");
    issues.push("  → The fallback search in apply_cache_to_messages may be too restrictive");
  }

  sectionHeader("Issues");
  if (issues.length === 0) {
    console.log("  ✅ No issues detected");
  } else {
    issues.forEach((line) => console.log(`  ${line}`));
  }

  console.log("═".repeat(72));
}

// ─── 参数解析 ─────────────────────────────────────────────────────

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  let i = 0;
  while (i < argv.length) {
    const arg = argv[i];
    if (arg.startsWith("--")) {
      const key = arg.slice(2);
      const next = argv[i + 1];
      if (next && !next.startsWith("--")) {
        flags[key] = next;
        i += 2;
      } else {
        flags[key] = true;
        i += 1;
      }
    } else {
      positional.push(arg);
      i += 1;
    }
  }
  flags._ = positional;
  return flags;
}

// ─── 主入口 ───────────────────────────────────────────────────────

const args = parseArgs(process.argv.slice(2));
const command = args._.shift();

switch (command) {
  case "list":
    cmdList(args);
    break;
  case "show":
    cmdShow(args);
    break;
  case "session":
    cmdSession(args);
    break;
  case "diff":
    cmdDiff(args);
    break;
  case "stats":
    cmdStats(args);
    break;
  case "cache":
    cmdCache(args);
    break;
  case "cache-debug":
    cmdCacheDebug(args);
    break;
  case "cache-control":
    cmdCacheControl(args);
    break;
  default:
    console.log("LLM Log Query Tool");
    console.log("");
    console.log("用法: bun run llm-log-query.mjs <command> [options]");
    console.log("");
    console.log("命令:");
    console.log("  list    列出请求摘要 [--model] [--session] [--route] [--after] [--before] [--errors] [--limit] [--dir]");
    console.log("  show    查看请求详情 <id> [--headers] [--body] [--messages] [--tools] [--stream] [--dir]");
    console.log("  session 追踪 session <session-id> [--full] [--dir]");
    console.log("          session <session-id> diff <round1> <round2>");
    console.log("  diff    对比两个请求 <id1> <id2> [--dir]");
    console.log("  stats   统计汇总 [--by model|session|route|hour] [--dir]");
    console.log("  cache   缓存率分析 [--session] [--by-session] [--after] [--before] [--dir]");
    console.log("  cache-debug  缓存诊断 <request-id-1> [request-id-2] ... [--dir]");
    console.log("  cache-control  断点地图 <request-id> [--dir]");
}
