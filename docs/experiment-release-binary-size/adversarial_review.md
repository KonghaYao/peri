# 对抗验证与生产配置安全审查：release 体积

日期：2026-10-07。审查快照：2026-10-07T13:17:45Z。

## 裁定与范围

**CONCLUSION_STANDS（限定于已测 macOS ARM64 体积与配置组合；生产交付验收尚未完成）**。

未找到足以推翻“thin/16 → fat/1 是本次主要可控体积来源”的致命缺陷。两种源码下配置效应均明显大于同配置源码差额；交互存在但没有改变方向。fat/1 可作为本次已测候选中面向分发体积的默认配置选择，不能升级为全部平台、构建速度、运行性能或完整行为安全性的最佳配置。

研究源码是 `8c8c346a`，历史 main 是 `d7ee444e`；当前 feature 为 `fix/release-binary-size-20261007`，集成基线为 `67087e9a5b5ca6fc46f34f47262bc99b065ca039`。最终默认 profile 构建由父 agent 串行执行，本审查没有运行 Cargo、源码修改、commit 或 merge，也没有以运行中的日志判定完成。

读取根 CLAUDE、standards 索引及 documentation/testing/git/rust/architecture 路由，计划、01/02 实验、01/02 verification、conclusion、三个 summary，以及必要的命令、wrapper 和 CI/构建脚本。02_verification 在审查期间已落盘，01_verification 的补证 PASS 修订亦已读取；文首旧 PARTIAL 是历史状态，不应抹除。

所有 exec 的 cwd 都是指定 worktree，`login=false`。没有读取历史 incoming-env、env.json 或环境全集；没有输出用户秘密。独占写入仅本文件。

## 独立复算，不依赖执行者度量函数

直接读取七个归档 binary，计算长度和 SHA-256，自行用 `struct` 解析 Mach-O segments/sections、LC_SYMTAB 和 export trie；逐个读取 tar 成员，不解压到工作区。未导入执行者 driver/verify 的实现。七个 binary SHA 与对应 summary 相符，七个包均仅含 `peri` 且成员内容等于各自 binary。

| 固定源码 / 配置 | binary B | tar.gz B | export trie B | 导出数 | locals |
| --- | ---: | ---: | ---: | ---: | ---: |
| main d7，fat/1 | 20,432,000 | 11,362,715 | 35,080 | 1,585 | 0 |
| main d7，thin/16 | 39,250,416 | 16,978,485 | 7,192,176 | 72,321 | 0 |
| 研究 8c，fat/1 | 22,492,464 | 12,624,909 | 24,264 | 1,123 | 0 |
| 研究 8c，thin/16 | 45,743,840 | 19,916,651 | 8,071,992 | 79,383 | 0 |
| 研究 8c，thin/16 export 诊断 | 37,608,832 | 17,875,765 | 48 | 2 | 0 |
| 研究 8c，thin16_argv 新执行 | 45,743,840 | 19,916,680 | 8,071,992 | 79,383 | 0 |
| 研究 8c，thin16_export_argv 新执行 | 37,608,832 | 17,875,783 | 48 | 2 | 0 |

section 尺寸、异常三表合计、const、LINKEDIT 均与报告相符。PAGEZERO 不计入磁盘文件体积，export trie 不与 LINKEDIT 重复相加。locals=0、既有 strip 交叉验证及正常度量共同支持正式样本裁剪有效，不是仅根据 Cargo exit=0 判断。

七份 `.tmp/release-smoke/<候选ID>-hashed/result.json` 均为 PASS、exit=0，所记 binary hash 与本次复算相符。这补足了早期 verification 对补证 smoke 的时间边界，但仍只证明记录所覆盖的 initialize/session/new/EOF 行为；本审查没有重跑 smoke。

## 攻击一：原 argv 错配意味着归因是伪证

**严重程度：原始缺口严重；补证后对体积方向为中等剩余限制，不致命。**

原 thin16 observed argv 实际含 fat/1，export 候选缺少原 argv，不能用意图配置或大小反推原执行。`raw/argv-mismatch-archive/README.md` 明确保留错误记录，没有把错配就地改写成 thin。现在的两个 `_argv` 是新执行，而非恢复旧 process 日志。

逐候选读取 `actual-rustc-commands.txt`，其根命令经去除行首尾空白后均存在于本候选 verbose build.log：

- `thin16_argv`：`opt-level=z`、`lto=thin`、`codegen-units=16`、`strip=symbols`。
- `thin16_export_argv`：同上，另有 `link-arg=-Wl,-exported_symbol,_main`。
- main_fat1：bare `-C lto`、CGU=1，对应历史 manifest 的 fat；main_thin16 为 thin/16。
- 研究 fat1 原 observed argv 与 fat/1 一致；其证据强度不同于有本候选 verbose 日志的后补 thin，不冒称所有历史 root 调用都具同等追溯性。

实验 Cargo 入口 wrapper 记录有效 Cargo 环境后通过 `execve` 原样转发 argv；不是补造 rustc 命令的文本模板。新命令与新产物独立编号、时间和 SHA 绑定，足以作为同研究源码配置效应的后续交叉支持，但不是全部依赖编译调用链或 LLVM 内部 LTO-unit 标志验证。

**实质剩余问题**：新旧每对 `__TEXT/__text` 均有 **4,930 字节内容不同**，大小相同，其他文件型 section 内容相同；新旧 SHA 不同。新旧两轮内部各自 thin/export 的 text 内容一致。故新证据复现的是体积与结构指标，不是机器码或功能等价。不能把 hash 差异全归为 UUID/签名，也不能把新包数值或新 smoke 转嫁给旧 hash。

**如何反驳**：若坚持声称原执行已全量溯源，必须提供原 process 同时采集的完整记录，目前未做到；若坚持字节级重现，需定位这 4,930 字节差异并控制其输入来源。现有结论不依赖这两个更强主张，因此无需为体积方向再构建。执行报告可显式补记 text 内容差异，以免“text 相同”被误解成内容相同。

## 攻击二：交互效应使“主要来自配置”失效

**严重程度：不构成缺陷；若给唯一归因百分比则成为中等误导。**

用独立读取的长度重算，配置效应是研究源码 **23,251,376 B**、main **18,818,416 B**；源码/依赖效应是 fat/1 下 **2,060,464 B**、thin/16 下 **6,493,424 B**。交互为：

`(45,743,840 - 22,492,464) - (39,250,416 - 20,432,000) = 4,432,960 B`。

总观测增长 **25,311,840 B** 可沿两条路径拆解：`2,060,464 + 23,251,376`，或 `18,818,416 + 6,493,424`。两条路径都以配置差额为较大项；但分解不是唯一，不应选择某条路径宣布固定归因比例。压缩包交互同样非零，为 **1,675,972 B**，裸文件与包不混比。

**如何反驳**：要推翻方向，应展示某个已测格错误，或证明它实际使用了不同的未控制输入并足以解释这些差额。现有 argv 补证、工具/锁/JS 输入记录与独立产物复算未支持该替代解释。此处 2×2 是“源码/依赖 × 配置组合”，不是“LTO × CGU”；缺少 thin/1、fat/16，不能拆出 LTO 或 CGU 的单独贡献。

## 攻击三：导出限制才是更合适的生产修复

**严重程度：未成立；扩展动态导出兼容性主张将是严重风险。**

thin/export 相对 thin 减少 **8,135,008 B**，全部文件段减量在 LINKEDIT，trie 自身减少 **8,071,944 B**。它依然比 fat/1 大 **15,116,368 B**。fat/1 的 text、展开表和常量也更小，不能用仅处理导出名称解释全部配置效应。

限制导出仍保留两个符号，并没有覆盖所有动态查找/FFI；相比延续历史 fat/1 组合，它新增一个未经完整行为验证的 linker 边界。fat/1 自身导出数也有变化，历史先例及局部 smoke 不构成完整 ABI/FFI 证明。

**如何反驳**：需提供实际依赖的动态查找/FFI 行为测试，以及一个在相同输入下小于 fat/1 且通过相关行为的已测候选。目前不存在；不应因此引入导出白名单或 panic=abort。

## 攻击四：8c 的结果被偷换成 670 的生产验收

**严重程度：交付门禁为严重；当前明确待验证，尚非致命研究缺陷。**

`git diff --numstat 8c8c346a..67087e9a` 显示 **113 个文件，新增 3,065 行、删除 1,236 行**。研究源码与当前生产基线不是同一源码，不能把 22,492,464 B、研究 smoke 或报告中的“同源码”转移到最终构建。该区间 Cargo.toml、Cargo.lock、两个构建入口脚本没有 diff，能支持输入连续性，但不能消除业务代码差异。

**如何反驳/关闭门禁**：父 agent 完成基于 670 的默认发布命令，记录命令/受控覆盖、退出码、实际 fat/1、有效 strip、新 binary SHA/尺寸、CLI 及绑定该 SHA 的 ACP smoke，并在 conclusion 补齐最终构建与提交/合入状态。最终命令应是正常 `build --release`，不能只拿实验 `rustc -- ...` 的重链替代。若失败，不得以研究 PASS 代替交付 PASS；也不因命令已启动而宣称成功。本报告截至快照没有对该最终执行作验收。

## 攻击五：局部候选被推广成全平台或性能最佳

**严重程度：中等外部效度限制；绝对“安全/最佳”措辞会升级为严重。**

单主机 macOS ARM64、单工具链、固定顺序共享 target cache、非 benchmark elapsed，不支持运行性能或构建速度结论。胖瘦两配置均缺少独立重复/随机顺序的计时研究；本次不需要也不允许本审查自行启动新构建。没有测试所有功能、优化参数或平台，缺少 thin/1、fat/16 等候选。

root `[profile.release]` 不只被原生 macOS 消费：release-agent CI 含 Linux、Windows 和 macOS 多目标；pre-release 还运行 Emscripten release。`scripts/cargo-wasm.sh` 有既存 panic=abort/链接覆盖，但没有 LTO/CGU override，因此 root 两项修改同样影响其默认值。“dev 不受影响”准确，“本次只影响原生 macOS”不准确。README 写原生发布默认值，不等于承诺仅修改原生路径。

**如何反驳**：由既有跨平台 CI 验证更新基线的构建与局部行为，再按风险补特定运行/FFI/恢复行为；不把 Linux/Windows/WASM 的体积和性能预先写成已验证。历史 main 使用 fat/1 是恢复既有政策的支持证据，不是新基线跨平台兼容的证明。未测平台的发布安全最终依赖发布门禁，不能由本机审查签字代替。

## 生产 diff 与文档精确性

以当前 HEAD 670 的 Cargo.toml 为基线，用 `tomllib` 比较结构，并检查实际 Git diff：

- release 只改 `lto: thin → fat`、`codegen-units: 16 → 1`。
- `strip="symbols"`、`opt-level="z"` 不变；没有新增/修改 panic 字段，原生默认 unwind 没有本次改成 abort。
- dev 的 `debug=1` 和依赖 `debug=false` 完全不变；profile 外 manifest 内容也完全相同。未改依赖、锁文件、构建脚本、源码或生产导出白名单。
- patches README 新增的 profile 描述与普通 wrapper 发布命令准确，链接到有平台限制的 conclusion；没有声称 fat/1 构建最快/运行最快。默认 profile 不能保证调用者外部覆盖后的参数，故最终实际构建仍须检查。
- 本次是发布默认优化政策恢复，不是架构/模块入口或 canonical 测试路由变更；受影响说明集中在 manifest、patches README 与调查结论，未发现必须新增平行标准规则的理由。

## 交付建议与证据保留

接受 fat/1 为已测候选中体积优先的选择，保留 strip/opt/panic/dev；不引入新 profile、导出白名单或依赖裁剪。**允许配置选择，不代表最终 commit/merge 已完成或已获本审查验收。** 由父 agent 完成用户已授权的最终验证、commit 和合入 `pre-release/main`；本审查只提出门禁，不执行这些操作。

原 argv 溯源缺口、text 内容差异、交互及未测平台应留在结论边界内，不以“全部验证通过”抹平。七份 hashed smoke 是后来新增证据，验证文件当时的 smoke“待补齐”不应被误当成今天仍缺。

raw/summary/binary 在 gitignored `.tmp`，报告进入 Git 不等于原始产物永久归档。这是中等可复查性限制，不影响本次现场复算；如果后续需长期独立复核，应受控保存清理过的原始调用、summary、hash 和产物，禁止归档历史环境全集或秘密。当前结论已明确本机证据并非永久存档，不必为本任务增加仓库大文件。

**修改 paths**：`docs/experiment-release-binary-size/adversarial_review.md`。本审查没有修改其余文件。
