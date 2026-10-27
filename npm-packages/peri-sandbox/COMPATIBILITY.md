# Compatibility and validation ledger

> Toolchain note: this ledger records the upstream npm-based run. After the move into the Peri repository, dependency installation uses Bun (`bun install`); runtime and tests still execute on Node (`node --test`).

Observed **October 27, 2026** on Apple Silicon macOS operating an isolated OrbStack Ubuntu 24.04 ARM64 VM (`peri-sandbox-accept`). Reproduce with `bash scripts/macos-acceptance.sh`; it builds and executes `npm test`, `node dist/src/cli.js doctor --config /srv/peri/control/config.json`, `node dist/tests/integration/linux.js /srv/peri/control/config.json` and `node dist/tests/integration/resources.js /srv/peri/control/config.json`. Latest Linux run: **40 unit cases (38 passed, 2 macOS-specific skipped), 16 Linux integration checks and 4 resource/network checks passed**. Logs and exact output come from those commands, not a security certificate.

macOS local preview: `npm test` passed all **41** cases; `npm run test:mac -- /absolute/config.json` against a temporary local service passed real MCP Write/Read/Bash, token read denial, stop and same-URL restart. A separately installed npm tarball started `up` and passed the same MCP exercise. Killing the temporary API with SIGKILL and running `up` again stopped/recovered its previous generation and reused the URL. macOS relies on SRT's native sandbox and process groups; unlike Linux, it **does not enforce `memoryMax`, `tasksMax`, or `cpuQuota`**, and detached descendants escaping a process group have not been shown to be reliably cleaned. Do not infer Linux A01–A18 or production multi-tenant guarantees from these macOS checks.

Programmatic macOS startup: `npm run test:mac:programmatic` starts a temporary in-process server via the public `startPeriSandbox()` API, discovers all seven Peri Workspace tools and invokes `Glob` through authenticated MCP, confirms a second startup reports a conflict without affecting the first, closes twice without exiting the host, confirms occupied-port fallback updates the Sandbox URL, and recovers the preferred URL after releasing the port. This does not expand the isolation guarantees above.

| Component | Observed version | Verification |
| --- | --- | --- |
| Node, Linux VM | 24.22.1 | ARM64 tarball checked against upstream SHASUMS256; build/tests/doctor passed |
| MCP TS core/client/server | 2.4.0, locked | `scripts/patch-sdk.mjs` applies the exact-anchor Tasks codec patch; wire fixtures and HTTP roundtrip passed |
| Peri | 4.3.2; SHA-256 `323a8e491a26e71be93a4e129ef60b18c7a53098b7bf252a5f309affe3af36c1` | Archive SHA-256 `750db3c41a3eb7d2b65d6ac8b4aedae338a72025cfdd747ef0c43368e9a98a6d`; real MCP Write/Read/Bash/tasks; update returns method-not-found |
| Anthropic sandbox-runtime CLI | npm dependency 0.0.81; CLI SHA-256 `2a3cc1c7c6ee703860ea548594d55de1bdf760a642786cac74506dd96ac7fcb1` in VM | Package-local CLI and seccomp helper resolved without a global `srt`; real SRT/bwrap/seccomp, path/network probes under user-systemd cgroup v2 |

Package smoke check: `npm pack` produced a tarball with only the public `dist/src` runtime tree, declarations, README, examples and SDK patch script; a separate temporary ESM consumer project installed the tarball and resolved the SRT CLI from its own npm dependencies while importing `connectSandboxControl`. This checks package wiring; the Linux VM tests check actual isolation, **not** Peri client discovery.

“Live” below means the Linux VM ran **real** SRT+Peri through the HTTP adapter and user-systemd provider. “Mock” means local automated unit/fixture test, **not** environmental isolation. Partial means not all conditions in the design's row were exercised.

| ID | Evidence | Result / remaining gap |
| --- | --- | --- |
| A01 | Live authenticated MCP TS client calls real Peri Write/Read/Bash | **Partial:** no real *Peri client* settings discovery/invocation; `examples/peri-mcp-client.json` not executed end-to-end |
| A02 | Live cgroup confirms exactly one Peri backend; no readiness sibling | Passed in this VM |
| A03 | Live stop/start keeps URL/files, rotates generation, rejects stale get/update/cancel | Passed in this VM |
| A04 | Live other workspace/home, symlink, relative/traversal and unowned sibling denials | Passed for tested paths in this VM |
| A05 | Live config/token/user bus denial and two-running-sandbox `/tmp/claude` isolation | Passed for tested paths in this VM |
| A06 | Live default egress denied, allowlisted HTTPS succeeds, another domain denied; doctor probes AF_UNIX | **Partial:** AF_UNIX proof from doctor, not a separate sandbox-to-sandbox socket probe |
| A07 | Live CPU throttling, PID max and memory OOM events in unit cgroups | Passed in this VM; filesystem has start-time free-space watermark, no per-sandbox hard disk quota |
| A08 | Live normal and nonzero Bash tasks, get/update no-op, cross-owner denial, cancel terminal + no shell marker; codec fixture | Passed for no-op update contract; no input handoff |
| A09 | Live task lookup after HTTP connection close | Passed within same generation |
| A10 | Live two connections share backend; task query is served during long Bash; mock independent abort/progress | **Partial:** same JSON-RPC IDs and cross-request progress/cancel not independently instrumented live |
| A11 | Mock start/stop/destroy races, startup timeout, late callbacks | Mock only, not stress-tested under a real concurrent supervisor |
| A12 | Live detached `setsid` double-fork descendant and separate Peri/SRT/API SIGKILL paths cleaned; cgroups empty | Passed for these failure modes in this VM |
| A13 | Mock stop failure retains `stop_incomplete` and forbids restart/delete | Mock only; unsafe real unkillable-cgroup scenario not exercised |
| A14 | Live `systemctl --user restart` and API SIGKILL/recovery: units stopped, data retained, no automatic replay; mock registry write failures | **Partial:** late systemd start job, real DB corruption/full and disk-full conditions not tested |
| A15 | Live unauthenticated/legacy/no-Tasks rejected; mock unsupported features/allowlist | Passed for tested protocol boundaries |
| A16 | Live >1 MiB request rejected; unit malformed/oversized stdio and rotating private diagnostics; HTTP concurrency capped at 32 | **Partial:** live slow consumer, unbroken huge stdout, stderr flood, Bash budget after quota, cumulative Peri task memory measurement outstanding |
| A17 | Live tombstone, directory-only removal, HTTP 410; mock traversal/symlink checks | **Partial:** destructive attack scenarios not all reproduced live |
| A18 | Live doctor succeeds in supported VM; mock controller mismatch/absent/unlimited rejects | **Partial:** live fault injection for disabled PrivateTmp, user namespace and cgroup controllers not executed |

Native Darwin is **not** the Linux isolation provider. The VM reuse check cannot establish absence of manually configured mounts or secrets. Node's tarball hash is obtained from a downloaded manifest rather than pinned; apt packages and SRT transitive dependencies can change. The service and profiles require a dedicated, reviewed Linux deployment; these tests do not establish portability, multi-tenant security or sustained-memory behavior.
