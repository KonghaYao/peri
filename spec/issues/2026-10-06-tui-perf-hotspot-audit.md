# TUI CPU / 内存热点扫描与 Astra 审计

**状态**：Open；三路扫描与 Astra（`gpt-6-astra`）独立审计已完成，修复与性能验收待实施。

**日期**：2026-10-06。**范围**：定位、登记与审计，不包含生产代码修复。
**优先级**：按审计后的具体发现排序；不将静态疑点直接升级为现场 P0 根因。

## 任务与证据边界

用户反馈 TUI CPU 与内存消耗过高，要求派 subagent 扫描、写 issue，再交 Astra 审计。
扫描分为渲染 CPU、内存所有权与驻留、事件循环与后台任务三路；以当前工作树为准。
初读 HEAD 为 `bd5e296b`，存在用户未提交的 Markdown、消息区及 backend 优化，全部保留。
本轮期间外部工作继续提交/修改：内存扫描读取时 HEAD 为 `f36d9fb0`，
随后主 agent 观察到 `b7c6770d` 及 `steer_state.rs` 的新修改。
这不是冻结工作树的可重复实验；扫描定位按符号复核，最终是否仍待修以 Astra 裁决为准。
本任务未执行任何 commit，也不将外部变更计为本轮修复。
本轮不启动新 Peri 会话，不读取真实会话正文，不修改源码、不 commit。

本进程同时承载 TUI 与 backend。进程资源异常不能直接等同于消息绘制异常；
源码可证明成本机制，但无法单独证明现场占比、泄漏或优化收益。

## 现场补采（主 agent）

2026-10-06 20:29:55 +0800，`lsof -a -p <pid> -d cwd,txt -Fn` 确认三个进程的
cwd 与 executable 均属于本仓库。20:30:09–20:30:12 +0800 并行运行
`sample <pid> 3 -file <output>`，在窗口两端记录 `ps`。未终止、重启或改变用户进程。

| PID | 窗口前/后 RSS（ps 原始 KiB） | 窗口前/后 ps %CPU | sample footprint / peak（原始口径） | 主线程等待包含计数 |
| --- | --- | --- | --- | --- |
| 9500 | 1,712,656 / 1,856,752 | 103.8 / 91.3 | 1.1G / 1.7G | 1,241 / 1,288 在 `__psynch_cvwait` |
| 55171 | 560,672 / 560,688 | 13.5 / 2.3 | 318.3M / 475.7M | 1,772 / 1,790 在 `__psynch_cvwait` |
| 49449 | 2,757,952 / 2,757,952 | 204.9 / 18.2 | 1.9G / 4.4G | 1,320 / 1,371 在 `__psynch_cvwait` |

**观察**：PID 9500 的 worker 分支出现
`WorkMutationBarrier → apply_work_mutation → write_work → encode<WorkState>`，
其中 `encode` 包含计数 1,255；PID 49449 出现 `WorkState` 解码分支（包含计数 393、391，
不同分支不能直接相加为 CPU 占比）。PID 55171 亦出现解码分支。
三个进程在此短窗口的主线程主要等待；不支持直接把该窗口高 CPU 归因于 TUI 绘制。
Backend 问题继续由 [已有资源异常 issue](2026-10-06-p0-dev-peri-high-cpu-memory.md) 承接，
本扫描不另建同因修复项。

**限制**：

- `ps %CPU` 是工具报告值，不是这三秒的 CPU 时间差分；并行采样有扰动。
- 栈包含计数含等待，既不是调用次数，也不是互斥 CPU 百分比；不能跨线程/分支相加。
- RSS、physical footprint 与 peak 口径不同，不直接相减、计算泄漏量或推断峰值同时发生。
- 未采堆、未关联 session 数据量、未受控回放；不能判断内存属于 UI、存储副本还是 allocator。
- 运行进程启动时间分别为 18:39:09、19:06:43、19:46:04；9500 与 55171 的二进制 UUID 相同，49449 为另一 UUID，共两个已加载映像身份。
  磁盘 `target/debug/peri` mtime 为 19:46:58，不能证明这些进程对应当前工作树或 HEAD。
- 原始材料位于 `/tmp/peri-tui-perf-20261006-2nsZNM/`，包括 observations、三份 sample、
  成功状态输出及 SHA256SUMS；临时文件不是仓库 fixture，可能包含本机路径，分享前脱敏。
- Astra 核实七份原始数据文件 hash 匹配，但 SHA256SUMS 意外包含自身的空文件 hash；
  不宣称整个清单校验通过。原材料保留，完整性限制见独立审计 §2。

## 扫描 issue 路由

- [CPU 渲染链](2026-10-06-tui-perf-cpu-render-scan.md)
- [内存所有权与驻留](2026-10-06-tui-perf-memory-scan.md)
- [事件循环与后台任务](2026-10-06-tui-perf-runtime-scan.md)
- [Astra 独立裁决](2026-10-06-tui-perf-astra-audit.md)（本批发现的定级与去重路由）
- 已有 [流式渲染冗余](2026-10-06-tui-streaming-render-redundancy.md) 与
  [历史发布链问题](2026-09-27-p0-tui-streaming-view-rebuild-cpu.md) 为去重依据；已修项不重新登记。

## Astra 审计要求

- 逐项复核当前源码可达性、频率、复制/保留量、释放条件，排除已修复与重复项。
- 明确「确认机制 / 条件成立 / 降级为测量假设 / 驳回」及理由；禁止把源码推断写成实测收益。
- 区分 UI 与同进程 backend；复核采样口径和运行二进制版本限制。
- 形成可实施顺序、修复边界、行为回归与性能验收，不用有损截断掩盖消息或错误。

## 审计裁决与下一步

三份扫描共有 15 个原始条目，全部由 Astra 独立复核；重复项合并，不将数量当独立缺陷数。

- **P1 / M1**：确认 reset 未清全局工具分组 memo，切到空会话后可继续持有旧整段快照；
  最多一个缓存版本，不是每次切换累计整会话，不确认 GiB 泄漏。
- **P2 / CPU-1、CPU-2、M3**：确认全文 reasoning 折行、稳定布局元数据搬运、live map 全表 owned 数据复制；
  先做局部短路/最小投影与所有权回归，保持显示、选择、复制和终态行为。
- **P2 / R1–R4、R7**：确认轮询、隐藏 History 已加载页重取、无变化唤醒与相同 revision mutation；
  Workflow 补 session/epoch 保护，框架 raw 事件按 render-demand 边界处理，不丢真实输入。
- **P2 / M2、M4、M6**：终态详情、大历史预览、恢复原稿按功能契约明确保留与字节预算；
  不直接清空可回查内容或用户草稿，不预设峰值倍数。
- **P2 / R6 与 M5 合并、R5**：统一事件/请求准入、gap single-flight 和 task owner 验证；
  R5 正常 CLI 退出长期泄漏推断被驳回，不能忽略 transport close 与 runtime drop。
- **现场未闭环**：本次 sample 中 backend WorkState 编解码可达；UI 缺陷不等于进程资源主因。
  后续先固定二进制身份、负载和分配归属，再量化 CPU 时间、队列字节、task/drop 与 heap/RSS。

本轮只登记和审计，未实施任何生产修复、未运行 Cargo 或性能基准，不提供改善百分比。

## 闭环清单

- [x] 三路 subagent 写入含证据、验证方法与验收标准的 issue。
- [x] Astra 审计完成，修正过度结论、重复项与优先级。
- [x] 五份文档的 22 个本地链接、含 untracked 新文件的空白检查及 `git diff --check` 通过；未构建或运行 Rust 测试。
- [ ] 后续按审计结论实施与受控性能验收（本轮不实施）。
