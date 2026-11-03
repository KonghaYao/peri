import { describe, expect, spyOn, test } from "bun:test";
import { compactFormState, parseOpenUIAction, type OpenUIActionEvent } from "./actions";

const event = (overrides: Partial<OpenUIActionEvent> = {}): OpenUIActionEvent => ({
  type: "continue_conversation",
  params: {},
  humanFriendlyMessage: "继续",
  ...overrides,
});

describe("parseOpenUIAction", () => {
  test("continue_conversation 优先 humanFriendlyMessage", () => {
    const action = parseOpenUIAction(event({ params: { message: "参数消息" } }));
    expect(action).toEqual({ kind: "send", text: "继续" });
  });

  test("humanFriendlyMessage 为空时回退 params.message", () => {
    const action = parseOpenUIAction(event({ humanFriendlyMessage: "  ", params: { message: "参数消息" } }));
    expect(action).toEqual({ kind: "send", text: "参数消息" });
  });

  test("空消息忽略", () => {
    expect(parseOpenUIAction(event({ humanFriendlyMessage: " ", params: {} }))).toEqual({ kind: "none" });
  });

  test("消息上限 8000 字符", () => {
    const action = parseOpenUIAction(event({ humanFriendlyMessage: "x".repeat(9_000) }));
    expect(action.kind).toBe("send");
    if (action.kind === "send") {
      expect(action.text.startsWith("x".repeat(8_000))).toBe(true);
      expect(action.text).toContain("（消息过长，已截断）");
    }
  });

  test("表单值精简后附在消息后", () => {
    const action = parseOpenUIAction(event({
      humanFriendlyMessage: "提交",
      formState: {
        name: { value: "Ada", componentType: "TextInput" },
        ok: { value: true, componentType: "Checkbox" },
        ignored: { value: { nested: 1 }, componentType: "Select" },
      },
    }));
    expect(action).toEqual({
      kind: "send",
      text: '提交\n\n表单数据：{"name":"Ada","ok":true}',
    });
  });

  test("表单数据截断时消息末尾带可见提示", () => {
    const action = parseOpenUIAction(event({
      humanFriendlyMessage: "提交",
      formState: { note: { value: "x".repeat(9_000), componentType: "TextArea" } },
    }));
    expect(action.kind).toBe("send");
    if (action.kind !== "send") return;
    expect(action.text).toContain("\n\n表单数据：");
    expect(action.text.endsWith("\n\n（表单数据过长，已截断）")).toBe(true);
    expect(action.text.length).toBeLessThan(9_000);
  });

  test("open_url 允许 http/https", () => {
    expect(parseOpenUIAction(event({ type: "open_url", params: { url: "https://example.com" } })))
      .toEqual({ kind: "open_url", url: "https://example.com/" });
    expect(parseOpenUIAction(event({ type: "open_url", params: { url: "http://example.com/a?b=1" } })))
      .toEqual({ kind: "open_url", url: "http://example.com/a?b=1" });
  });

  test("open_url 拒绝 javascript/data/vbscript/file 与非法 URL", () => {
    const warn = spyOn(console, "warn").mockImplementation(() => {});
    for (const url of [
      "javascript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "vbscript:msgbox(1)",
      "file:///etc/passwd",
      "not a url",
      "https://user:pass@evil.com/x",
      `https://example.com/${"a".repeat(2_100)}`,
    ]) {
      expect(parseOpenUIAction(event({ type: "open_url", params: { url } }))).toEqual({ kind: "none" });
    }
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  test("未知类型返回 none 并 warn", () => {
    const warn = spyOn(console, "warn").mockImplementation(() => {});
    expect(parseOpenUIAction(event({ type: "custom_action" }))).toEqual({ kind: "none" });
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });
});

describe("compactFormState", () => {
  test("去掉 componentType，只保留 primitive", () => {
    expect(compactFormState({
      name: { value: "Ada", componentType: "TextInput" },
      age: { value: 42, componentType: "TextInput" },
      enabled: { value: false, componentType: "Toggle" },
      bad: { value: Number.NaN },
      object: { value: { nested: true } },
      "$var": "ignored",
    })).toEqual({ name: "Ada", age: 42, enabled: false });
  });

  test("Form 包裹的一层结构被展开", () => {
    expect(compactFormState({
      contact: {
        name: { value: "Ada", componentType: "TextInput" },
        message: { value: "hello", componentType: "TextArea" },
      },
    })).toEqual({ "contact.name": "Ada", "contact.message": "hello" });
  });

  test("空表单返回 undefined", () => {
    expect(compactFormState(undefined)).toBeUndefined();
    expect(compactFormState({})).toBeUndefined();
  });

  test("输出上限 2000 字符，长字符串被截断", () => {
    const compact = compactFormState({ note: { value: "x".repeat(5_000), componentType: "TextArea" } });
    expect(compact).toBeDefined();
    expect(JSON.stringify(compact).length).toBeLessThanOrEqual(2_000);
    expect((compact?.note as string).length).toBeGreaterThan(0);
    expect((compact?.note as string).length).toBeLessThan(5_000);
  });
});
