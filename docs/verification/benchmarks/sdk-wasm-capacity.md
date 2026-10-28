# SDK WASM Agent 容量基准（1 GiB 宿主 RSS）

> 状态：现行基准规程。本文规定如何复测 `npm-packages/@peri-sdk` 在 Bun + WASM 后端下，1 GiB 宿主 RSS 内可同时存活的空转 Agent 数；历史数字见第 6 节，不代表其他主机、Bun 版本或真实对话负载的容量承诺。本文不记录单次执行的通过标记；每轮原始 JSON 放在仓库外，只有方法和汇总基线进入本文。
>
> 入口：[SDK 构建与示例](../../../npm-packages/@peri-sdk/README.md)、WASM 产物缓存 [`artifact-cache.ts`](../../../npm-packages/@peri-sdk/src/wasm/artifact-cache.ts)、共享实例注入 [`loader.ts`](../../../npm-packages/@peri-sdk/src/wasm/loader.ts)。测试证据规则以[测试规范](../../standards/testing.md)为准；本文不复制测试规范。
>
> 触发方式：本基准是**手动触发的容量复测规程**，**不纳入 CI，不作为 PR 门禁**。仅在用户或维护者明确要求复核容量时运行；常规开发、提交和代码评审不需要执行它。

## 1. 目的与口径

### 1.1 测量对象

1. 单宿主 Bun 进程，构造一个 `ChatManager`；每个 Agent 是一个 `chat` 句柄。
2. `chatConfig.runtime.kind = "wasm"`，每个 chat 建立独立 ACP session 后**空转**，不发送 prompt。
3. 串行启动，启动后不关闭、不销毁；默认每新增一个 Agent 采样一次；从 200 起步时先预热到 200 再逐点采样。
4. 主指标是宿主进程 RSS：`ps -o rss= -p <pid>`，单位 KiB，除以 1024 得到 MiB。`process.memoryUsage().rss` 只作对照。
5. 本地 sqld 是独立进程，约 32 MiB，**不计入宿主 RSS**。
6. 停止条件：宿主 RSS ≥ 1024 MiB，或达到脚本上限，或启动失败。
7. WASM 线性内存只增不减；同一个进程跑过大 N 后再关闭 chat，RSS 不会回到基线。每轮必须使用全新进程。

### 1.2 指标定义

| 指标 | 定义 |
| --- | --- |
| `initial` | 每个 N 启动后立即按 3.1 执行的 3 次中位数采样；停止判定只使用它。 |
| `settled` | 2 s / 10 s 后的复采中位数；只用于复核峰值与尾部回归，不改变 `N_max` / `N_stop`。 |
| `N_max` | `initial` < 1024 MiB 的最大 N。 |
| `N_stop` | 首个 `initial` ≥ 1024 MiB 的 N。启动失败单独记为 failure，不产生容量值。 |
| `peak` | 所有 initial + settled 采样的最大宿主 RSS，不含 Manager 基线；脚本在记录基线后从 0 累计。不是只取最后一个点。 |
| `first increment` | N=1 的 `initial` RSS − Manager 构造后的宿主 RSS。只有 `--start 1` 能测；从 200 起步时为 `null`。 |
| `slope` | 尾部最后 8 个已采样点的最终 RSS（有 `settled` 取 `settled`，否则取 `initial`）的 OLS 斜率，单位 MiB/agent。 |
| `R²` | 同一回归的判定系数；`R² < 0.95` 视为未测稳。 |
| `N_est` | `(1024 - intercept) / slope`，只表示拟合窗口内与 1024 的交点，不替代实测 `N_max`。 |
| `startup p50/p95` | `createNewSession` 的墙钟时间；包含首个 Agent 的冷启动，所以 p95 比 max 更能代表常态。 |

### 1.3 基线摘要

1. 1 GiB 宿主 RSS 下实测 `N_max`：**219 / 220 / 222**（三次独立 run），`N_stop`：**220 / 221 / 223**。
2. 后续复测直接从 **200 个**起步：N=200 约 **941–946 MiB**，留约 **80 MiB** 余量。
3. 每 Agent 边际内存约 **4.2 MiB**（三次 3.905 / 4.380 / 4.508，R² 0.9907–0.9962）。
4. 首个 Agent 一次性增量约 **111–114 MiB**；Manager 基线约 **30 MiB**。
5. 优化前：`N_max=20`、**39.6 MiB/agent**、首个 **+118.8 MiB**、p50 **79.5 ms**。
6. 优化后：p50 **45.5–46.2 ms**、p95 **49.5–50.0 ms**、峰值约 **1026–1028 MiB**。
7. sqld 独立进程约 **32 MiB**，不计入宿主 RSS。

`process.memoryUsage().rss` 不能作为主指标：WASM 线性内存由实例持有，Bun 对该部分记账不完整。优化前 N=21 实测 `ps` 1035.7 MiB、`process.memoryUsage().rss` 676.2 MiB，差 **359.5 MiB**；优化后各轮接近阈值时的最大差约 **174.1–176.5 MiB**。用后者会把容量系统性高估。

## 2. 前提与构建

### 2.1 构建 dist

```bash
cd npm-packages/@peri-sdk
MISE_ENV=wasm mise exec -- bun run build
```

1. 该命令先构建 `peri-wasm` release（`scripts/build.ts` 调用 `cargo-wasm.sh build --locked --release -p peri-wasm --target wasm32-unknown-emscripten`；`--locked` 是 cargo 层参数，由构建脚本传入，不属于 `bun run build`），随后 `rm -rf dist` 并重建 SDK bundle；增量通常 1–2 分钟，冷构建更久。
2. `MISE_ENV=wasm` 注入 Emscripten 6.0.10；直接 `bun run build` 在仅有 mise shim 的环境会把 shim 路径当作工具链并失败。
3. 构建后确认三个产物存在：

```bash
cd npm-packages/@peri-sdk
ls -la dist/index.js dist/wasm/peri-wasm.js dist/wasm/peri_wasm.wasm
```

4. 基准只消费构建后的 `dist`。SDK 入口使用 `file:///.../npm-packages/@peri-sdk/dist/index.js`；WASM glue 使用 `dist/wasm/peri-wasm.js`。不要从 `src` 或上一轮进程缓存的模块启动。
5. 基准脚本放在仓库外（例如 `/tmp/sdk-wasm-capacity.mjs`），不要提交进仓库。

### 2.2 启动 sqld

1. 本地 sqld 必须在第一个 WASM 启动前可用。脚本默认在 `npm-packages/@peri-sdk` 下执行 `mise exec -- sqld`；也可用 `PERI_TEST_SQLD_BIN` 指向已安装的 sqld 可执行文件。
2. 每次 run 选一个空闲 TCP 端口和一个独立 `--db-path`，避免复用旧会话：

```bash
mkdir -p /tmp/sdk-wasm-capacity
cd npm-packages/@peri-sdk
mise exec -- sqld --no-welcome \
  --http-listen-addr 127.0.0.1:8081 \
  --db-path /tmp/sdk-wasm-capacity/sqld
```

3. 手工命令中的 8081 只是示例，执行前先确认端口空闲；脚本用 `unusedPort()` 选端口，并把 sqld 启动失败（含端口冲突）重试最多 5 次（见第 4 节 `startSqld`）。sqld 不会自动创建 `--db-path` 的父目录，手工执行时先 `mkdir -p`。
4. 等待 sqld 的 TCP 端口可连接后再启动第一个 Agent；否则 `createNewSession` 会以启动失败结束。
5. sqld 的 RSS 单独记录，约 32 MiB；不要把它加进 1 GiB 宿主预算。

### 2.3 模型配置

```js
const settings = {
  config: {
    active_alias: "haiku",
    providers: [{
      id: "bench",
      type: "openai",
      apiKey: "bench-placeholder",
      baseUrl: "http://127.0.0.1:9/v1",
    }],
    profiles: {
      haiku: { provider: "bench", model: "bench-model" },
    },
  },
};
```

1. 不需要真实模型凭证：`session/new` 不请求模型，provider 与 `apiKey` 非空即可。
2. `active_alias` 必须是合法档位 `fable`、`opus`、`sonnet`、`haiku` 之一。自造别名会在 WASM 启动时报 `No LLM provider configured in settings`，这会被包装成通用启动失败。
3. `baseUrl` 指向不可达的本地端口（例如 9），避免误触网和产生费用；不要放真实 key。
4. 传入 `runtime.onDiagnostic`，否则启动失败只能看到通用错误，无法区分配置、产物或链接失败。
5. 不要发送 prompt。一旦发送，测量对象就不再是空转 Agent，且可能触发真实模型调用。

### 2.4 进程与平台

1. 基线环境：Bun 1.4.3、macOS 26.6.2（arm64）、16 GiB RAM；主机负载约 1.7–2.3。
2. 需要 `Bun.gc(true)`；脚本启动时检查，不可用就中止，不要静默跳过 GC。
3. 每轮一个独立进程。不要在同一进程里先跑大 N 再跑小 N；WASM 线性内存和编译缓存都不会回退。
4. `ps -o rss=` 适用于 Unix-like 主机；Windows 需要替换 RSS 采集方式并单独记录，不能直接复用本协议的数字。
5. 测量期间不要并行跑其他基准、构建或大型测试；它们会改变可用内存和启动延迟。

## 3. 采样协议

### 3.1 单点采样

每次采样按固定顺序执行，不省略 sleep：

1. `sleep 150ms`
2. `Bun.gc(true)`
3. `sleep 80ms`
4. `Bun.gc(true)`
5. `sleep 120ms`
6. 读取 `ps -o rss= -p <host pid>`，KiB / 1024 = MiB
7. 同时记录 `process.memoryUsage().rss / 1048576`，只作对照

连续执行 3 次，取中位数作为该 N 的采样值。两次 GC 和等待是为了让 WASM 线性内存增长、异步任务和 finalizer 落到稳定状态；缩短等待会让相邻点抖动变大，历史数字不可比。

### 3.2 采样节奏

1. 每新增一个 Agent，先做一次 initial 采样（3 次中位数）。
2. 每 10 个 N（N 为 10 的倍数），静置 2 s 后再做一次 settled 采样（3 次中位数）；它只用于复核和尾部回归，不参与停止判定。
3. `initial` ≥ 1024 MiB 或达到 `--max` 时，最后一个成功点静置 10 s 后再做一次 settled 采样作为复核值；停止判定仍以该点的 `initial` 为准。启动失败没有可采样的新点，直接记录失败并停止。
4. 从 `--start` 指定的 N 起步时，锚点只采样一次；从下一个 N 起恢复逐点采样。锚点不额外做 2 s 复采；若它同时是最后一个点，按 10 s 末点规则处理。推荐用 `--start 200`。
5. Manager 构造后立即记录一次 `ps` 值作为基线；首增量以它为基准。该基线不参与 3 次中位数。
6. `peak` 取所有 initial 和 settled 采样的最大值，不含 Manager 基线；不要只取最后一个点。

### 3.3 停止与失败

1. 某 N 的 `initial` ≥ 1024 MiB：停止，记 `N_stop = N`；`N_max` 取此前最后一个 `initial` < 1024 MiB 的 N。`settled` 只用于复核：任何点若 `initial` 与 `settled` 分居 1024 两侧，不改变停止点，按未测稳处理并重跑。
2. 启动失败：记录 N、错误消息和堆栈或 cause，停止；失败的 N 不能算入容量，`N_max` 仍是此前最后一个成功的 N。不要静默重试，重试会改变样本总体。脚本对 `fatal` 和启动失败都返回退出码 1，人工复核时据此判定该轮无效；CI 不运行本基准。
3. 空 `sessionId`、重复 `sessionId`：按启动失败处理并停止。
4. 达到 `--max` 但 RSS 仍 < 1024 MiB：停止原因为 `hard-cap`，`N_max = --max` 只表示“测到上限”，不是容量结论；提高上限后用新进程重跑。
5. 停止检查发生在 `initial` 采样之后，所以 `N_stop` 的 `initial` 可能略高于 1024 MiB（历史 1025.7–1027.6 MiB）；这不是协议错误。
6. 运行中不要关闭 chat。关闭不会让 WASM 线性内存缩回，反而会改变后续点的累计峰值。

### 3.4 尾部回归

1. 取该轮最后 8 个已采样点，以 N 为 x、该点的最终 RSS（有 `settled` 取 `settled`，否则取 `initial`）为 y，做最小二乘。
2. 报告斜率（MiB/agent）、截距（MiB）、R²、`N_est`。
3. `R² < 0.95` 或尾部不足 8 个点：视为未测稳，不把 `N_est` 当作容量；延长静置或重跑。
4. 截距是局部拟合量，不等于 Manager 基线或首个 Agent 成本。三次优化的截距为 34.5 / 155.3 / 60.5 MiB，差异很大；不要用截距解释启动开销。
5. `N_est` 只表示拟合窗口内与 1024 的交点；实际容量以观察到的 `N_max` 为准。
6. 至少跑 2 轮独立进程，报告中位数与极差。单轮只能作为冒烟或排查，不能作为基线。

## 4. 脚本骨架

以下脚本是单文件、可直接复制运行的骨架，放在仓库外，例如 `/tmp/sdk-wasm-capacity.mjs`。运行前置是 Bun、`ps` 和可用的 sqld（默认经 `mise exec -- sqld` 启动，也可用 `PERI_TEST_SQLD_BIN` 指定）；脚本只消费已构建的 `dist`，不 import 仓库源码；`/tmp/peri-dist-bench/bench.mjs` 与 `/tmp/verify-capacity-dist/verify-capacity.mjs` 是历史实现，可能不存在，不作为依赖。脚本默认 `PERI_SDK_DIR=/Users/mino/code/peri/npm-packages/@peri-sdk`、`PERI_BENCH_DIR=/tmp/sdk-wasm-capacity`；仓库在别处或要换输出目录时用环境变量覆盖。执行前先把下面的代码块保存为 `/tmp/sdk-wasm-capacity.mjs`；文件不存在时命令会直接报错。

先用 3 个 Agent 冒烟，确认共享编译和 session 唯一性，再跑大轮：

```bash
# 冒烟：约 10 s 末点静置 + 启动时间
bun /tmp/sdk-wasm-capacity.mjs --run smoke --start 1 --max 3

# 全曲线：测首增量、完整 N_max（Manager 基线在任何 --start 下都会记录；约 5–6 分钟）
bun /tmp/sdk-wasm-capacity.mjs --run full --start 1 --max 260

# 同一协议的两轮独立进程：从 200 起步，预热 200 个并采样一次，再逐个加到阈值
bun /tmp/sdk-wasm-capacity.mjs --run 1 --start 200 --max 260
bun /tmp/sdk-wasm-capacity.mjs --run 2 --start 200 --max 260
```

```js
#!/usr/bin/env bun
/**
 * Peri SDK WASM agent capacity bench.
 * Keep this file outside the repository.
 *
 *   bun /tmp/sdk-wasm-capacity.mjs --run 1 --start 1 --max 260
 *   bun /tmp/sdk-wasm-capacity.mjs --run 2 --start 200 --max 260
 *
 * Protocol: one host Bun process; one ChatManager; one ACP session per chat;
 * idle after createNewSession (no prompt); serial start; ps RSS as primary.
 */
import { mkdir, realpath, stat, writeFile } from "node:fs/promises";
import { createServer, connect } from "node:net";
import { once } from "node:events";
import * as os from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const SDK_DIR = process.env.PERI_SDK_DIR ?? "/Users/mino/code/peri/npm-packages/@peri-sdk";
const SDK_ENTRY = resolve(SDK_DIR, "dist/index.js");
const MODULE_URL = pathToFileURL(resolve(SDK_DIR, "dist/wasm/peri-wasm.js"));
const BASE = process.env.PERI_BENCH_DIR ?? "/tmp/sdk-wasm-capacity";
const TARGET_MIB = 1024;
const START_WATCHDOG_MS = 30_000;

const argv = process.argv.slice(2);
const arg = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const runLabel = arg("--run", "1");
const startAt = Number(arg("--start", "1"));
const maxAgents = Number(arg("--max", "260"));
const outPath = arg("--out", `${BASE}/out/run-${runLabel}.json`);
if (!Number.isInteger(startAt) || startAt < 1) throw new Error("--start must be a positive integer");
if (!Number.isInteger(maxAgents) || maxAgents < startAt) throw new Error("--max must be >= --start");
if (typeof Bun.gc !== "function") throw new Error("This bench requires Bun.gc(true)");

const round = (value, digits = 3) =>
  value === null || value === undefined || !Number.isFinite(value)
    ? null
    : Number(value.toFixed(digits));

const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
};

function percentile(values, p) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const index = (sorted.length - 1) * p;
  const lo = Math.floor(index);
  const hi = Math.ceil(index);
  if (lo === hi) return sorted[lo];
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (index - lo);
}

function linreg(points) {
  const n = points.length;
  if (n < 2) return null;
  const xs = points.map((point) => point.n);
  const ys = points.map((point) => point.rssMiB);
  const mx = xs.reduce((a, b) => a + b, 0) / n;
  const my = ys.reduce((a, b) => a + b, 0) / n;
  let sxx = 0;
  let sxy = 0;
  let syy = 0;
  for (let i = 0; i < n; i++) {
    const dx = xs[i] - mx;
    const dy = ys[i] - my;
    sxx += dx * dx;
    sxy += dx * dy;
    syy += dy * dy;
  }
  if (sxx === 0) return null;
  const slope = sxy / sxx;
  const intercept = my - slope * mx;
  let ssRes = 0;
  for (let i = 0; i < n; i++) {
    const error = ys[i] - (intercept + slope * xs[i]);
    ssRes += error * error;
  }
  const r2 = syy === 0 ? null : 1 - ssRes / syy;
  return {
    window: [xs[0], xs[n - 1]],
    slope,
    intercept,
    r2,
    nEst: slope > 0 ? (TARGET_MIB - intercept) / slope : null,
  };
}

async function psRssMiB(pid) {
  const child = Bun.spawn(["ps", "-o", "rss=", "-p", String(pid)], {
    stdin: "ignore",
    stdout: "pipe",
    stderr: "ignore",
  });
  const text = await new Response(child.stdout).text();
  const status = await child.exited;
  if (status !== 0) throw new Error(`ps exited ${status}`);
  const kib = Number(text.trim());
  if (!Number.isFinite(kib) || kib <= 0) {
    throw new Error(`ps rss parse failed: ${JSON.stringify(text)}`);
  }
  return kib / 1024;
}

function gitHead(cwd) {
  try {
    const child = Bun.spawnSync(["git", "-C", cwd, "rev-parse", "HEAD"], {
      stdin: "ignore",
      stdout: "pipe",
      stderr: "ignore",
    });
    if (child.exitCode !== 0) return null;
    return child.stdout.toString().trim() || null;
  } catch {
    return null;
  }
}

async function fileMtime(path) {
  try {
    return (await stat(path)).mtime.toISOString();
  } catch {
    return null;
  }
}

function osProductVersion() {
  if (process.platform !== "darwin") return null;
  try {
    const child = Bun.spawnSync(["sw_vers", "-productVersion"], {
      stdin: "ignore",
      stdout: "pipe",
      stderr: "ignore",
    });
    return child.exitCode === 0 ? child.stdout.toString().trim() || null : null;
  } catch {
    return null;
  }
}

async function oneSample() {
  await Bun.sleep(150);
  Bun.gc(true);
  await Bun.sleep(80);
  Bun.gc(true);
  await Bun.sleep(120);
  return {
    ps: await psRssMiB(process.pid),
    proc: process.memoryUsage().rss / 1048576,
  };
}

async function sampleMedian() {
  const raw = [];
  for (let i = 0; i < 3; i++) raw.push(await oneSample());
  return {
    psMiB: round(median(raw.map((sample) => sample.ps))),
    processMiB: round(median(raw.map((sample) => sample.proc))),
    rawPsMiB: raw.map((sample) => round(sample.ps)),
  };
}

async function unusedPort() {
  const listener = createServer();
  listener.listen(0, "127.0.0.1");
  await once(listener, "listening");
  const address = listener.address();
  if (!address || typeof address === "string") throw new Error("no TCP port");
  await new Promise((done) => listener.close(done));
  return address.port;
}

async function waitForSqld(port, child) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (child.exitCode !== null) throw new Error(`sqld exited (${child.exitCode})`);
    try {
      const socket = connect(port, "127.0.0.1");
      await once(socket, "connect");
      socket.destroy();
      return;
    } catch {
      await Bun.sleep(50);
    }
  }
  throw new Error("sqld did not listen within ten seconds");
}

async function startSqld(command, dbPath) {
  let lastError = null;
  for (let attempt = 1; attempt <= 5; attempt++) {
    // Keep spawn inside the retry boundary: a missing binary or port race must count as an attempt.
    let child = null;
    try {
      const port = await unusedPort();
      child = Bun.spawn([
        ...command,
        "--no-welcome",
        "--http-listen-addr", `127.0.0.1:${port}`,
        "--db-path", dbPath,
      ], {
        cwd: SDK_DIR,
        stdin: "ignore",
        stdout: "ignore",
        stderr: "pipe",
      });
      // Drain stderr so a chatty sqld cannot block on a full pipe.
      new Response(child.stderr).text().catch(() => {});
      await waitForSqld(port, child);
      // A connect can succeed against a non-sqld listener holding the raced port;
      // give sqld a beat to exit before declaring success.
      await Bun.sleep(100);
      if (child.exitCode !== null) throw new Error(`sqld exited during startup (${child.exitCode})`);
      return { child, port, attempt };
    } catch (error) {
      lastError = error;
      if (child && child.exitCode === null) child.kill("SIGTERM");
      if (child) await child.exited;
      await Bun.sleep(100);
    }
  }
  throw new Error(
    `sqld did not start after 5 attempts: ${lastError instanceof Error ? lastError.message : String(lastError)}`,
  );
}

// Install instrumentation before importing the SDK; the artifact cache calls
// WebAssembly.compile on first activation.
const wasmStats = {
  compileCalls: 0,
  compileByteLengths: [],
  instantiateCalls: 0,
  sources: { module: 0, arraybuffer: 0, view: 0, other: 0 },
};
const nativeCompile = WebAssembly.compile;
const nativeInstantiate = WebAssembly.instantiate;
WebAssembly.compile = function compile(bytes, options) {
  wasmStats.compileCalls++;
  wasmStats.compileByteLengths.push(bytes?.byteLength ?? null);
  return nativeCompile.call(WebAssembly, bytes, options);
};
WebAssembly.instantiate = function instantiate(source, imports) {
  wasmStats.instantiateCalls++;
  if (source instanceof WebAssembly.Module) wasmStats.sources.module++;
  else if (source instanceof ArrayBuffer) wasmStats.sources.arraybuffer++;
  else if (ArrayBuffer.isView(source)) wasmStats.sources.view++;
  else wasmStats.sources.other++;
  return nativeInstantiate.call(WebAssembly, source, imports);
};
const restoreWasm = () => {
  WebAssembly.compile = nativeCompile;
  WebAssembly.instantiate = nativeInstantiate;
};

const result = {
  run: runLabel,
  meta: {
    bun: Bun.version,
    pid: process.pid,
    platform: process.platform,
    arch: process.arch,
    osVersion: os.version(),
    osRelease: os.release(),
    osProductVersion: osProductVersion(),
    totalMemMiB: Math.round(os.totalmem() / 1048576),
    loadavgStart: os.loadavg().map((value) => round(value, 2)),
    commit: null,
    distIndexMtime: null,
    startedAt: new Date().toISOString(),
  },
  config: {
    sdkEntry: SDK_ENTRY,
    moduleUrl: MODULE_URL.href,
    startAt,
    maxAgents,
    targetMiB: TARGET_MIB,
    primaryMetric: "ps -o rss=",
    stopBasis: "initial",
    settledRole: "review",
    sampling: "3x[sleep150,gc,sleep80,gc,sleep120] median; 2s every 10; 10s at final",
  },
  sqld: null,
  baseline: {},
  points: [],
  failures: [],
  stop: null,
  wasm: wasmStats,
  summary: null,
  cleanup: [],
  fatal: null,
};

let sqld = null;
let manager = null;
const chats = []; // Keep chat handles alive for the whole run.
const sessionIds = new Set();
const sessionIdList = [];
const startupTimes = [];
let peakMiB = 0;
let stop = null;

try {
  await mkdir(resolve(BASE, "out"), { recursive: true });
  result.meta.commit = gitHead(SDK_DIR);
  result.meta.distIndexMtime = await fileMtime(SDK_ENTRY);
  const runRoot = resolve(BASE, "work", `${runLabel}-${Date.now()}`);
  const workspaceDir = resolve(runRoot, "ws");
  const dbPath = resolve(runRoot, "sessions");
  await mkdir(workspaceDir, { recursive: true });

  // sqld is a separate process; its RSS is recorded but excluded from the host budget.
  const sqldCommand = process.env.PERI_TEST_SQLD_BIN
    ? [process.env.PERI_TEST_SQLD_BIN]
    : ["mise", "exec", "--", "sqld"];
  const startedSqld = await startSqld(sqldCommand, dbPath);
  sqld = startedSqld.child;
  const sqlPort = startedSqld.port;
  result.sqld = {
    pid: sqld.pid,
    port: sqlPort,
    attempts: startedSqld.attempt,
    dbPath,
    rssStartMiB: round(await psRssMiB(sqld.pid)),
  };

  const sdk = await import(pathToFileURL(SDK_ENTRY).href);
  const { ChatManager, ChatUserAction } = sdk;
  if (typeof ChatManager !== "function") {
    throw new Error(`${SDK_ENTRY} does not export ChatManager`);
  }
  if (!ChatUserAction || typeof ChatUserAction.createNewSession !== "function") {
    throw new Error(`${SDK_ENTRY} does not export ChatUserAction.createNewSession`);
  }
  result.baseline.afterSdkImportMiB = round(await psRssMiB(process.pid));

  const database = {
    listSessions: async () => [],
    getSession: async () => null,
  };
  const kvStore = new Map();
  const kv = {
    getItem: async (key) => kvStore.get(key) ?? null,
    setItem: async (key, value) => {
      kvStore.set(key, structuredClone(value));
    },
    removeItem: async (key) => {
      kvStore.delete(key);
    },
  };
  const settings = {
    config: {
      active_alias: "haiku",
      providers: [{
        id: "bench",
        type: "openai",
        apiKey: "bench-placeholder",
        baseUrl: "http://127.0.0.1:9/v1",
      }],
      profiles: {
        haiku: { provider: "bench", model: "bench-model" },
      },
    },
  };
  const workspace = await realpath(workspaceDir);
  const workspaceRef = {
    id: `ws:${workspace}`,
    hostPath: workspace,
    runtimePath: workspace,
  };

  manager = new ChatManager({
    store: {
      storage: {
        url: `http://127.0.0.1:${sqlPort}`,
        authToken: "local-dev",
      },
      machineId: "00000000-0000-4000-8000-0000000000b0",
    },
    database,
    kv,
    chatConfig: {
      runtime: {
        kind: "wasm",
        moduleUrl: MODULE_URL,
        onDiagnostic: (kind, error) => {
          console.error(`[diagnostic] ${kind}: ${error?.message ?? error}`);
        },
      },
      session: {},
      lifecycle: {
        activationCloseTimeoutMs: 20_000,
        closeDrainTimeoutMs: 20_000,
      },
    },
  });
  result.baseline.afterManagerMiB = round(await psRssMiB(process.pid));
  result.baseline.processRssAfterManagerMiB = round(process.memoryUsage().rss / 1048576);
  // peakMiB stays 0: peak covers agent initial/settled samples, not the Manager baseline.

  const startOne = async () => {
    const chat = manager.createChat({
      workspace: workspaceRef,
      agent: { settings },
    });
    const started = performance.now();
    let timer;
    const watchdog = new Promise((_, reject) => {
      timer = setTimeout(
        () => reject(new Error(`startup watchdog ${START_WATCHDOG_MS}ms`)),
        START_WATCHDOG_MS,
      );
    });
    try {
      const ready = await Promise.race([
        chat.execute(ChatUserAction.createNewSession({})),
        watchdog,
      ]);
      const elapsed = performance.now() - started;
      if (!ready || typeof ready.sessionId !== "string" || ready.sessionId.length === 0) {
        throw new Error(`createNewSession returned no sessionId: ${JSON.stringify(ready)}`);
      }
      if (sessionIds.has(ready.sessionId)) {
        throw new Error(`duplicate sessionId ${ready.sessionId}`);
      }
      sessionIds.add(ready.sessionId);
      sessionIdList.push(ready.sessionId);
      chats.push(chat);
      startupTimes.push(elapsed);
      return { elapsed, sessionId: ready.sessionId };
    } finally {
      clearTimeout(timer);
    }
  };

  const points = [];
  for (let n = 1; n <= maxAgents; n++) {
    let started;
    try {
      started = await startOne();
    } catch (error) {
      result.failures.push({
        n,
        message: error instanceof Error ? error.message : String(error),
        cause: error instanceof Error && error.cause ? String(error.cause) : null,
        stack: error instanceof Error ? error.stack : undefined,
      });
      stop = { n, reason: "startup-failure" };
      break;
    }

    if (n < startAt) {
      if (n % 50 === 0) console.log(`[run ${runLabel}] warmed ${n}/${startAt - 1}`);
      continue;
    }

    const initial = await sampleMedian();
    peakMiB = Math.max(peakMiB, initial.psMiB);
    const point = {
      n,
      sessionId: started.sessionId,
      startupMs: round(started.elapsed),
      initial,
      settled: null,
      finalSource: "initial",
    };
    // Capacity stop is decided by the post-startup initial sample; settled only reviews it.
    const hitTarget = initial.psMiB >= TARGET_MIB;
    const atCap = n >= maxAgents;
    const isAnchor = n === startAt;

    // Warm-start anchor: one sample at N=200, then continue one by one.
    if (isAnchor && !hitTarget && !atCap) {
      point.finalRssMiB = initial.psMiB;
      points.push(point);
      console.log(
        `[run ${runLabel}] N=${n} anchor ps=${point.finalRssMiB}MiB ` +
        `proc=${initial.processMiB}MiB start=${point.startupMs}ms`,
      );
      continue;
    }

    if (!hitTarget && !atCap && !isAnchor && n % 10 === 0) {
      await Bun.sleep(2_000);
      point.settled = await sampleMedian();
      point.finalSource = "settled-2s";
      point.finalRssMiB = point.settled.psMiB;
      peakMiB = Math.max(peakMiB, point.settled.psMiB);
    } else {
      point.finalRssMiB = initial.psMiB;
    }

    if (hitTarget || atCap) {
      await Bun.sleep(10_000);
      point.settled = await sampleMedian();
      point.finalSource = "settled-10s";
      point.finalRssMiB = point.settled.psMiB;
      peakMiB = Math.max(peakMiB, point.settled.psMiB);
      stop = {
        n,
        reason: hitTarget ? "rss" : "hard-cap",
        initialRssMiB: initial.psMiB,
        settledRssMiB: point.settled.psMiB,
      };
    }

    points.push(point);
    console.log(
      `[run ${runLabel}] N=${n} ps=${point.finalRssMiB}MiB ` +
      `proc=${(point.settled ?? initial).processMiB}MiB ` +
      `start=${point.startupMs}ms src=${point.finalSource} ` +
      `compiles=${wasmStats.compileCalls} inst=${wasmStats.instantiateCalls}`,
    );
    if (stop) break;
  }

  result.points = points;
  result.stop = stop;
  const successful = points.filter((point) => Number.isFinite(point.initial?.psMiB));
  const lastUnder = [...successful].reverse().find((point) => point.initial.psMiB < TARGET_MIB) ?? null;
  const tail = successful.slice(-8);
  const regression = tail.length >= 2
    ? linreg(tail.map((point) => ({ n: point.n, rssMiB: point.finalRssMiB })))
    : null;
  const first = points.find((point) => point.n === 1);
  const n200 = points.find((point) => point.n === 200);
  const firstIncrementMiB = first && Number.isFinite(result.baseline.afterManagerMiB)
    ? round(first.initial.psMiB - result.baseline.afterManagerMiB)
    : null;

  result.summary = {
    nMax: lastUnder ? lastUnder.n : null,
    nStop: stop && stop.reason === "rss" ? stop.n : null,
    stopReason: stop ? stop.reason : null,
    pointCount: successful.length,
    peakRssMiB: successful.length ? round(peakMiB) : null,
    lastPointRssMiB: successful.length ? successful[successful.length - 1].finalRssMiB : null,
    n200RssMiB: n200 ? n200.finalRssMiB : null,
    firstIncrementMiB,
    startupMs: {
      count: startupTimes.length,
      p50: round(percentile(startupTimes, 0.5)),
      p95: round(percentile(startupTimes, 0.95)),
      min: round(Math.min(...startupTimes)),
      max: round(Math.max(...startupTimes)),
    },
    regression: regression
      ? {
        window: regression.window,
        tailCount: tail.length,
        slopeMbPerAgent: round(regression.slope),
        interceptMiB: round(regression.intercept),
        r2: round(regression.r2, 6),
        nEst: round(regression.nEst, 2),
        stable: tail.length >= 8 && regression.r2 !== null && regression.r2 >= 0.95,
      }
      : null,
    wasm: {
      compileCalls: wasmStats.compileCalls,
      compileByteLengths: wasmStats.compileByteLengths,
      instantiateCalls: wasmStats.instantiateCalls,
      instantiateSources: { ...wasmStats.sources },
    },
    sessionIds: {
      count: sessionIdList.length,
      unique: new Set(sessionIdList).size,
    },
    failures: result.failures.length,
    sqldRssEndMiB: null,
  };
  if (sqld.exitCode === null) {
    result.summary.sqldRssEndMiB = round(await psRssMiB(sqld.pid));
    result.sqld.rssEndMiB = result.summary.sqldRssEndMiB;
  }
} catch (error) {
  result.fatal = {
    message: error instanceof Error ? error.message : String(error),
    stack: error instanceof Error ? error.stack : undefined,
  };
  console.error(`[run ${runLabel}] FATAL: ${result.fatal.message}`);
} finally {
  restoreWasm();
  result.meta.loadavgEnd = os.loadavg().map((value) => round(value, 2));
  result.meta.finishedAt = new Date().toISOString();
  await mkdir(resolve(BASE, "out"), { recursive: true });
  await writeFile(outPath, `${JSON.stringify(result, null, 2)}\n`);

  if (manager) {
    try {
      const outcome = await Promise.race([
        manager.shutdown().then(
          () => "completed",
          (error) => `failed: ${error instanceof Error ? error.message : String(error)}`,
        ),
        Bun.sleep(30_000).then(() => "timeout"),
      ]);
      result.cleanup.push(`manager.shutdown: ${outcome}`);
    } catch (error) {
      result.cleanup.push(
        `manager.shutdown threw: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }
  if (sqld && sqld.exitCode === null) {
    sqld.kill("SIGTERM");
    const exited = await Promise.race([
      sqld.exited,
      Bun.sleep(5_000).then(() => "timeout"),
    ]);
    result.cleanup.push(`sqld: ${exited === "timeout" ? "kill timeout" : "exited"}`);
  }

  // Rewrite after cleanup so the JSON carries cleanup evidence.
  await writeFile(outPath, `${JSON.stringify(result, null, 2)}\n`);
  console.log(`[run ${runLabel}] wrote ${outPath}`);
  process.exit(result.fatal || result.failures.length > 0 ? 1 : 0);
}
```

## 5. 结果解读

### 5.1 先检查共享编译

| 检查 | 通过条件 | 不通过的含义 |
| --- | --- | --- |
| `wasm.compileCalls` | 1 | `0` 不能单独判定旧路径：旧 dist 的 0 主因是旧 loader 没有 `WebAssembly.compile`；Node 回退、注入 `moduleFactory` 也会得到 0。`instantiateWasm`/`locateFile` 只在直接调用 loader、经 `PeriWasmInstantiateOptions.moduleOptions`（`loader.ts:31-32`）注入时绕过共享缓存；`ChatManager` 不向 loader 透传 `moduleOptions`（`manager.ts:152-160`），基准路径不会因这两个选项得到 0。需同时看 `instantiateSources`（`module` vs `arraybuffer`/`view`）和 `dist/index.js` 是否包含 `loadWasmArtifact`/`WebAssembly.compile` 缓存标记。>1 表示存在多个产物 URL 或绕过了共享缓存。 |
| `wasm.instantiateCalls` | 等于本轮实际启动的 Agent 数；0 失败时即 `sessionIds.count` | 少于启动数说明有路径没有实例化；多于启动数说明有额外实例或失败启动也实例化了。 |
| `wasm.instantiateSources.module` | 等于 `instantiateCalls` | `arraybuffer` / `view` > 0 表示还在用旧 bytes 路径（旧 loader、旧 dist 或 Node 回退）。 |
| `sessionIds.count / unique` | 二者相等，且每个 `sessionId` 非空 | 重复或空 session 必须按失败处理。 |
| `failures` | 0 | 非 0 时先看第 7 节，再决定是否重跑。 |
| `regression.stable` | `true`（尾部至少 8 点且 R² ≥ 0.95） | 尾部不足或抖动过大；不要用该轮 `N_est` 下结论。 |
| `stopReason` | `rss` | `hard-cap` 只说明测到上限；`startup-failure` 说明容量被启动失败截断。 |

### 5.2 再读容量

1. 用 `nMax` 作为本轮容量，用 `nStop` 作为首个 RSS 越界点（仅 `stopReason=rss` 时存在；`hard-cap` / 启动失败看 `stopReason` 与 `stop.n`）；两者都由各 N 的 `initial` 判定，通常相差 1。
2. `nEst` 应与 `nMax` / `nStop` 接近（历史差 < 1.5）；若差很多，先看 R² 和尾部窗口，不要直接改容量。
3. `peakRssMiB` 是采样峰值，可能比 1024 高 1–4 MiB；`N_stop` 的 `initial` 也会略高于 1024。这是停止检查发生在 `initial` 采样之后的正常结果。
4. `n200RssMiB` 用于快速对照 200 起点；历史为 941–946 MiB。`--start 1` 时它是 N=200 的 2 s settled，`--start 200` 时它是预热锚点 initial，差异按采样噪声处理。
5. `firstIncrementMiB` 只在 `--start 1` 时存在。它包含一次性编译、Emscripten 运行时和首个 session；不要拿它除 N 得到边际成本。
6. `slopeMbPerAgent` 是尾部边际成本，历史约 4.2 MiB；它与首增量不是同一个量。

### 5.3 复测表模板

每轮独立进程填一行，至少两轮后计算中位数与极差：

| 指标 | run 1 | run 2 | 中位数 | 极差 |
| --- | ---: | ---: | ---: | ---: |
| `N_max` | | | | |
| `N_stop` | | | | |
| `slopeMbPerAgent` | | | | |
| `R²` | | | | |
| `firstIncrementMiB` | | | | |
| `startup p50 / p95` | | | | |
| `peakRssMiB` | | | | |
| `n200RssMiB` | | | | |
| `sqldRssEndMiB` | | | | |
| `failures` | | | | |

### 5.4 偏离基线时的排查顺序

1. `compileCalls > 1`：检查是否解析出多个不同的 wasm 绝对 URL、是否绕过 SDK loader、是否混用旧 dist；共享缓存按绝对 URL 键控。`ChatCreateOptions.runtime` 只有 `env` 和 `onDiagnostic`，没有 `moduleUrl`；`moduleUrl` 只在 Manager 级 `chatConfig.runtime` 设置，不能按 chat 覆盖产物位置。
2. `compileCalls == 0`：不能单独断定仍走旧路径。旧 dist 为 0 的主因是旧 loader 没有 `WebAssembly.compile`；Node 回退、注入 `moduleFactory` 也会为 0。`instantiateWasm`/`locateFile` 只经 `PeriWasmInstantiateOptions.moduleOptions`（`loader.ts:31-32`）影响直接调用 loader 的宿主，`ChatManager` 不透传 `moduleOptions`（`manager.ts:152-160`），基准路径不会因它们为 0。先看 `instantiateSources`：`arraybuffer`/`view` 等于 `instantiateCalls` 表示旧 bytes 路径；`module` 等于 `instantiateCalls` 表示预编译或注入模块路径。再检查 `dist/index.js` 是否包含 `loadWasmArtifact`/`WebAssembly.compile`（共享缓存实现），并确认其构建时间晚于 `dcb76ed6b`。若 `compileCalls=1` 但 `arraybuffer`/`view` > 0，同样说明没有把共享 Module 交给 glue。
3. p50 接近 79–80 ms：很像优化前的每 Agent 编译路径；确认 dist 是最新提交构建、进程是新建的。
4. `N_max` 比 219–222 低很多：先确认没有并行构建/测试、sqld 未计入、没有发送 prompt、启动失败为 0；再比较 Bun 版本和主机内存。
5. 启动失败含 `No LLM provider configured in settings`：`active_alias` 不是合法档位，改成 `haiku` 等四档之一。
6. 启动等待约 20 s 后失败：可能是 wasm 产物缺失或 Emscripten 链接失败；检查 `dist/wasm/peri_wasm.wasm`，并看 `onDiagnostic` 输出。SDK 的启动预算是 20 s，脚本外层 watchdog 是 30 s。
7. `R² < 0.95` 或尾部不足 8 点：该轮未测稳；延长静置、减少并行负载后重跑，不要只挑高 R² 的窗口。

## 6. 历史基线

### 6.1 环境与来源

1. 日期：2026-10-28（+0800）；分支 `feat/wasm-module-share`。优化前数据对应父提交 `34307c5f7`（原始 JSON 未记录 HEAD，按 09:37–09:40 +0800 的时间戳推断）；优化后数据对应 `dcb76ed6b`（10:41 +0800）。
2. 环境：Bun 1.4.3、macOS 26.6.2（arm64）、16 GiB RAM；主机负载约 1.7–2.3。
3. 优化前原始 JSON：`/tmp/peri-wasm-capacity/out/run-{1,2,3}.json`。
4. 优化后原始 JSON：`/tmp/peri-dist-bench/out/run-{1,2}.json`；独立复核：`/tmp/verify-capacity-dist/out/full.json`。
5. 复核过程记录在 `.peri/plans/verification-report-dist-capacity.md`（工作目录，已被 `.gitignore` 忽略，不入库）。
6. 上述 `/tmp` 产物不保证长期存在；本文的汇总值是持久事实源，重跑时按本文协议生成新 JSON，不要覆盖历史基线。

### 6.2 优化后三次 run

| 指标 | run 1 | run 3（独立复核） | run 2 | 中位数 | 极差 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `N_max` | 219 | 220 | 222 | 220 | 219–222 |
| `N_stop` | 220 | 221 | 223 | 221 | 220–223 |
| `slopeMbPerAgent` | 4.508 | 4.380 | 3.905 | 4.380 | 3.905–4.508 |
| `R²` | 0.99207 | 0.99071 | 0.99617 | 0.99207 | 0.99071–0.99617 |
| `firstIncrementMiB` | 111.1 | 114.0 | 110.8 | 111.1 | 110.8–114.0 |
| `startup p50 ms` | 45.92 | 45.457 | 46.235 | 45.92 | 45.457–46.235 |
| `startup p95 ms` | 49.536 | 49.910 | 50.01 | 49.910 | 49.536–50.01 |
| `peakRssMiB`（all samples） | 1025.922 | 1027.578 | 1025.734 | 1025.922 | 1025.734–1027.578 |
| `n200RssMiB` | 945.719 | 944.109 | 941.406 | 944.109 | 941.406–945.719 |
| `sqldRssEndMiB` | 32.531 | 31.984 | 32.563 | 32.531 | 31.984–32.563 |
| Manager 基线 MiB | 29.531 | 29.188 | 30.594 | 29.531 | 29.188–30.594 |

三次 run 的 sessionId 全部唯一，失败数 0，停止原因均为 `rss`。独立复核（run 3）额外插桩了 WebAssembly：`compileCalls=1`、`instantiateCalls=221`、`instantiateSources.module=221`、`arraybuffer` / `view` 均为 0；run 1 / run 2 未记录该字段。独立复核还记录了 sqld 启动约 15.7 MiB、结束 32.0 MiB，以及 `manager.shutdown: completed`、`sqld: exited` 的清理结果。

### 6.3 优化前后对照

| 指标 | 优化前 | 优化后 |
| --- | ---: | ---: |
| `N_max` | 20 | 219–222 |
| `N_stop` | 21 | 220–223 |
| 每 Agent 边际 | 39.6 MiB | 3.905–4.508 MiB（约 4.2） |
| 首个 Agent 增量 | +118.8 MiB | +110.8–114.0 MiB |
| `startup p50` | 79.5 ms | 45.457–46.235 ms |
| `peakRssMiB` | 约 1032.5–1036.0 | 约 1025.7–1027.6 |
| N=20 总 RSS | 约 1015–1019 MiB | — |

优化前三次 run 的斜率分别为 40.119 / 38.823 / 39.595（中位 39.595），R² 0.99268 / 0.99375 / 0.99332，首增量 118.812 / 117.016 / 119.907（中位 118.812），p50 79.517 / 79.923 / 79.488（中位 79.517）。优化前 N=20 的 RSS 为 1015.172 / 1015.688 / 1019.438 MiB；N=21 为 1032.719 / 1032.141 / 1035.719 MiB。

### 6.4 数字之间的关系

1. 优化前每个 Agent 都会重新编译约 10 MB 的 wasm；在该 Bun/JSC 版本上，同一 buffer 连续编译也不复用编译产物，所以斜率约 39.6 MiB/agent。
2. 共享 `WebAssembly.Module` 后，编译只在首次发生；后续 Agent 只承担实例、ACP session 和运行期数据，斜率降到约 4.2 MiB/agent。
3. 首个 Agent 仍包含一次性编译、Emscripten 运行时和首个 session，所以首增量只从 118.8 降到 111–114 MiB，并没有同比例下降；容量提升主要来自“不再每个 Agent 编译一次”。
4. `N_max` 的 3 个 Agent 跨度（219–222）来自尾部线性内存页增长和 GC 抖动；实测跨度 3 个 Agent（219–222），全宽约 1.4%（半幅约 ±0.7%，中位数 220），不是逻辑错误。

## 7. 边界与常见误判

### 7.1 适用边界

1. **只适用于空转。** 每个 session 只 `createNewSession`，不发送 prompt、不调用工具。真实对话会随上下文、工具结果和 compact 历史增长；本基准的 N 不能直接当作生产对话并发上限。
2. **只验证 Bun。** Bun 的 `fetch(file://)` 可用，能走共享编译；Node 的 `fetch(file://)` 不可用，会回退到 glue 自带的文件加载路径。功能不变，但没有共享编译收益。Node 上预期 `compileCalls=0`，`instantiateSources.arraybuffer/view` 会大于 0，不要拿来和 Bun 数字比较。
3. **Emscripten 钩子没有 reject 通道。** 共享 Module 的 `WebAssembly.instantiate` 链接失败时只能记录错误，factory Promise 不会 reject；SDK 的 20 s 启动超时是唯一兜底。脚本外层用 30 s watchdog，看到约 20 s 启动失败应按链接/产物问题排查，不要当成进程挂死。
4. **编译缓存按 URL 常驻。** `artifacts` 以 wasm 绝对 URL 为键、进程级缓存；替换 wasm 产物后必须重启进程，否则仍在用旧 Module。每轮基准都应是新进程，构建后也不要复用旧进程。
5. **容量依赖主机和 Bun 版本。** 219–222 只代表基线环境（macOS arm64、Bun 1.4.3、16 GiB）。换 CPU、内存压力、Bun/JSC 版本或操作系统都要重测，不能直接套用。
6. **WASM 线性内存不会缩回。** 实例增长过的内存页不会因为关闭 chat 而归还；常驻值由初始化峰值决定。因此不能在同一进程里先跑大 N 再跑小 N，也不能用“关闭后 RSS”证明容量。
7. **`ps` 是 Unix 口径。** Windows 需要换 RSS 采集方式；不同工具的 RSS 口径（是否含共享页、是否含子进程）不同，不能混用。
8. **sqld 不计入宿主。** sqld 独立进程约 32 MiB；把它加进 1 GiB 会得到另一条曲线。若业务要求端到端总内存，应另立口径并同时报告宿主与 sqld。
9. **容量是同时存活数，不是吞吐。** 本文不测请求速率、并发创建/关闭、长时稳定性或 GC 压力。

### 7.2 常见误判

1. **用 `process.memoryUsage().rss` 当主指标。** WASM 线性内存会被低报：优化前 N=21 差 359.5 MiB（三次范围 359.5–388.3），优化后各轮接近阈值时的最大差约 174.1–176.5 MiB。看到 850 MiB 不代表容量更高，只是口径不同。
2. **把启动失败算进容量。** 启动失败的 N 没有 session，不能计入 `N_max`；必须记录 failure 并停止，否则会把“启动不了”误读成“还能跑”。
3. **把回归截距当 Manager 基线或首增量。** 截距是尾部 8 点的局部拟合，三次为 34.5 / 155.3 / 60.5 MiB；Manager 基线应看 `baseline.afterManagerMiB`，首增量应看 N=1 的 `initial` 与它的差。
4. **把 `N_est` 当 `N_max`。** `N_est` 是拟合窗口内与 1024 的交点，不是观察到的容量；R² < 0.95 时连拟合都不可靠。
5. **用自造 `active_alias`。** `active_alias: "bench"` 之类会在启动时报 `No LLM provider configured in settings`；只使用 `fable`、`opus`、`sonnet`、`haiku`。
6. **不加 `onDiagnostic`。** 启动失败会被包装成通用错误；没有诊断回调很难区分配置、产物、链接和超时。
7. **在同一进程跑两轮。** 编译缓存、WASM 线性内存和 ChatManager 状态都会残留；两轮必须两个独立进程。
8. **在 import SDK 之后才 patch `WebAssembly`。** 共享编译在首次激活前就可能发生，patch 晚了计数会漏；必须在 `await import` 之前安装。
9. **以为优化后 `compileCalls` 应等于 N。** 优化后应恒为 1；等于 N 说明每个 Agent 都触发了编译（例如每个 Agent 解析出不同 URL）。优化前 loader 没有 `WebAssembly.compile`，走 glue 的 `WebAssembly.instantiate(binary)`；旧路径的典型组合是 `compileCalls=0`、`instantiateCalls=N`、`instantiateSources.view/arraybuffer=N`，不要用 `compileCalls=N` 描述优化前。
10. **把峰值必须 ≤ 1024 当验收条件。** 停止检查在 `initial` 采样之后，峰值 1025.7–1027.6 MiB 是正常现象；判据是 `N_max` 的 `initial` < 1024 且 `N_stop` 的 `initial` ≥ 1024，10 s `settled` 只作复核。
11. **每次从 1 开始复测阈值。** 从 200 起步只需约 1 分钟，且能直接观察 200→阈值；`afterManagerMiB` 在任何 `--start` 下都会记录，只有首增量（`firstIncrementMiB`）和完整曲线才要求从 1 开始。
12. **复测时不记录环境。** 脚本 `meta` 已采集 Bun 版本、`os.version()`/`os.release()`（macOS 另记 `sw_vers -productVersion`）、arch、总内存、启动/结束 `loadavg`、仓库 `HEAD` 和 `dist/index.js` 的 mtime（构建时间近似）；报告时一并带上，否则数字无法与历史基线比较。历史 JSON 只记录了部分环境字段，没有 commit、dist mtime、OS 版本和主机负载，不能反推。
