import { SessionDocReplica, decodeSyncFrame, encodeAckFrame } from "../../worker/sdk";
import { readSyncState } from "../../shared/sync-state";
import type { SyncSocket, SocketHost } from "../../worker/chat/sync";
import type { ChatCommand, ChatCommandResult } from "../../shared/sync";
import type { SyncState } from "../../shared/sync";

/** 内存 socket：二进制帧喂 SDK 副本，文本帧留给控制帧断言。 */
export class FixtureSocket implements SyncSocket {
  readonly frames: (Uint8Array | string)[] = [];
  readonly replica = new SessionDocReplica();
  readyState = 1;
  autoAck = true;
  deferClose = false;
  closed?: { code: number; reason: string };
  private readonly messages: ((event: { data: unknown }) => void)[] = [];
  private readonly closures: (() => void)[] = [];
  private readonly errors: (() => void)[] = [];

  accept(): void {}

  send(message: Uint8Array | string): void {
    this.frames.push(message);
    if (typeof message === "string") return;
    const frame = decodeSyncFrame(message);
    if (frame.type === "snapshot") this.replica.applySnapshot(frame.snapshot);
    else this.replica.applyUpdate(frame.update);
    if (this.autoAck) queueMicrotask(() => this.receive(encodeAckFrame(frame.delivery)));
  }

  close(code: number, reason: string): void {
    this.closed = { code, reason };
    if (this.deferClose) { this.readyState = 2; return; }
    this.confirmClose();
  }

  confirmClose(): void {
    this.readyState = 3;
    for (const listener of this.closures) listener();
  }

  addEventListener(type: string, listener: ((event: { data: unknown }) => void) | (() => void)): void {
    if (type === "message") this.messages.push(listener as (event: { data: unknown }) => void);
    else if (type === "close") this.closures.push(listener as () => void);
    else this.errors.push(listener as () => void);
  }

  /** 主动注入传输错误（close 不得触发 error 监听器）。 */
  fail(): void { for (const listener of this.errors) listener(); }

  receive(data: unknown): void { for (const listener of this.messages) listener({ data }); }

  state(): SyncState { return readSyncState(this.replica.chat, this.replica.session); }

  /** 已收到的控制帧回执。 */
  results(): ChatCommandResult[] {
    return this.frames.filter((frame): frame is string => typeof frame === "string")
      .map((frame) => JSON.parse(frame) as ChatCommandResult);
  }

  /** 等待指定 id 的控制帧回执。 */
  async result(id: string, budgetMs = 4_000): Promise<ChatCommandResult> {
    const deadline = Date.now() + budgetMs;
    for (;;) {
      const found = this.results().find((result) => result.id === id);
      if (found) return found;
      if (Date.now() > deadline) throw new Error(`No control result for ${id} within ${budgetMs}ms`);
      await Bun.sleep(2);
    }
  }

  async resultFor(command: ChatCommand, budgetMs = 4_000): Promise<ChatCommandResult> {
    return this.result(command.id, budgetMs);
  }

  /** 最近一次发送（二进制/文本都算）。 */
  lastFrame(): Uint8Array | string | undefined { return this.frames.at(-1); }
}

/** 平台 seam 的内存实现：记录 socket、保活 promise 与可注入时钟。 */
export class FixtureSocketHost implements SocketHost {
  readonly sockets: FixtureSocket[] = [];
  readonly work: Promise<unknown>[] = [];

  constructor(private readonly clock: () => number = Date.now) {}

  upgrade() {
    const socket = new FixtureSocket();
    this.sockets.push(socket);
    return { socket, response: new Response(null, { status: 101 }) };
  }

  keepAlive(promise: Promise<unknown>): void { this.work.push(promise); }

  now(): number { return this.clock(); }

  last(): FixtureSocket { return this.sockets.at(-1)!; }

  async drain(): Promise<void> {
    // 保活里可能混入被拒绝的执行收尾 promise：测试只关心是否结算完成。
    await Promise.all(this.work.map((promise) => promise.catch(() => {})));
  }
}

/** 等待连接副本收敛到指定展示状态（跨上下文投递按泵节奏异步到达）。 */
export async function untilState(client: { state(): SyncState }, predicate: (state: SyncState) => boolean,
  budgetMs = 2_000): Promise<void> {
  await until(() => predicate(client.state()), budgetMs);
}

/** 等待条件成立（默认 2s 预算）。 */
export async function until(predicate: () => boolean, budgetMs = 2_000): Promise<void> {
  const deadline = Date.now() + budgetMs;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error("condition not reached in time");
    await Bun.sleep(2);
  }
}

/** 等待异步条件成立。 */
export async function untilAsync(predicate: () => Promise<boolean>, budgetMs = 2_000): Promise<void> {
  const deadline = Date.now() + budgetMs;
  while (!(await predicate())) {
    if (Date.now() > deadline) throw new Error("condition not reached in time");
    await Bun.sleep(2);
  }
}
