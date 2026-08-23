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
- **Middleware:** `true` keeps the middleware enabled; `false` removes the middleware, including its tools, hooks, and prompt contributions.
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

### Middleware controls

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
| `FilesystemMiddleware` | `false` | Disables direct filesystem tools supplied by Peri. |
| `GitAttributionMiddleware` | `false` | Disables automatic Git attribution instructions and behavior. |
| `TerminalMiddleware` | `false` | Disables terminal and shell execution tools supplied by Peri. |
| `WebMiddleware` | `false` | Disables web search and web fetch tools. |
| `TodoMiddleware` | `false` | Disables the todo-list tool and its task-tracking behavior. |
| `CronMiddleware` | `false` | Disables scheduled-task discovery and registration tools. |
| `HookMiddleware` | `false` | Disables configured lifecycle hooks. |
| `PermissionMiddleware` | `false` | Disables tool approval hooks and removes the permission/HITL approval prompt section. |
| `HumanInTheLoopMiddleware` | `false` | Disables the `AskUserQuestion` tool and its user-question guidance. This is separate from tool approval. |
| `SubAgentMiddleware` | `true` | Enables the `Agent` tool, project subagent discovery, and the `11_subagent` prompt section. |
| `McpMiddleware` | `true` | Enables MCP server connections, resources, and MCP-provided tools. Project MCP servers are configured in `.mcp.json`. |
| `WorkflowMiddleware` | `false` | Disables workflow registration and workflow execution tools. |
| `ToolSearch` | `true` | Enables `SearchExtraTools` and `ExecuteExtraTool`, allowing deferred tools such as MCP tools to be discovered and invoked on demand. |
| `ArtifactMiddleware` | `false` | Disables the public artifact upload tool. |
| `LspMiddleware` | `false` | Disables Language Server Protocol tools and diagnostics integration. |
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
