# 第二轮：main archive 的 release 配置对照

日期：2026-10-07。状态：两组串行构建完成、第二轮证据核验通过、父 agent 两组 ACP smoke PASS；未修改生产源码或配置。

## 输入与授权边界

使用父 agent 已准备的 `.tmp/main-release-source`，对应 main 固定提交 `d7ee444efe7a461b0696bd6c27c6c91f946fdec1`；不读取 main 工作区 WIP，不修改 snapshot source、production 文件或两份 Cargo.lock，不新增第四轴。

第一轮正式三组已经完成。本轮复用其本 worktree `target`，不重新构建或重新测量第一轮，也不使用原 worktree target。构建顺序为 `main_fat1` → `main_thin16`，严格串行，完成后立即复制独立 binary。

| 固定项 | 值 |
| --- | --- |
| archive tracked blob 核验 | 构建前 2,147 个 blob，source 差异 0 |
| manifest SHA-256 | `49507b5ca807c49c5b6d1e11afbd5d230d9f53d828f2f660fc86e3aea60ac45a` |
| main Cargo.lock 构建前 SHA-256 | `905becbdd5f03771813d66b7a9774da0b3c29921df6421a79c0ddfb5fbe1ebdc` |
| Workflow dist JS SHA-256 | `0ca2007a66096bfa792bbaed1ba8f57142bdbd5dcb670ed53a395920ec8947a4`，与第一轮相同 |
| Rust / host | Rust 1.99.0、内置 LLVM23.1.1、aarch64-apple-darwin |
| 基础 release | opt-level=z、strip=symbols、默认 features、默认 panic unwind |

源码完整性以 `git ls-tree -r -z <commit>` 与 archive 文件 git blob hash 对比；只比较 tracked blob，不把复制的未跟踪 dist JS 当 source 改动。JS 单独 SHA-256 校验。构建后再次比较全部 tracked blob、manifest、lock 与 JS。submodule gitlink 不作为本地 blob 内容比对，本轮不声称验证其未归档内部内容。

## 构建方法

main 原生 Cargo 入口，不调用 HEAD 的 patched wrapper，也不传任何 HEAD patch 配置。main lock 中 rmcp=3.1.4、hyper-util=0.1.20、mio=1.2.2 来自锁定 registry；缺失依赖容许按锁下载。

```bash
cargo rustc --verbose --locked --manifest-path /Users/konghayao/code/ai/peri-release-size-20261007/.tmp/main-release-source/Cargo.toml \
  -p peri-tui --release --bin peri -- -C strip=symbols
CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  cargo rustc --verbose --locked --manifest-path /Users/konghayao/code/ai/peri-release-size-20261007/.tmp/main-release-source/Cargo.toml \
  -p peri-tui --release --bin peri -- -C strip=symbols
```

main 原 manifest `lto=true`、codegen-units=1，即 fat1。第二候选仅覆盖 thin / 16。`--verbose` 只为保存实际根 rustc 的 LTO / CGU 调用，不改变构建优化设置。driver 使用与第一轮 hash 相同的工具链真实 Cargo 路径，不经第一轮的实验 cargo 入口 shim；完整命令在每组 command.json。

实际 Cargo / Rust objcopy 使用 `DYLD_LIBRARY_PATH=<Rust sysroot>/lib`；Homebrew LLVM21 的 `llvm-size` 和 `llvm-objdump` 单独清除 DYLD_LIBRARY_PATH / DYLD_FALLBACK_LIBRARY_PATH 后运行。所有执行 cwd 都是指定 worktree，exec login=false。外部 RUSTFLAGS、profile、Rust wrapper / target flags 等按第一轮规则清理；环境证据只记录安全白名单。

分析与第一轮正式样本同口径：系统 tar/gzip，`COPYFILE_DISABLE=1 tar -czf <artifactdir>/peri.tar.gz -C <artifactdir> peri`；只打包同名 `peri`，不带 AppleDouble sidecar。文件与包 SHA-256、架构、otool、llvm-size、exports trie、CLI 与独立状态 JSON 均保存。分析命令必须 exit=0 才统计导出，失败值为 null 而非假零。

## 正式结果

单位为 B；MiB=1,048,576 B。只使用自动 strip 成功的正式 binary，LINKEDIT 按文件 filesize 而非 VM 对齐大小。

| main 候选 | binary B | binary MiB | tar.gz B | tar.gz MiB |
| --- | ---: | ---: | ---: | ---: |
| main_fat1 | 20,432,000 | 19.485 | 11,362,715 | 10.836 |
| main_thin16 | 39,250,416 | 37.432 | 16,978,485 | 16.192 |

| main 候选 | text | gcc_except_tab | unwind_info | eh_frame | 三项异常表合计 | const 合计 | LINKEDIT | export trie | 导出项 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| main_fat1 | 13,104,016 | 1,720,868 | 415,760 | 983,200 | 3,119,828 | 3,412,472 | 377,984 | 35,080 | 1,585 |
| main_thin16 | 20,205,288 | 2,581,888 | 986,216 | 2,438,368 | 6,006,472 | 4,651,880 | 7,825,904 | 7,192,176 | 72,321 |

const 只合计名为 `__const` 的 section；异常表合计含 gcc_except_tab、unwind_info、eh_frame 三项。trie 包含于 LINKEDIT，不重复相加。

| main 候选 | binary SHA-256 | tar.gz SHA-256 |
| --- | --- | --- |
| main_fat1 | `f2c10f156e04f1bc2438f5980229dfc3644b4b308b1b930be8958d8c2332ffb5` | `fd9bb86b910a087ae38628f3fbc9f9215146be6c2857df8d869d46b9ea434a1d` |
| main_thin16 | `36f148a2ef478c08ed631305392f0da9b3969da193e664d4e339244780353346` | `15dbe059e3dc6d652e93e5bef29d4a9d1d04189fb329c76a1b9c71b6e68b1ef6` |

两组架构均 Mach-O 64-bit executable arm64。build / file / otool / llvm-size / exports / tar / --version / --help 各自 exit=0，无 Rust objcopy strip 失败 warning。CLI 输出版本均 `peri 0.2.0`。

实际 root rustc 命令在每组 `actual-rustc-commands.txt`，并保留完整 verbose build.log；main_fat1 为 `-C opt-level=z -C lto -C codegen-units=1`，main_thin16 为 `-C opt-level=z -C lto=thin -C codegen-units=16`；均有生产 profile 和根调用的 `strip=symbols`。无 HEAD patch 路径。

| 候选 | ACP initialize / session-new / EOF |
| --- | --- |
| main_fat1 | 父 agent PASS：`.tmp/release-smoke/main-fat1/result.json` |
| main_thin16 | 父 agent PASS：`.tmp/release-smoke/main-thin16/result.json` |

父 agent 使用修正后的同一 reader-thread / queue smoke fixture，避免 TextIO 缓冲与 selectors 漏响应；未发模型请求。只证明该离线协议路径，不代表完整功能验证。

## 2×2 对照与解释

第一轮数据直接读取既有 summary，不重新测量。源码轴包括依赖锁与生产 patch 差异；配置轴同时改变 LTO 和 CGU，不能把两者分别归因。

| 源码 | fat1 binary B | thin16 binary B | thin − fat B | fat1 tar.gz B | thin16 tar.gz B |
| --- | ---: | ---: | ---: | ---: | ---: |
| main d7ee444e | 20,432,000 | 39,250,416 | 18,818,416 | 11,362,715 | 16,978,485 |
| HEAD 8c8c346a | 22,492,464 | 45,743,840 | 23,251,376 | 12,624,909 | 19,916,651 |

- main 同源码 fat1 相对 thin16：裸文件减少 18,818,416 B（以 thin 为基准 47.94%）；tar.gz 减少 5,615,770 B（33.08%）。两源码上 fat1 都更小，不只是当前分支特例。
- 同 fat1，HEAD 比 main 增加 **2,060,464 B（以 main 为基准 10.08%）**；同 thin16 增加 **6,493,424 B（16.54%）**。同配置 tar.gz 增量分别 1,262,194 B / 2,938,166 B。
- 裸文件交互差分为 **4,432,960 B**：`(HEAD thin − HEAD fat) − (main thin − main fat)`；压缩包交互为 1,675,972 B。配置效应与源码效应明显依赖另一轴，不能给唯一的“源码占比 / 配置占比”。
- 当前默认 HEAD thin 相对原 main fat 的观察差额为 25,311,840 B；可沿两条路径分解：`2,060,464 + 23,251,376` 或 `18,818,416 + 6,493,424`，均成立但项的数值不同。不能挑一条路径当唯一因果份额。
- 同 thin，HEAD trie 比 main 多 879,816 B、导出多 7,062 项；同 fat，HEAD trie 反而少 10,816 B、导出少 462 项。总回归不能仅用 trie 增长解释。
- 就本实验两个候选的体积目标，fat1 是两源码上更小的已测配置。此为体积建议，不代表完整运行性能、跨平台效果或用户批准前的生产修改。

所有差分详见 `.tmp/release-size/comparison.json`；生成过程只读取现存两轮 summary，没有新增实验轴。

## 证据与验证

- driver：`.tmp/release-size/driver-main.py`。
- 汇总：`.tmp/release-size/summary-main.json`。
- metadata / integrity：`.tmp/release-size/raw/main-metadata.json`、`main-final-integrity.json`。
- 每组 raw：`.tmp/release-size/raw/main_fat1/`、`main_thin16/`；含实际 `actual-rustc-commands.txt`。
- 独立 binary / archive：`.tmp/release-size/artifacts/main_fat1/peri`、`main_thin16/peri` 及各自 `peri.tar.gz`。
- 交叉核验：`python3 .tmp/release-size/verify.py --main`，只核验第二轮产物与环境，结果写 `verification-main.json`。
- ACP 由父 agent 另行烟测；本 driver 不重复模型或 ACP 测试，产物保存后通知。

构建后再次核验 2,147 个 tracked blob，source 差异 **0**；manifest / Cargo.lock / JS 均与构建前相同。metadata 和 final-integrity 包含确切 hash。`verification-main.json` 两组 errors=[]，complete_expected_variants=true；raw 中有完整 section 交叉核验、tar 内容 hash 与各命令状态。

有效 strip 的 LC_SYMTAB 项数为 389 / 398，locals 均 0；仍有 3 个 defined symbol（`__mh_execute_header`、`__rjem_malloc_conf`、`__rjem_malloc_conf_2_conf_harder`），不能武断要求 defined symbols 全为零。核验器初版误用该条件，随后又误要求重复 objcopy 后 SHA 幂等；误判结果保留为 `verification-main.false-positive.*`，未据此重建 main。

在诊断副本上用同一 Rust23 objcopy 再次 strip-all，sections 名称/尺寸与 trie bytes 保持相同，文件只少 16 B；otool 明确显示差额全部是 LC_CODE_SIGNATURE datasize。正式样本不替换为诊断副本。核验改为检查 strip 成功、无 locals、section/trie 不变，以及文件差额等于签名差额，不再声称 bitwise 幂等。

main 构建仅有既有 atomic fetch_update deprecation warning；不修无关 source。elapsed 分别 215.71 / 85.94 秒，全部为非 benchmark，不比较构建速度。第二轮未重建或重测第一轮。

## 解释边界

同源码比较用于配置效应；同配置比较当前 HEAD 与 main 时，源码轴包含 dependency lock 和既有 patch 差异，不把差额全部归因于业务代码。2×2 若有交互，分别报告两个配置下的源码增量，不给唯一归因百分比。

单次固定顺序构建、组间缓存与并行主机活动不支持时间性能结论；elapsed 只记录非 benchmark 操作计时。裸文件与下载压缩包分开比较。单 macOS ARM64 结果不能推广到 Linux/Windows，也不代替真实工具、模型和工作流完整验证。

## 第一轮补证与交付状态

第二轮结束后，独立审查指出第一轮 thin16 的 observed argv 错配到 fat1，export 候选缺少原 actual argv。第一轮报告已明确原执行证据边界，并新增两个同 HEAD 8c 的独立 verbose 重链样本 `thin16_argv` / `thin16_export_argv`，详见 `01_experiment_report.md` 的补证 ID、hash、采集路径与交叉比较。

补证两组裸文件及 text/unwind/const/LINKEDIT/trie/count 与对应原样本相同，hash 与 tar bytes 不同。**本报告 2×2 继续使用原五组样本的明确数值，不把补证 hash、压缩数值或 ACP 结果混入原表。** 补证没有重建/重测 main，main_fat1 与 main_thin16 始终保留本报告列出的原 hash。main 实际 root argv 自身已有 verbose log，不需要补链。

main 两组、第一轮三组、两组 argv 补证的实验构建均已结束，父 agent 已获告知可以接管生产配置。所有源码/锁/hash/HEAD 的完整性说明都是对应采样结束时的事实，不依赖之后工作区仍处于 8c。后续生产分支与 fat1 配置、最终 release 验证、commit/merge 由父 agent 负责；本 agent 仅交付实验脚本、证据和 01/02 报告，不修改 production、00_plan.md 或其独占的 conclusion.md。
