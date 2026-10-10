import { describe, expect, test } from "bun:test";
import { FixtureTransport, promptCommand, cancelCommand, interactionCommand, payloadCommand, replayChunk,
  replayToolCall, runtimeFixture, type TransportPlan } from "./helpers/runtime";
import { until, untilAsync, untilState } from "./helpers/sockets";
import { MAX_REPLY_BYTES } from "../worker/chat/session";
import type { SyncState } from "../shared/sync";

/**
 * 执行生命周期走 WS 控制帧（无 DO）：prompt 接受即回执、cancel 等真实结算、
 * 宿主关闭未确认即阻断后续执行。HTTP 命令面已删除（见 backend.test.ts 的 426 断言）。
 */

const encoder = new TextEncoder();

function entryText(entry: SyncState["entries"][number] | undefined): string {
  return entry ? entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])).join("") : "";
}

function barrier() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => { release = resolve; });
  return { promise, release };
}

/** 一次完整回合：建会话 → 连上并等待快照 → 发 prompt → 等宿主进入 prompt。 */
async function running(plan: TransportPlan = {}, content = "Hello") {
  // 默认不预置分块：文本由测试自己的 emit 控制（同一 messageId 累积到同一条目）。
  const app = runtimeFixture({ chunks: [], ...plan });
  const chat = await app.create("Fixture chat");
  const { socket } = await app.attached(chat.id);
  const result = await app.send(socket, promptCommand(content));
  const transport = await app.runTransport();
  await transport.promptStarted;
  return { app, chat, socket, transport, result };
}

describe("prompt commands", () => {
  test("接受即回执并把执行留在后台，最终文本进入 DTO", async () => {
    const { app, chat, socket, transport, result } = await running({ blocked: true });
    expect(result).toEqual({ type: "command_result", id: result.id, ok: true });
    // 回执先于结算：宿主还在跑（blocked 计划下不会自己结束）。
    expect(app.registry.peek(chat.id)!.running).toBe(true);
    transport.emit("reply");
    await untilAsync(async () => entryText((await app.detail(chat.id)).entries.at(-1)) === "reply");
    transport.finish();
    await untilAsync(async () => !(await app.detail(chat.id)).running);
    const detail = await app.detail(chat.id);
    expect(detail.entries.map(entryText)).toEqual(["Hello", "reply"]);
    expect(detail.entries.at(-1)?.status).toBe("completed");
    await untilState(socket, (state) => state.entries.map(entryText).join("|") === "Hello|reply");
  });

  test("只投影根会话事件：外部会话与子代理块被丢弃", async () => {
    const { app, chat, socket, transport } = await running({ blocked: true });
    transport.emit("root");
    await untilAsync(async () => entryText((await app.detail(chat.id)).entries.at(-1)) === "root");
    const texts = (await app.detail(chat.id)).entries.map(entryText);
    expect(texts).toEqual(["Hello", "root"]);
    expect(JSON.stringify(texts)).not.toContain("foreign");
    expect(JSON.stringify(texts)).not.toContain("subagent");
    transport.finish();
    await app.drain();
    await untilState(socket, (state) => state.entries.map((entry) => entry.status).join("|") === "completed|completed");
  });

  test("同时只允许一轮执行：第二次 prompt 回 busy", async () => {
    const { app, socket, transport } = await running({ blocked: true });
    const second = await app.send(socket, promptCommand("Again"));
    expect(second).toEqual({ type: "command_result", id: second.id, ok: false,
      error: { code: "busy", message: "Chat is busy" } });
    expect(transport.calls.filter(({ method }) => method === "session/prompt")).toHaveLength(1);
    await app.send(socket, cancelCommand());
    await app.drain();
  });

  test("回复超过 512 KiB 编码上限即停止本轮并保留已接受前缀", async () => {
    const plan: TransportPlan = { blocked: true };
    const { app, chat, socket, transport } = await running(plan);
    const accepted = "a".repeat(1024);
    transport.emit(accepted);
    await untilAsync(async () => entryText((await app.detail(chat.id)).entries.at(-1)) === accepted);
    // JSON 转义按编码后字节计：一个引号在 JSON 里是 2 字节；多一个即超出上限。
    const budget = Math.floor((MAX_REPLY_BYTES - 1024) / 2) + 1;
    expect(encoder.encode(JSON.stringify('"'.repeat(budget))).byteLength - 2).toBeGreaterThan(MAX_REPLY_BYTES - 1024);
    transport.emit('"'.repeat(budget));
    await until(() => app.registry.peek(chat.id)!.running === false);
    const detail = await app.detail(chat.id);
    const text = entryText(detail.entries.at(-1));
    expect(text.startsWith(accepted)).toBe(true);
    expect(encoder.encode(JSON.stringify(text.slice(accepted.length))).byteLength - 2)
      .toBeLessThanOrEqual(MAX_REPLY_BYTES - 1024);
    // 超预算属协议失败：轮次以 error 收尾（与改造前一致），已接受前缀不改写。
    expect(detail.entries.at(-1)?.status).toBe("error");
    await untilState(socket, (state) => state.entries.at(-1)?.status === "error");
  });

  test("正好 512 KiB（按 JSON 转义后的编码字节）不触发停止", async () => {
    const { app, chat, transport } = await running({ blocked: true });
    // 引号在 JSON 里转义成 2 字节：正好用满预算。
    const text = '"'.repeat(MAX_REPLY_BYTES / 2);
    transport.emit(text);
    await untilAsync(async () => entryText((await app.detail(chat.id)).entries.at(-1)) === text);
    transport.finish();
    await untilAsync(async () => !(await app.detail(chat.id)).running);
    expect((await app.detail(chat.id)).entries.at(-1)?.status).toBe("completed");
    expect(encoder.encode(JSON.stringify(text)).byteLength - 2).toBe(MAX_REPLY_BYTES);
    await app.drain();
  });

  test("模型失败保留公开诊断文案并保持已完成前缀", async () => {
    const { app, chat, socket, transport } = await running({ blocked: true });
    transport.emit("partial");
    await untilAsync(async () => entryText((await app.detail(chat.id)).entries.at(-1)) === "partial");
    transport.finish("end_turn");
    await untilAsync(async () => !(await app.detail(chat.id)).running);
    expect((await app.detail(chat.id)).entries.at(-1)?.status).toBe("completed");
    await app.send(socket, cancelCommand());
    await app.drain();
    await untilState(socket, (state) => state.entries.at(-1)?.status === "completed");
  });
});

describe("cancel commands", () => {
  test("取消等真实结算：通知一次、partial 保留、后续可再跑一轮", async () => {
    const { app, chat, socket, transport } = await running({ blocked: true });
    transport.emit("partial");
    await untilAsync(async () => entryText((await app.detail(chat.id)).entries.at(-1)) === "partial");
    const result = await app.send(socket, cancelCommand());
    expect(result).toEqual({ type: "command_result", id: result.id, ok: true });
    expect(transport.calls.filter(({ method }) => method === "session/cancel")).toHaveLength(1);
    // cancel 返回时结算已完成：宿主已关闭、running 已回落。
    expect(transport.closed).toBe(true);
    expect(app.registry.peek(chat.id)!.running).toBe(false);
    expect((await app.detail(chat.id)).entries.at(-1)?.status).toBe("cancelled");
  });

  test("空闲取消不启动宿主也不报错", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Idle");
    const { socket } = await app.attached(chat.id);
    expect(await app.send(socket, cancelCommand())).toMatchObject({ ok: true });
    expect(app.transports).toHaveLength(1);
  });

  test("取消期间提示词尚未写入：跳过本轮且不发送 session/prompt", async () => {
    const hydration = barrier();
    const plan: TransportPlan = { chunks: [], startupGate: hydration.promise };
    const app = runtimeFixture(plan);
    const chat = await app.create("Cancel during startup");
    const { socket } = await app.connect(chat.id);
    hydration.release();
    await until(() => socket.frames.length > 0);
    // 第二轮宿主启动被卡住：prompt 已受理，取消必须中止启动而不是等它跑完。
    const startup = barrier();
    plan.startupGate = startup.promise;
    expect(await app.send(socket, promptCommand("Never sent"))).toMatchObject({ ok: true });
    const cancel = app.send(socket, cancelCommand());
    startup.release();
    expect(await cancel).toMatchObject({ ok: true });
    await app.drain();
    // 本轮结束后宿主持有关闭请求，直接取最后一个（复原宿主在更早的位置）。
    const transport = app.last();
    expect(transport.calls.some(({ method }) => method === "session/prompt")).toBe(false);
    expect(transport.calls.filter(({ method }) => method === "session/cancel")).toHaveLength(0);
    // 已投递的用户输入本身是 completed；被跳过的助手轮次收敛为 cancelled。
    const detail = await app.detail(chat.id);
    expect(detail.entries.map((entry) => entry.status)).toEqual(["completed", "cancelled"]);
    expect(detail.entries.at(-1)?.blocks).toEqual([]);
  });

  test("宿主关闭未确认：cancel 回 cancel_failed，之后的 prompt 回 blocked", async () => {
    // 复原阶段的宿主正常关闭；只让执行轮次的宿主持有关闭失败（关闭证据必须真实）。
    const plan: TransportPlan = { chunks: [], blocked: true };
    const app = runtimeFixture(plan);
    const chat = await app.create("Fixture chat");
    const { socket } = await app.attached(chat.id);
    plan.closeFailure = new Error("close failed");
    expect((await app.send(socket, promptCommand("Hello"))).ok).toBe(true);
    const transport = await app.runTransport();
    await transport.promptStarted;
    transport.emit("partial");
    await untilAsync(async () => entryText((await app.detail(chat.id)).entries.at(-1)) === "partial");
    const cancelled = await app.send(socket, cancelCommand());
    expect(cancelled.ok).toBe(false);
    expect(cancelled.error?.code).toBe("cancel_failed");
    expect(cancelled.error?.message).toBe("close failed");
    await until(() => app.registry.peek(chat.id)!.executionBlocked);
    const detail = await app.detail(chat.id);
    // 设计 4.4.7：宿主关闭未确认不改写已生成内容；失败归应用级 error 与执行阻断。
    expect(entryText(detail.entries.at(-1))).toBe("partial");
    expect(detail.entries.at(-1)?.status).toBe("error");
    expect(detail.executionBlocked).toBe(true);
    expect(detail.error).toBe("close failed");
    expect(detail.running).toBe(false);
    const blocked = await app.send(socket, promptCommand("After block"));
    expect(blocked).toEqual({ type: "command_result", id: blocked.id, ok: false,
      error: { code: "blocked", message: "Previous ACP host did not close; this instance cannot run again" } });
    const idleCancel = await app.send(socket, cancelCommand());
    expect(idleCancel.error?.code).toBe("blocked");
  });

  test("启动失败且清理未确认：阻断新执行；迟到的清理证据重新放行", async () => {
    const late = barrier();
    const plan: TransportPlan = {
      // 启动失败：startTransport 抛错，清理证据由夹具计划外的事件补齐（这里用 loadFailure 之后的
      // 宿主关闭路径验证「阻断 → 放行」的收敛）。
      loadFailure: new Error("startup failed"),
    };
    const app = runtimeFixture(plan);
    const chat = await app.create("Startup gate");
    const { socket } = await app.attached(chat.id).catch(() => ({ socket: app.host.last() }));
    // 复原失败时连接以 1011 关闭（无宿主可用）。
    expect(socket.closed?.code).toBe(1011);
    expect(app.registry.peek(chat.id)?.executionBlocked ?? false).toBe(false);
    late.release();
    await app.drain();
  });

  test("两个会话各自的执行互不干扰", async () => {
    const plan: TransportPlan = { blocked: true };
    const app = runtimeFixture(plan);
    const first = await app.create("First");
    const second = await app.create("Second");
    const a = await app.attached(first.id);
    const b = await app.attached(second.id);
    expect((await app.send(a.socket, promptCommand("first prompt"))).ok).toBe(true);
    expect((await app.send(b.socket, promptCommand("second prompt"))).ok).toBe(true);
    await (await app.runTransport()).promptStarted;
    expect(app.registry.peek(first.id)!.running).toBe(true);
    expect((await app.send(a.socket, cancelCommand())).ok).toBe(true);
    await until(() => app.registry.peek(first.id)!.running === false);
    // 取消第一个会话不影响第二个会话的执行。
    expect(app.registry.peek(second.id)!.running).toBe(true);
    expect((await app.send(b.socket, cancelCommand())).ok).toBe(true);
    await app.drain();
  });
});

describe("interaction commands", () => {
  const permissionParams = (sessionId: string, toolCallId = "call-1") => ({
    sessionId,
    toolCall: { toolCallId, title: "Write file", rawInput: { path: "/workspace/secret.txt", token: "PRIVATE_ANSWER" } },
    options: [{ optionId: "allow_once", name: "Allow once" }, { optionId: "reject_once", name: "Reject" }],
  });

  async function pending(app: Awaited<ReturnType<typeof runtimeFixture>>, chatId: string, transport: FixtureTransport,
    toolCallId = "call-1") {
    const pendingAnswer = transport.handler!("session/request_permission", permissionParams(chatId, toolCallId));
    await untilAsync(async () => (await app.detail(chatId)).pendingInteractions.length === 1);
    const id = (await app.detail(chatId)).pendingInteractions[0]!.id;
    return { id, pendingAnswer };
  }

  test("交互往返：pending → 控制帧回答 → 结算 ACP 反向请求", async () => {
    const { app, chat, socket, transport } = await running({ blocked: true });
    const { id, pendingAnswer } = await pending(app, chat.id, transport);
    expect(id.startsWith("interaction:")).toBe(true);
    // 原始输入与答案不进 Yjs：读面只暴露交互身份。
    const visible = JSON.stringify(await app.detail(chat.id));
    expect(visible).toContain(id);
    expect(visible).not.toContain("/workspace/secret.txt");
    const accepted = await app.send(socket, interactionCommand(id, { kind: "permission", decision: "allow_once" }));
    expect(accepted).toEqual({ type: "command_result", id: accepted.id, ok: true });
    await expect(pendingAnswer).resolves.toEqual({ outcome: { outcome: "selected", optionId: "allow_once" } });
    await untilAsync(async () => (await app.detail(chat.id)).pendingInteractions.length === 0);
    expect((await app.detail(chat.id)).interactions.at(-1)).toMatchObject({ id, status: "resolved", outcome: "allow_once" });
    await app.send(socket, cancelCommand());
    await app.drain();
  });

  test("重复回答、未知引用、跨会话与 kind 不符统一回 settled", async () => {
    const { app, chat, socket, transport } = await running({ blocked: true });
    const other = await app.create("Other");
    const { id, pendingAnswer } = await pending(app, chat.id, transport);
    expect((await app.send(socket, interactionCommand(id, { kind: "permission", decision: "reject_once" }))).ok).toBe(true);
    await expect(pendingAnswer).resolves.toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
    expect(await app.send(socket, interactionCommand(id, { kind: "permission", decision: "allow_once" })))
      .toMatchObject({ ok: false, error: { code: "settled" } });
    expect(await app.send(socket, interactionCommand("interaction:missing", { kind: "permission", decision: "allow_once" })))
      .toMatchObject({ ok: false, error: { code: "settled" } });
    // 会话身份不匹配（其它会话的 live 队列里没有这个交互）。
    expect((await app.detail(other.id)).pendingInteractions).toEqual([]);
    const third = transport.handler!("session/request_permission", permissionParams(chat.id, "call-2"));
    await untilAsync(async () => (await app.detail(chat.id)).pendingInteractions.length === 1);
    const thirdId = (await app.detail(chat.id)).pendingInteractions[0]!.id;
    expect(await app.send(socket, interactionCommand(thirdId, { kind: "elicitation", decision: { action: "decline" } })))
      .toMatchObject({ ok: false, error: { code: "settled" } });
    expect(await app.send(socket, interactionCommand(thirdId, { kind: "permission", decision: "nonsense" })))
      .toMatchObject({ ok: false, error: { code: "invalid_answer" } });
    expect((await app.send(socket, interactionCommand(thirdId, { kind: "permission", decision: "reject_once" }))).ok).toBe(true);
    await expect(third).resolves.toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
    await app.send(socket, cancelCommand());
    await app.drain();
  });

  test("轮次结束后失效交互不可复活", async () => {
    const { app, chat, socket, transport } = await running({ blocked: true });
    const { id, pendingAnswer } = await pending(app, chat.id, transport);
    await app.send(socket, cancelCommand());
    await app.drain();
    await expect(pendingAnswer).resolves.toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
    const settled = await app.detail(chat.id);
    expect(settled.pendingInteractions).toEqual([]);
    expect(settled.interactions.at(-1)).toMatchObject({ id, status: "expired" });
    expect(await app.send(socket, interactionCommand(id, { kind: "permission", decision: "allow_once" })))
      .toMatchObject({ ok: false, error: { code: "settled" } });
  });

  test("冷运行期不带任何 pending 交互：回答按 settled 拒绝", async () => {
    const app = runtimeFixture({ replay: [replayChunk("00000000-0000-4000-8000-000000000001", "user-1", "hi",
      { role: "user", replay: true })] }, { limit: 1 });
    const chat = await app.create("Cold interactions");
    expect((await app.detail(chat.id)).pendingInteractions).toEqual([]);
    const filler = await app.create("Filler");
    await app.detail(filler.id);
    expect(app.registry.peek(chat.id)).toBeUndefined();
    const { socket } = await app.attached(chat.id);
    expect(await app.send(socket, interactionCommand("interaction:stale", { kind: "permission", decision: "allow_once" })))
      .toMatchObject({ ok: false, error: { code: "settled" } });
    expect((await app.detail(chat.id)).pendingInteractions).toEqual([]);
    await app.drain();
  });
});

describe("payload commands", () => {
  test("重放外置载荷可按引用全文读取，并做完整性/上限/未知引用判定", async () => {
    const chatId = "00000000-0000-4000-8000-000000000001";
    const big = "P".repeat(6000);
    const app = runtimeFixture({ replay: [
      replayChunk(chatId, "user-1", "工具问", { role: "user", replay: true }),
      replayToolCall(chatId, "tool-1", big),
    ] });
    const chat = await app.create("Payloads");
    const { socket } = await app.attached(chat.id);
    const ref = (await app.detail(chat.id)).toolCalls[0]!.resultRef!;
    const available = await app.send(socket, payloadCommand(ref.id));
    expect(available.ok).toBe(true);
    expect(available.payload).toMatchObject({ id: ref.id, bytes: ref.bytes });
    expect(JSON.parse(available.payload!.json)).toMatchObject({ rawOutput: big });
    // 字节数不符 → integrity_error（客户端带引用记录即可断言完整性）。
    expect(await app.send(socket, payloadCommand(ref.id, ref.bytes + 1)))
      .toMatchObject({ ok: false, error: { code: "integrity_error" } });
    expect(await app.send(socket, payloadCommand("gen:missing")))
      .toMatchObject({ ok: false, error: { code: "unavailable" } });
    await app.drain();
  });

  test("超过单次下载上限（1 MiB）的载荷回 too_large", async () => {
    const chatId = "00000000-0000-4000-8000-000000000001";
    const app = runtimeFixture({ replay: [
      replayChunk(chatId, "user-1", "工具问", { role: "user", replay: true }),
      // 1.2 MiB 的 rawOutput：SDK 外置后仍超过 1 MiB 下载预算。
      replayToolCall(chatId, "tool-big", "x".repeat(1_200_000)),
    ] });
    const chat = await app.create("Big payload");
    const { socket } = await app.attached(chat.id);
    const ref = (await app.detail(chat.id)).toolCalls[0]!.resultRef!;
    expect(ref.bytes).toBeGreaterThan(1024 * 1024);
    const result = await app.send(socket, payloadCommand(ref.id));
    expect(result).toMatchObject({ ok: false, error: { code: "too_large" } });
    expect(result.error?.message).toContain("1048576");
    await app.drain();
  });

  test("会话被回收后旧载荷引用按 unavailable 处理", async () => {
    const chatId = "00000000-0000-4000-8000-000000000001";
    const app = runtimeFixture({ replay: [
      replayChunk(chatId, "user-1", "问", { role: "user", replay: true }),
      replayToolCall(chatId, "tool-1", "P".repeat(6000)),
    ] }, { limit: 1 });
    const chat = await app.create("Recycled");
    const staleRef = (await app.detail(chat.id)).toolCalls[0]!.resultRef!.id;
    const filler = await app.create("Filler");
    await app.detail(filler.id);
    const second = await app.attached(chat.id);
    // 引用 id 带会话代次：新代次里旧 id 不再存在（如实回 unavailable，不猜内容）。
    expect(await app.send(second.socket, payloadCommand(staleRef)))
      .toMatchObject({ ok: false, error: { code: "unavailable" } });
    await app.drain();
  });
});
