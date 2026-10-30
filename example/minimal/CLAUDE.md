# Minimal example

## Scope

A standalone Peri project that demonstrates project-level prompt/config overrides and a minimal
stdio MCP server. It is not part of the root Cargo workspace.

## Layout

- `.peri/settings.json` — `config.meta_harness` overrides: prompt sections, middleware close keys,
  builtin instance policy keys, and `BuiltInSubagents: false` to keep only project agents.
  The full key table lives in `README.md`.
- `.peri/meta/*.md` — replacement text for the prompt sections enabled above.
- `.claude/agents/minimal-helper.md` — project subagent with `tools: []`.
- `.mcp.json` — registers `minimal-mcpp` as `bun run src/index.ts`; Peri 从示例根目录启动时按该目录解析相对路径（MCP 配置没有 `cwd` 字段）。
- `src/index.ts` — MCP server built on `@peri-code/mcpp`; exposes the `echo` tool over stdio.

## Commands

```bash
bun install
bun run src/index.ts   # run the MCP server directly; Peri normally starts it from .mcp.json
```

Start Peri from this directory to load these project overrides.

## Invariants

- Configuration and prompt content are frozen when a session is created; start a new session after
  editing them.
- Keep the example dependency-light: Bun runtime, `@modelcontextprotocol/server`, `@peri-code/mcpp`,
  and `zod`. No build step or test suite.
