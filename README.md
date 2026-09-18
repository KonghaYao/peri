<div align="center">

<img src="images/img_peri_black_marble_banner_s03.webp" alt="Peri Code — All Coding for you." width="960">

# Peri Code

### All Coding for you.

A native Rust coding agent for macOS, Linux, and Windows.<br>
Bring an Anthropic or OpenAI-compatible endpoint.

[**Get started ↓**](#get-started) · [Documentation](https://konghayao.github.io/peri-cool/) · [Releases](https://github.com/konghayao/peri/releases)

</div>

---

<div align="center">

**AGENTS, WORKING TOGETHER**

Plan delivery. Coordinate agents. Delegate tasks.

</div>

<table>
<tr>
<td width="33%" valign="top">
<h3>01 / Ultra-ADLC</h3>
<strong>From idea to delivery.</strong>
<p>Discover, design, review, build, and verify.</p>
<code>/ultra-adlc</code>
</td>
<td width="33%" valign="top">
<h3>02 / Ultracode</h3>
<strong>Work as a team.</strong>
<p>Run agents in parallel, choose their models, and track progress.</p>
<code>/ultracode</code>
</td>
<td width="33%" valign="top">
<h3>03 / Multitask <small>(Coming soon)</small></h3>
<strong>Delegate and keep talking.</strong>
<p>Let agents work in the background while you stay in control.</p>
<code>/multitask</code>
</td>
</tr>
</table>

<table>
<tr>
<td colspan="2">
<h3>🔌 MCP Plus</h3>
<strong>More than tools.</strong>
<p>Load remote skills and agents on demand. Exchange messages and approvals through compatible channels.</p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<h3>🛠️ MetaHarness</h3>
<strong>Make the agent yours.</strong>
<p>Customize prompts, capabilities, and built-in agents.</p>
</td>
<td width="50%" valign="top">
<h3>🧩 MCP Apps</h3>
<strong>Connect interactive apps.</strong>
<p>Use apps in a compatible ACP host, with Peri's tool permissions.</p>
</td>
</tr>
<tr>
<td valign="top">
<h3>📄 Artifacts</h3>
<strong>Share your results.</strong>
<p>Publish HTML or Markdown reports as public links.</p>
</td>
<td valign="top">
<h3>⏱️ Goals &amp; Automation</h3>
<strong>Keep the work moving.</strong>
<p>Track goals across turns and schedule recurring tasks while Peri runs.</p>
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

Follow the installer's PATH instructions, then run `peri` in your project. Complete model setup and restart Peri.

```bash
peri                         # Start in your project
peri -c                      # Continue your last session
peri -p "Review this branch"  # Run a task and exit
peri update                  # Update Peri
```

Tool calls are auto-approved by default. For approval prompts, use `--permission-mode default`.

<details>
<summary><strong>Configuration & runtime notes</strong></summary>

- `/login`: providers · `/model`: model profiles · `/threads`: saved sessions.
- Settings: `~/.peri/settings.json` (`--config-file`). Sessions: `~/.peri/threads/threads.db` (`--db-path`).
- ACP client: `peri acp --cwd /path/to/project`. Browser terminal: `peri web --host 127.0.0.1`.
- Workflows require Node.js. Extensions may need other dependencies.
- Claude Code compatibility varies by feature. Caching depends on your provider and workload.
- Coordination uses skills and a shared runtime. MetaHarness changes apply to new sessions; MCP Apps needs a compatible host.

Full options: `peri --help`. More: [Documentation](https://konghayao.github.io/peri-cool/).

</details>

<details>
<summary><strong>Build & contribute</strong></summary>

Requires Rust with Edition 2024 support and native build tools.

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
