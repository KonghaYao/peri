<div align="center">

# Peri Code

### One goal. Coordinated agents. Your models.

A native Rust coding agent for macOS, Linux, and Windows.<br>
Bring an Anthropic or OpenAI-compatible endpoint.

[**Get started ↓**](#get-started) · [Documentation](https://konghayao.github.io/peri-cool/) · [Releases](https://github.com/konghayao/peri/releases)

</div>

---

<div align="center">

**HIGH-LEVEL AGENT COORDINATION**

From the whole delivery to the individual tool call.

</div>

<table>
<tr>
<td width="33%" valign="top">
<h3>01 / Ultra-ADLC</h3>
<strong>Coordinate delivery.</strong>
<p>Discovery → design → independent review → implementation → verification.</p>
<p>Decisions, handoffs, and acceptance evidence stay with the project.</p>
<code>/ultra-adlc</code>
</td>
<td width="33%" valign="top">
<h3>02 / Ultracode</h3>
<strong>Orchestrate a team.</strong>
<p>Parallel investigations. Staged execution. Models chosen per agent.</p>
<p>Track workflow progress and resume saved runs.</p>
<code>/ultracode</code>
</td>
<td width="33%" valign="top">
<h3>03 / PTC</h3>
<strong>Compose tool calls.</strong>
<p>Use JavaScript to call, filter, and combine tools.</p>
<p>Keep intermediate data in code; bring concise results back to the model.</p>
<code>/ptc</code>
</td>
</tr>
</table>

```text
/ultracode Review this branch for correctness, performance, and test coverage
in parallel. Compare the findings and return an actionable review.
```

<table>
<tr>
<td colspan="2">
<h3>🔌 MCPP Support</h3>
<strong>Connect tools, expertise, and conversations.</strong>
<p>Remote Skills &amp; Agents · Version-aware discovery caching · Dynamic MCP · Channels</p>
Load capabilities as work evolves. Receive messages and approval responses through compatible Channel servers.
</td>
</tr>
<tr>
<td width="50%" valign="top">
<h3>🛠️ MetaHarness</h3>
<strong>Shape the agent itself.</strong>
<p>Replace prompt sections. Select middleware capabilities. Control built-in agent definitions.</p>
</td>
<td width="50%" valign="top">
<h3>🧩 MCP Apps Support</h3>
<strong>Connect interactive apps.</strong>
<p>App resources and interactions over ACP, with Peri's tool permissions. Your compatible host supplies the UI.</p>
</td>
</tr>
<tr>
<td valign="top">
<h3>📄 Artifacts</h3>
<strong>Give your results a URL.</strong>
<p>Publish reports, charts, and progress pages from HTML or Markdown through a hosting service. Share public pages beyond the terminal.</p>
</td>
<td valign="top">
<h3>⏱️ Goals &amp; Automation</h3>
<strong>Keep the work moving.</strong>
<p>Track goals across turns. Schedule recurring checks and reports while Peri is running.</p>
</td>
</tr>
</table>

<div align="center">

**The essentials, included.**

Streaming Markdown · Compaction · LSP · Langfuse<br>
Skills & hooks · Plugins · Model profiles<br>
Rewind · Fork · Resume<br>
**Terminal · Headless · ACP · Web PTY**

</div>

## Get started

**macOS / Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.sh | bash
```

**Windows PowerShell**

```powershell
irm https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.ps1 | iex
```

Enter your project and run `peri`. Follow the installer’s PATH instructions and the first-run model setup, then restart Peri after saving.

```bash
peri                         # Start in your project
peri -c                      # Continue your last session
peri -p "Review this branch"  # Run a task and exit
peri update                  # Update Peri
```

Default mode automatically approves tool calls. Use `--permission-mode default` for approval prompts.

<details>
<summary><strong>Configuration & runtime notes</strong></summary>

- `/login` manages providers; `/model` selects profiles; `/threads` opens saved sessions.
- Settings: `~/.peri/settings.json`. Sessions: `~/.peri/threads/threads.db`. Override with `--config-file` and `--db-path`.
- `peri acp --cwd /path/to/project` connects ACP clients; `peri web --host 127.0.0.1` starts a local browser terminal.
- Workflow and PTC require Node.js. PTC executes ordinary Node.js code, not a sandbox. Extensions may have additional dependencies.
- Claude Code configuration import and compatibility vary by feature. Cache behavior depends on the provider and workload.
- Coordination modes use built-in skills and the shared runtime. MetaHarness changes apply to new sessions; MCP Apps requires a compatible host.

Full options: `peri --help`. More: [Documentation](https://konghayao.github.io/peri-cool/).

</details>

<details>
<summary><strong>Build & contribute</strong></summary>

Use a Rust toolchain supporting Edition 2024 and your platform's native build tools.

```bash
git clone https://github.com/konghayao/peri.git
cd peri
cargo build -p peri-tui --release
cargo run -p peri-tui
```

AI-assisted development, with human responsibility for review and release.

[Repository guidance](CLAUDE.md) · [Architecture](docs/design/architecture.md) · [Code index](docs/code-index/) · [Standards](docs/standards/index.md) · [Testing](docs/standards/testing.md)

</details>

---

Built with [Ratatui](https://ratatui.rs), [ratatui-kit](https://github.com/KonghaYao/ratatui-kit), [Tokio](https://tokio.rs), [ACP](https://agentclientprotocol.com), and [Langfuse](https://langfuse.com). Thanks to [Claude Code Best](https://github.com/claude-code-best/claude-code), [Superpowers](https://github.com/obra/superpowers), and [Matt Pocock's Skills](https://github.com/mattpocock/skills).

[Apache 2.0](LICENSE)
