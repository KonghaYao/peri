# Peri improvements reproduction scripts

These scripts are read-only reproductions of the 2026-09-13 Peri task sample and error index. They reuse `DataLoader`, its normalizer, and `exportTaskPacket`; they do not parse the SQLite database independently.

Run from `side-projects/agent-defect-analyzer`:

```bash
out=$(mktemp -d)/peri-improvements
bun run reports/2026-09-13-peri-improvements/scripts/select-and-export.ts --out "$out"
bun run reports/2026-09-13-peri-improvements/scripts/error-index.ts --out "$out"
```

`select-and-export.ts` requires a new `--out` directory and refuses to overwrite an existing `manifest.json` or `cases/`. It defaults to the half-open UTC window `[2026-09-06T00:00:00Z, 2026-09-13T00:00:00Z)`, visible root threads, cwd equal to the repository root or a descendant, seed `peri-improvements-2026-09-13-v1`, two cases per stratum, and the prior pilot manifest at `reports/2026-09-13-task-effectiveness/pilot-manifest.json`. It exports content with `maxBytes=1048576` and `maxMessages=1000`.

Selection accepts `--db PATH`, `--since ISO`, `--until ISO`, and `--cwd-root PATH`; it also accepts `--seed VALUE`, `--per-stratum N`, and `--prior-manifest PATH`. Its database may instead be supplied through `PERI_RESEARCH_DB`. Relative paths are resolved from the current working directory. The selection shuffle is the task-packets algorithm, using `${seed}:${short|medium|long}` for each stratum.

The second command expects the first command's schemaVersion 1 manifest and inherits its window, cwd root, database identity, schema identity, roots scope, and `includeHidden=false`. It accepts matching explicit `--db`, `--since`, `--until`, and `--cwd-root` values for auditability, but rejects conflicts before opening a transaction or writing output. It does not let `PERI_RESEARCH_DB` silently override the manifest database. It retains paired and orphan explicit errors, duplicate-ID counters, tool names, IDs, first error lines, and local error text. It refuses to overwrite an existing error index.

For a dry check against the current local snapshot, use a fresh temporary output and compare the reported counts and packet hashes with the ignored output directory under `output/peri-improvements-2026-09-13/`. The output path is intentionally caller supplied so a reproduction cannot silently replace the frozen sample.
