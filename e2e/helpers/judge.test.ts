import { beforeEach, describe, expect, it, vi } from "vitest";

const mockResponse = vi.hoisted(() => ({ content: "" }));
const mockCreate = vi.hoisted(() => vi.fn());

vi.mock("openai", () => ({
  default: class MockOpenAI {
    chat = {
      completions: {
        create: mockCreate,
      },
    };
  },
}));

import { judge, parseJudgeChecks } from "./judge.js";

describe("judge response validation", () => {
  beforeEach(() => {
    process.env.OPENAI_API_KEY = "test-key";
    mockCreate.mockReset();
    mockCreate.mockImplementation(async () => ({
      choices: [{ message: { content: mockResponse.content } }],
      usage: { prompt_tokens: 1, completion_tokens: 1 },
    }));
  });

  it("rejects an empty checks response", async () => {
    mockResponse.content = JSON.stringify({ checks: [] });

    const result = await judge({
      ansiRaw: "screen",
      criteria: ["expected criterion"],
    });

    expect(result.pass).toBe(false);
    expect(result.checks).toEqual([
      expect.objectContaining({
        criterion: "expected criterion",
        pass: false,
      }),
    ]);
  });

  it("accepts ordered numeric ids without requiring a criterion echo", async () => {
    mockResponse.content = JSON.stringify({
      checks: [
        {
          id: 1,
          criterion: "改写后的检查项",
          pass: true,
          detail: "第一项满足",
        },
        { id: 2, pass: true, detail: "第二项满足" },
      ],
    });

    const result = await judge({
      ansiRaw: "screen",
      criteria: ["第一个原始检查项", "第二个原始检查项"],
    });

    expect(result.pass).toBe(true);
    expect(result.checks).toEqual([
      {
        criterion: "第一个原始检查项",
        pass: true,
        detail: "第一项满足",
      },
      {
        criterion: "第二个原始检查项",
        pass: true,
        detail: "第二项满足",
      },
    ]);

    const request = mockCreate.mock.calls[0]?.[0] as {
      messages: Array<{ content?: string }>;
      response_format: unknown;
    };
    expect(request.response_format).toEqual({ type: "json_object" });
    expect(request.messages[1]?.content).toContain("[id: 1] 第一个原始检查项");
    expect(request.messages[1]?.content).toContain("[id: 2] 第二个原始检查项");
  });

  it("keeps a valid false Judge check blocking", async () => {
    mockResponse.content = JSON.stringify({
      checks: [{ id: 1, pass: false, detail: "屏幕不满足检查项" }],
    });

    const result = await judge({
      ansiRaw: "screen",
      criteria: ["expected criterion"],
    });

    expect(result.pass).toBe(false);
    expect(result.checks).toEqual([
      {
        criterion: "expected criterion",
        pass: false,
        detail: "屏幕不满足检查项",
      },
    ]);
  });

  it("rejects reordered numeric ids", () => {
    const checks = parseJudgeChecks(
      JSON.stringify({
        checks: [
          { id: 2, pass: true, detail: "第二项" },
          { id: 1, pass: true, detail: "第一项" },
        ],
      }),
      ["first criterion", "second criterion"],
    );

    expect(checks).toEqual([
      expect.objectContaining({ criterion: "first criterion", pass: false }),
      expect.objectContaining({ criterion: "second criterion", pass: false }),
    ]);
  });

  it("rejects duplicate numeric ids", () => {
    const checks = parseJudgeChecks(
      JSON.stringify({
        checks: [
          { id: 1, pass: true, detail: "第一项" },
          { id: 1, pass: true, detail: "第二项" },
        ],
      }),
      ["first criterion", "second criterion"],
    );

    expect(checks).toEqual([
      expect.objectContaining({ criterion: "first criterion", pass: false }),
      expect.objectContaining({ criterion: "second criterion", pass: false }),
    ]);
  });

  it("does not expose an invalid Judge response in checks or results", async () => {
    const sensitiveMarker = "sensitive-marker-must-not-leak";
    mockResponse.content = JSON.stringify({
      checks: [],
      note: sensitiveMarker,
    });

    const result = await judge({
      ansiRaw: "screen",
      criteria: ["expected criterion"],
    });

    expect(result.checks[0]?.detail).not.toContain(sensitiveMarker);
    expect(result).not.toHaveProperty("raw_response");
  });

  it.each([
    ["invalid JSON", "{"],
    ["top-level array", JSON.stringify([])],
    ["missing checks", JSON.stringify({})],
    ["wrong check count", JSON.stringify({ checks: [] })],
    [
      "too many checks",
      JSON.stringify({
        checks: [
          { id: 1, pass: true, detail: "第一项" },
          { id: 2, pass: true, detail: "第二项" },
        ],
      }),
    ],
    ["missing id", JSON.stringify({ checks: [{ pass: true, detail: "原因" }] })],
    ["string id", JSON.stringify({ checks: [{ id: "1", pass: true, detail: "原因" }] })],
    ["wrong id", JSON.stringify({ checks: [{ id: 2, pass: true, detail: "原因" }] })],
    ["non-object check", JSON.stringify({ checks: [[]] })],
    ["non-boolean pass", JSON.stringify({ checks: [{ id: 1, pass: "true", detail: "原因" }] })],
    ["missing detail", JSON.stringify({ checks: [{ id: 1, pass: true }] })],
    ["blank detail", JSON.stringify({ checks: [{ id: 1, pass: true, detail: "   " }] })],
    ["non-string detail", JSON.stringify({ checks: [{ id: 1, pass: true, detail: null }] })],
  ])("marks %s as failed", (_name, response) => {
    const checks = parseJudgeChecks(response, ["expected criterion"]);

    expect(checks).toEqual([
      expect.objectContaining({
        criterion: "expected criterion",
        pass: false,
      }),
    ]);
  });
});
