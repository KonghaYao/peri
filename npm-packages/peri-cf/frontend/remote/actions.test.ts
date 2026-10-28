import { describe, expect, test } from 'bun:test';
import { ChatUserAction } from '@peri-code/sdk';
import { PayloadReadError } from '@peri-code/sdk/view';
import {
  RemoteActionError, createRemoteActions, parseActionTarget,
} from './actions';

const target = { sessionId: 'session-1', bindingRevision: 2 };

function actions(respond: (request: Request) => Response | Promise<Response>) {
  const requests: Request[] = [];
  const client = createRemoteActions({
    origin: 'https://example.test',
    fetch: (async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input, init);
      requests.push(request);
      return await respond(request);
    }) as typeof fetch,
  });
  return { client, requests };
}

function body(request: Request): Promise<Record<string, unknown>> {
  return request.json() as Promise<Record<string, unknown>>;
}

describe('POST /api/chats handle creation', () => {
  test('only registers a handle and returns the empty binding', async () => {
    const { client, requests } = actions(() => Response.json({
      chat: { id: 'chat-1', sessionId: null, bindingRevision: 0 },
    }, { status: 201 }));
    expect(await client.createHandle()).toEqual({ id: 'chat-1', sessionId: null, bindingRevision: 0 });
    expect(requests[0]?.method).toBe('POST');
    expect(requests[0]?.url).toBe('https://example.test/api/chats');
    expect(await body(requests[0]!)).toEqual({});
  });

  test('forwards the target session id so the server can prepare its sandbox', async () => {
    const { client, requests } = actions(() => Response.json({
      chat: { id: 'chat-1', sessionId: null, bindingRevision: 0 },
    }, { status: 201 }));
    await client.createHandle('session-1');
    expect(await body(requests[0]!)).toEqual({ sessionId: 'session-1' });
  });
});

describe('Action envelope and 202 receipt', () => {
  test('send posts flat action + target and parses inputId/binding', async () => {
    const { client, requests } = actions(() => Response.json({
      ok: true, requestId: 'request-1', chatId: 'chat-1', sessionId: 'session-1',
      bindingRevision: 2, inputId: 'input-1', accepted: true,
    }, { status: 202 }));
    const action = ChatUserAction.send({ text: '你好', requestId: 'request-1' });
    const receipt = await client.execute('chat-1', action, target);
    expect(receipt).toMatchObject({
      requestId: 'request-1', chatId: 'chat-1', sessionId: 'session-1',
      bindingRevision: 2, inputId: 'input-1', accepted: true,
    });
    expect(await body(requests[0]!)).toEqual({
      type: 'send', requestId: 'request-1', payload: { text: '你好' }, target,
    });
  });

  test('invalid local target is not sent and reports not_sent', async () => {
    const { client, requests } = actions(() => Response.json({}));
    await expect(client.execute('chat-1', ChatUserAction.cancel(), { sessionId: null, bindingRevision: -1 }))
      .rejects.toMatchObject({ code: 'invalid_target', disposition: 'not_sent' });
    expect(requests).toHaveLength(0);
  });

  test('rejects a receipt whose requestId does not match the sent action', async () => {
    const { client } = actions(() => Response.json({
      ok: true, requestId: 'other', chatId: 'chat-1', sessionId: 'session-1', bindingRevision: 2,
      inputId: 'input-1',
    }, { status: 202 }));
    await expect(client.execute('chat-1', ChatUserAction.send({ text: 'x', requestId: 'request-1' }), target))
      .rejects.toMatchObject({ disposition: 'unknown', code: 'transport_error' });
  });

  test('non-202 success status is treated as unknown, not delivered', async () => {
    const { client } = actions(() => Response.json({
      ok: true, requestId: 'request-1', chatId: 'chat-1', sessionId: 'session-1', bindingRevision: 2,
    }, { status: 200 }));
    await expect(client.execute('chat-1', ChatUserAction.cancel({ requestId: 'request-1' }), target))
      .rejects.toMatchObject({ disposition: 'unknown', status: 200 });
  });
});

describe('Action HTTP error mapping', () => {
  test('400 invalid_action stays rejected', async () => {
    const { client } = actions(() => Response.json({
      ok: false, code: 'invalid_action', message: 'bad action', disposition: 'rejected', requestId: 'request-1',
    }, { status: 400 }));
    await expect(client.execute('chat-1', ChatUserAction.cancel({ requestId: 'request-1' }), target))
      .rejects.toMatchObject({ code: 'invalid_action', disposition: 'rejected', status: 400 });
  });

  test('409 busy/binding_conflict/not_ready preserve server disposition', async () => {
    for (const code of ['busy', 'binding_conflict', 'not_ready'] as const) {
      const { client } = actions(() => Response.json({
        ok: false, code, message: code, disposition: 'rejected',
      }, { status: 409 }));
      await expect(client.execute('chat-1', ChatUserAction.cancel(), target))
        .rejects.toMatchObject({ code, disposition: 'rejected', status: 409 });
    }
  });

  test('410 unknown chat', async () => {
    const { client } = actions(() => Response.json({
      ok: false, code: 'chat_not_found', message: 'Chat not found', disposition: 'rejected', chatId: 'missing',
    }, { status: 410 }));
    await expect(client.execute('missing', ChatUserAction.cancel(), { sessionId: null, bindingRevision: 0 }))
      .rejects.toMatchObject({ code: 'chat_not_found', status: 410 });
  });

  test('500 internal error is unknown and carries createdSessionId/stage', async () => {
    const { client } = actions(() => Response.json({
      ok: false, code: 'session_creation_failed', message: 'rename failed', disposition: 'unknown',
      createdSessionId: 'created-session', stage: 'rename',
    }, { status: 500 }));
    await expect(client.execute('chat-1', ChatUserAction.createNewSession(), { sessionId: null, bindingRevision: 0 }))
      .rejects.toMatchObject({
        code: 'session_creation_failed', disposition: 'unknown', status: 500,
        createdSessionId: 'created-session', stage: 'rename',
      });
  });

  test('unparseable error bodies never claim the action was rejected', async () => {
    const { client } = actions(() => new Response('not-json', { status: 502 }));
    await expect(client.execute('chat-1', ChatUserAction.cancel(), target))
      .rejects.toMatchObject({ code: 'internal_error', disposition: 'unknown', status: 502 });
  });

  test('network failure is unknown because the action may have been written', async () => {
    const { client } = actions(() => { throw new TypeError('Failed to fetch'); });
    const error = await client.execute('chat-1', ChatUserAction.cancel(), target).catch(value => value);
    expect(error).toBeInstanceOf(RemoteActionError);
    expect(error).toMatchObject({ code: 'transport_error', disposition: 'unknown' });
  });
});

describe('payload and close endpoints', () => {
  test('reads payload with the referenced byte count', async () => {
    const { client, requests } = actions(() => Response.json({
      ok: true, payload: { id: 'ref-1', bytes: 2, json: '{}' },
    }));
    expect(await client.readPayload('chat-1', { id: 'ref-1', bytes: 2 }))
      .toEqual({ id: 'ref-1', bytes: 2, json: '{}' });
    expect(requests[0]?.url).toContain('/api/chats/chat-1/payload/ref-1?bytes=2');
  });

  test('maps payload error codes without reinterpreting HTTP status', async () => {
    const { client } = actions(() => Response.json({
      ok: false, code: 'too_large', message: 'too big',
    }, { status: 413 }));
    const error = await client.readPayload('chat-1', { id: 'ref-1', bytes: 2 }).catch(value => value);
    expect(error).toBeInstanceOf(PayloadReadError);
    expect(error).toMatchObject({ code: 'too_large' });
  });

  test('close treats 204 and already-missing 410 as finished', async () => {
    const { client } = actions((request) => request.url.endsWith('/chat-1')
      ? new Response(null, { status: 204 })
      : Response.json({ ok: false, code: 'chat_not_found', message: 'gone', disposition: 'rejected' }, { status: 410 }));
    await expect(client.close('chat-1')).resolves.toBeUndefined();
    await expect(client.close('chat-2')).resolves.toBeUndefined();
  });

  test('close_incomplete keeps the incomplete flag and failure reasons', async () => {
    const { client } = actions(() => Response.json({
      ok: false, code: 'close_incomplete', message: 'Chat is closed but cleanup is incomplete',
      disposition: 'unknown', incomplete: true, errors: ['Sandbox shutdown is incomplete'], chatId: 'chat-1',
    }, { status: 500 }));
    const error = await client.close('chat-1').catch((value: unknown) => value);
    expect(error).toBeInstanceOf(RemoteActionError);
    expect(error).toMatchObject({ code: 'close_incomplete', disposition: 'unknown', incomplete: true });
    expect(error).toHaveProperty('errors', ['Sandbox shutdown is incomplete']);
  });
});

describe('target parsing', () => {
  test('accepts empty and bound targets; rejects malformed revisions', () => {
    expect(parseActionTarget({ sessionId: null, bindingRevision: 0 })).toEqual({ sessionId: null, bindingRevision: 0 });
    expect(parseActionTarget(target)).toEqual(target);
    expect(() => parseActionTarget({ sessionId: 's', bindingRevision: -1 })).toThrow(RemoteActionError);
    expect(() => parseActionTarget({ sessionId: 42, bindingRevision: 1 })).toThrow(RemoteActionError);
  });
});
