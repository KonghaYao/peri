# 第一轮：当前 HEAD release 体积对照

日期：2026-10-07。状态：三组原正式样本度量完成、两组独立 argv 补证完成；原 argv 的错配/缺失已明确记录，补证度量与实际参数核验通过，独立审查的最终裁定由验证者补齐。未改生产源码、Cargo.lock 或 `00_plan.md`。

## 结论与边界

- 同一 HEAD 下，fat LTO / 1 CGU 相对 thin LTO / 16 CGU，裸文件减少 **23,251,376 B（50.83%）**，同名 tar.gz 减少 **7,291,742 B（36.61%）**。
- thin/16 限制导出的诊断候选减少 **8,135,008 B（17.78%）**；text、三项异常表、const 完全同大小，文件减量全部落在 `__LINKEDIT`。export trie 本身减少 8,071,944 B；不把整个 LINKEDIT 减量都叫 trie。
- 该诊断仍有 `_main` 与 `__mh_execute_header` 两个导出，不是字面上的单导出文件。它不是已批准的生产修复；动态查找、FFI 等完整兼容性未验证。
- 本轮只证明同源码配置差异，**尚不能解释与 main 的源码/依赖增量**；后续 main 对照另见第二轮。
- 初次构建出现 Rust objcopy 动态库加载失败，Cargo 仅 warning。初次未 strip 样本全部无效，不进入以上结论。正式样本在修正的统一环境下重新链接并自动 strip。

## 固定输入与环境

| 项 | 固定值 |
| --- | --- |
| worktree | `/Users/konghayao/code/ai/peri-release-size-20261007` |
| HEAD | `8c8c346a7c6c32c616418884152f2b9794ecd62b` |
| Cargo.lock SHA-256，前后不变 | `dac72db776e1705fac780d4f9d70fc95066c10c66e0f540b9e2804df6269caa5` |
| Workflow dist JS SHA-256，前后不变 | `0ca2007a66096bfa792bbaed1ba8f57142bdbd5dcb670ed53a395920ec8947a4` |
| Rust | `1.99.0 (b940084d7 2026-09-28)`，内置 LLVM `23.1.1` |
| target / 主机 | `aarch64-apple-darwin`，macOS `26.5.1 (25F80)` |
| release | `opt-level=z`、`strip=symbols`、默认 features、默认 panic unwind |
| target cache | 仅此 worktree 的 `target`；初次无 release cache，组间串行复用 |

完整工具链版本与二进制 hash 在 `.tmp/release-size/raw/metadata.json`。Workflow 文件为 `npm-packages/@peri-workflow/dist/peri-workflow.js`。外部 RUSTFLAGS、encoded flags、release profile 覆盖、Rust wrapper、target flags 等在 driver 中清理；fat1 只显式增加两项 profile 变量。未使用原 worktree target。

构建实际固定 `DYLD_LIBRARY_PATH=<rustc sysroot>/lib`，加载 Rust 自带 LLVM23，不用 Homebrew LLVM21 为 Rust objcopy 提供库。仓库 wrapper 的 `/usr/bin/env` 会剥离 DYLD，因此实验专用 `.tmp/release-size/bin/cargo` 在进入固定的真实 Cargo 可执行文件前恢复此变量。每组 `effective-cargo-env.json` 保存 Cargo 入口的实际有效构建环境。

`llvm-size` / `llvm-objdump` 使用 Homebrew LLVM `21.1.5`，仅用于分析；其分析环境移除 `DYLD_LIBRARY_PATH` 和 `DYLD_FALLBACK_LIBRARY_PATH`，防止误加载 Rust LLVM23。第一次分析工具 abort 的证据保留为 `failed-dyld-*`；已经重采，正式指标对应分析命令 exit=0。

环境证据只保留构建相关白名单，并拒绝 TOKEN、SECRET、API_KEY、KEY、PASSWORD、credentials 等键或值。初始完整环境快照已原位清理，不在报告中复制凭证。正在运行的旧 driver 用旁路清理器处理，后续 driver 本身已过滤。环境文件 chmod 600。

## 命令与顺序

初次严格串行：

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --release --bin peri
CARGO_PROFILE_RELEASE_LTO=fat CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
  ./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --release --bin peri
./scripts/cargo-rmcp-patched.sh rustc --locked -p peri-tui --release --bin peri \
  -- -C link-arg=-Wl,-exported_symbol,_main
```

初次产物和证据保留在 `artifacts/{thin16,fat1,thin16_export}_v1/`、`raw-v1/`、`v1-summary.json`。修正环境后，同顺序重新链接，复用编译缓存，不修改 production profile：

```bash
./scripts/cargo-rmcp-patched.sh rustc --locked -p peri-tui --release --bin peri -- -C strip=symbols
CARGO_PROFILE_RELEASE_LTO=fat CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
  ./scripts/cargo-rmcp-patched.sh rustc --locked -p peri-tui --release --bin peri -- -C strip=symbols
./scripts/cargo-rmcp-patched.sh rustc --locked -p peri-tui --release --bin peri \
  -- -C strip=symbols -C link-arg=-Wl,-exported_symbol,_main
```

`-C strip=symbols` 与 manifest 原值一致，同时强制根 binary 重链。请求执行的命令、环境、起始时间、退出状态和 build log 分别保存在每组 `command.json`、环境 JSON、`build.status.json`、`build.log`。command.json 不能替代实际 rustc argv；原 thin16 / export 的实际参数记录不足，另见下文补证。可重跑 driver 为 `.tmp/release-size/driver.py --relink`，但拒绝覆盖已有 summary；重跑前应保留并移走本轮 raw、同 ID artifacts 与 summary。所有 exec 均在指定 worktree，login=false。

压缩统一 `COPYFILE_DISABLE=1 tar -czf <artifactdir>/peri.tar.gz -C <artifactdir> peri`，工具均为系统 tar/gzip，验证包内唯一文件名为 `peri`，且解包内容 hash 与对应 binary 一致。不做时间戳归一化，也不宣称压缩包字节级可重复。初次包的 AppleDouble `._peri` 已标记，不与正式压缩数据混用。

## 正式结果

全部单位为字节；MiB=1,048,576 B。LINKEDIT 使用文件尺寸 `filesize`，不是 `llvm-size -m` 显示的 VM 对齐尺寸；也不使用包含 PAGEZERO 的 llvm-size total。

| 候选 | 裸文件 B | 裸文件 MiB | tar.gz B | tar.gz MiB |
| --- | ---: | ---: | ---: | ---: |
| thin16 | 45,743,840 | 43.625 | 19,916,651 | 18.994 |
| fat1 | 22,492,464 | 21.450 | 12,624,909 | 12.040 |
| thin16_export | 37,608,832 | 35.867 | 17,875,765 | 17.048 |

| 候选 | text | gcc_except_tab | unwind_info | eh_frame | 三项异常表合计 | const 合计 | LINKEDIT | export trie | 导出项 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| thin16 | 24,132,084 | 3,322,916 | 1,166,824 | 2,914,368 | 7,404,108 | 4,884,920 | 8,797,920 | 8,071,992 | 79,383 |
| fat1 | 14,621,552 | 2,061,608 | 465,072 | 1,113,928 | 3,640,608 | 3,442,824 | 390,448 | 24,264 | 1,123 |
| thin16_export | 24,132,084 | 3,322,916 | 1,166,824 | 2,914,368 | 7,404,108 | 4,884,920 | 662,912 | 48 | 2 |

const 合计只指各 segment 中名为 `__const` 的 section，不包括 cstring 或所有只读数据。每 section / segment 细项在可解析 `.tmp/release-size/summary.json`。text、异常表、const、LINKEDIT 不是文件完整无重叠归因模型，trie 已包含在 LINKEDIT 内，不重复相加。

| 候选 | Binary SHA-256 | tar.gz SHA-256 |
| --- | --- | --- |
| thin16 | `9057644514e8ed85d036675e98390fd06b50980be7b8ccfbda803c9075e94194` | `c306e877a7e5bdc57c3b30718bf161546a965ed11ca30bc14b83e4c26d2b8ff4` |
| fat1 | `ac2d250bf7d507c84ecf32c3003f975bd0572c8b5e150344b46f86d533a57210` | `fba21a3983582197468bf5f7aaf8a53697c67a8ff8eed6b85fd768d0b00cd1f6` |
| thin16_export | `73fe74b909551661f9d1bab2aa1df400f3778701298a47a33b62f01ab3584f4f` | `2dca97ebd37d67bc6427f2ec5288fa41d20d827733192e8915ffdd26e77b0fd6` |

三组均 `Mach-O 64-bit executable arm64`；正式独立产物在 `.tmp/release-size/artifacts/<id>/peri`，各候选完成后立即复制，不引用随后被覆盖的 target 文件。

## 环境发现与有效性复验

| 初次无效样本 | 未 strip 裸文件 B | Rust23 手动 strip-all 后 B | 正式重链并 strip 后 B |
| --- | ---: | ---: | ---: |
| thin16_v1 | 89,150,072 | 45,743,824 | 45,743,840 |
| fat1_v1 | 44,463,544 | 22,492,448 | 22,492,464 |
| thin16_export_v1 | 81,015,048 | 37,608,816 | 37,608,832 |

`normalize_v1.py` 使用 Rust23 `rust-objcopy --strip-all` 生成诊断副本，并保留原始未 strip 产物。三组标准化副本与正式重链产物：所有 section 名称/尺寸一致、export 名称一致、trie 字节数一致；正式文件均多 16 B，差异在 LINKEDIT，未声称二进制 SHA 相同或完整字节等价。详见 `normalization-comparison.json`。本报告只使用正式重链数据，不用历史另一产物的 45,720,256 B 替换本次实测。

正式 LC_SYMTAB 项数分别 331 / 317 / 331，远小于初次 372,104 / 623,373 / 372,104；每组最终 peri 的 strip 失败 warning 数为 0。正式 build log 仍回放一条 **缓存的 rmcp build-script** 警告（旧 PID），不是这次 peri strip 失败。

`verify.py` 独立使用 otool 文本与 llvm-size section 对照二进制 header 解析、核对 trie load-command bytes 与导出行数、检查 tar 内容与 hash、检查工具及 CLI 退出状态、检查环境白名单。`.tmp/release-size/verification.json` 三组 errors=[]，complete_three_variants=true；这个结果只覆盖度量与采集，不是原样本优化参数的充分执行证据。分析失败不会作为零导出结论；修正后证据覆盖正式指标，失败输出另存。

## 可观察行为与计时

| 候选 | build exit | --version exit | --help exit | ACP initialize/session-new/EOF |
| --- | ---: | ---: | ---: | --- |
| thin16 | 0 | 0 | 0 | 父 agent PASS：`.tmp/release-smoke/head-thin16-final/result.json` |
| fat1 | 0 | 0 | 0 | 父 agent PASS：`.tmp/release-smoke/head-fat1/result.json` |
| thin16_export | 0 | 0 | 0 | 父 agent PASS：`.tmp/release-smoke/head-thin16-export/result.json` |

版本输出均 `peri 0.2.0`。CLI HOME/XDG 隔离在 `.tmp/release-size/cli-sandbox/`；ACP 不在本 driver 重复执行。ACP fixture 以父 agent 更正后的同一 smoke 脚本为准，仅覆盖离线初始化、新会话与 EOF 退出，不代表模型、工具、动态查找或完整工作流通过。

初次 elapsed 分别约 147 / 253 / 38 秒；正式重链 elapsed 为 32.60 / 140.50 / 31.97 秒，精确值在各 summary。**全部仅为非 benchmark 操作计时**：缓存状态不同、固定顺序且同主机有其他任务，不推断构建速度或运行性能。

## 实际 argv 缺口与独立补证

独立审查对原记录给出 PARTIAL：`raw/thin16/observed-rustc-command.txt` 实际含 fat / 1 的 argv，是按进程快照采样时错配，不能用于证明 thin / 16；原 thin16_export 没有 observed argv。原非 verbose build.log 不包含可追溯的完整根 rustc 调用，不能从 command.json 反造旧 process/PID 的证据。原 fat1 自己的 observed argv 为 fat / 1；本次仅重链两个有缺口的候选，不重建 fat1 或 main。

错配原文件没有覆盖或删除，原样复制到 `.tmp/release-size/raw/argv-mismatch-archive/thin16-observed-rustc-command.mismatched.txt`，并归档 fat1 原记录与说明。原三个正式样本、第二轮两个 main 样本的 binary、raw 与 summary 保留不变。

补证是 **同 HEAD 8c8c346a、同工具链/lock/JS、同 profile 控制下的新执行**，不是恢复旧运行记录，也不是第四实验轴。使用 `.tmp/release-size/driver.py --relink --argv-evidence`，严格串行执行 `thin16_argv` → `thin16_export_argv`，两条原正式 rustc 命令仅增加 `--verbose`，保存每组独立 build.log 与 `actual-rustc-commands.txt`。

| 独立补证 ID | 实际根 rustc 参数 | binary B | tar.gz B | 对应原样本 |
| --- | --- | ---: | ---: | --- |
| thin16_argv | `-C lto=thin -C codegen-units=16 -C strip=symbols`，无 export 限制 | 45,743,840 | 19,916,680 | thin16 |
| thin16_export_argv | 同 thin / 16 / strip，并有 `-C link-arg=-Wl,-exported_symbol,_main` | 37,608,832 | 17,875,783 | thin16_export |

| 独立补证 ID | binary SHA-256 | tar.gz SHA-256 |
| --- | --- | --- |
| thin16_argv | `bfe2c5044975f7f15c490ab419d3c52a0089a6f270a3d08a84acdc0902d8145b` | `950766801aad1f7c607254b97c5d28e7389d3118a6ff8c7b4701c691c970b1e6` |
| thin16_export_argv | `01621564c7f752d3d073ba1eee894eeb01f01ff48e5d4b4f64a23c9169c98db4` | `44999d38bbdca7471540d8782ff87abb84ee395f32d564c17b632440da88619b` |

各补证产物在 `.tmp/release-size/artifacts/<补证ID>/peri`，raw 在 `.tmp/release-size/raw/<补证ID>/`，可解析结果为 `summary-argv.json`。实际 argv 从候选自身的 verbose build.log 提取，不再用可能命中下一候选的 `ps` 快照。`verification-argv.json` 两组 errors=[]、complete_expected_variants=true，同时检查实际 LTO/CGU、export flag、有效 strip、分析状态、archive 内容和环境白名单。

交叉比较结果：两组与各自原样本的 **file/text/三项异常表合计/const/LINKEDIT/trie bytes/export count 全部相同**，分别仍为 79,383 / 2 个导出；因此裸文件及 section 体积结论复现。**binary SHA 与原样本不同，不声称 bitwise 重现或旧 argv 已被恢复**。补证 tar.gz 分别比原包大 29 B / 18 B；不将这些差额解释为代码增长，压缩包时间戳等 metadata 未归一化。正文与第二轮 2×2 表继续明确使用原样本的 hash / tar 数值，不与补证包混用。

补证 build / 分析 / --version / --help 均 exit=0，locals=0，最终 peri strip 失败数为 0；各 log 仍回放一条缓存的 rmcp build-script warning。Cargo 入口有效 DYLD 指向固定 Rust23 lib，分析环境无两项 DYLD 覆盖。`argv-final-integrity.json` 在采样结束时记录 HEAD 8c、原 Cargo.lock 与 Workflow hash 未变。补证 elapsed 为 80.50 / 27.58 秒，只是非 benchmark 操作计时。

补证产物已通知父 agent，ACP smoke 由其另行补齐；不能将上表原样本的 ACP PASS 自动转移到不同 hash 的补证产物。本 agent 不等待 smoke 而新增构建，也不声称独立验证者已对补证作出最终 PASS。

## 后续与文件边界

第二轮经用户 follow-up 授权并已完成，使用 archive main `d7ee444e`，见 `02_experiment_report.md`；没有新增第四轴。本轮没有架构、模块入口、standard 或 canonical 测试路由变更，DOC-UPDATE-001 无需更新其它事实源。实验采样与补证结束时，生产 tracked diff 为空，lock / JS hash 前后相同。此为采样时状态，不要求父 agent 后续生产修复仍停留在该 HEAD。

本轮修改仅 `.tmp/release-size/` 内实验 driver、脚本、raw、JSON、隔离 CLI 目录及产物，以及本报告；target 是授权的本 worktree 编译缓存。未改 `00_plan.md`，未 commit，未使用原 worktree target。

两轮与 argv 补证构建已经结束；后续配置/最终 release/commit/merge 由父 agent 接管，本 agent 不操作 branch/checkout，不改生产 profile，不再运行依赖 ROOT HEAD 仍为 8c 的脚本，不修改父 agent 独占的 conclusion.md。
