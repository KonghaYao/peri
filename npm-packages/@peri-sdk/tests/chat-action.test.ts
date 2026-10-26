import { expect, test } from "bun:test";
import type { InteractionAnswer } from "../src/contracts/interaction";
import {
    ChatUserAction,
    parseChatUserAction,
    type ChatUserAction as ChatUserActionValue,
} from "../src/chat/action";
import { ChatActionValidationError } from "../src/chat/errors";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function roundTrip(action: ChatUserActionValue): ChatUserActionValue {
    return parseChatUserAction(JSON.parse(JSON.stringify(action)));
}

function captureValidation(fn: () => unknown): ChatActionValidationError {
    try {
        fn();
    } catch (error) {
        if (error instanceof ChatActionValidationError) return error;
        throw error;
    }
    throw new Error("expected ChatActionValidationError");
}

test("八类工厂生成对应 type、requestId 与纯数据 payload", () => {
    const create = ChatUserAction.createNewSession({ title: "新会话", requestId: "r-create" });
    const change = ChatUserAction.changeSessionTo("s1", { requestId: "r-change" });
    const send = ChatUserAction.send({ text: "hello", requestId: "r-send" });
    const force = ChatUserAction.forceSend("i1", { requestId: "r-force" });
    const takeBack = ChatUserAction.takeBack("i1", { requestId: "r-back" });
    const history = ChatUserAction.loadEarlierHistory({ limit: 20, requestId: "r-history" });
    const cancel = ChatUserAction.cancel({ requestId: "r-cancel" });
    const answer = ChatUserAction.answer({
        interactionId: "int-1",
        answer: { kind: "permission", decision: "allow_once" },
        requestId: "r-answer",
    });

    expect(create).toEqual({ type: "create_new_session", requestId: "r-create", payload: { title: "新会话" } });
    expect(change).toEqual({ type: "change_session", requestId: "r-change", payload: { sessionId: "s1" } });
    expect(send).toEqual({ type: "send", requestId: "r-send", payload: { text: "hello" } });
    expect(force).toEqual({ type: "force_send", requestId: "r-force", payload: { inputId: "i1" } });
    expect(takeBack).toEqual({ type: "take_back", requestId: "r-back", payload: { inputId: "i1" } });
    expect(history).toEqual({ type: "load_earlier_history", requestId: "r-history", payload: { limit: 20 } });
    expect(cancel).toEqual({ type: "cancel", requestId: "r-cancel", payload: {} });
    expect(answer).toEqual({
        type: "answer", requestId: "r-answer",
        payload: { interactionId: "int-1", answer: { kind: "permission", decision: "allow_once" } },
    });
});

test("requestId 默认生成，显式值与 wire 往返均保持不变", () => {
    const generated = ChatUserAction.send({ text: "hello" });
    expect(generated.requestId).toMatch(UUID);

    const explicit = ChatUserAction.send({ text: "hello", requestId: "remote:42" });
    expect(explicit.requestId).toBe("remote:42");
    const parsed = roundTrip(explicit);
    expect(parsed.requestId).toBe("remote:42");
    expect(parsed).toEqual(explicit);
});

test("工厂结果、payload 与嵌套 answer 均冻结且可 JSON 序列化", () => {
    const action = ChatUserAction.answer({
        interactionId: "int-1",
        answer: { kind: "elicitation", decision: { action: "accept", content: { name: "Ada" } } },
        requestId: "r-freeze",
    });

    expect(Object.isFrozen(action)).toBe(true);
    expect(Object.isFrozen(action.payload)).toBe(true);
    expect(Object.isFrozen(action.payload.answer)).toBe(true);
    expect(JSON.parse(JSON.stringify(action))).toEqual(action);
});

test("解析接受普通对象与无原型对象，不依赖 instanceof", () => {
    const parsed = parseChatUserAction({ type: "send", requestId: "wire-1", payload: { text: "hello" } });
    expect(parsed).toEqual({ type: "send", requestId: "wire-1", payload: { text: "hello" } });
    expect(Object.getPrototypeOf(parsed)).toBe(Object.prototype);
    expect(Object.isFrozen(parsed)).toBe(true);

    const bare = Object.assign(Object.create(null), {
        type: "cancel", requestId: "wire-2", payload: {},
    });
    expect(parseChatUserAction(bare)).toEqual({ type: "cancel", requestId: "wire-2", payload: {} });
});

test("send 文本必须非空、非纯空白且不超过 32768 字符", () => {
    const atLimit = "x".repeat(32_768);
    expect(parseChatUserAction({ type: "send", requestId: "r", payload: { text: atLimit } })).toEqual({
        type: "send", requestId: "r", payload: { text: atLimit },
    });

    const tooLong = captureValidation(() =>
        parseChatUserAction({ type: "send", requestId: "r-long", payload: { text: "x".repeat(32_769) } }));
    expect(tooLong.requestId).toBe("r-long");

    for (const text of ["", "   ", "\n\t", "\u00a0"]) {
        const blank = captureValidation(() =>
            parseChatUserAction({ type: "send", requestId: "r-blank", payload: { text } }));
        expect(blank.requestId).toBe("r-blank");
        expect(blank.code).toBe("invalid_action");
        expect(blank.disposition).toBe("rejected");
    }

    const blankFactory = captureValidation(() =>
        ChatUserAction.send({ text: "   ", requestId: "r-blank-factory" }));
    expect(blankFactory.requestId).toBe("r-blank-factory");
    expect(blankFactory.code).toBe("invalid_action");

    const padded = "  hello  ";
    expect(parseChatUserAction({ type: "send", requestId: "r-padded", payload: { text: padded } })).toEqual({
        type: "send", requestId: "r-padded", payload: { text: padded },
    });
    expect(ChatUserAction.send({ text: padded, requestId: "r-padded-factory" }).payload.text).toBe(padded);
});

test("create_new_session 的 title 可选且必须是字符串", () => {
    expect(parseChatUserAction({ type: "create_new_session", requestId: "r", payload: {} })).toEqual({
        type: "create_new_session", requestId: "r", payload: {},
    });
    expect(parseChatUserAction({ type: "create_new_session", requestId: "r", payload: { title: "标题" } })).toEqual({
        type: "create_new_session", requestId: "r", payload: { title: "标题" },
    });
    expect(() =>
        parseChatUserAction({ type: "create_new_session", requestId: "r", payload: { title: 1 } }))
        .toThrow(ChatActionValidationError);
});

test("change_session 要求非空 sessionId", () => {
    expect(parseChatUserAction({ type: "change_session", requestId: "r", payload: { sessionId: "s1" } })).toEqual({
        type: "change_session", requestId: "r", payload: { sessionId: "s1" },
    });
    expect(() => parseChatUserAction({ type: "change_session", requestId: "r", payload: { sessionId: "" } }))
        .toThrow(ChatActionValidationError);
    expect(() => parseChatUserAction({ type: "change_session", requestId: "r", payload: {} }))
        .toThrow(ChatActionValidationError);
});

test("force_send 与 take_back 要求非空 inputId", () => {
    for (const type of ["force_send", "take_back"] as const) {
        expect(parseChatUserAction({ type, requestId: "r", payload: { inputId: "i1" } })).toEqual({
            type, requestId: "r", payload: { inputId: "i1" },
        });
        expect(() => parseChatUserAction({ type, requestId: "r", payload: { inputId: "" } }))
            .toThrow(ChatActionValidationError);
    }
});

test("load_earlier_history 的 limit 必须为正整数或省略", () => {
    expect(parseChatUserAction({ type: "load_earlier_history", requestId: "r", payload: {} })).toEqual({
        type: "load_earlier_history", requestId: "r", payload: {},
    });
    expect(parseChatUserAction({ type: "load_earlier_history", requestId: "r", payload: { limit: 1 } })).toEqual({
        type: "load_earlier_history", requestId: "r", payload: { limit: 1 },
    });
    for (const limit of [0, -1, 1.5, "2"]) {
        expect(() => parseChatUserAction({ type: "load_earlier_history", requestId: "r", payload: { limit } }))
            .toThrow(ChatActionValidationError);
    }
});

test("cancel 只接受空 payload", () => {
    expect(parseChatUserAction({ type: "cancel", requestId: "r", payload: {} })).toEqual({
        type: "cancel", requestId: "r", payload: {},
    });
    expect(() => parseChatUserAction({ type: "cancel", requestId: "r", payload: { force: true } }))
        .toThrow(ChatActionValidationError);
});

test("answer 复用 parseInteractionAnswer 校验并保留公开答案", () => {
    const parsed = parseChatUserAction({
        type: "answer", requestId: "r",
        payload: {
            interactionId: "int-1",
            answer: { kind: "elicitation", decision: { action: "accept", content: { name: "Ada" } } },
        },
    });
    if (parsed.type !== "answer") throw new Error("expected answer action");
    expect(parsed.payload.answer).toEqual({
        kind: "elicitation", decision: { action: "accept", content: { name: "Ada" } },
    });
    expect(Object.isFrozen(parsed.payload.answer)).toBe(true);

    expect(parseChatUserAction({
        type: "answer", requestId: "r-decline",
        payload: { interactionId: "int-2", answer: { kind: "elicitation", decision: { action: "decline" } } },
    })).toEqual({
        type: "answer", requestId: "r-decline",
        payload: { interactionId: "int-2", answer: { kind: "elicitation", decision: { action: "decline" } } },
    });

    for (const answer of [
        { kind: "permission", decision: "allow_always" },
        { kind: "elicitation", decision: { action: "accept" } },
        { decision: { action: "decline" } },
    ]) {
        expect(() => parseChatUserAction({ type: "answer", requestId: "r", payload: { interactionId: "int-3", answer } }))
            .toThrow(ChatActionValidationError);
    }
});

function acceptedAnswer(content: Record<string, unknown>): InteractionAnswer {
    return { kind: "elicitation", decision: { action: "accept", content } };
}

function expectAnswerContentRejected(content: unknown, reason: string): void {
    const factoryError = captureValidation(() => ChatUserAction.answer({
        interactionId: "int-bad",
        answer: acceptedAnswer({ bad: content }),
        requestId: "r-bad-factory",
    }));
    expect(factoryError.code).toBe("invalid_action");
    expect(factoryError.requestId).toBe("r-bad-factory");
    expect(factoryError.message).toContain("payload.answer.decision.content.bad");
    expect(factoryError.message).toContain(reason);

    const parseError = captureValidation(() => parseChatUserAction({
        type: "answer", requestId: "r-bad-parse",
        payload: { interactionId: "int-bad", answer: acceptedAnswer({ bad: content }) },
    }));
    expect(parseError.code).toBe("invalid_action");
    expect(parseError.requestId).toBe("r-bad-parse");
    expect(parseError.message).toContain("payload.answer.decision.content.bad");
    expect(parseError.message).toContain(reason);
}

test("answer content 拒绝非 JSON 值，不静默丢字段也不让 stringify 抛错", () => {
    expectAnswerContentRejected(() => 1, "function is not a JSON value");
    expectAnswerContentRejected(Symbol("s"), "symbol is not a JSON value");
    expectAnswerContentRejected(1n, "bigint is not a JSON value");
    expectAnswerContentRejected(undefined, "undefined is not a JSON value");
    expectAnswerContentRejected(NaN, "NaN is not a finite number");
    expectAnswerContentRejected(Infinity, "Infinity is not a finite number");
    expectAnswerContentRejected(-Infinity, "-Infinity is not a finite number");

    const circular: Record<string, unknown> = {};
    circular.self = circular;
    expectAnswerContentRejected(circular, "circular reference is not JSON data");

    expectAnswerContentRejected(new Date(0), "Date instance is not JSON data");
    expectAnswerContentRejected(new Map([["k", "v"]]), "Map instance is not JSON data");
    expectAnswerContentRejected(new Set(["v"]), "Set instance is not JSON data");
    expectAnswerContentRejected(Promise.resolve(1), "Promise instance is not JSON data");
    class Secret { readonly value = 1; }
    expectAnswerContentRejected(new Secret(), "Secret instance is not JSON data");

    expectAnswerContentRejected([1, () => 2], "function is not a JSON value");
    const sparse = new Array(2);
    sparse[1] = "x";
    expectAnswerContentRejected(sparse, "sparse array holes are not JSON data");
    const extra = [1] as unknown[] & { extra?: number };
    extra.extra = 2;
    expectAnswerContentRejected(extra, "extra array properties are not JSON data");
});

test("answer content 深拷贝、深冻结并保持 JSON 往返无损", () => {
    const nested = { value: 1 };
    const list = [{ value: 2 }];
    const input: InteractionAnswer = {
        kind: "elicitation",
        decision: { action: "accept", content: { nested, list, ok: true, nil: null } },
    };
    const action = ChatUserAction.answer({ interactionId: "int-json", answer: input, requestId: "r-json" });
    if (action.payload.answer.kind !== "elicitation" || action.payload.answer.decision.action !== "accept") {
        throw new Error("expected accepted elicitation");
    }
    const content = action.payload.answer.decision.content as {
        nested: { value: number };
        list: Array<{ value: number }>;
        ok: boolean;
        nil: null;
    };

    expect(Object.isFrozen(action)).toBe(true);
    expect(Object.isFrozen(action.payload)).toBe(true);
    expect(Object.isFrozen(action.payload.answer)).toBe(true);
    expect(Object.isFrozen(content)).toBe(true);
    expect(Object.isFrozen(content.nested)).toBe(true);
    expect(Object.isFrozen(content.list)).toBe(true);
    expect(Object.isFrozen(content.list[0])).toBe(true);

    nested.value = 99;
    list[0].value = 99;
    expect(content.nested.value).toBe(1);
    expect(content.list[0].value).toBe(2);
    expect(JSON.parse(JSON.stringify(action))).toEqual(action);
});

test("非法顶层形状被拒绝", () => {
    for (const raw of [
        null, undefined, [], "send",
        { type: "unknown", requestId: "r", payload: {} },
        { type: "send", payload: { text: "hello" } },
        { type: "send", requestId: "", payload: { text: "hello" } },
        { type: "send", requestId: 1, payload: { text: "hello" } },
        { type: "send", requestId: "r" },
        { type: "send", requestId: "r", payload: [] },
        { type: "send", requestId: "r", payload: "hello" },
    ]) {
        expect(() => parseChatUserAction(raw)).toThrow(ChatActionValidationError);
    }
});

test("工厂同样拒绝非法输入，并保留或生成 requestId", () => {
    const emptySend = captureValidation(() => ChatUserAction.send({ text: "" }));
    expect(emptySend.code).toBe("invalid_action");
    expect(emptySend.disposition).toBe("rejected");
    expect(emptySend.requestId).toMatch(UUID);

    const changing = captureValidation(() => ChatUserAction.changeSessionTo(""));
    expect(changing.code).toBe("invalid_action");

    expect(() => ChatUserAction.loadEarlierHistory({ limit: 0 })).toThrow(ChatActionValidationError);
    expect(() => ChatUserAction.createNewSession({ title: 1 as unknown as string }))
        .toThrow(ChatActionValidationError);
    expect(() => ChatUserAction.answer({
        interactionId: "int-1",
        answer: { kind: "permission", decision: "maybe" } as unknown as never,
    })).toThrow(ChatActionValidationError);

    const preserved = captureValidation(() =>
        ChatUserAction.send({ text: "", requestId: "factory:7" }));
    expect(preserved.requestId).toBe("factory:7");
});

test("八个动作经 JSON wire 往返后语义与 requestId 不变", () => {
    const actions: ChatUserActionValue[] = [
        ChatUserAction.createNewSession({ title: "新会话", requestId: "r1" }),
        ChatUserAction.changeSessionTo("s1", { requestId: "r2" }),
        ChatUserAction.send({ text: "hello", requestId: "r3" }),
        ChatUserAction.forceSend("i1", { requestId: "r4" }),
        ChatUserAction.takeBack("i1", { requestId: "r5" }),
        ChatUserAction.loadEarlierHistory({ requestId: "r6" }),
        ChatUserAction.cancel({ requestId: "r7" }),
        ChatUserAction.answer({
            interactionId: "int-1",
            answer: { kind: "permission", decision: "reject_once" },
            requestId: "r8",
        }),
    ];
    for (const action of actions) expect(roundTrip(action)).toEqual(action);
});
