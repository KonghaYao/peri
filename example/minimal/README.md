# Minimal

This example demonstrates how a Peri project directory can take precedence over global defaults and define a minimal agent harness.

- `.peri/settings.json` enables only the custom system prompt, project-defined subagents, MCP, and ToolSearch.
- `.peri/meta/*.md` replaces the base prompt, language, persona, and subagent sections.
- `.claude/agents/minimal-helper.md` defines a project-local subagent with no tool access.
- `.mcp.json` registers only this project's `minimal-mcpp` server.

Configuration and prompt content are frozen when a session is created. Start a new session after changing them.

## MetaHarness configuration

All entries below are configured under `config.meta_harness` in `.peri/settings.json`.

The Boolean value has different meanings depending on the field category:

- **Prompt section:** `true` replaces the built-in section with `.peri/meta/<field>.md`; `false` keeps the built-in section.
- **Middleware:** `true` keeps the middleware enabled; `false` removes the middleware, including its tools, hooks, and prompt contributions. Keys here are chain slot names (the value returned by the middleware's `name()`) — except the builtin instance policy keys listed below.
- **Builtin instance policy key:** `false` closes that built-in MCP instance's tool face for this session only. The instance, its handler, its state (host LSP pool / cron scheduler and its 1s tick) and its readiness are **kept** — a policy close is not a physical shutdown.
- **Policy:** the value directly enables or disables the named policy.

### Prompt section overrides

| Field | Current value | Meaning |
| --- | ---: | --- |
| `01_intro` | `true` | Replaces the built-in role introduction with `.peri/meta/01_intro.md`. |
| `02_system` | `true` | Replaces the built-in system-level instructions with `.peri/meta/02_system.md`. |
| `03_doing_tasks` | `true` | Replaces the built-in task execution guidance with `.peri/meta/03_doing_tasks.md`. |
| `04_actions` | `true` | Replaces the built-in action safety and scope guidance with `.peri/meta/04_actions.md`. |
| `05_using_tools` | `true` | Replaces the built-in tool usage rules with `.peri/meta/05_using_tools.md`. |
| `06_tone_style` | `true` | Replaces the built-in response tone and style rules with `.peri/meta/06_tone_style.md`. |
| `07_runtime` | `true` | Replaces the generated runtime context section with `.peri/meta/07_runtime.md`. |
| `11_subagent` | `true` | Replaces the SubAgent usage instructions with `.peri/meta/11_subagent.md`. This section is present only while `SubAgentMiddleware` is enabled. |
| `persona` | `true` | Replaces the generated persona section with `.peri/meta/persona.md`. |
| `language` | `true` | Replaces the generated language section with `.peri/meta/language.md`. |

### Middleware and builtin instance controls

**Note**: `WebMiddleware` / `ArtifactMiddleware` / `CronMiddleware` / `LspMiddleware` are no
longer middleware slots, and `WorkspaceMiddleware` is the fifth key of the same kind (it
replaces the removed `FilesystemMiddleware` / `TerminalMiddleware` slot names). Web, artifact,
cron, LSP and file/shell capabilities are provided by the built-in in-process MCP instances
(`web` / `artifact` / `cron` / `lsp` / `workspace`); these five keys are their close keys
(`BUILTIN_INSTANCE_POLICY_KEYS`), so the rows below stay valid, and old configs using the four
former slot names are still recognized.

The one name that looks similar but is **not** an instance key is `LspSyncMiddleware`: it is the
chain slot name of the document-sync middleware. `LspMiddleware: false` (builtin `lsp` instance
key) closes the `mcp__lsp__LSP` tool face **and** the document-sync target; `LspSyncMiddleware:
false` stops the sync only and leaves `mcp__lsp__LSP` visible. Neither destroys the host LSP pool.

`cron` and `lsp` tools are always **deferred** (`mcp__cron__cron_register` /
`mcp__cron__cron_list` / `mcp__cron__cron_remove` / `mcp__lsp__LSP`): the model reaches them
through `SearchExtraTools` → `ExecuteExtraTool`, so closing these two instances only shrinks the
deferred catalog and its search results.

| Field | Current value | Meaning |
| --- | ---: | --- |
| `DefaultSystemPromptMiddleware` | `true` | Enables the middleware that owns the base system prompt sections, including `01_intro` through `07_runtime` and `persona`. |
| `LangMiddleware` | `true` | Enables language prompt handling and the `language` section. |
| `AgentsMdMiddleware` | `false` | Disables loading project instruction files such as `AGENTS.md` and `CLAUDE.md` into the agent prompt. |
| `AgentDefineMiddleware` | `true` | Enables discovery of project-defined agents from `.claude/agents/`. |
| `PluginMiddleware` | `false` | Disables plugin discovery and plugin-provided commands, agents, skills, hooks, and MCP configuration. |
| `SkillsMiddleware` | `false` | Disables skill discovery and skill-related tools and prompt content. |
| `SkillPreloadMiddleware` | `false` | Disables automatic preloading of selected skills into the session. |
| `AtMentionMiddleware` | `false` | Disables `@`-mention processing and related context injection. |
| `ImageMiddleware` | `false` | Disables image attachment handling and image-related prompt contributions. |
| `WorkspaceMiddleware` | `false` | Closes the built-in `workspace` instance's tool face (`mcp__workspace__Read` / `mcp__workspace__Write` / `mcp__workspace__Edit` / `mcp__workspace__Glob` / `mcp__workspace__Grep` / `mcp__workspace__folder_operations` / `mcp__workspace__Bash`, direct tools). These seven are the only filesystem and shell tools on the chain; external MCP servers and the subagent `WriteSandbox` tool are not affected by this key. |
| `GitAttributionMiddleware` | `false` | Disables automatic Git attribution instructions and behavior. |
| `WebMiddleware` | `false` | Closes the built-in `web` instance's tool face (`mcp__web__WebSearch` / `mcp__web__WebFetch`, direct tools). |
| `TodoMiddleware` | `false` | Disables the todo-list tool and its task-tracking behavior. |
| `CronMiddleware` | `false` | Closes the built-in `cron` instance's tool face (`mcp__cron__cron_register` / `mcp__cron__cron_list` / `mcp__cron__cron_remove`, deferred). The instance, its scheduler and its 1s tick keep running. |
| `HookMiddleware` | `false` | Disables configured lifecycle hooks. |
| `PermissionMiddleware` | `false` | Disables tool approval hooks and removes the permission/HITL approval prompt section. |
| `HumanInTheLoopMiddleware` | `false` | Disables the `AskUserQuestion` tool and its user-question guidance. This is separate from tool approval. |
| `SubAgentMiddleware` | `true` | Enables the `Agent` tool, project subagent discovery, and the `11_subagent` prompt section. |
| `McpMiddleware` | `true` | Enables MCP server connections, resources, and MCP-provided tools. Project MCP servers are configured in `.mcp.json`. |
| `WorkflowMiddleware` | `false` | Disables workflow registration and workflow execution tools. |
| `ToolSearch` | `true` | Enables `SearchExtraTools` and `ExecuteExtraTool`, allowing deferred tools such as MCP tools to be discovered and invoked on demand. |
| `ArtifactMiddleware` | `false` | Closes the built-in `artifact` instance's tool face (`mcp__artifact__artifact`, a direct tool). |
| `LspMiddleware` | `false` | Closes the built-in `lsp` instance's tool face (`mcp__lsp__LSP`, deferred) **and** the document-sync target: no file is read and no `didChange` / `didSave` is sent. The host LSP pool, the instance and readiness are kept. Use the middleware slot name `LspSyncMiddleware: false` instead if you only want to stop the sync. |
| `GoalMiddleware` | `false` | Disables goal-management prompt content and tools. |

### Policy controls

| Field | Current value | Meaning |
| --- | ---: | --- |
| `BuiltInSubagents` | `false` | Prevents fallback to Peri's compile-time built-in subagent definitions. Only project-defined agents, such as `.claude/agents/minimal-helper.md`, are available. |

## Install dependencies

```bash
bun install
```

## Run the MCP server

```bash
bun run index.ts
```

This project was created with `bun init` using Bun v1.4.0. [Bun](https://bun.com) is an all-in-one JavaScript runtime.
