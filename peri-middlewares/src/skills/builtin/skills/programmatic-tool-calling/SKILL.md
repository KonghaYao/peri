---
name: programmatic-tool-calling
aliases:
  - ptc
description: >
  Use programmatic tool calling to discover extra tools, compose multiple tool
  calls in ESM code, and return a compact JSON-compatible result.
---

# Programmatic Tool Calling

This builtin skill is named `programmatic-tool-calling` and keeps the generic alias `ptc`, so users may invoke it as `/ptc`.

Use this skill when a task benefits from discovering extra tools and orchestrating several dependent or repetitive calls in one bounded program. It is especially useful for filtering, joining, or summarizing structured tool results without copying large intermediate payloads into the conversation.

Do not use it for a single ordinary tool call, when the required tool is already directly available, for interactive workflows that need user input between steps, or to bypass an approval or access-control boundary.

## Workflow

1. Call `SearchExtraTools` to locate `RunPtcCode` and every deferred tool the program needs, and inspect their schemas.
2. Invoke canonical deferred target `RunPtcCode` through `ExecuteExtraTool` with a small ESM program that invokes the discovered tools, checks results, and returns only the JSON-compatible data needed by the task.
3. Use `ExecuteExtraTool` directly instead when no program is needed, or to validate another discovered tool before composing it.

The PTC entry pattern is `SearchExtraTools → ExecuteExtraTool(RunPtcCode)`. `RunPtcCode` is not a direct tool; existing direct tools remain unchanged. The old `run_code` name is only a search migration keyword, not an executable alias. Never guess an extra tool name or input schema.

## Code and result constraints

- Code runs in an ESM-only function body. Load Node modules only with dynamic `await import("node:...")`; static `import` statements and CommonJS `require`/`module.exports` are unavailable.
- Inputs, intermediate tool arguments, and the final return value must be JSON-compatible: objects, arrays, strings, numbers, booleans, or `null`. Do not return functions, classes, cyclic values, streams, or binary handles.
- Keep programs bounded. Limit tool-call count, loop iterations, concurrency, output size, and retained intermediate data. Prefer projection and aggregation over returning raw bulk responses.
- Handle missing fields and tool errors explicitly. Return a compact structured error rather than retrying indefinitely.

## Security and approvals

`RunPtcCode` does not expand authority. Every discovered tool remains subject to its own security policy, permissions, and approval boundary; policy, HITL, event, and tool-card projections use the effective target. Any assistant raw wrapper call is retained only for protocol pairing. Programmatic composition must not suppress, combine away, or work around approvals. Treat credentials and sensitive tool results as secrets: do not log them, serialize them into diagnostics, or include them in the final result unless the user explicitly needs that data and access is authorized.

Do not use PTC to perform destructive, irreversible, or high-impact actions without the same explicit user authorization required for calling those tools directly. If a tool requires approval or user confirmation, stop at that boundary and let the host request it.
