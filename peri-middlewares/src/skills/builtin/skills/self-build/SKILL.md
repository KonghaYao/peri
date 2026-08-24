---
name: self-build
description: >
  Builds isolated npm capability packages that operate on real project code and
  connects them to Peri through MCP/MCPP and MetaHarness. Use when adding tools,
  resources, remote skills or agents, creating a Bun/Node.js stdio server,
  linking .mcp.json, or changing the active prompt and middleware set.
userInvocable: true
argumentHint: "[capability to add or harness change]"
---

# Self Build

Extend yourself through a project-local MCP server, adding MCPP only when its extra capabilities are required. Build and verify the real code-related capability one layer at a time. Modifying the host is a last resort, not the default extension point.

The unit of delivery is **one capability per npm package**. Keep the package as pure and isolated as practical, but ground its behavior in the repository's actual code and a concrete engineering task. A generic demo is only a connection tracer, never the finished capability.

## Boundaries

- Locate evidence before choosing an implementation: inspect the repository root manifest and lockfiles, then search for `.mcp.json`, `.peri/settings.json`, MCP stdio configuration types, and the nearest runnable MCP example. Record the exact paths used. If a source cannot be found, list the missing evidence and do not treat this skill's snippets as authoritative for package versions or APIs.
- MCP adds callable capabilities, not authority. Preserve approval, tool search, allowlist, and user-confirmation boundaries.
- Reserve stdio stdout for protocol frames. Send diagnostics to stderr. Never print, persist, or cache tokens, cookies, API keys, environment-variable values, or sensitive results.
- Give each tool a minimal input schema, deterministic output, and an explicit timeout. State which adapter enforces the timeout; for subprocesses, terminate the child rather than relying only on the host request timeout. Describe every side effect accurately.
- Use a resource for stable, addressable read-only content. Use a tool for dynamic inputs, computation, bounded local processes, or operation-specific errors and timeouts.
- Change one layer at a time. Always retain a recovery path that removes the `.mcp.json` entry or MetaHarness key.

## Package Boundary

Treat each capability as an independent npm package with its own `package.json`, source, tests, build output, and MCP entrypoint. The package may be private and project-local; publishing it is not required. Prefer a workspace package when the repository already uses npm workspaces; otherwise place it in a dedicated project-local directory without introducing a new package manager. Respect the repository's existing lockfile boundary rather than creating a second dependency universe.

Keep the package boundary narrow:

- Export one coherent domain capability, not a miscellaneous collection of unrelated tools.
- Communicate with Peri only through MCP/MCPP. Do not import Peri internals, patch host code, or depend on unpublished in-process state.
- Avoid global installs, user-home files, ambient services, hidden mutable state, and undeclared environment variables. Declare every runtime dependency and configuration input.
- Prefer pure domain functions beneath thin MCP handlers. Pass repository paths and task inputs explicitly; isolate filesystem, process, network, and credential access behind small adapters. For caller-supplied paths, state whether the allowed root is technically enforced, approved by the host per call, or only a documented convention.
- When the project's own toolchain is the semantic source of truth, prefer a bounded local subprocess to reimplementing its model. Invoke it without a shell, pass arguments separately, enforce timeout and output limits, terminate it on timeout, make network behavior explicit, and parse its output in a pure function.
- Keep generated files, caches, and build output inside the package or an explicitly configured temporary/data directory. Do not write elsewhere by convention.
- External connectivity is opt-in. A code-analysis or transformation capability should work from the local repository whenever possible; if network access is essential, expose it as an explicit input and document its authorization boundary.

Isolation does not mean synthetic work. Before defining the MCP schema, inspect the target repository and choose a real task tied to its code: for example, query its dependency graph, validate its project-specific configuration, inspect its AST, run its established test command, or apply a bounded transformation to user-selected files. Reuse the repository's language, manifests, conventions, and test fixtures. Do not invent a parallel toy model of the codebase.

A capability is complete only when its package can be built and tested independently, then invoked through MCP against a real repository fixture or the target workspace. `echo` proves transport only and must not be reported as the delivered capability.

## Capability Map

| Goal | Preferred primitive |
| --- | --- |
| Add an executable action | MCP tool |
| Expose read-only content | MCP resource |
| Provide behavior instructions on demand | MCPP Skill resource |
| Expose a delegatable role | `agent://.../agent.md` resource |
| Replace a system prompt section | MetaHarness section override |
| Remove a built-in capability | MetaHarness middleware switch |
| Control built-in subagents | MetaHarness `BuiltInSubagents` policy |

## Workflow

1. Inspect the target code and select one concrete engineering task. Write down the package name, capability name, repository inputs, output, side effects, timeout, authorization boundary, and acceptance call. Define ambiguous domain terms—including inclusion rules, ordering, and duplicate handling—before writing the schema.
2. Choose the protocol layer before selecting dependencies: use plain MCP for portable tools and resources; add MCPP only for remote Skills, remote Agents, its cache contract, or another verified MCPP capability.
3. Create one isolated npm package for that capability. Give it its own manifest, source, tests, build output, and MCP entrypoint. Match the repository's existing package manager and workspace conventions; do not overwrite dependency constraints.
4. Separate pure domain logic from the MCP adapter. Test the domain logic against real project fixtures or representative files from the target repository.
5. Implement the real code-related capability, complete type checking and building, and run the package's handler tests. If the repository lacks a verified stdio example, temporarily add a harmless `echo` tracer to isolate transport failures; remove it before delivery unless tracing belongs to the coherent capability.
6. Add the server to the project `.mcp.json` using an absolute entrypoint path. Create a new session so the host reloads frozen configuration and connects to it.
7. Discover the server, schema, and bridge name from inside the agent, then invoke the real capability on an explicit repository path or bounded project input. Never guess a deferred tool name.
8. If a temporary tracer was used, remove it and repeat `detail -> list -> exact search -> real capability call` against the final tool surface.
9. Add further tools or resources only when they belong to the same coherent capability; otherwise create another npm package. Test every handler and side-effect boundary.
10. Adjust MetaHarness last. Verify the system prompt, tool surface, and approval boundaries after each change in a new session.

## Choose MCP or MCPP

- Use plain MCP when the package only needs portable tools or resources.
- Add MCPP only when the capability requires remote Skills, remote Agents, the MCPP cache contract, or another capability verified against the installed package.
- Make this choice before adding dependencies. Do not introduce MCPP merely because the example below uses it.

## Optional MCPP Transport Tracer

The current repository example uses the MCP Server SDK v2 API. Use this tracer only when a verified stdio example is unavailable, and remove it before delivery. The imports and APIs below are illustrative until verified against the target package manifest, lockfile, installed declarations, and a runnable neighboring example. Never derive dependency versions from this snippet:

```ts
import { McpServer } from "@modelcontextprotocol/server";
import {
  createCacheVersion,
  createMcppServerFactory,
  startServer,
} from "@peri-code/mcpp";
import { z } from "zod";

const cacheVersion = await createCacheVersion({
  schemaVersion: "1",
  tools: ["echo"],
});

const createServer = createMcppServerFactory(
  { cacheVersion },
  (_request, mcpp) => {
    const server = new McpServer(
      { name: "self-build", version: "0.1.0" },
      { capabilities: mcpp.capabilities },
    );

    server.registerTool(
      "echo",
      {
        description: "Return text unchanged to verify the MCP connection.",
        inputSchema: z.object({ text: z.string() }),
      },
      ({ text }) => ({ content: [{ type: "text", text }] }),
    );

    return server;
  },
);

await startServer(createServer, { mode: "stdio" });
```

### Bun and Node.js

- **Bun direct run:** Type-check the project, then start it with `bun run /absolute/path/to/src/server.ts`.
- **Node.js artifact:** Bundle the TypeScript entrypoint and any TypeScript-source dependencies as Node ESM, then run the generated `dist/server.mjs`. If the project already uses Bun as its builder:

```bash
bun build src/server.ts --target=node --format=esm --outfile=dist/server.mjs
node dist/server.mjs
```

Start the artifact with the target Node runtime as a real verification step. Do not assume that Node can execute `.ts` files or load a dependency whose entrypoint is TypeScript. A stdio server that waits silently for stdin during a smoke test is behaving normally; it must not write diagnostics to stdout.

If only portable MCP tools and resources are needed, use the installed SDK's `serveStdio` factory directly and omit MCPP-specific capabilities. Prefer a transport pattern already verified in the repository.

## Link the Server to Peri

The fixed version below applies to the verified MCPP lifecycle. For plain MCP, use the protocol version verified against the installed SDK and Peri configuration contract; do not copy the MCPP version by default.

MCPP `startServer` uses the strict MCP `2026-07-28` lifecycle, so configure the protocol version explicitly:

```json
{
  "mcpServers": {
    "self-build": {
      "command": "bun",
      "args": ["run", "/absolute/path/to/src/server.ts"],
      "protocolVersion": "2026-07-28"
    }
  }
}
```

For Node, change `command` to `node` and point `args` at the absolute path of the built artifact. The current Peri stdio configuration contract models `command`, `args`, `env`, and `protocolVersion`; do not rely on an unmodeled working-directory field. Keep credentials out of configuration. Inherit a controlled process environment or use the host credential facilities instead.

MCP configuration and MetaHarness state are frozen at `session/new`. Create a new session after changing either one. Rewriting a file does not alter the current session.

## Verify from Inside the Agent

1. Call `SearchExtraTools("DiscoverMCP")` and use the returned canonical name and schema.
2. Invoke the read-only detail query through `ExecuteExtraTool`:

```json
{"tool_name":"DiscoverMCP","params":{"method":"detail","params":{"server":"self-build"}}}
```

3. List the server's tools, resources, skills, and agents:

```json
{"tool_name":"DiscoverMCP","params":{"method":"list","params":{"server":"self-build"}}}
```

4. If a temporary tracer was necessary, search for its returned bridge name and make one harmless call. This proves transport only. Remove the tracer before delivery.
5. Search for the real capability's returned bridge name and invoke it with an explicit target-repository fixture or bounded workspace input. Never guess or retain the typical bridge name from an example.
6. Repeat `detail` and `list` after every tool-surface change and confirm that the final server exposes only tools and resources belonging to the package's coherent capability.

Transport evidence, when a tracer was needed: the server is `connected`, `detail` reports the expected protocol and capabilities, the tracer call returns its sentinel value, and stdout remains protocol-only.

Delivery evidence must go further: invoke the real capability against an explicit target-repository fixture; compare every returned field with an independent repository source of truth; record the exact input, expected semantics, discovered bridge name, timeout behavior, and evidence that no undeclared side effect occurred. A tracer call is never delivery acceptance.

## Extend with MCPP

### Skills

`ResourceForSkills` projects `skills/<name>/SKILL.md` and budget-limited companion files as `skill://<name>/...` resources:

```ts
import { ResourceForSkills } from "@peri-code/mcpp";

ResourceForSkills(server, {
  skillsDir: new URL("./skills", import.meta.url).pathname,
  origin: "self-build",
  ...mcpp.resourceCache,
});
```

Each direct child directory must contain a root `SKILL.md`, and its frontmatter `name` must equal the directory name. This helper scans resources dynamically, allowing Peri to use the `skill://` resource fallback. If the server declares the standard Skills extension, implement the `skills/list`, `skills/get`, digest, and complete frontmatter comparison contracts instead. Never expose two inconsistent metadata views.

### Agents

Expose an agent definition as one text resource whose URI is `agent://<name>/agent.md`. Its body must use Peri agent Markdown frontmatter. Peri discovers metadata from `resources/list` and reads the body only on first activation. A remote definition cannot override a local definition, and activating it does not implicitly authorize its declared Skills. Use the `agents` domain from `DiscoverMCP` to inspect the final agent ID.

### Cache

The input to `createCacheVersion` must cover tools, prompts, resources, skills, and all cacheable content. Any content change must produce a new version. A private cache requires an opaque authorization context generated by the host and must remain isolated by authorization context. Never use a token, cookie, or raw credential as the cache identity. A cache hit must not bypass current authorization.

## MetaHarness Controls

Project configuration lives in `.peri/settings.json`; replacement documents live directly under `.peri/meta/*.md`. The configuration map has three relevant behaviors:

- A section ID set to `true` replaces that complete system prompt section with `.peri/meta/<ID>.md`. It is replacement, not appending. Content is not trimmed, and directory scanning is not recursive.
- A middleware name set to `false` removes the whole middleware at assembly time, including its tools and prompt contribution.
- `BuiltInSubagents` controls whether compile-time built-in subagent definitions are available.

Minimal example:

```json
{
  "config": {
    "meta_harness": {
      "05_using_tools": true,
      "WebMiddleware": false,
      "BuiltInSubagents": false
    }
  }
}
```

Then write the complete replacement section to `.peri/meta/05_using_tools.md`. Override one section or disable one middleware at a time, and compare the tool list and system behavior in a new session.

Keep `McpMiddleware` and `ToolSearch` enabled while building MCP capabilities. Remote Skills and Agents also require `SkillsMiddleware`, `SkillPreloadMiddleware`, and `SubAgentMiddleware`. Do not disable `PermissionMiddleware` or `HumanInTheLoopMiddleware` to evade approval or user-question boundaries. To recover, remove the corresponding key; for a section override, also remove `.peri/meta/<ID>.md`, then create a new session.

## Failure Layers

- **Spawn:** The command is missing, the absolute path is wrong, the artifact was not built, or the process exits immediately.
- **Protocol:** stdout contains diagnostics, or the server is missing the protocol version required by its verified transport contract; the fixed `2026-07-28` value applies to the MCPP lifecycle described above.
- **Discovery:** The server is connected but the tool or resource was not registered, or a Skill/Agent URI or frontmatter violates its contract.
- **Execution:** The schema and handler disagree, the call times out, a side-effect boundary is incorrect, or approval is denied.
- **Harness:** The files changed but the session is stale, a section ID or middleware name is wrong, or a required middleware was disabled.

Fix only the current layer, then repeat `detail -> list -> exact search -> real capability call`. On delivery, report the added files, package and server keys, startup path, final schemas and tool surface, independent source-of-truth comparison, discovery and real-call evidence, MetaHarness changes, recovery path, the package's intended operational scope, and the authority actually enforced by OS, sandbox, host policy, and input validation:

```text
Declared filesystem read scope:
Enforced filesystem read boundary:
Declared filesystem write scope:
Enforced filesystem write boundary:
Subprocesses:
Network use and enforced network boundary:
Credentials:
Host internals:
Persistent state:
Recovery action:
```

Do not describe authority as technically ungranted unless an OS, sandbox, or host control enforces that boundary. Otherwise report it as unused or outside the declared operational scope.
