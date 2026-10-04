# SDK Yjs benchmark

Deterministic ACP workloads exercise source files selected by `--root`. Each sample
runs in a fresh Bun process; scenarios run serially. No build or network is used.

```sh
cd npm-packages/@peri-sdk
/Users/konghayao/.bun/bin/bun benchmarks/run.ts \
  --root /tmp/peri-yjs-sdk-baseline-567640f1837771442628970a56ce68c5a8f8e256 \
  --output ../../docs/experiment-yjs-sdk/raw/baseline \
  --api legacy --scenarios tools,view,stream,large --samples 3
```

The source root must contain `src`, `examples/demo/session-doc-stream.ts`, and a
`node_modules` directory containing the locked Yjs dependency. Use Bun 1.4.2 and
Yjs 13.6.33 for both rounds. The current SDK is the default root. The runner loads
Yjs from that root so independently installed roots keep matching Yjs type identities.

To reconstruct a baseline from a clean checkout, run the following from the repo
root. The temporary destination must not already exist. Install uses the original
package and lockfile; no package update is needed. Change the Bun executable path
if necessary, after confirming its version is 1.4.2.

```sh
/Users/konghayao/.bun/bin/bun --version
git archive --format=tar --output=/tmp/peri-yjs-baseline-source.tar \
  567640f1837771442628970a56ce68c5a8f8e256 \
  npm-packages/@peri-sdk/src \
  npm-packages/@peri-sdk/examples/demo/session-doc-stream.ts \
  npm-packages/@peri-sdk/package.json npm-packages/@peri-sdk/bun.lock \
  npm-packages/@peri-sdk/tsconfig.json
mkdir /tmp/peri-yjs-sdk-baseline-rebuilt
tar -xf /tmp/peri-yjs-baseline-source.tar \
  -C /tmp/peri-yjs-sdk-baseline-rebuilt --strip-components=2
cd /tmp/peri-yjs-sdk-baseline-rebuilt
/Users/konghayao/.bun/bin/bun install --frozen-lockfile
```

Then run the current checkout's `benchmarks/run.ts` with `--root` pointing at that
directory. `generatorSha256`, `dependencyLockSha256`, and `sourceSha256` identify
the tested inputs. `--scenarios contracts --samples 1` checks the public consumer
assertions with 20 tools, 3 async updates and a separate 1 MiB result; this is a
supplementary correctness run, not a substitute for the full workload.

`fixtures/round1-harness.tar.gz` preserves the exact four first-round TypeScript
files. Their SHA-256 values are recorded in
[`provenance.json`](../../../docs/experiment-yjs-sdk/raw/baseline/provenance.json).
For exact first-round harness reproduction, extract the archive into a new
`benchmarks` directory under the reconstructed baseline root and invoke its
`run.ts` there. This also ensures the original harness shares the tested Yjs
installation. First-round raw data is retained unchanged.

`workloads.ts` is the single generator for both rounds. `harness.ts` handles budgets,
source fingerprints and exact validation; `scenarios.ts` applies workloads;
`run.ts` manages fresh child processes and JSON artifacts. `validation.ts` checks
public history and resolves payload references. `candidate-stream.ts` exercises
the binary V2 API with `--api candidate`. Production code does not import these files.

Each `SCENARIO-SAMPLE.json` contains measurements, content validation, environment,
source SHA-256, resource budget and final process exit status. The corresponding
`.progress.jsonl` retains checkpoints if the process is terminated. `summary.json`
combines all samples without discarding failures. The runner exits nonzero if any
sample fails or is truncated. `--timeout-ms` defaults to 120000; `--rss-mib` to 2048.
RSS is checked internally and by a parent process polling `ps` every 250 ms.
Existing raw sample files are never overwritten; select a fresh output directory.

The tool projection timer excludes payload generation and JSON size accounting.
The view latency includes ACP accept plus its microtask publication. The legacy
stream timer includes synchronous JSON/base64 accounting and replica application;
application time is also reported separately. Candidate `projectionMs` measures
accept calls and any threshold-triggered flush during them. In both current
adapters, `endToEndMs` starts before the user notification and ends after completion,
all replica application, and (for candidate) explicit flush/awaited close. This
includes generation inside the stream loop, and excludes initial snapshot and
final validation. Only the supplemented baseline has this new timing field.

Candidate `updateBytes` sums native V2 payloads; `jsonBytes` calculates the exact
equivalent demo SSE JSON/base64 body. The binary API has no serialized binary
envelope, so these counts do not invent envelope bytes. Neither count includes
HTTP/SSE/TCP headers. Candidate configuration is explicitly recorded: 16 ms timer,
64 KiB batch target, 256 updates per batch, and 4 MiB subscriber budget. The stream
runs without simulated wall-clock arrival gaps. RSS includes the Bun runtime, source
documents, buffers, replica and validation work, rather than only retained state.

The tool scenario reports the primary encoding (baseline V1, candidate V2), plus
candidate V1 encoded bytes for the same hot documents. Public view references are
resolved through `SessionDocs.readPayload`, and exact JSON bytes are counted
separately as `onDemandReadBytes`/`referencedPayloadBytes`; these are logical live
payload bytes, not complete storage heap overhead. Public history validation and
one post-timing tool metadata update ensure preserved history and cache invalidation.

`--scenarios stream-sse` uses the actual demo adapter and decodes its base64 updates
before applying them. Use this candidate path for elapsed-time comparison with the legacy
demo path; the native `stream` path has no base64 decode and is a separate metric.
Both account for JSON body serialization and exclude HTTP/SSE I/O and JSON.parse.

`--scenarios combined` seeds the full 12,000-tool history, opens the actual demo
adapter, and creates a replica plus its SessionViewStore. It then sends the same
16 MiB stream, flushing and yielding one microtask every 64 chunks for both APIs.
Expected view publications are 256 streaming updates plus completion. It checks
all historical payloads before and after the stream, outside the stream timer.
The original 120-second/2-GiB budget covers the entire scenario. Truncated baselines
are retained with their completed chunk count and cannot support a full-run speedup.

Payload content is deterministic; Yjs generates random client IDs, so encoded
byte counts may vary slightly across samples. No machine-dependent timing is a
test pass threshold. Success requires exact data, expected counts and budget
compliance. Results describe these local synthetic workloads, not production
network performance. See [the experiment plan](../../../docs/experiment-yjs-sdk/00_plan.md).

## Browser smoke

`bun benchmarks/browser-smoke.ts` starts an isolated loopback fixture with 12,000
tools and drives the real demo in a temporary headless Chrome profile. It checks
the 200-block initial window, zero initial payload reads, on-demand full data,
DOM identity after streaming, loading earlier history, and a bounded long-text
preview with a complete download Blob. No model, credentials
or persistent Session Store is needed. Set `CHROME_BIN` to a Chrome/Chromium
executable; the default is the macOS Google Chrome application. It emits JSON
and closes its own browser and server. This is a behavior check, not a timing
benchmark. `browser-fixture.ts` can also run alone for manual inspection.
