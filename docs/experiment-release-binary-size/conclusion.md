# macOS release 体积调查与构建配置决策

日期：2026-10-07。状态：对照、独立验证、对抗审查及最终 macOS 默认配置构建验证完成；用户已授权提交并合入 `pre-release/main`，集成状态以 Git 日志为准。

## 结论

体积增长的主要可控来源是 release 配置组合从 Fat LTO / 1 CGU 改成 Thin LTO / 16 CGU，而不是简单的依赖数量或功能数量增长。当前源码在旧配置下仍接近 main 的大小。这里的“最佳”限定为本次已测候选中兼顾现有行为与分发体积的选择，不是对所有优化参数、平台或运行性能的全局最优证明。

## 2×2 对照

同一 macOS ARM64 主机、Rust 1.99.0 工具链、默认生产 feature、`opt-level=z`、`strip=symbols`、panic unwind，使用每个提交自己的锁文件和生产构建入口。

| 源码 / 配置组合 | 裸文件 B | 裸文件 MiB | tar.gz B | tar.gz MiB |
| --- | ---: | ---: | ---: | ---: |
| main `d7ee444e`，fat/1 | 20,432,000 | 19.485 | 11,362,715 | 10.836 |
| main `d7ee444e`，thin/16 | 39,250,416 | 37.432 | 16,978,485 | 16.192 |
| 当前 `8c8c346a`，fat/1 | 22,492,464 | 21.450 | 12,624,909 | 12.040 |
| 当前 `8c8c346a`，thin/16 | 45,743,840 | 43.625 | 19,916,651 | 18.994 |

用户记忆中的约 20 MB 和约 10 MB，在 main 的裸文件与压缩发布包这两种口径下都有对应实测。压缩包只含同名 `peri`，元数据时间戳未归一化，因此压缩字节数是本次观测，不宣称逐字节可重复。

- 同一当前源码改配置：减少 **23,251,376 B，50.83%**；压缩包减少 **7,291,742 B，36.61%**。
- 同一 main 源码改配置：thin/16 增加 **18,818,416 B**。
- 同一 fat/1 配置比较源码：当前比 main 增加 **2,060,464 B，约 1.965 MiB**。
- 同一 thin/16 配置比较源码：当前比 main 增加 **6,493,424 B，约 6.193 MiB**。
- 配置与源码的交互差为 **4,432,960 B**。不能给出唯一、与比较路径无关的“源码占增长百分之几”。源码轴包含锁文件和补丁变化，不单独等同于业务代码。

## 当前源码的分段解释

| 字节项 | thin/16 | fat/1 |
| --- | ---: | ---: |
| `__text` | 24,132,084 | 14,621,552 |
| 三项异常展开表合计 | 7,404,108 | 3,640,608 |
| 各段 `__const` 合计 | 4,884,920 | 3,442,824 |
| `__LINKEDIT` | 8,797,920 | 390,448 |
| 其中 export trie | 8,071,992 | 24,264 |
| 动态导出项数 | 79,383 | 1,123 |

Fat/1 不只减少导出名称，还减少机器代码、常量和异常展开数据。export trie 是 LINKEDIT 的子项，不可重复相加；文件字节不能用 `__PAGEZERO` 或 `vmsize` 衡量。此对照同时改变 LTO 与 CGU，不声称分别测出了两者的贡献。

Thin/16 限制导出的诊断候选为 **37,608,832 B（35.867 MiB）**：text、常量及三项异常表与 thin/16 相同，减量位于 LINKEDIT。它仍比 fat/1 大很多，而且动态查找/FFI 兼容性未完整验证，所以不采用为生产修复。

## 配置决策

将默认 `[profile.release]` 恢复为 `lto = "fat"`、`codegen-units = 1`，保留 `opt-level = "z"`、`strip = "symbols"` 及 panic unwind；dev 配置不变。

原因：用户关注分发体积，现有 release 路由自动消费该 profile；Fat/1 为已测安全候选中最小，且延续 main 原有配置语义。不新增 fast profile、平台专用导出白名单或依赖兼容层。开发构建仍走 dev，需要临时比较快速 release 时可显式用 Cargo profile 环境覆盖，不改变发布默认值。

不修改 `panic = "abort"`，因为仓库已有基于 `catch_unwind` 的失败结算路径。也不以删 TIFF/SQLite/TLS 为本次修复，因为 main 已有相同版本的大依赖，相关功能仍使用它们；workspace lock package 条目减少不能替代最终链接分析。

## 验证与适用范围

五组正式候选均通过 `--version`、`--help` 与隔离 HOME/cwd 的 ACP initialize、session/new、EOF 退出 smoke；不发 prompt，模型使用 dummy loopback 地址。这只覆盖局部装配和协议行为，不等于模型执行、所有工具、Workflow、恢复、TUI 或跨平台完整回归。

本机初次 Rust objcopy 无法加载自带 LLVM 动态库，Cargo exit=0 但 strip 失败，造成错误的大文件。此类样本全部排除；固定有效工具环境后重新链接，独立核验实际 strip、Mach-O、文件 hash 及包内容。Homebrew 度量工具和 Rust 工具使用各自匹配的动态库环境。调查不修本机工具链安装、不向项目生产构建脚本加入这个机器专属 workaround。

第一次逐候选 rustc argv 记录错配/缺失，已用独立的新执行补证其配置及体积方向，独立验证最终为限定 PASS。原执行溯源仍不完整；新旧 hash 不同，text 有 4,930 字节内容差异，但 section 尺寸、裸大小和导出度量一致。不声称字节级重现，不把后续 argv 冒充原执行证据。七组归档候选已按各自 hash 重新通过 ACP 局部 smoke。

不依据一次固定顺序、有缓存且存在其它主机任务的 elapsed 推断构建速度或运行性能。其它 OS/架构未本机验证；配置来自 main 的既有跨平台 profile，但本次实测收益只适用于 macOS ARM64。

## 最终生产配置验证

独立 feature 分支 `fix/release-binary-size-20261007` 同步到已提交的 `pre-release/main` 基线 `67087e9a5b5ca6fc46f34f47262bc99b065ca039`，只修改两项 release profile，随后用默认 profile 构建（不通过环境覆盖 LTO/CGU），实际 root rustc argv 确认 Fat LTO / 1 CGU。

| 最终交付验证 | 结果 |
| --- | --- |
| macOS ARM64 release build | exit 0，Rust 1.99.0 |
| 裸文件 | 22,509,088 B，21.466 MiB |
| 同名 tar.gz | 12,631,036 B，12.046 MiB |
| Binary SHA-256 | 见本次 worktree `.tmp/release-size/raw/final-default-release/summary.json`，ACP 记录绑定同 hash |
| export trie / 导出项 | 24,264 B / 1,123 |
| 静态 symbol 项数 | 317，strip 有效 |
| CLI | `--version`、`--help` exit 0 |
| ACP 局部 smoke | initialize、session/new、EOF exit 0，PASS |
| pre-commit | typos、22 条依赖边界检查通过；无 Rust 源码改动，Rust glob 门未命中 |
| 文档 / diff | 本次文档相对链接与 `git diff --check` 通过 |

研究基线 `8c8c346a` 与最终集成基线分开：最终产物不覆盖 2×2 的同源码数据，不将其与研究 thin/16 直接计算唯一减量百分比。Cargo.lock 未变；原工作区的未提交改动不纳入此提交。

最终验收曾有构建 exit 0 但 output 目录随后消失，无法读取产物；该次不算交付通过。另一次未固定 rustc 时，dependency cwd 的旧 MSRV toolchain 被 rustup 选择，造成混合版本 metadata 错误，亦作废。最终在私有 `.tmp` 输出目录固定绝对 Rust 1.99.0 编译器/Cargo 和 `RUSTUP_TOOLCHAIN=stable`，成功后立即归档 binary 再验收。这些机器环境处理不写入生产构建脚本，不修无关依赖，也不把失败变成成功。

最终归档 binary 位于 `.tmp/release-size/artifacts/final-default-release/peri`，hash 绑定的 ACP 结果为 `.tmp/release-smoke/final-default-release/result.json`。原始日志与 metadata 保留在 `.tmp/release-size/`，独立 target 位于其中的 `build-output-final-fixed`。

## 证据导航

- [计划](00_plan.md)、[第一轮实验](01_experiment_report.md)、[第一轮独立验证](01_verification.md)。
- [第二轮实验](02_experiment_report.md)、[第二轮独立验证](02_verification.md)、[对抗审查](adversarial_review.md)。
- 原始命令、环境白名单、构建/分析日志、可解析 summary 和五组二进制位于本 worktree gitignored 的 `.tmp/release-size/`；ACP smoke 位于 `.tmp/release-smoke/`。这些本机大产物不提交，报告保留度量、输入和复验命令，不声称它们已永久归档。
