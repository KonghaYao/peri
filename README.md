<div align="center">

# Peri Code

**A Rust-built coding agent — fast, lean, Claude Code compatible, any LLM.**

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/Built%20with-Rust%20🦀-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20Windows-blue.svg)](#install)
[![Models](https://img.shields.io/badge/LLMs-Anthropic%20%7C%20DeepSeek%20%7C%20GLM%20%7C%20Qwen-green.svg)](#why-peri)
[![Stars](https://img.shields.io/github/stars/konghayao/peri?style=social)](https://github.com/konghayao/peri)

</div>

One **13 MB binary**, **~50 MB of RAM**, **98% cache hits** — bring any API key (DeepSeek, GLM, Qwen, Anthropic) and switch on the fly. Your Claude Code config works today: skills, hooks, MCP, plugins, sub-agents. **Zero migration, zero lock-in.**

We believe the agent pattern is proven — planning, tool use, context management, delegation. What's missing is a harness that makes this complexity feel simple: fast startup, any provider, every surface. So we rebuilt it from scratch in Rust, with ACP at the core. We are the best like Claude Code. The foundation every agent deserves. Three things we bet on:

## Why Perihelion

### ⚡ Perf Care

The agent that respects your machine — not the one that borrows it.

- 🦀 **Rust, not Node.js** — 13 MB binary, ~50 MB RAM. Starts instantly, stays out of your way
- ⚡ **95–99% cache hit rate** — Frozen system prompt never recomputes. Tokens you don't pay for
- 🗜️ **Auto Compact** — Hours-long sessions stay lean automatically. Micro at 70%, full at 85% budget

### 🧠 Harness Design

Built for agents that plan, delegate, and finish — not just reply.

- 🤖 **7 Sub-agents + Fork Mode** — coder, explorer, plan, code-reviewer, web-researcher, verification, general-purpose. Fork clones context for deep follow-up. All run in background
- 🔄 **Ultracode Workflow** — Split one task into N agents, merge results. Pipeline, parallel, or sequential — one command
- 🎯 **Goal Tracking** — Declare a goal, the agent keeps going across turns. No babysitting
- 🔍 **Deferred Tool Search** — 12 core tools visible, the rest on demand. Lean prompt, hot cache, cheap tokens
- 🌐 **Any LLM, no lock-in** — Anthropic, OpenAI, DeepSeek, GLM, Qwen. Swap mid-session, bring your own key
- 🔌 **Claude Code compatible** — Skills, hooks, MCP, plugins. Point at your config and it just works

### 🖥️ TUI & Ecosystem

Every surface you need, everywhere you work.

- 🪟 **macOS · Linux · Windows** — One binary. Native ConPTY, true color, cross-platform spawn
- 📝 **Streaming Markdown** — Code blocks, tables, diffs render as the agent types. Read while it writes
- 📡 **Channel Support** — WeChat, Slack, Feishu. Reply in-thread, terminal stays synced
- 🌐 **Web Terminal** (`peri web`) — Browser shell, one command. xterm.js + split panes
- 🔧 **LSP + Langfuse** — Code intelligence and per-turn tracing out of the box

---

## Architecture

Perihelion is not just a TUI. It's a layered platform where the **agent core** is decoupled from the **frontend** via the [Agent Client Protocol](https://agentclientprotocol.com). The same core powers three entry points:

```mermaid
graph TD
    TUI["peri-tui<br/>Terminal (ratatui-kit)"]
    IDE["Zed / JetBrains<br/>IDE (ACP client)"]
    STDIO["Stdio<br/>Headless / CI / Cloud"]

    TUI -->|MpscTransport| ACP
    IDE -->|ACP Stdio| ACP
    STDIO -->|ACP Stdio| ACP

    ACP["peri-acp — ACP Server<br/>session · executor · prompt · commands"]

    ACP --> AGENT["peri-agent<br/>ReAct loop · LLM adapter · tools · SQLite storage"]
    ACP --> MW["peri-middlewares<br/>20 middlewares: FS · HITL · SubAgent · Skills · MCP · Hooks · Compact · Goal · Workflow"]
    ACP --> LSP["peri-lsp<br/>LSP client"]

    AGENT -.->|telemetry| LF["langfuse-client"]
    MW -.->|renders with| WIDGETS["peri-widgets<br/>Markdown · code blocks · tables"]

    TUI --> THEME["peri-theme<br/>Dark/Light · palette"]
    ACP --> WORKFLOW["peri-workflow<br/>Multi-agent pipelines"]
    TUI --> E2E["e2e<br/>tmux black-box tests"]
```

**Crate topology**: `peri-tui` → `peri-acp` → `peri-agent` / `peri-middlewares` · `peri-widgets` · `langfuse-client` · `peri-lsp` · `peri-web-pty` · `peri-acp-types` · `peri-workflow` · `peri-theme`

**One core, three frontends.** Terminal users get `peri-tui`. IDE users connect via ACP (Zed today, more to come). Headless / CI / cloud scenarios use the Stdio transport. Change the agent logic once — every frontend benefits.

---

## Install

Binaries available for macOS (x86_64 / Apple Silicon), Linux (x86_64 / aarch64 / riscv64), and Windows (x86_64).

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.sh | bash

```

```bash
# Windows (PowerShell)
irm https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.ps1 | iex
```

```bash
# start peri
peri

# self-update
peri update
```

First launch guides you through model and API key configuration — no config file editing required.

### CLI 全局参数（路径重定向）

- `--config-file <path>`（别名 `--configFile`）— 重定向全局配置文件（默认 `~/.peri/settings.json`），TUI / `-p` print / `peri acp` 三路径的配置读取与保存均跟随；相对路径按启动时 cwd 解析。
- `--db-path <path>`（别名 `--dbPath`）— 重定向 SQLite 会话数据库（默认 `~/.peri/threads/threads.db`）；显式指定路径打开失败时直接报错（不 fallback 临时目录），TUI/print/acp 均以非零码退出。
- `--settings <file|json>` 语义不同：仅注入 env 字段（接受 JSON 字符串），不改变配置读写路径；`-p` 模式下 `--settings` 全权替换配置加载，`--config-file` 仍负责 env 注入与保存目标。

---

## Built by AI, Published by Human

Perihelion's code is 99% AI-generated, primarily by DeepSeek and GLM-5.2. The development workflow is a closed loop the agent drives itself:

| When you... | The loop kicks off |
|---|---|
| **Find a bug or tech debt** | `auto-issue-fixer` → `systematic-debugging` → `writing-plans` → `subagent-driven-development` → `auto-issue-fixer`（归档）→ update `CLAUDE.md` |
| **Want a new feature** | `grill-me` → `writing-plans` → `subagent-driven-development` |
| **Codebase getting messy** | `slop-cleaner` → `improve-codebase-architecture` → `writing-plans` → `subagent-driven-development` |

Each fix that reveals a non-obvious constraint gets written back into `CLAUDE.md` as a **TRAP** — a hard rule the agent follows on every subsequent iteration. The dozens of TRAPs in the repo weren't authored by humans; they were extracted by the agent at the scene of each bug. That's how quality compounds without human code review.

→ Read the full story: [Nobody Coding](docs/blogs/ai-coding-paradigm/nobody-coding.md)

---

## Acknowledgments

- [Claude Code Best](https://github.com/claude-code-best/claude-code) — community support and feedback
- [Superpowers](https://github.com/obra/superpowers) & [Matt Pocock's Skills](https://github.com/mattpocock/skills) — the skill suites that drive Perihelion's AI engineering workflow
- [ACP](https://agentclientprotocol.com) — open protocol for agent-IDE communication
- [rmcp](https://github.com/anthropics/rmcp) — Rust MCP client library
- [ratatui-kit](https://github.com/KonghaYao/ratatui-kit) — React-style component framework powering the entire TUI (components, hooks, state atoms, routing, the `element!` macro)
- [Ratatui](https://ratatui.rs) — terminal rendering backend
- [Tokio](https://tokio.rs)
- [Langfuse](https://langfuse.com) — LLM observability
- [Zed](https://zed.dev) — first ACP-compatible IDE

## License

Apache 2.0
