import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { resolve, join } from "node:path";
import { cpus, platform, release, totalmem } from "node:os";
import { Budget, loadApi, sourceDigest, type Sample } from "./harness";
import { legacyStreamScenario, toolsScenario, viewScenario } from "./scenarios";
import { candidateStreamScenario } from "./candidate-stream";
import { combinedScenario } from "./combined";
import { workload } from "./workloads";

function option(name: string, fallback: string): string {
    const index = process.argv.indexOf(`--${name}`);
    return index < 0 ? fallback : process.argv[index + 1]!;
}

const root = resolve(option("root", join(import.meta.dir, "..")));
const output = resolve(option("output", join(import.meta.dir, "results")));
const mode = option("api", "legacy");
const timeoutMs = Number(option("timeout-ms", "120000"));
const maxRssBytes = Number(option("rss-mib", "2048")) * 1024 * 1024;
const sample = Number(option("sample", "1"));
const scenario = option("scenario", "tools");
const scenarios = option("scenarios", "tools,view,stream,large").split(",");
const samples = Number(option("samples", "3"));
if (!["legacy", "candidate"].includes(mode)) throw new Error(`Unknown API adapter: ${mode}`);
if (!Number.isInteger(samples) || samples < 1 || !Number.isInteger(sample) || sample < 1) throw new Error("Sample counts must be positive integers");
if (!(timeoutMs > 0) || !(maxRssBytes > 0)) throw new Error("Resource limits must be positive");
mkdirSync(output, { recursive: true });

function fileDigest(file: string) {
    return existsSync(file) ? createHash("sha256").update(readFileSync(file)).digest("hex") : null;
}

async function worker() {
    const result: Sample = {
        scenario, sample, root, api: mode, workload, startedAt: new Date().toISOString(),
        status: "running", budget: { timeoutMs, maxRssBytes },
        environment: { bun: Bun.version, yjs: JSON.parse(readFileSync(join(root, "node_modules/yjs/package.json"), "utf8")).version,
            platform: platform(), release: release(), arch: process.arch, cpu: cpus()[0]?.model, totalMemoryBytes: totalmem() },
        sourceSha256: sourceDigest(root),
        dependencyLockSha256: fileDigest(join(root, "bun.lock")),
        generatorSha256: fileDigest(join(import.meta.dir, "workloads.ts")),
        harnessVersion: 3,
    };
    const file = join(output, `${scenario}-${sample}.json`);
    const budget = new Budget(result, timeoutMs, maxRssBytes);
    try {
        const api = await loadApi(root, mode);
        const encoding = mode === "candidate" ? "v2" : "v1";
        if (scenario === "tools" || scenario === "large") await toolsScenario(api, result, budget, encoding, scenario === "large");
        else if (scenario === "view") await viewScenario(api, result, budget);
        else if (scenario === "stream") {
            if (mode === "candidate") await candidateStreamScenario(api, result, budget);
            else await legacyStreamScenario(api, root, result, budget);
        } else if (scenario === "stream-sse") {
            if (mode === "candidate") await candidateStreamScenario(api, result, budget, root);
            else await legacyStreamScenario(api, root, result, budget);
        } else if (scenario === "combined") await combinedScenario(api, root, mode, result, budget);
        else if (scenario === "contracts") {
            result.contracts = { large: {}, view: {} };
            await toolsScenario(api, result.contracts.large, budget, encoding, true);
            await viewScenario(api, result.contracts.view, budget, 20, 3);
            result.validation = { exact: true, purpose: "supplementary public assertions, not a pressure comparison",
                tools: 20, asyncUpdates: 3, largeResultBytes: workload.largeResultBytes };
        }
        else throw new Error(`Unknown scenario: ${scenario}`);
        budget.check("complete");
        result.status = "passed";
    } catch (error) {
        result.status = error instanceof Error && error.message.includes("BUDGET_EXCEEDED") ? "truncated" : "failed";
        result.error = error instanceof Error ? { message: error.message, stack: error.stack } : String(error);
    } finally {
        result.elapsedMs = performance.now() - budget.started;
        result.memory = { ...result.memory, finalRssBytes: process.memoryUsage.rss(),
            processPeakRssBytes: process.resourceUsage().maxRSS * 1024 };
        writeFileSync(file, JSON.stringify(result, null, 2) + "\n");
        process.stdout.write(JSON.stringify({ resultFile: file, status: result.status, elapsedMs: result.elapsedMs }) + "\n");
    }
    process.exitCode = result.status === "passed" ? 0 : 1;
}

async function run() {
    const runs: Sample[] = [];
    for (const name of scenarios) for (let index = 1; index <= samples; index++) {
        const file = join(output, `${name}-${index}.json`);
        if (existsSync(file)) throw new Error(`Refusing to overwrite raw sample: ${file}; choose a new output directory`);
        console.log(`Starting ${mode} ${name} ${index}/${samples}`);
        const progressFile = join(output, `${name}-${index}.progress.jsonl`);
        const child = Bun.spawn([process.execPath, import.meta.path, "--worker", "--root", root, "--output", output,
            "--api", mode, "--scenario", name, "--sample", String(index), "--timeout-ms", String(timeoutMs),
            "--rss-mib", String(maxRssBytes / 1024 / 1024)], { stdout: Bun.file(progressFile), stderr: "pipe" });
        let reason: string | null = null;
        let externalPeakRssBytes = 0;
        const timeout = setTimeout(() => { reason = "external_timeout"; child.kill("SIGKILL"); }, timeoutMs);
        let inspecting = false;
        const monitor = setInterval(async () => {
            if (inspecting) return;
            inspecting = true;
            try {
                const check = Bun.spawn(["ps", "-o", "rss=", "-p", String(child.pid)], { stdout: "pipe", stderr: "ignore" });
                const rss = Number((await new Response(check.stdout).text()).trim()) * 1024;
                externalPeakRssBytes = Math.max(externalPeakRssBytes, rss || 0);
                if (rss > maxRssBytes) { reason = "external_rss_budget"; child.kill("SIGKILL"); }
                await check.exited;
            } finally { inspecting = false; }
        }, 250);
        const stderrPromise = new Response(child.stderr).text();
        const exitCode = await child.exited;
        clearTimeout(timeout); clearInterval(monitor);
        const stderr = await stderrPromise;
        let data: Sample;
        try { data = JSON.parse(readFileSync(file, "utf8")); }
        catch { data = { scenario: name, sample: index, root, api: mode, status: reason ? "truncated" : "failed", reason,
            lastProgress: readFileSync(progressFile, "utf8").trim().split("\n").at(-1) ?? null }; }
        data.process = { exitCode, reason, externalPeakRssBytes, stderr };
        if (exitCode !== 0 && data.status === "passed") data.status = "failed";
        writeFileSync(file, JSON.stringify(data, null, 2) + "\n");
        runs.push(data);
        console.log(JSON.stringify({ scenario: name, sample: index, status: data.status, elapsedMs: data.elapsedMs,
            peakRssBytes: data.memory?.processPeakRssBytes ?? externalPeakRssBytes, reason }));
        writeFileSync(join(output, "summary.json"), JSON.stringify({ root, api: mode, workload, runs }, null, 2) + "\n");
    }
    process.exitCode = runs.every((run) => run.status === "passed") ? 0 : 1;
}

if (process.argv.includes("--worker")) await worker();
else await run();
