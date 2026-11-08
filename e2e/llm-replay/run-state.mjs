import { EventEmitter } from 'node:events';
import { canonicalJson, identityKey, identityOf, ReplayError, validateContext } from './matcher.mjs';

function bindingKey({ field, live, owner = '', execution = '', call = '' }) {
  return JSON.stringify([field, owner, execution, call, live]);
}

export class ReplayRun extends EventEmitter {
  constructor(id, mode, entries, options = {}) {
    super();
    if (typeof id !== 'string' || !id) throw new ReplayError('invalid_run', 'run ID 必须为非空字符串');
    this.id = id;
    this.mode = mode;
    this.entries = entries;
    this.options = options;
    this.calls = new Map();
    this.errors = [];
    this.identityBindings = new Map();
    this.identityReverse = new Map();
    this.gates = new Map();
    this.finalized = false;
    this.upstreamCalls = 0;
    this.pendingDurable = 0;
    this.pendingCleanup = 0;
    this.activeGeneration = options.generation;
    this.ownerGenerations = new Map(Object.entries(options.generations ?? {}));
    for (const gate of options.gates ?? []) {
      validateContext({ run: id, ...gate.identity });
      if (!Number.isSafeInteger(gate.afterBytes) || gate.afterBytes < 0) throw new ReplayError('invalid_gate', 'afterBytes 必须为非负整数');
      this.gates.set(identityKey(gate.identity), { limit: gate.afterBytes, reached: false, waiters: new Set() });
    }
  }

  bindIdentity(binding) {
    const { field, live, canonical, anchor, owner = '', execution = '', call = '' } = binding;
    if (!['owner', 'execution', 'call', 'tool_call'].includes(field) || ![live, canonical, anchor].every((v) => typeof v === 'string' && v)) {
      throw new ReplayError('invalid_identity_binding', '身份绑定须给出字段、实际值、逻辑值及因果锚');
    }
    if (field !== 'owner' && !owner) throw new ReplayError('invalid_identity_binding', '执行与调用绑定必须指定逻辑 owner');
    if (field === 'call' && !execution) throw new ReplayError('invalid_identity_binding', '调用绑定必须指定逻辑 execution');
    if (field === 'tool_call' && (!execution || !call)) throw new ReplayError('invalid_identity_binding', '工具绑定必须指定所属 execution 与 call');
    const key = bindingKey({ field, live, owner, execution, call });
    const reverse = bindingKey({ field, live: canonical, owner, execution, call });
    const previous = this.identityBindings.get(key);
    if (previous && (previous.canonical !== canonical || previous.anchor !== anchor)) {
      throw new ReplayError('identity_binding_conflict', '同一实际身份不能改绑');
    }
    if (this.identityReverse.has(reverse) && this.identityReverse.get(reverse) !== live) {
      throw new ReplayError('identity_binding_conflict', '两个实际身份不能绑定到同一逻辑身份');
    }
    this.identityBindings.set(key, { canonical, anchor });
    this.identityReverse.set(reverse, live);
  }

  async resolveContext(rawContext, signal) {
    validateContext(rawContext);
    if (rawContext.run !== this.id) throw new ReplayError('wrong_run', '调用身份属于其他 run');
    await this.options.resolveContext?.(Object.freeze({ ...rawContext }), this, signal);
    const context = { ...rawContext };
    context.owner = this.identityBindings.get(bindingKey({ field: 'owner', live: context.owner }))?.canonical ?? context.owner;
    const generation = this.ownerGenerations.get(context.owner) ?? this.activeGeneration;
    if (generation !== undefined && rawContext.generation !== generation) {
      throw new ReplayError('stale_generation', '请求来自该 owner 未激活的进程 generation');
    }
    context.execution = this.identityBindings.get(bindingKey({ field: 'execution', owner: context.owner, live: context.execution }))?.canonical ?? context.execution;
    context.call = this.identityBindings.get(bindingKey({ field: 'call', owner: context.owner, execution: context.execution, live: context.call }))?.canonical ?? context.call;
    if (context.parent) {
      const parent = { ...context.parent };
      parent.owner = this.identityBindings.get(bindingKey({ field: 'owner', live: parent.owner }))?.canonical ?? parent.owner;
      parent.execution = this.identityBindings.get(bindingKey({ field: 'execution', owner: parent.owner, live: parent.execution }))?.canonical ?? parent.execution;
      parent.call = this.identityBindings.get(bindingKey({ field: 'call', owner: parent.owner, execution: parent.execution, live: parent.call }))?.canonical ?? parent.call;
      parent.tool_call = this.identityBindings.get(bindingKey({ field: 'tool_call', owner: parent.owner, execution: parent.execution, call: parent.call, live: parent.tool_call }))?.canonical ?? parent.tool_call;
      context.parent = parent;
    }
    return context;
  }

  setGeneration(generation) {
    if (typeof generation !== 'string' || !generation) throw new ReplayError('invalid_generation', 'generation 必须非空');
    if ([...this.calls.values()].some((call) => call.state === 'reserved' || call.state === 'streaming')) {
      throw new ReplayError('active_generation', '旧进程连接未终结，不能切换 generation');
    }
    this.activeGeneration = generation;
  }

  setOwnerGeneration(owner, generation) {
    if (![owner, generation].every((value) => typeof value === 'string' && value)) throw new ReplayError('invalid_generation', 'owner/generation 必须非空');
    if ([...this.calls.values()].some((call) => call.identity.owner === owner && ['reserved', 'streaming'].includes(call.state))) {
      throw new ReplayError('active_generation', '该 owner 旧进程连接尚未终结');
    }
    this.ownerGenerations.set(owner, generation);
  }

  reserve(context) {
    if (this.finalized) throw new ReplayError('run_finalized', '完成后的 run 不接受新请求');
    const key = identityKey(context);
    if (this.calls.has(key)) throw new ReplayError('duplicate_identity', '同一调用 attempt 不得重复消费', { identity: identityOf(context) });
    const sameCall = (identity) => ['owner', 'execution', 'call', 'purpose'].every((field) => identity[field] === context[field]);
    const earlier = [...this.calls.values()].filter((call) => sameCall(call.identity));
    const previousAttempt = earlier.reduce((max, call) => Math.max(max, call.identity.attempt), 0);
    if (context.attempt !== previousAttempt + 1 || earlier.some((call) => ['reserved', 'streaming'].includes(call.state))) {
      throw new ReplayError('attempt_order', '同一调用的重试必须在前一次终结后按 attempt 顺序发生');
    }
    const entry = this.mode === 'replay' ? this.entries.get(key) : undefined;
    if (this.mode === 'replay' && !entry) throw new ReplayError('unmatched_request', 'cassette 没有该调用身份', { identity: identityOf(context) });
    if (entry && canonicalJson(entry.identity.parent ?? null) !== canonicalJson(context.parent ?? null)) {
      throw new ReplayError('parent_mismatch', '调用的父工具因果身份与录制不符');
    }
    const call = { identity: identityOf(context), state: 'reserved', writtenBytes: 0, terminal: null, entry };
    this.calls.set(key, call);
    this.emit('change');
    return call;
  }

  fail(error) {
    this.errors.push({ code: error.code ?? 'proxy_error', message: error.message, details: error.details ?? {} });
    this.emit('change');
  }

  start(call) { call.state = 'streaming'; this.emit('change'); }

  progress(call, bytes) { call.writtenBytes += bytes; this.emit('change'); }

  finish(call, kind) {
    call.state = kind === 'complete' ? 'complete' : 'abort';
    call.terminal = kind;
    if (this.mode === 'replay' && call.entry.response.terminal.kind !== kind) {
      this.fail(new ReplayError('terminal_mismatch', '实际终结方式与录制不同', {
        identity: call.identity, expected: call.entry.response.terminal.kind, actual: kind,
      }));
    }
    const expectedSize = call.expectedResponseBytes ?? call.entry?.response.submittedBytes ?? call.entry?.response.size;
    if (this.mode === 'replay' && call.writtenBytes !== expectedSize) {
      this.fail(new ReplayError('response_prefix_mismatch', '终结时发送的响应前缀与录制不符', {
        identity: call.identity, expectedSubmittedBytes: expectedSize, actualSubmittedBytes: call.writtenBytes,
      }));
    }
    this.emit('change');
  }

  async waitAtGate(call, signal) {
    const gate = this.gates.get(identityKey(call.identity));
    while (gate && call.writtenBytes >= gate.limit) {
      if (signal.aborted) throw signal.reason;
      gate.reached = true;
      this.emit('change');
      await new Promise((resolve, reject) => {
        const released = () => { cleanup(); resolve(); };
        const aborted = () => { cleanup(); reject(signal.reason); };
        const cleanup = () => { gate.waiters.delete(released); signal.removeEventListener('abort', aborted); };
        gate.waiters.add(released);
        signal.addEventListener('abort', aborted, { once: true });
        if (signal.aborted) aborted();
      });
    }
  }

  permittedBytes(call, length) {
    const gate = this.gates.get(identityKey(call.identity));
    return gate ? Math.min(length, Math.max(0, gate.limit - call.writtenBytes)) : length;
  }

  release(identity, throughBytes = Infinity) {
    const gate = this.gates.get(identityKey(identity));
    if (!gate) throw new ReplayError('unknown_gate', '该调用没有控制门');
    if (!(throughBytes === Infinity || Number.isSafeInteger(throughBytes)) || throughBytes <= gate.limit) {
      throw new ReplayError('invalid_gate_release', '放行边界必须严格递增');
    }
    gate.limit = throughBytes;
    gate.reached = false;
    for (const release of [...gate.waiters]) release();
    this.emit('change');
  }

  waitReached(identity, options = {}) {
    const key = identityKey(identity);
    if (!this.gates.has(key)) return Promise.reject(new ReplayError('unknown_gate', '该调用没有控制门'));
    return this.waitFor(() => {
      if (this.errors.length) throw new ReplayError('run_failed', '控制门等待期间 run 已失败', { errors: [...this.errors] });
      if (this.gates.get(key).reached) return true;
      const call = this.calls.get(key);
      if (call && ['complete', 'abort'].includes(call.state)) throw new ReplayError('gate_not_reached', '调用已结束，未到达指定控制门', { identity, terminal: call.terminal });
      return false;
    }, options);
  }

  waitFor(predicate, { timeout = 5000 } = {}) {
    return new Promise((resolve, reject) => {
      let timer;
      const cleanup = () => { this.off('change', check); clearTimeout(timer); };
      const check = () => {
        try {
          const snapshot = this.snapshot();
          if (predicate(snapshot)) { cleanup(); resolve(snapshot); }
        } catch (error) { cleanup(); reject(error); }
      };
      this.on('change', check);
      timer = setTimeout(() => { cleanup(); reject(new ReplayError('wait_timeout', '等待录放状态超时', { snapshot: this.snapshot() })); }, timeout);
      check();
    });
  }

  snapshot() {
    const calls = [...this.calls.values()].map(({ entry, ...call }) => ({ ...call, identity: { ...call.identity } }));
    return {
      id: this.id, mode: this.mode, finalized: this.finalized,
      upstreamCalls: this.upstreamCalls, errors: [...this.errors], calls,
      pendingDurable: this.pendingDurable,
      pendingCleanup: this.pendingCleanup,
      active: calls.filter((call) => ['reserved', 'streaming'].includes(call.state)).length + this.pendingDurable + this.pendingCleanup,
      missing: this.mode === 'replay' ? [...this.entries.keys()].filter((key) => !this.calls.has(key)).map((key) => this.entries.get(key).identity) : [],
    };
  }

  complete() {
    const summary = this.snapshot();
    if (!summary.calls.length || summary.active || summary.missing.length || summary.errors.length) {
      throw new ReplayError('run_incomplete', 'run 存在缺失、在途或失败调用', { summary });
    }
    this.finalized = true;
    return { ...summary, finalized: true, passed: true };
  }
}
