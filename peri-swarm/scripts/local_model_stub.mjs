#!/usr/bin/env node
// Local OpenAI-compatible model stub for the S9a1/S9a2 end-to-end runs.
//
// Peri always requests `POST /v1/chat/completions` with `"stream": true`; this
// stub answers with deterministic SSE text so the Swarm gateway's item log can
// be checked end to end without a real model. It recognises Peri's background
// prediction/title helper (the request carries `<prediction_directive>`) and
// keeps those calls out of the turn counter.
//
// S9a2 adds one deterministic tool turn: when the request advertises the
// workspace `Bash` tool and no tool result is in the messages yet, the stub
// emits an OpenAI `delta.tool_calls` stream that runs
// `echo hello-from-swarm > hello.txt` (overridable with PERI_STUB_TOOL_COMMAND).
// The call id is `call_stub_<n>`, the argument object is `{"command": ...}`, and
// `finish_reason` is `tool_calls`. The next main call (which carries the tool
// message) returns the usual text reply.
//
// S9a3b1 makes the tool command a function of the *input*: the last user
// message may carry `[[tool:<name>]]`, resolved against `PERI_STUB_COMMANDS`
// (a JSON object of name -> shell command). That turn-scoped decision is
// stateless and concurrency-safe: a tool call is emitted only while the
// conversation after the directive-bearing user message has no `tool` result
// yet, so several sessions can interleave turns on one stub. The legacy
// `PERI_STUB_TOOL_COMMAND` single-tool-turn behaviour is unchanged for
// requests without a directive.
//
// The reply is a pure function of the turn's user text (the directive-bearing
// message when present, else the last user message) and the main-call index, so
// a repeated run produces the same text for the same input:
//
//     stub-echo:<sha1(normalized text)[0..12]>#<main call index>
//
// Usage:
//     node peri-swarm/scripts/local_model_stub.mjs
//     PERI_STUB_PORT=9496 node peri-swarm/scripts/local_model_stub.mjs
//     PERI_STUB_COMMANDS='{"write-a":"printf a-content > a.txt"}' \
//       node peri-swarm/scripts/local_model_stub.mjs
//
// Introspection used by the SDK driver:
//     GET  /healthz   -> liveness + call counters
//     GET  /__stats   -> counters and every main-call turn (reply + last user text)
//     POST /__reset   -> zero the counters (repeatable runs)
//
// No dependency, no credential, loopback only.

import crypto from 'node:crypto';
import http from 'node:http';

const HOST = process.env.PERI_STUB_HOST ?? '127.0.0.1';
const PORT = Number(process.env.PERI_STUB_PORT ?? '9496');
const CHUNK_SIZE = Number(process.env.PERI_STUB_CHUNK ?? '6');
const CHUNK_DELAY_MS = Number(process.env.PERI_STUB_CHUNK_DELAY_MS ?? '15');
const MAX_TURNS = 64;
const TOOL_COMMAND = process.env.PERI_STUB_TOOL_COMMAND ?? 'echo hello-from-swarm > hello.txt';
/** The single-tool-turn fallback is opt-in: it needs the explicit command. */
const TOOL_FALLBACK_ENABLED = process.env.PERI_STUB_TOOL_COMMAND !== undefined;
const DIRECTIVE_PATTERN = /\[\[tool:([A-Za-z0-9._-]+)\]\]/;

/** `PERI_STUB_COMMANDS` is a JSON object of directive name -> shell command. */
function parseCommands(raw) {
  if (raw === undefined || raw.trim().length === 0) return {};
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    console.error(`[model-stub] PERI_STUB_COMMANDS is not JSON: ${error.message}`);
    process.exit(2);
  }
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    console.error('[model-stub] PERI_STUB_COMMANDS must be a JSON object');
    process.exit(2);
  }
  const commands = {};
  for (const [name, command] of Object.entries(parsed)) {
    if (!/^[A-Za-z0-9._-]+$/.test(name) || typeof command !== 'string' || command.trim().length === 0) {
      console.error(`[model-stub] invalid PERI_STUB_COMMANDS entry: ${JSON.stringify(name)}`);
      process.exit(2);
    }
    commands[name] = command;
  }
  return commands;
}

const COMMANDS = parseCommands(process.env.PERI_STUB_COMMANDS);

const state = {
  mainCalls: 0,
  helperCalls: 0,
  otherCalls: 0,
  toolCalls: 0,
  turns: [],
  otherRequests: [],
  startedAt: new Date().toISOString(),
};

function reset() {
  state.mainCalls = 0;
  state.helperCalls = 0;
  state.otherCalls = 0;
  state.toolCalls = 0;
  state.turns = [];
  state.otherRequests = [];
  state.startedAt = new Date().toISOString();
}

function messageText(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (typeof part === 'string') return part;
        if (part && typeof part === 'object') {
          if (typeof part.text === 'string') return part.text;
          if (typeof part.input_text === 'string') return part.input_text;
        }
        return '';
      })
      .join('');
  }
  return '';
}

function messagesOf(body) {
  return Array.isArray(body?.messages) ? body.messages : [];
}

function isBackgroundHelper(body) {
  return messagesOf(body).some((message) =>
    messageText(message?.content).includes('<prediction_directive>'),
  );
}

/** Model-facing names of the tools advertised in this request. */
function toolNames(body) {
  const tools = Array.isArray(body?.tools) ? body.tools : [];
  return tools
    .map((tool) => {
      if (typeof tool?.function?.name === 'string') return tool.function.name;
      if (typeof tool?.name === 'string') return tool.name;
      return '';
    })
    .filter((name) => name.length > 0);
}

/**
 * The workspace tool is exposed directly (`Bash`) when the session workspace is
 * a remote MCP; keep the `mcp__<server>__Bash` fallback for other assemblies.
 */
function bashToolName(body) {
  const names = toolNames(body);
  return (
    names.find((name) => name === 'Bash') ??
    names.find((name) => name.endsWith('__Bash')) ??
    names.find((name) => name.toLowerCase().endsWith('bash')) ??
    null
  );
}

function hasToolResult(body) {
  return messagesOf(body).some((message) => message?.role === 'tool' || message?.role === 'function');
}

/**
 * Whether the *current* turn already ran its tool call. The turn is bounded by
 * the user message that carries the directive, not by the last user message:
 * Peri appends a `<system-reminder>` capability message as the final user
 * message of every request, so "since the last user message" would never see
 * the tool result and would call the tool again forever.
 */
function toolResultSinceDirective(body) {
  const messages = messagesOf(body);
  let directiveIndex = -1;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message?.role !== 'user') continue;
    if (DIRECTIVE_PATTERN.test(messageText(message?.content))) {
      directiveIndex = index;
      break;
    }
  }
  if (directiveIndex < 0) return hasToolResult(body);
  return messages
    .slice(directiveIndex + 1)
    .some((message) => message?.role === 'tool' || message?.role === 'function');
}

/** Text of the last user message carrying `[[tool:<name>]]`. */
function directiveTextOf(body) {
  const users = messagesOf(body).filter((message) => message?.role === 'user');
  for (let index = users.length - 1; index >= 0; index -= 1) {
    const text = messageText(users[index]?.content);
    if (DIRECTIVE_PATTERN.test(text)) return text;
  }
  return null;
}

/** `[[tool:<name>]]` in a user message selects the tool command. */
function directiveOf(text) {
  const match = DIRECTIVE_PATTERN.exec(text);
  return match === null ? null : match[1];
}

function lastUserText(body) {
  const users = messagesOf(body).filter((message) => message?.role === 'user');
  return users.length > 0 ? messageText(users[users.length - 1]?.content) : '';
}

function replyFor(text, index) {
  const normalized = text.trim().replace(/\s+/g, ' ');
  const digest = crypto.createHash('sha1').update(normalized).digest('hex').slice(0, 12);
  return `stub-echo:${digest}#${index}`;
}

function chunksOf(text) {
  const chunks = [];
  for (let offset = 0; offset < text.length; offset += CHUNK_SIZE) {
    chunks.push(text.slice(offset, offset + CHUNK_SIZE));
  }
  return chunks;
}

function completionChunk({ id, model, delta, finishReason }) {
  return {
    id,
    object: 'chat.completion.chunk',
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [
      {
        index: 0,
        delta,
        ...(finishReason ? { finish_reason: finishReason } : {}),
      },
    ],
  };
}

function sendJson(response, status, body) {
  const encoded = JSON.stringify(body);
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(encoded),
  });
  response.end(encoded);
}

function sendCompletion(response, { id, model, text }) {
  sendJson(response, 200, {
    id,
    object: 'chat.completion',
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [
      { index: 0, message: { role: 'assistant', content: text }, finish_reason: 'stop' },
    ],
    usage: { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
  });
}

function sendToolCallCompletion(response, { id, model, callId, toolName, argumentsText }) {
  sendJson(response, 200, {
    id,
    object: 'chat.completion',
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [
      {
        index: 0,
        message: {
          role: 'assistant',
          content: null,
          tool_calls: [
            {
              id: callId,
              type: 'function',
              function: { name: toolName, arguments: argumentsText },
            },
          ],
        },
        finish_reason: 'tool_calls',
      },
    ],
    usage: { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
  });
}

/**
 * OpenAI chat-completions streaming shape for one tool call: the first chunk
 * carries `id`/`type`/`name`, the second carries the full argument string, and
 * the terminal chunk sets `finish_reason: "tool_calls"`.
 */
async function sendToolCallSse(response, { id, model, callId, toolName, argumentsText }) {
  response.writeHead(200, {
    'content-type': 'text/event-stream',
    'cache-control': 'no-cache',
    connection: 'keep-alive',
    'x-accel-buffering': 'no',
  });
  response.flushHeaders?.();

  response.write(
    `data: ${JSON.stringify(
      completionChunk({
        id,
        model,
        delta: {
          role: 'assistant',
          tool_calls: [
            {
              index: 0,
              id: callId,
              type: 'function',
              function: { name: toolName, arguments: '' },
            },
          ],
        },
      }),
    )}\n\n`,
  );
  if (CHUNK_DELAY_MS > 0) await new Promise((resolve) => setTimeout(resolve, CHUNK_DELAY_MS));
  response.write(
    `data: ${JSON.stringify(
      completionChunk({
        id,
        model,
        delta: {
          tool_calls: [{ index: 0, function: { arguments: argumentsText } }],
        },
      }),
    )}\n\n`,
  );
  response.write(
    `data: ${JSON.stringify(
      completionChunk({ id, model, delta: {}, finishReason: 'tool_calls' }),
    )}\n\n`,
  );
  response.end('data: [DONE]\n\n');
}

async function sendSse(response, { id, model, text }) {
  response.writeHead(200, {
    'content-type': 'text/event-stream',
    'cache-control': 'no-cache',
    connection: 'keep-alive',
    'x-accel-buffering': 'no',
  });
  response.flushHeaders?.();

  const chunks = chunksOf(text);
  for (let index = 0; index < chunks.length; index += 1) {
    const delta = index === 0
      ? { role: 'assistant', content: chunks[index] }
      : { content: chunks[index] };
    response.write(`data: ${JSON.stringify(completionChunk({ id, model, delta }))}\n\n`);
    if (CHUNK_DELAY_MS > 0 && index + 1 < chunks.length) {
      await new Promise((resolve) => setTimeout(resolve, CHUNK_DELAY_MS));
    }
  }
  response.write(
    `data: ${JSON.stringify(
      completionChunk({ id, model, delta: {}, finishReason: 'stop' }),
    )}\n\n`,
  );
  response.end('data: [DONE]\n\n');
}

async function handleChat(_request, response, body) {
  const model = typeof body.model === 'string' ? body.model : 'unknown-model';
  const stream = body.stream !== false;

  if (isBackgroundHelper(body)) {
    state.helperCalls += 1;
    const id = `chatcmpl-stub-helper-${state.helperCalls}`;
    console.error(
      `[model-stub] helper call #${state.helperCalls} model=${model} messages=${messagesOf(body).length}`,
    );
    if (stream) return sendSse(response, { id, model, text: 'stub-helper' });
    return sendCompletion(response, { id, model, text: 'stub-helper' });
  }

  if (messagesOf(body).length === 0) {
    state.otherCalls += 1;
    state.otherRequests.push({ model, reason: 'no messages', at: new Date().toISOString() });
    const id = `chatcmpl-stub-other-${state.otherCalls}`;
    if (stream) return sendSse(response, { id, model, text: 'stub-empty' });
    return sendCompletion(response, { id, model, text: 'stub-empty' });
  }

  state.mainCalls += 1;
  const index = state.mainCalls;
  const directiveText = directiveTextOf(body);
  // The reply digest follows the turn's own directive text, not the trailing
  // `<system-reminder>` user message (which is identical for every turn).
  const userText = directiveText ?? lastUserText(body);
  const bashName = bashToolName(body);
  const directive = directiveText === null ? null : directiveOf(directiveText);
  const directiveCommand = directive === null ? undefined : COMMANDS[directive];
  if (directive !== null && directiveCommand === undefined) {
    state.otherCalls += 1;
    state.otherRequests.push({
      model,
      reason: `unknown tool directive ${directive}`,
      at: new Date().toISOString(),
    });
    console.error(`[model-stub] main call #${index} unknown tool directive ${directive}`);
    return sendJson(response, 400, { error: { message: `unknown tool directive: ${directive}` } });
  }
  if (directiveCommand !== undefined && bashName === null) {
    state.otherCalls += 1;
    state.otherRequests.push({
      model,
      reason: `tool directive ${directive} without a Bash tool`,
      at: new Date().toISOString(),
    });
  }
  // S9a3b1: a directive is a per-turn tool request and stays enabled for every
  // turn of every session. The legacy path keeps the S9a2 "one tool turn per
  // run" contract and only runs when `PERI_STUB_TOOL_COMMAND` was set.
  const directiveTurn =
    directiveCommand !== undefined && bashName !== null && !toolResultSinceDirective(body);
  const legacyTurn =
    TOOL_FALLBACK_ENABLED && bashName !== null && state.toolCalls === 0 && !hasToolResult(body);
  if (directiveTurn || (directiveCommand === undefined && legacyTurn)) {
    state.toolCalls += 1;
    const callId = `call_stub_${index}`;
    const command = directiveTurn ? directiveCommand : TOOL_COMMAND;
    const argumentsText = JSON.stringify({ command });
    state.turns.push({
      n: index,
      kind: 'tool_call',
      toolName: bashName,
      callId,
      arguments: argumentsText,
      directive,
      toolNames: toolNames(body),
      lastUserText: userText.slice(0, 400),
      messageCount: messagesOf(body).length,
      stream,
      at: new Date().toISOString(),
    });
    if (state.turns.length > MAX_TURNS) state.turns.shift();
    console.error(
      `[model-stub] main call #${index} model=${model} tool=${bashName} arguments=${argumentsText}`,
    );
    const id = `chatcmpl-stub-tool-${index}`;
    if (stream) return sendToolCallSse(response, { id, model, callId, toolName: bashName, argumentsText });
    return sendToolCallCompletion(response, { id, model, callId, toolName: bashName, argumentsText });
  }

  const reply = replyFor(userText, index);
  state.turns.push({
    n: index,
    kind: 'final',
    reply,
    directive,
    lastUserText: userText.slice(0, 400),
    toolNames: toolNames(body),
    toolResultSeen: hasToolResult(body),
    messageCount: messagesOf(body).length,
    stream,
    at: new Date().toISOString(),
  });
  if (state.turns.length > MAX_TURNS) state.turns.shift();
  console.error(
    `[model-stub] main call #${index} model=${model} messages=${messagesOf(body).length} reply=${reply}`,
  );
  const id = `chatcmpl-stub-main-${index}`;
  if (stream) return sendSse(response, { id, model, text: reply });
  return sendCompletion(response, { id, model, text: reply });
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    const parts = [];
    request.on('data', (chunk) => {
      parts.push(chunk);
      if (parts.reduce((total, part) => total + part.length, 0) > 8 * 1024 * 1024) {
        reject(new Error('request body too large'));
        request.destroy();
      }
    });
    request.on('end', () => {
      const raw = Buffer.concat(parts).toString('utf8');
      if (raw.length === 0) return resolve({});
      try {
        resolve(JSON.parse(raw));
      } catch (error) {
        reject(error);
      }
    });
    request.on('error', reject);
  });
}

const server = http.createServer(async (request, response) => {
  const url = new URL(request.url ?? '/', `http://${HOST}:${PORT}`);

  if (request.method === 'GET' && url.pathname === '/healthz') {
    return sendJson(response, 200, {
      status: 'ok',
      mainCalls: state.mainCalls,
      helperCalls: state.helperCalls,
      otherCalls: state.otherCalls,
      toolCalls: state.toolCalls,
    });
  }
  if (request.method === 'GET' && url.pathname === '/__stats') {
    return sendJson(response, 200, {
      startedAt: state.startedAt,
      mainCalls: state.mainCalls,
      helperCalls: state.helperCalls,
      otherCalls: state.otherCalls,
      toolCalls: state.toolCalls,
      directives: Object.keys(COMMANDS),
      turns: state.turns,
      otherRequests: state.otherRequests,
    });
  }
  if (request.method === 'POST' && url.pathname === '/__reset') {
    reset();
    return sendJson(response, 200, { reset: true });
  }
  if (request.method !== 'POST' || url.pathname !== '/v1/chat/completions') {
    return sendJson(response, 404, { error: { message: 'not found', path: url.pathname } });
  }

  try {
    const body = await readBody(request);
    await handleChat(request, response, body);
  } catch (error) {
    console.error(`[model-stub] rejected request: ${error.message}`);
    if (!response.headersSent) {
      sendJson(response, 400, { error: { message: error.message } });
    } else {
      response.end();
    }
  }
});

server.listen(PORT, HOST, () => {
  console.error(`[model-stub] listening on http://${HOST}:${PORT}/v1/chat/completions`);
});
