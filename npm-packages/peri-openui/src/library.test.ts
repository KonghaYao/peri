import { describe, expect, test } from "bun:test";
import { createParser } from "@openuidev/react-lang";
import { generateSystemPrompt, type LibrarySpec, type SystemPromptOptions } from "@openuidev/lang-core";
import artifact from "../spec.json";
import { openuiLibrary, openuiPromptOptions } from "./library";

const EXPECTED_COMPONENTS = [
  "Stack",
  "Row",
  "Grid",
  "Spacer",
  "Divider",
  "Text",
  "H1",
  "H2",
  "H3",
  "Code",
  "Link",
  "Pill",
  "Card",
  "Callout",
  "Stat",
  "UsageBar",
  "Gauge",
  "DiffStats",
  "DiffView",
  "ChangedFiles",
  "Sources",
  "TodoList",
  "Table",
  "BarChart",
  "LineChart",
  "PieChart",
  "TextInput",
  "TextArea",
  "Checkbox",
  "Toggle",
  "Select",
  "Button",
] as const;

describe("openui library", () => {
  test("生成产物与 library.tsx 保持同步", () => {
    expect(openuiLibrary.toSpec()).toEqual(artifact.spec);
    expect(openuiPromptOptions).toEqual(artifact.promptOptions as unknown as typeof openuiPromptOptions);
  });

  test("包含 32 个组件且 root 为 Stack", () => {
    const spec = openuiLibrary.toSpec() as ReturnType<typeof openuiLibrary.toSpec> & {
      schema?: { $defs?: Record<string, unknown> };
    };
    expect(Object.keys(spec.components)).toEqual([...EXPECTED_COMPONENTS]);
    expect(spec.root).toBe("Stack");
    expect(spec.schema?.$defs).toBeDefined();
  });

  test("新增组件落在 Containers & data，且不包含 Slider", () => {
    const spec = openuiLibrary.toSpec();
    const containers = spec.componentGroups?.find((group) => group.name === "Containers & data");
    expect(containers?.components).toEqual(
      expect.arrayContaining(["Gauge", "ChangedFiles", "Sources"]),
    );
    expect(spec.components).not.toHaveProperty("Slider");
  });

  test("每个组件都有非空 description", () => {
    for (const name of EXPECTED_COMPONENTS) {
      const description = openuiLibrary.toSpec().components[name]?.description;
      expect(description?.trim().length ?? 0, `${name} 缺少 description`).toBeGreaterThan(0);
    }
  });

  test("每个组件只属于一个分组，且四组覆盖全部组件", () => {
    const spec = openuiLibrary.toSpec();
    const groups = spec.componentGroups ?? [];
    expect(groups).toHaveLength(4);

    const seen = new Map<string, string[]>();
    for (const group of groups) {
      for (const name of group.components) {
        seen.set(name, [...(seen.get(name) ?? []), group.name]);
      }
    }
    for (const name of EXPECTED_COMPONENTS) {
      expect(seen.get(name), `${name} 未出现在任何分组`).toHaveLength(1);
    }
    expect([...seen.keys()].sort()).toEqual([...EXPECTED_COMPONENTS].sort());
  });

  test("组件签名不暴露 style/onClick/ReactNode", () => {
    for (const [name, component] of Object.entries(openuiLibrary.toSpec().components)) {
      expect(component.signature, `${name} 签名泄露宿主 props`).not.toMatch(/style|onClick|ReactNode/);
    }
  });

  test("提示词示例是可解析的完整程序", () => {
    const parser = createParser(openuiLibrary.toJSONSchema(), openuiLibrary.root);
    for (const [index, program] of openuiPromptOptions.examples.entries()) {
      const parsed = parser.parse(program);
      expect(parsed.root, `example ${index + 1} 缺少 root`).toBeTruthy();
      expect(parsed.meta.incomplete, `example ${index + 1} 不完整`).toBe(false);
    }
  });

  test("提示词示例包含 Gauge、ChangedFiles 与 Sources", () => {
    const examples = openuiPromptOptions.examples.join("\n");
    expect(examples).toContain("Gauge(");
    expect(examples).toContain("ChangedFiles(");
    expect(examples).toContain("Sources(");
  });

  test("生成的系统提示词可加载且不含 Cloud/密钥标记", () => {
    const prompt = generateSystemPrompt({
      library: artifact.spec as unknown as LibrarySpec,
      promptOptions: artifact.promptOptions as unknown as SystemPromptOptions,
    });
    console.info(`[openui] generated system prompt: ${prompt.length} chars`);
    expect(prompt.length).toBeGreaterThan(0);
    expect(prompt).not.toContain("]]>openui:config");
    expect(prompt).not.toContain("THESYS_API_KEY");
  });
});
