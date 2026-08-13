import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { WsClient, type WsProtocolIssue } from './ws-client';

class FakeWebSocket {
  static readonly OPEN = 1;
  static instances: FakeWebSocket[] = [];

  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  send = vi.fn<(data: string) => void>();
  close = vi.fn();

  constructor(readonly url: string) {
    FakeWebSocket.instances.push(this);
  }

  deliver(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

function harness(overrides: Partial<ConstructorParameters<typeof WsClient>[0]> = {}) {
  const statuses = vi.fn();
  const frames = vi.fn();
  const issues = vi.fn<(issue: WsProtocolIssue) => void>();
  const client = new WsClient({
    url: 'ws://127.0.0.1:8456/',
    onStatus: statuses,
    onFrame: frames,
    onProtocolIssue: issues,
    ...overrides,
  });
  client.connect();
  return { client, socket: FakeWebSocket.instances.at(-1)!, statuses, frames, issues };
}

beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.stubGlobal('WebSocket', FakeWebSocket);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('WsClient protocol boundary', () => {
  it('rejects binary input without exposing payload content', () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const { socket, frames, issues } = harness();
    socket.deliver(new Blob(['private-prompt']));

    expect(frames).not.toHaveBeenCalled();
    expect(issues).toHaveBeenCalledWith({ kind: 'non_text_frame', size: 14 });
    expect(error.mock.calls.flat().join(' ')).not.toContain('private-prompt');
  });

  it('reports malformed text by size and continues with the next frame', () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const { socket, frames, issues } = harness();
    socket.deliver('{not-json:private-prompt}');
    socket.deliver('{"t":"future.frame","value":1}');

    expect(issues).toHaveBeenCalledWith({ kind: 'malformed_frame', size: 25 });
    expect(frames).toHaveBeenCalledOnce();
    expect(frames).toHaveBeenCalledWith({ t: 'future.frame', value: 1 });
  });

  it('isolates a consumer exception so later deliveries still run', () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const onFrame = vi.fn()
      .mockImplementationOnce(() => { throw new Error('secret callback detail'); })
      .mockImplementationOnce(() => undefined);
    const { socket, issues } = harness({ onFrame });
    socket.deliver('{"t":"first"}');
    socket.deliver('{"t":"second"}');

    expect(onFrame).toHaveBeenCalledTimes(2);
    expect(issues).toHaveBeenCalledWith({ kind: 'callback_error', callback: 'frame' });
  });

  it('returns unavailable when native send throws and reports no frame body', () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const { client, socket, issues } = harness();
    socket.readyState = FakeWebSocket.OPEN;
    socket.send.mockImplementation(() => { throw new Error('transport rejected private-prompt'); });

    expect(client.send({ t: 'action', payload: 'private-prompt' })).toBe(false);
    expect(issues).toHaveBeenCalledWith({ kind: 'send_error' });
    expect(error.mock.calls.flat().join(' ')).not.toContain('private-prompt');
  });

  it('does not report a healthy heartbeat when pong cannot be written', () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const { socket, statuses, issues } = harness();
    socket.readyState = FakeWebSocket.OPEN;
    socket.send.mockImplementation(() => { throw new Error('closed during pong'); });
    socket.deliver('{"t":"keep_alive"}');

    expect(issues).toHaveBeenCalledWith({ kind: 'send_error' });
    expect(statuses).not.toHaveBeenCalledWith('heartbeat', {});
  });

  it('contains status callback failures without aborting connection setup', () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const onStatus = vi.fn(() => { throw new Error('consumer detail'); });
    const { socket, issues } = harness({ onStatus });

    expect(socket).toBeDefined();
    expect(issues).toHaveBeenCalledWith({ kind: 'callback_error', callback: 'status' });
  });

  it('never exposes a legacy bearer token when auth send throws', () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const onStatus = vi.fn();
    const { socket } = harness({ token: 'super-secret-token', onStatus });
    socket.send.mockImplementation(() => { throw new Error('serialized super-secret-token'); });
    socket.onopen?.();

    expect(onStatus).toHaveBeenLastCalledWith('fatal', { code: 0, reason: 'auth 发送失败' });
    expect(error.mock.calls.flat().join(' ')).not.toContain('super-secret-token');
  });
});
