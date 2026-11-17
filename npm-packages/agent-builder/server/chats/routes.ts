import {
  ChatCreateRequest,
  type ChatListResponse,
  type ChatResponse,
  ChatSendRequest,
  type ChatSummary,
} from '@shared/chat';
import { Hono } from 'hono';
import { z } from 'zod';
import { AppError, readJsonBody } from '../errors';
import { SSE_HEARTBEAT_MS } from './keepalive';
import type { ChatService } from './service';
import type { StreamEvent } from './stream';

const ChatCancelRequest = z.strictObject({
  requestId: z.string().min(1).max(200).optional(),
});

const AgentIdQuery = z.uuid();

function parseSinceSeq(query: string | undefined, header: string | undefined): number | undefined {
  for (const raw of [query, header]) {
    if (raw === undefined || raw.length === 0) continue;
    const value = Number(raw);
    if (Number.isInteger(value) && value >= 0) return value;
  }
  return undefined;
}

function formatEvent(event: StreamEvent): string {
  return `id: ${event.seq}\nevent: ${event.event}\ndata: ${JSON.stringify(event.data)}\n\n`;
}

const SSE_HEADERS = {
  'content-type': 'text/event-stream; charset=utf-8',
  'cache-control': 'no-cache, no-transform',
  connection: 'keep-alive',
  'x-accel-buffering': 'no',
} as const;

export function createChatRoutes(service: ChatService): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const agentId = c.req.query('agentId');
    let filter: string | undefined;
    if (agentId !== undefined) {
      const parsed = AgentIdQuery.safeParse(agentId);
      if (!parsed.success) throw new AppError('invalid_input', 'agentId must be a UUID');
      filter = parsed.data;
    }
    const body: ChatListResponse = { chats: service.list(filter) };
    return c.json(body);
  });

  routes.post('/', async (c) => {
    const request = ChatCreateRequest.parse(await readJsonBody(c));
    const chat: ChatSummary = await service.create(request);
    const body: ChatResponse = { chat };
    return c.json(body, 201);
  });

  routes.get('/:chatId/events', async (c) => {
    const chatId = c.req.param('chatId');
    const sinceSeq = parseSinceSeq(c.req.query('sinceSeq'), c.req.header('Last-Event-ID'));
    // Start the actor/projection before committing the response so 404/503 stay real HTTP errors.
    const hub = await service.prepareStream(chatId);

    const abort = new AbortController();
    const requestSignal = c.req.raw.signal;
    const onAbort = () => abort.abort();
    requestSignal.addEventListener('abort', onAbort);

    const encoder = new TextEncoder();
    let finish = () => {};
    let send = (_event: StreamEvent) => {};
    let unsubscribe: (() => void) | null = null;
    let done = false;
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        const heartbeat = setInterval(() => {
          if (done) return;
          try {
            controller.enqueue(encoder.encode(': ping\n\n'));
          } catch {
            finish();
          }
        }, SSE_HEARTBEAT_MS);

        finish = () => {
          if (done) return;
          done = true;
          clearInterval(heartbeat);
          requestSignal.removeEventListener('abort', onAbort);
          abort.signal.removeEventListener('abort', finish);
          unsubscribe?.();
          try {
            controller.close();
          } catch {
            // Already closed by the client; nothing to release.
          }
        };
        abort.signal.addEventListener('abort', finish);

        send = (event: StreamEvent) => {
          if (done) return;
          try {
            controller.enqueue(encoder.encode(formatEvent(event)));
          } catch {
            finish();
            return;
          }
          // `closed` is terminal: a reconnect must start a new stream, not wait for more facts.
          if (event.event === 'closed') finish();
        };
      },
      cancel() {
        abort.abort();
      },
    });

    try {
      unsubscribe = await hub.subscribe(sinceSeq, send, finish);
      if (requestSignal.aborted) abort.abort();
      if (done) unsubscribe();
    } catch (error) {
      finish();
      throw error;
    }
    return new Response(stream, { headers: SSE_HEADERS });
  });

  routes.post('/:chatId/send', async (c) => {
    const request = ChatSendRequest.parse(await readJsonBody(c));
    return c.json(await service.send(c.req.param('chatId'), request.requestId, request.text));
  });

  routes.post('/:chatId/cancel', async (c) => {
    const body = await readJsonBody(c).catch(() => ({}));
    const request = ChatCancelRequest.parse(body);
    return c.json(await service.cancel(c.req.param('chatId'), request.requestId));
  });

  routes.post('/:chatId/close', async (c) => {
    const body = await service.close(c.req.param('chatId'));
    return c.json(body, body.status === 'closed' ? 200 : 202);
  });

  routes.delete('/:chatId', async (c) => {
    await service.remove(c.req.param('chatId'));
    return c.body(null, 204);
  });

  return routes;
}
