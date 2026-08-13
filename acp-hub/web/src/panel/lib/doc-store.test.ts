import { describe, expect, it, vi } from 'vitest';
import * as Y from 'yjs';
import { bytesToBase64 } from './protocol';
import { DocStore } from './doc-store';

describe('DocStore', () => {
  it('applies v1 updates and coalesces repeated renders for one document', () => {
    const callbacks: FrameRequestCallback[] = [];
    vi.spyOn(globalThis, 'requestAnimationFrame').mockImplementation((callback) => {
      callbacks.push(callback);
      return callbacks.length;
    });
    const source = new Y.Doc();
    source.getMap('root').set('status', 'ready');
    const frame = { doc: 'hub:registry', update: bytesToBase64(Y.encodeStateAsUpdate(source)) };
    const store = new DocStore();
    const onUpdate = vi.fn();
    store.onUpdate = onUpdate;

    store.applyUpdateFrame(frame);
    store.applyUpdateFrame(frame);

    expect(store.docFor(frame.doc).getMap('root').get('status')).toBe('ready');
    expect(callbacks).toHaveLength(1);
    expect(onUpdate).not.toHaveBeenCalled();
    callbacks[0](0);
    expect(onUpdate).toHaveBeenCalledOnce();
    expect(onUpdate).toHaveBeenCalledWith(frame.doc);
  });

  it('contains a corrupt frame, preserves prior state and still schedules reconciliation render', () => {
    const callbacks: FrameRequestCallback[] = [];
    vi.spyOn(globalThis, 'requestAnimationFrame').mockImplementation((callback) => {
      callbacks.push(callback);
      return callbacks.length;
    });
    const warning = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const store = new DocStore();
    store.docFor('chat:1').getMap('root').set('safe', 'existing');
    const onUpdate = vi.fn();
    store.onUpdate = onUpdate;

    expect(() => store.applyUpdateFrame({ doc: 'chat:1', update: 'not-base64%%%' })).not.toThrow();
    expect(store.docFor('chat:1').getMap('root').get('safe')).toBe('existing');
    expect(warning).toHaveBeenCalledOnce();
    callbacks[0](0);
    expect(onUpdate).toHaveBeenCalledWith('chat:1');
  });

  it('destroys cached identities and recreates an empty document after clear', () => {
    const store = new DocStore();
    const previous = store.docFor('session:1');
    previous.getMap('root').set('secret', 'projection');

    store.clear();
    const next = store.docFor('session:1');

    expect(next).not.toBe(previous);
    expect(next.getMap('root').has('secret')).toBe(false);
  });

  it('fences queued renders from a cleared connection without consuming new work', () => {
    const callbacks: FrameRequestCallback[] = [];
    vi.spyOn(globalThis, 'requestAnimationFrame').mockImplementation((callback) => {
      callbacks.push(callback);
      return callbacks.length;
    });
    const source = new Y.Doc();
    source.getMap('root').set('generation', 'new');
    const frame = { doc: 'hub:registry', update: bytesToBase64(Y.encodeStateAsUpdate(source)) };
    const store = new DocStore();
    const onUpdate = vi.fn();
    store.onUpdate = onUpdate;

    store.applyUpdateFrame(frame);
    store.clear();
    store.applyUpdateFrame(frame);
    expect(callbacks).toHaveLength(2);

    callbacks[0](0);
    expect(onUpdate).not.toHaveBeenCalled();
    callbacks[1](1);
    expect(onUpdate).toHaveBeenCalledOnce();
    expect(store.docFor(frame.doc).getMap('root').get('generation')).toBe('new');
  });
});
