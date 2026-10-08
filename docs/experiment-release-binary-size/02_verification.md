# 第二轮独立验证：main 两格与 2×2 交互

日期：2026-10-07。最终判定：**PASS（限定于已测体积、构建配置与局部 smoke）**。

main 两格的产物、实际 rustc 配置、strip、源码完整性和局部 ACP 证据均通过独立复核。2×2 数值和交互公式成立；第一轮 HEAD 的配置缺口已由后来独立归档的新执行补证支持，不能把新 argv 冒充旧产物的原始调用，也不声称字节级可复现或完整功能回归通过。

## 审查范围与方法

- main 固定提交：`d7ee444efe7a461b0696bd6c27c6c91f946fdec1`；源码仅读取实验 worktree 内 `.tmp/main-release-source`，不访问原 main 工作树 WIP。
- HEAD 研究提交：`8c8c346a7c6c32c616418884152f2b9794ecd62b`。下文“HEAD”均指该固定研究提交，不代表未来切换分支后的工作区 HEAD。
- 独立重采 file、nm、otool、llvm-size、exports；自行解析 Mach-O header／load commands／sections，不调用执行者的度量实现。每个子进程 cwd 均是指定实验 worktree，exec `login=false`。
- 自行计算 binary／tar.gz SHA-256，逐字比对包内 peri 与独立 binary；分析工具清除 DYLD 环境，避免 LLVM21／23 混载。
- 直接从 Git 对象核对 main archive 的全部 tracked blob；检查 lock、manifest、JS、工具链 hash，以及逐候选 actual argv、构建日志与构建白名单键。
- 对新出现的 HEAD argv 补证另作比较，并在 `01_verification.md` 追加修订；保留第一轮原始 PARTIAL 历史。
- 未构建、未运行模型／网络调用、未写生产文件、执行者脚本或 raw。验证者仅写两份 verification；没有创建分支或提交。

## main 源码、锁文件与环境

独立执行 `git ls-tree -r -z` 并按 Git blob 格式计算 archive 文件 hash：**2,147 个 tracked blob，差异 0**。symlink 核对 link 内容，不通过 symlink 读取原工作树。gitlink `e2e/tui-tester`、`peri-cool` 不在 blob 核查范围；不声称核验其内部内容。

| 输入 | 独立核查 SHA-256 |
| --- | --- |
| main Cargo.toml | `49507b5ca807c49c5b6d1e11afbd5d230d9f53d828f2f660fc86e3aea60ac45a` |
| main Cargo.lock | `905becbdd5f03771813d66b7a9774da0b3c29921df6421a79c0ddfb5fbe1ebdc` |
| main 嵌入 Workflow JS | `0ca2007a66096bfa792bbaed1ba8f57142bdbd5dcb670ed53a395920ec8947a4` |

均与 metadata 的构建前值、每格构建后值及 `main-final-integrity.json` 一致；JS 与第一轮相同。构建前／后时间点以执行者记录为证，本次另独立确认了验证时 archive 的现存内容，不声称独立观察过整个构建过程。

main 直接使用真实 Cargo 可执行文件，命令不含 HEAD patch `--config`，日志未出现 `target/peri-rmcp-patches/` 或 `target/peri-hyper-util-patches/`。main 锁定 registry 版本为 rmcp 3.1.4、hyper-util 0.1.20、mio 1.2.2、tokio 1.53.1。源码／依赖轴包含这类差异，不要求与 HEAD 锁文件相同。

独立复算 rustc、cargo、Rust libLLVM 文件 hash，均与 metadata 及第一轮工具链一致。Rust 1.99.0、LLVM23.1.1；Homebrew LLVM21.1.5 只作分析。

main 的 `env.json` 是 driver 传给直接 Cargo 子进程的构建白名单快照，不是第一轮 wrapper 后另截获的 effective env。仅读取构建相关键，不读取 incoming-env 或完整历史环境。两格均使用同一 RUSTC／toolchain／target dir、Rust sysroot 的 DYLD_LIBRARY_PATH、COPYFILE_DISABLE=1；所审查记录无 RUSTFLAGS、encoded flags 或 Rust wrapper 覆盖。只有 main_thin16 增加 LTO=thin、CGU=16。

### 实际参数核对

两格 `actual-rustc-commands.txt` 各有一个根 binary 调用，逐字存在于各自 build.log；日志中的编译工作区为 main archive。实际根参数为：

| main 候选 | 根 rustc 的相关 `-C` 参数 |
| --- | --- |
| main_fat1 | `opt-level=z`、`lto`、`codegen-units=1`、`strip=symbols` |
| main_thin16 | `opt-level=z`、`lto=thin`、`codegen-units=16`、`strip=symbols` |

main manifest 的 `lto=true` 对应 bare `-C lto`，即 Fat LTO，不能误判成没有 LTO 参数。两格都含第二个同值 strip 参数，来自显式根 rustc 参数，不是不同 strip 语义。未见 panic=abort 覆盖；不声称仅靠 argv 已验证 LLVM 内部 LTO-unit 模块标志。

共享 target cache 不直接判作污染；main 原生入口、不同依赖图与实际根调用共同支持生产输入边界。构建顺序 main_fat1 → main_thin16，操作计时约 215.71／85.94 秒，缓存与固定顺序使其不能用于构建性能结论。

## 正式 main 度量

单位 B，计数除外。所有值均由本验证独立复算。

| 指标 | main_fat1 | main_thin16 |
| --- | ---: | ---: |
| 裸文件 | 20,432,000 | 39,250,416 |
| tar.gz | 11,362,715 | 16,978,485 |
| `__text` | 13,104,016 | 20,205,288 |
| 各段 `__const` 合计 | 3,412,472 | 4,651,880 |
| 三项异常／展开表合计 | 3,119,828 | 6,006,472 |
| `__LINKEDIT.filesize` | 377,984 | 7,825,904 |
| export trie | 35,080 | 7,192,176 |
| 动态导出项 | 1,585 | 72,321 |
| LC_SYMTAB nsyms／新采 nm 行数 | 389／389 | 398／398 |
| local symbols | 0 | 0 |
| signature blob | 158,544 | 304,432 |

| 文件段 filesize | main_fat1 | main_thin16 |
| --- | ---: | ---: |
| `__PAGEZERO` | 0 | 0 |
| `__TEXT` | 18,792,448 | 29,655,040 |
| `__DATA_CONST` | 1,179,648 | 1,687,552 |
| `__DATA` | 81,920 | 81,920 |
| `__LINKEDIT` | 377,984 | 7,825,904 |

段 fileoff 连续且 filesize 合计等于裸文件大小。两格 `__DATA.vmsize=1,179,648` 明显大于 filesize；PAGEZERO 的 4 GiB 映射和 DATA 的 zerofill 不能算进发布体积。export 信息来自 `LC_DYLD_INFO_ONLY`，trie 已在 LINKEDIT 内，不重复相加。

| main 候选 | binary SHA-256 | tar.gz SHA-256 |
| --- | --- | --- |
| main_fat1 | `f2c10f156e04f1bc2438f5980229dfc3644b4b308b1b930be8958d8c2332ffb5` | `fd9bb86b910a087ae38628f3fbc9f9215146be6c2857df8d869d46b9ea434a1d` |
| main_thin16 | `36f148a2ef478c08ed631305392f0da9b3969da193e664d4e339244780353346` | `15dbe059e3dc6d652e93e5bef29d4a9d1d04189fb329c76a1b9c71b6e68b1ef6` |

新采 file／nm／otool／llvm-size／exports 全部 exit=0、stderr 为空。otool 与 exports 原始输出逐字匹配 raw；llvm-size 的 section 名称与尺寸逐项匹配独立 Mach-O 解析。raw 的 build、file、otool-l、llvm-size-m、exports-trie、tar、version、help 状态均 0，无 timeout。三项异常表与 const 的合计口径同第一轮。

## strip 与警告分类

main 两格没有 objcopy strip-failure warning。build.log 的既有 warning 是 atomic fetch_update deprecation，不是 strip／LTO 失败，不修无关源码。

直接解析 LC_DYSYMTAB：locals=0，external defined=3；三个 defined 项确为 `__mh_execute_header`、`__rjem_malloc_conf`、`__rjem_malloc_conf_2_conf_harder`。有效 strip 不要求所有 defined 项为零，更不能把动态导出计数与静态符号计数混同。

执行者保留的 `_strip_probe` 副本独立核对：重复 Rust23 strip-all 后，各 section 名称／尺寸及 trie 长度不变，文件各少 16 B，LC_CODE_SIGNATURE datasize 也各少 16 B。正式文件不替换为 probe。不能要求重复 strip 后 binary SHA 幂等；执行者的 false-positive 记录是核验规则误判，不是正式候选构建失败。本验证不依赖其 errors=[] 来替代独立复算。

## 2×2：两种源码增量与交互

使用第一轮原正式归档，不用 argv 新执行或 strip probe 替换任何格。以下 HEAD 固定为研究提交 8c8c346a。

| 源码 | fat/1 裸文件 | thin/16 裸文件 | thin−fat | fat/1 tar.gz | thin/16 tar.gz |
| --- | ---: | ---: | ---: | ---: | ---: |
| main d7ee444e | 20,432,000 | 39,250,416 | 18,818,416 | 11,362,715 | 16,978,485 |
| HEAD 8c8c346a | 22,492,464 | 45,743,840 | 23,251,376 | 12,624,909 | 19,916,651 |

设 `S(source,config)` 为裸文件大小，交互定义为 `(HEAD thin−HEAD fat)−(main thin−main fat)`。

| 差分 | 裸文件 B | 百分比基数／结果 | tar.gz B |
| --- | ---: | --- | ---: |
| fat/1 下 HEAD−main | +2,060,464 | main fat：+10.0845% | +1,262,194 |
| thin/16 下 HEAD−main | +6,493,424 | main thin：+16.5436% | +2,938,166 |
| main 下 thin−fat | +18,818,416 | 改为 fat，相对 main thin 减少 47.9445% | +5,615,770 |
| HEAD 下 thin−fat | +23,251,376 | 改为 fat，相对 HEAD thin 减少 50.8295% | +7,291,742 |
| 交互项 | **+4,432,960** | 不给唯一归因比例 | **+1,675,972** |

两种同配置源码增量明显不同，必须分别保留。HEAD thin 相对 main fat 的总差 **25,311,840 B** 有两条等价路径：

- `2,060,464 + 23,251,376`：先在 fat 配置下换源码，再在 HEAD 下换配置。
- `18,818,416 + 6,493,424`：先在 main 下换配置，再在 thin 配置下换源码。

配置组合是在两提交上都更大的可控体积因素，但不能选其中一条路径生成唯一“源码／配置贡献百分比”。仅两种已测配置，支持 fat/1 在本机体积更小；不证明全参数／全平台最优，也不证明运行性能更优。

### 分段交互辅助解释

| 字节项 | fat 下源码增量 | thin 下源码增量 | 交互 |
| --- | ---: | ---: | ---: |
| `__text` | 1,517,536 | 3,926,796 | 2,409,260 |
| 三项异常／展开表 | 520,780 | 1,397,636 | 876,856 |
| const | 30,352 | 233,040 | 202,688 |
| LINKEDIT | 12,464 | 972,016 | 959,552 |
| 其中 export trie | -10,816 | 879,816 | 890,632 |

trie 是 LINKEDIT 子项，表不能全部相加当完整互斥归因。fat 下 HEAD 导出反而少 462 项，thin 下多 7,062 项；源码体积增长不能全解释成 trie 增长。源码轴整体包含 Rust 代码、依赖版本、feature 图及既有生产 patch，不单独等于业务代码量。

压缩包有不同 mtime，未归一化；百分比与交互只描述本次封装观测，不保证重打包逐字相同。后续 HEAD argv 补证的压缩包也确有小幅变化，不能偷偷替换原 2×2 的压缩样本。

## HEAD 配置补证及跨轮有效性

本次审查期间出现 `summary-argv.json`、`raw/thin16_argv`、`raw/thin16_export_argv` 和独立产物。actual argv 可追溯到各自 build.log，实际为 thin/16，export 补证另含 exported_symbol,_main；输入 metadata 固定 8c8c346a、同工具链、相同 lock／JS。原错误记录另行归档。

这是 **2026-10-07 13:06:03 UTC／13:07:26 UTC 开始的新执行**。新旧裸长度、section 名称／尺寸、主要聚合指标、trie 长度和导出名称均相同；但 binary hash 不同，且各自 `__text` 有 **4,930 个字节不同**，不是纯 UUID／签名差异。新候选之间的 text 内容相同，原候选之间的 text 内容也相同。未定位这 4,930 字节差异的原因，不推断其功能等价或归咎于随机性。

因此配置目标和体积结论获得新执行的交叉支持，第一轮缺口可在限定意义下闭环；不证明找回原 thin 执行记录、不证明 bitwise reproducibility。详细 hash、压缩变化和最终判定见第一轮日期修订。2×2 保持原正式样本身份不变。

## CLI、ACP 与叙述核对

两格 CLI `--version`／`--help` exit=0，version 为 peri 0.2.0。父 agent 现存 ACP 证据：

- `.tmp/release-smoke/main-fat1/result.json`：PASS，protocolVersion=1，非空 sessionId，EOF exit=0。
- `.tmp/release-smoke/main-thin16/result.json`：同上。

result 的 binary 路径为本次复算的正式候选；未记录 binary hash 的时间绑定，不能声称验证者亲自重新运行过 smoke。共用已核对的隔离 fixture，仅 initialize／session/new，不发 prompt；dummy provider 为 127.0.0.1:9。只证明此离线路径，不证明模型、工具、Workflow、动态查找、执行恢复或全面 teardown 正确。

`02_experiment_report.md` 在本次审查期间从方法稿更新为完成报告；更新后的正式数值、hash、配置差分、两种源码增量、交互、strip probe 及 warning 分类均与独立复算一致。`conclusion.md` 的 2×2 数值及“不可唯一百分比归因”也相符。

交付前再次核对，两份执行报告已同步第一轮新执行身份、新 SHA、压缩变化及不替换原样本的边界，与验证一致；尚未具体记录补证 text 的 4,930 字节差异。不能将“section 尺寸一致”写成“section 内容完全一致”。conclusion 中“兼顾现有行为”只能解释为当前 smoke 所覆盖行为，不能理解为完整行为／性能兼容性已经证明。

用户随后将生产配置应用到更新的 670 基线，是新的交付验证，不属于本固定源码实验。该最终构建与提交／合并状态未由本报告验证，不将未来基线混入本 2×2。

## 证据路径与后续

- 执行报告：`docs/experiment-release-binary-size/02_experiment_report.md`。
- 本独立验证：`docs/experiment-release-binary-size/02_verification.md`。
- 第一轮及日期修订：`docs/experiment-release-binary-size/01_verification.md`。
- main summary：`.tmp/release-size/summary-main.json`。
- main raw：`.tmp/release-size/raw/main_fat1/`、`main_thin16/`，含 actual-rustc-commands.txt。
- main 独立产物：`.tmp/release-size/artifacts/main_fat1/peri`、`main_thin16/peri`。
- 完整性：`.tmp/release-size/raw/main-metadata.json`、`main-final-integrity.json`。
- HEAD 新执行补证：`.tmp/release-size/summary-argv.json`、`raw/*_argv/`、`artifacts/*_argv/peri`。

当前未发现阻止本机体积选择的严重缺陷；历史 argv 不完整和新旧 text 差异均保留为明确局限，不隐去失败或宣称额外功能通过。

## 日期修订：2026-10-07，七组 hash 绑定的新 ACP 执行

父 agent 另行重跑全部七组候选并记录启动前 `binary_sha256`。本验证对 `.tmp/release-smoke/<id>-hashed/result.json` 逐组独立核对：binary 路径、result hash、归档 binary 的新算 SHA-256、对应 summary 的 hash 一致；七组均 PASS、protocolVersion=1、非空 sessionId、EOF exit=0。

main 新执行结果为 `.tmp/release-smoke/main_fat1-hashed/result.json`、`.tmp/release-smoke/main_thin16-hashed/result.json`；其余五组 ID 为 thin16、fat1、thin16_export、thin16_argv、thin16_export_argv，详情见第一轮本次日期修订。旧 run 的“只按路径关联／未记录 hash”历史说明不删除，但不再是当前证据缺口；新 result 不冒充旧 run。验证者只复核证据，未运行构建或烟测。

最终限定 PASS 不变，2×2 数值与交互不变。此次增加的是 hash 绑定的局部 ACP 覆盖，不是完整功能或性能回归。

670 基线生产构建尚未有可核验的最终产物。依据父 agent 通报，先前 target 消失无法取产物、cwd 工具链选择导致 1.83／MSRV 混编的两次尝试已排除；本验证未独立调查这两次环境故障，也不将其计入固定 8c／main 的 2×2。新的固定绝对 Rust1.99／Cargo、stable 环境及独立 target 构建仍待结果，**不宣称最终生产验证通过**。
