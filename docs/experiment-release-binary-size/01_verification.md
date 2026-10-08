# 第一轮独立验证：HEAD release 体积与导出诊断

日期：2026-10-07。状态：**PARTIAL**。正式产物度量、strip 有效性及已记录的局部 smoke 通过；实际配置的逐候选 rustc argv 证据不完整且存在错配，不能将配置归因升级为完全验证通过。

## 范围与判定

本轮只审查 HEAD `8c8c346a7c6c32c616418884152f2b9794ecd62b` 的 `thin16`、`fat1`、`thin16_export` 正式候选，不解释 main→HEAD 的源码／依赖增量。main 两格及交互效应属于第二轮。

| 验证项 | 判定 | 说明 |
| --- | --- | --- |
| 正式文件、hash、Mach-O、动态导出、压缩包 | PASS | 独立重采与直接二进制解析均与 summary 相符 |
| 最终 strip | PASS | LC_SYMTAB 与新采 nm 一致；标准化副本交叉支持 |
| 实际 LTO／CGU 的逐候选执行证明 | NEEDS_MORE | thin16 的 observed argv 显示 fat/1，export 候选没有该文件；没有找到可补证的逐候选 wrapper argv |
| CLI 与已记录 ACP smoke | PASS，局部 | 仅覆盖元信息、初始化、新会话和 EOF 退出 |
| 实验报告叙述 | PARTIAL | 数值及主要边界正确；配置证明欠缺，export ACP 的“待补齐”已过时，失败分析记录标签需要细分 |

未发现正式二进制仍未 strip、正式度量工具失败或 summary 数值造假的证据。argv 缺陷已经即时通知；它是重要证据溯源缺口，不等价于已证明 thin16 实际用了 fat 配置。

## 输入与独立方法

先读 `00_plan.md`、根 `CLAUDE.md` 和相关 standards；本轮继续核对执行者后来写入的 `01_experiment_report.md`。验证者没有构建、没有修改候选、计划、生产代码或执行者文件，也没有网络／模型调用或子 agent。

独立采集只在指定 worktree 执行，所有 exec 使用 `login=false`。方法不调用执行者的 `macho_metrics` 或 `verify.py`：

1. 对每个正式文件重新读取逻辑长度并计算 SHA-256；直接用 Python `struct` 解析 Mach-O header、`LC_SEGMENT_64`、section、`LC_SYMTAB`、`LC_DYLD_INFO_ONLY` 和 `LC_CODE_SIGNATURE`。
2. 重新运行 `file`、`nm`、`otool -l`、Homebrew `llvm-size -m`、`llvm-objdump --macho --exports-trie`；分析子进程移除 `DYLD_*`，不加载 Rust LLVM23 到 Homebrew LLVM21。
3. 新采 otool／export stdout 与 raw 逐字核对；llvm-size 的 section 名称与尺寸逐项对照独立二进制解析。导出项按实际 export 输出计数，不以 nm 代替动态导出表。
4. 对 section 的文件内容计算 hash，比较 thin16 与 export 候选；zerofill 不作为磁盘内容比较。读取 tar.gz，确认唯一成员 `peri` 的内容逐字等于正式文件，并复算压缩包 hash。
5. 检查 raw 中构建、工具和 CLI 状态，以及父 agent 的 ACP result 和 smoke 脚本；不把脚本存在当作执行成功。
6. 不读取历史 incoming-env 或原完整环境快照；仅从正式 `effective-cargo-env.json` 提取构建键，核对白名单过滤实现。没有输出环境全集或秘密。

本次新采五种工具对三候选全部 exit=0，stderr 均为空。raw 的 build、file、otool-l、llvm-size-m、exports-trie、tar、version、help 状态也全部为 0；build 状态的字段是 `build_exit_code`，不能误读为缺少 `exit_code`。

## 正式度量复算

所有数值单位为 B，计数除外。三文件均为单切片 arm64 Mach-O executable。

| 指标 | thin16 | fat1 | thin16_export |
| --- | ---: | ---: | ---: |
| 裸文件 | 45,743,840 | 22,492,464 | 37,608,832 |
| tar.gz | 19,916,651 | 12,624,909 | 17,875,765 |
| `__text` | 24,132,084 | 14,621,552 | 24,132,084 |
| `__const` 合计 | 4,884,920 | 3,442,824 | 4,884,920 |
| 三项异常／展开表合计 | 7,404,108 | 3,640,608 | 7,404,108 |
| `__LINKEDIT.filesize` | 8,797,920 | 390,448 | 662,912 |
| export trie | 8,071,992 | 24,264 | 48 |
| 动态导出项 | 79,383 | 1,123 | 2 |
| LC_SYMTAB nsyms | 331 | 317 | 331 |
| 新采 nm 输出行数 | 331 | 317 | 331 |
| code signature blob | 354,768 | 174,512 | 291,696 |

异常／展开合计仅包括 `__gcc_except_tab`、`__unwind_info`、`__eh_frame`；const 合计仅为各段中名为 `__const` 的 section，不代表所有只读数据。

| 候选 | 正式 binary SHA-256 | tar.gz SHA-256 |
| --- | --- | --- |
| thin16 | `9057644514e8ed85d036675e98390fd06b50980be7b8ccfbda803c9075e94194` | `c306e877a7e5bdc57c3b30718bf161546a965ed11ca30bc14b83e4c26d2b8ff4` |
| fat1 | `ac2d250bf7d507c84ecf32c3003f975bd0572c8b5e150344b46f86d533a57210` | `fba21a3983582197468bf5f7aaf8a53697c67a8ff8eed6b85fd768d0b00cd1f6` |
| thin16_export | `73fe74b909551661f9d1bab2aa1df400f3778701298a47a33b62f01ab3584f4f` | `2dca97ebd37d67bc6427f2ec5288fa41d20d827733192e8915ffdd26e77b0fd6` |

### 文件段与导出诊断

| 文件段 filesize | thin16 | fat1 | thin16_export |
| --- | ---: | ---: | ---: |
| `__PAGEZERO` | 0 | 0 | 0 |
| `__TEXT` | 35,110,912 | 20,840,448 | 35,110,912 |
| `__DATA_CONST` | 1,753,088 | 1,179,648 | 1,753,088 |
| `__DATA` | 81,920 | 81,920 | 81,920 |
| `__LINKEDIT` | 8,797,920 | 390,448 | 662,912 |

各段 fileoff 连续且 filesize 合计等于裸大小。`__PAGEZERO.vmsize=4,294,967,296` 不计磁盘字节；LINKEDIT 的映射大小也不替代 filesize。三个候选的 export 信息来自 `LC_DYLD_INFO_ONLY`，不是假设一定存在 `LC_DYLD_EXPORTS_TRIE`。

thin16−fat1 的观测裸差为 **23,251,376 B，按 thin16 为基数减少 50.8295%**；tar.gz 差为 **7,291,742 B**。这支持候选间存在显著差异，但在补齐实际 argv 前，“确认只由 fat/1 对 thin/16 配置组合产生”保留为待完全验证的归因。

thin16−thin16_export 为 **8,135,008 B，17.7838%**；所有 file-backed section 内容 hash 相同，不止主要 section 尺寸相同。因此在这些实际产物上，没有观察到限制导出带来的 section 内容／代码删除；减量全部落在 LINKEDIT。

trie 减少 **8,071,944 B**，不能冒充整个文件减量；其余 **63,064 B** 包含签名 blob 减少 **63,072 B** 及其它 LINKEDIT 布局的净抵消 **8 B**。此为文件布局观察，不声称 signature 是唯一其它链接元数据。限制导出后仍有 `__mh_execute_header` 和 `_main` 两项。

不把 Fat LTO／Thin LTO 名称理解为固定导出契约；不把 strip 理解为删除动态 export trie。本候选结果不证明动态查找／FFI 兼容，也不能推广成所有程序的限制导出均不影响代码保活。

## strip 故障与复验

正式 Cargo 入口环境含同工具链 `DYLD_LIBRARY_PATH=<sysroot>/lib`。实验 cargo adapter 在 exec 真正 Cargo 前恢复该键；分析工具则移除该键。重新计算 rustc、cargo、rust-objcopy 和 libLLVM 的 hash，均与 metadata 相同。Rust objcopy 自报 LLVM `23.1.1-rust-1.99.0-stable`，分析工具为 Homebrew LLVM21，不能混用动态库。

初次 `_v1` 的 objcopy 加载失败，即使 Cargo exit=0，仍属无效样本，不纳入正式增长／差值结论。独立核对保留产物与标准化副本：

| 候选 | v1 原文件 | v1 LC_SYMTAB nsyms | v1 nm 行数 | 标准化副本 | 正式−标准化 |
| --- | ---: | ---: | ---: | ---: | ---: |
| thin16 | 89,150,072 | 372,104 | 367,631 | 45,743,824 | 16 |
| fat1 | 44,463,544 | 623,373 | 148,491 | 22,492,448 | 16 |
| thin16_export | 81,015,048 | 372,104 | 367,631 | 37,608,816 | 16 |

v1 的 nsyms 不等于 nm 输出行数，执行者报告中的初始 372,104／623,373／372,104 是 LC_SYMTAB 计数，不能改写成 nm 计数。

三组标准化副本与正式文件的 section 名称／尺寸、动态 export 名称及 trie 长度一致；正式文件的签名 blob 各多 16 B，LINKEDIT 和总长度也各多 16 B。支持最终 strip 已生效，但不声称整个文件字节相等。没有读取或使用父 agent `.tmp/release-strip-diagnostic` 作为正式候选。

正式 build log 的 objcopy warning 出现在 `Compiling peri-tui` 前，并明确归属 `rmcp` build script；薄候选重复旧 PID 72865、fat1 为旧 PID 812。最终 peri 编译阶段没有该 warning，文件 nsyms 的大幅下降及标准化交叉验证亦支持缓存警告与最终 strip 失败不是一回事。这里并非只依据“警告在前面”就认定成功。

分析工具失败另行分类：fat1 和 thin16_export 的 `failed-dyld-{llvm-size-m,exports-trie}` 状态确为 -6，已保留并重采成功；thin16 下同名记录却是 exit=0、stderr 为空，不能因文件名前缀而称其为 abort。此标签缺陷不影响现有正式指标，但报告应准确描述。

## 配置证据缺口与可操作修正

正式 manifest 是 `lto="thin"`、`codegen-units=16`；构建命令及 effective Cargo 环境相互一致：fat1 增加 LTO=fat／CGU=1，其余两组无这些覆盖，也未发现所审查的 RUSTFLAGS／encoded flags／wrapper 覆盖。源码 SHA、lock、嵌入 JS 在核查时与首末 integrity 相符：

- Cargo.lock：`dac72db776e1705fac780d4f9d70fc95066c10c66e0f540b9e2804df6269caa5`。
- Workflow JS：`0ca2007a66096bfa792bbaed1ba8f57142bdbd5dcb670ed53a395920ec8947a4`。

但以下实际 argv 证据与候选身份不一致：

- `raw/thin16/observed-rustc-command.txt` 明确含 `-C lto=fat -C codegen-units=1`，不能支持 thin16 的实际配置。
- `raw/fat1/observed-rustc-command.txt` 含 fat/1，与其目标配置一致；但不是完整逐候选 wrapper 调用链。
- `raw/thin16_export/observed-rustc-command.txt` 不存在；所审查 raw 未找到其它逐候选 wrapper argv 补证。

文件大小、profile 环境、运行时能力和输出 hash 不能代替实际编译调用，也不能单独证明 LLVM LTO-unit 内部标志。复用缓存意味着还需分清最终链接调用与此前依赖编译配置。

建议执行者保留错配记录，补充可追溯到候选及时间的 wrapper 原始 argv（若早已存在，不必重构建），明确是正式链接、旧构建还是事后诊断；若只能事后重链采集，要另记新产物 hash 并交叉比较，不将新调用冒充旧样本的原始执行证据。若要声称验证 LLVM 的 LTO-unit 标志，还须保存对应输入模块证据；仅 profile 值或 rustc `-C lto` 不足以证明该内部标志。

同源码两组之间始终把 LTO 与 CGU 视作配置组合；不将 50.83% 减量独立归给 LTO，不给主干源码增量百分比。

## CLI、ACP 与报告交叉核对

raw CLI 三组均 `--version`／`--help` exit=0，version 为 `peri 0.2.0`。父 agent 的三个正式 result 均 PASS、协议版本 1、非空 sessionId、EOF 后 exit=0，result 中 binary 路径指向本次复算的对应正式文件：

- `.tmp/release-smoke/head-thin16-final/result.json`
- `.tmp/release-smoke/head-fat1/result.json`
- `.tmp/release-smoke/head-thin16-export/result.json`

核对 smoke 脚本只发 initialize 和 session/new，不发 prompt；以隔离 HOME／workspace／TMPDIR、dummy provider `127.0.0.1:9`、`sonnet` alias 运行，未透传真实模型凭据。脚本会检查响应 ID／error、sessionId，并等待 EOF 后退出。这里复核现存执行证据，没有重新运行 smoke；result 未附 binary hash，因此路径关联不等于独立证明烟测当时文件 hash，不过现存路径与正式归档一致。

初始 fixture FAIL 仍在 `.tmp/release-smoke/prior-baseline/result.json`，更正后的 PASS 在 `prior-baseline-corrected/result.json`。FAIL 的 result 是“stdout closed before response”，stderr 是“No LLM provider configured”；alias 原因来自任务上下文与更正过程，不能仅凭该 result 当作已独立定位的唯一根因。

这些局部 smoke 不证明模型推理、prompt、工具执行、Workflow、动态查找、完整恢复或所有 teardown 行为正确；EOF exit=0 也不是资源泄漏审计。

`01_experiment_report.md` 的正式大小、hash、主要分段、计数、strip 交叉数据及 main 尚未验证的边界与复算一致。需要修订／补充的叙述：

1. 加入上述实际配置 argv 错配与缺失，不将“原始证据交叉核验通过”理解为所有方法证据完整通过。
2. thin16_export ACP 的“待补齐”已有 PASS result，应更新执行状态和路径。
3. failed-dyld 标签按实际 exit code 区分，thin16 同名记录不是失败。
4. 正式重链复用 release cache，耗时不作 benchmark；报告已正确限定，不需推断速度。

压缩包成员文件名／mode／uid／gid相同，但 mtime 不同，tar 未归一化时间。解包内容和各包尺寸／hash 已核实；压缩减量仅是本轮实际封装观测，不保证重打包得到相同字节或完全排除元数据的微小贡献。

## 报告与证据路径

- 计划：`docs/experiment-release-binary-size/00_plan.md`。
- 执行报告：`docs/experiment-release-binary-size/01_experiment_report.md`。
- 本独立验证：`docs/experiment-release-binary-size/01_verification.md`。
- 正式汇总：`.tmp/release-size/summary.json`。
- 正式证据：`.tmp/release-size/raw/{thin16,fat1,thin16_export}/`。
- 正式候选：`.tmp/release-size/artifacts/{thin16,fat1,thin16_export}/peri`。
- 原无效样本：`.tmp/release-size/raw-v1/`、`artifacts/*_v1/`；标准化交叉副本：`artifacts/*_v1_normalized/`。

本验证独占写入仅此文件；不改执行者报告。后续第二轮在 `02_verification.md` 中处理，不能用第一轮替代尚未完成的 2×2 源码／依赖归因。

## 日期修订：2026-10-07，HEAD argv 新执行补证

**最终判定：PASS（体积结论及实际配置的新执行交叉验证）**。文首 PARTIAL 和当时发现的错配／缺失完整保留，代表原证据状态；本修订不声称找回原始 thin16／export 构建调用，不证明 bitwise reproducibility 或全面功能兼容。

本次用户授权补核并追加此修订。补证为新的独立执行，非旧 raw 覆盖：

| 补证候选 | 开始时间 UTC | 裸文件 B | 新 binary SHA-256 |
| --- | --- | ---: | --- |
| thin16_argv | 2026-10-07 13:06:03 | 45,743,840 | `bfe2c5044975f7f15c490ab419d3c52a0089a6f270a3d08a84acdc0902d8145b` |
| thin16_export_argv | 2026-10-07 13:07:26 | 37,608,832 | `01621564c7f752d3d073ba1eee894eeb01f01ff48e5d4b4f64a23c9169c98db4` |

`summary-argv.json`、两组 `actual-rustc-commands.txt` 和 build.log 的实际根命令彼此相符：均为 `opt-level=z`、`lto=thin`、`codegen-units=16`、`strip=symbols`；export 候选另有 `link-arg=-Wl,-exported_symbol,_main`。有效 Cargo 构建键与原 thin 目标一致，无所审查的 flags／wrapper／profile 覆盖；metadata 和末次 integrity 固定研究提交 8c8c346a、原 lock／JS 和工具链 hash，不依赖后来分支当前 HEAD。

错配证据归档在 `raw/argv-mismatch-archive/`。这不是把原 thin16 的 fat argv 就地修成 thin；新的 `_argv` 目录和执行时间明确分开。

### 新旧产物比较

验证者重新读取补证 binary／tar 并计算 hash，重采 otool／llvm-size／exports／nm；工具 exit=0、stderr 为空，新 nm 计数均 331。raw 的构建、分析及 CLI／tar 状态均为 0。未调用执行者核验脚本、未重构建或改动候选。

- 原正式 binary hash 与新 hash **不相同**；原 2×2 和第一轮表格仍使用原正式归档，不用补证替代。
- 新旧全部 section 名称／尺寸、裸大小、const／异常表／LINKEDIT／trie 长度、动态导出名称与计数一致。
- 新旧文件型 section 内容中，`__TEXT/__text` 不同：每对均 **4,930 字节不同**；其他文件型 section 内容 hash 一致，zerofill 不当作磁盘内容。
- 原两候选 text SHA-256 为 `ee6746c8b19b59c816ad67cbba6552c67e65570ccf0233d71808fee0e94421a4`；新两候选均为 `020dfd92f7dc0af7352b2fa8c56b52a4a7278e0fcf6e56bafef38c69988174fc`。
- UUID 也不同，signature blob 尺寸相同；不能把新旧 SHA 差异解释成“仅 UUID／签名变化”。未定位 text 差异来源，不推断功能等价或优化随机性。
- 同一轮内部 thin／export 的 text 内容依然相同，支持“此导出诊断的减量落在 LINKEDIT”的观察；不保证任意导出限制都不会改变代码。

新 tar.gz 为 19,916,680 B／17,875,783 B，比原正式包分别多 29 B／18 B；其内容各等于对应新 binary，不是原 binary。新包 SHA 分别为 `950766801aad1f7c607254b97c5d28e7389d3118a6ff8c7b4701c691c970b1e6`、`44999d38bbdca7471540d8782ff87abb84ee395f32d564c17b632440da88619b`。不能因裸大小一致就声称压缩样本一致，也不能用新包替换第一轮正式包参与交互计算。

### 判定理由与剩余边界

新执行证明了 thin/16 和限制导出参数实际生效，且独立重现了旧正式产物的体积、结构尺寸和 export 指标。fat/1 的既有 recorded argv 与其环境／目标一致。就体积研究而言，原配置缺口获得足够的后续交叉支持，故最终判定限定为 PASS；原 thin 调用的历史溯源仍不完整，字节级重现没有通过。

`verification-argv.json` 的两组 errors=[] 与上述度量核对相符，但其 section 检查主要是名称／尺寸，不足以证明 section 内容 hash 相同；此修订以独立字节比较明确补上差异。

补证 CLI 状态已通过；现有父 agent ACP PASS 仍绑定原正式候选路径，不自动延伸为新 hash 候选已跑 ACP。不宣称新候选完成额外 ACP、模型或全面功能回归。

交付前再次核对更新后的 `01_experiment_report.md`：export ACP 已更新为 PASS，原“待补齐”问题关闭；新增补证章节已正确区分原执行与新执行，列出新 SHA、压缩变化，并明确不把原 ACP 自动转移到新候选。仍未具体记载独立字节比较发现的 4,930 字节 text 差异；“text 相同”仅可理解为尺寸相同，不是内容相同。第二轮报告也已追加补证身份边界，固定 2×2 数值与交互见 `02_verification.md`，不在本修订把第一轮变成主干源码归因。

本次仍只写两份 verification。未来基于更新 670 提交的生产 release 构建是新的交付验证，不改变本实验固定 8c8c346a 的研究身份。

## 日期修订：2026-10-07，新执行的 hash 绑定 ACP 烟测

父 agent 重新执行烟测，不复用旧 run。本验证核对新 result 的 `binary_sha256` 与对应归档 binary 的独立 SHA-256、原正式或补证 summary 三方一致，binary 路径也匹配：

- `.tmp/release-smoke/thin16-hashed/result.json`
- `.tmp/release-smoke/fat1-hashed/result.json`
- `.tmp/release-smoke/thin16_export-hashed/result.json`
- `.tmp/release-smoke/thin16_argv-hashed/result.json`
- `.tmp/release-smoke/thin16_export_argv-hashed/result.json`

五组均 PASS、protocolVersion=1、非空 sessionId、EOF exit=0。脚本在启动前记录 binary hash，只发送 initialize／session/new，不发 prompt；验证者复核现存新执行证据，没有自行重跑。此前“不同 hash 的补证候选尚无 ACP 关联”局部缺口现已关闭，原说明仍保留为当时事实。

最终限定 PASS 不变；hash 关联不证明完整模型／工具／Workflow 回归，也不消除新旧 text 的 4,930 字节差异或恢复原 argv。五组原始与补证身份、体积和压缩度量保持不变，不因新烟测替换 2×2 样本。
