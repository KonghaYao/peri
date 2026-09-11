# md-scan-matrix 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-10
> 依据：`side-projects/md-scan-matrix/Cargo.toml`、源码与 testing standard（无项目级 CLAUDE.md）

## 架构速览

- 独立实验 crate：`side-projects/md-scan-matrix/Cargo.toml` 自带 `[workspace]`，根 workspace build/test 不覆盖。
- 数据流：`matrix::Case → evaluation::run → Experiment → report::print → main ExitCode`；scanner 提供真实 Markdown 扫描、替换与解析，report 不定义成功条件。
- 用途：验证 pulldown-cmark 0.12 与 ratatui-kit-markdown 0.3.0 的图片占位策略；不接入生产 TUI 图片渲染路径。
- 预期负例仅为明确输入中的 token 碰撞：指定编号必须精确出现两次；token 消失、额外重复、结构变化、命中错误仍为失败。`EXPECTED COLLISION` 与未预期 `FAIL` 分开显示。
- 退出契约：A–E/G 矩阵、F 流式、G 碰撞补充与 H reference 四组均运行且无未预期失败才返回 0，否则返回 1。

## 速查表

| 我想做什么 | 主文件（项目目录内） | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 新增 Markdown 实验输入 | `src/matrix.rs` | `Case`（:6）；`matrix_a`（:18）至 `matrix_g`（:323） | 每个输入明确命中数、说明与指定 token 碰撞；45 个原矩阵 case 按输入族组织 |
| 改扫描范围、alt/url/title/id | `src/scanner.rs` | `md_options`（:30）；`scan_images`（:39） | pulldown-cmark OffsetIter 的原文 byte range；代码块/字面回退不当图片；不调用 report |
| 改占位 token 或重编号 | `src/scanner.rs` | `TokenKind`（:78）；`replace_images`（:98）；`replace_collision_free`（:108） | 逆序替换原区间；重编号跳过源文本已存在的 token，保持图片与 side table 顺序 |
| 改结构与 span 投影 | `src/scanner.rs` | `block_rows`（:144）；`shape`（:185）；`parse`（:191） | shape 比较块种类与每行 span 数，忽略内容；token 保留另行评估 |
| 改矩阵语义或精确负例 | `src/evaluation.rs` | `evaluate`（:41）；`tokens_match_expectation`（:140）；`Outcome::pass` | 命中、原文切片、两种占位结构、token 精确计数、语法残留分别裁决；负例不反转整条保留断言 |
| 改流式序列与前缀检查 | `src/evaluation.rs` | `streaming_checks`（:249）；`closed_prefix_stable`（:233） | F1–F4 零命中、F5 一命中；`get(..n)` 比较已闭合前缀，当前块减少时判失败且不越界 |
| 改碰撞解决验收 | `src/evaluation.rs` | `collision_checks`（:305）；`collision_resolved`（:375） | NUL/PUA/Plain 朴素碰撞精确两次；重编号后每图一个唯一 token，源文本不占用新 token，用户原串计数不变 |
| 改 reference 验收 | `src/evaluation.rs` | `reference_checks`（:389） | 检查定义/未定义引用及 inline 混排的 alt/url/id 与原文切片 |
| 改进程退出结果 | `src/main.rs` + `src/evaluation.rs` | `main`；`Experiment::{failures,exit_code}`；`run`（:211） | 所有实验组统一决定退出码；空组不能以空集合视为通过 |
| 改展示 | `src/report.rs` | `print`（:5）；`verdict`（:59） | 只消费结构化 verdict，显示 PASS / EXPECTED COLLISION / FAIL 与失败数 |
| 验证评估与退出契约 | `src/evaluation_test.rs` | `evaluation::tests` | 用真实 Markdown 解析验证额外碰撞、字面回退、前缀缩短、重编号吞字与漏组导致非零退出 |

## 验证入口

在独立项目目录运行 `cargo test` 和 `cargo run`；或从根目录使用
`cargo test --manifest-path side-projects/md-scan-matrix/Cargo.toml` 与对应 `cargo run`。
实际输出和进程最终退出码共同作为实验结果证据。

## 跨模块契约

- 本实验不跨生产 crate 传递事件、取消或配置，不另建生产契约。
- TEST-EVIDENCE-001：执行终态、非零测试数与实际退出码是验证事实源；见 `docs/standards/testing.md`。
- 测试规范 §8.3：独立项目需按自身 manifest 验证，根 workspace 通过不能代替。
