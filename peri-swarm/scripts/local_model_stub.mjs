#!/usr/bin/env node
// Local OpenAI-compatible model stub for the S9a1 end-to-end run.
//
// Peri always requests `POST /v1/chat/completions` with `"stream": true`; this
// stub answers with deterministic SSE text so the Swarm gateway's item log can
// be checked end to end without a real model. It recognises Peri's background
// prediction/title helper (the request carries `<prediction_directive>`) and
// keeps those calls out of the turn counter.
//
// The reply is a pure function of the last user message and the main-call
// index, so a repeated run produces the same text for the same input:
//
//     stub-echo:<sha1(normalized text)[0..12]>#<main call index>
//
// Usage:
//     node peri-swarm/scripts/local_model_stub.mjs
//     PERI_STUB_PORT=9496 node peri-swarm/scripts/local_model_stub.mjs
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

const state = {
  mainCalls: 0,
  helperCalls: 0,
  otherCalls: 0,
  turns: [],
  otherRequests: [],
  startedAt: new Date().toISOString(),
};

function reset() {
  state.mainCalls = 0;
  state.helperCalls = 0;
  state.otherCalls = 0;
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

  const userText = lastUserText(body);
  if (messagesOf(body).length === 0) {
    state.otherCalls += 1;
    state.otherRequests.push({ model, reason: 'no messages', at: new Date().toISOString() });
    const id = `chatcmpl-stub-other-${state.otherCalls}`;
    if (stream) return sendSse(response, { id, model, text: 'stub-empty' });
    return sendCompletion(response, { id, model, text: 'stub-empty' });
  }

  state.mainCalls += 1;
  const index = state.mainCalls;
  const reply = replyFor(userText, index);
  state.turns.push({
    n: index,
    reply,
    lastUserText: userText.slice(0, 400),
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
    });
  }
  if (request.method === 'GET' && url.pathname === '/__stats') {
    return sendJson(response, 200, {
      startedAt: state.startedAt,
      mainCalls: state.mainCalls,
      helperCalls: state.helperCalls,
      otherCalls: state.otherCalls,
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
