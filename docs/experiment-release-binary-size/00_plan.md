# macOS release 体积对照计划

日期：2026-10-07。初始范围：调查实验，不改变生产构建配置。用户后续追加授权：实验结束后选定最佳已测配置、提交并合入 `pre-release/main`；生产修改与最终验证见结论，研究输入仍固定于下列提交。

## 问题与成功标准

解释当前分支相对本地 `main` 的 release 体积增长，区分源码变化、release 优化配置及 Mach-O 动态导出表。交付可重跑的命令、裸文件/压缩包大小、Mach-O 分段及 CLI/ACP smoke 结果；不把下载包与裸文件混比，不把一次有缓存的构建时间当性能结论。

## 固定边界

- 独立 detached worktree：`/Users/konghayao/code/ai/peri-release-size-20261007`。
- 当前源码固定 `8c8c346a`；main 固定 `d7ee444e`，使用此 worktree 内的 archive 快照，不读 main 工作区的未提交内容。
- 同一 macOS ARM64 主机、Rust 工具链、目标平台、默认生产 feature、`opt-level=z`、`strip=symbols`、panic unwind；锁文件各用对应提交，不更新依赖。
- Workflow JS 源码在两提交间没有生产变动，使用同一份预构建 JS，记录 SHA-256。变更的 README/test 不参与生产构建。
- 所有构建串行，共用本实验独立 target cache；只复用下载缓存，不修改原工作区。
- 构建环境只记录白名单字段，不保存用户凭证。必须确认 strip 实际生效，不能仅检查 Cargo 退出码。本机 Rust objcopy 需显式定位同工具链 LLVM 动态库；该路径不得传给不同 LLVM 版本的度量工具。
- 容许为了锁定依赖下载 Cargo.lock 中的依赖；不下载历史发布二进制。

## 轮次

1. 当前源码：thin LTO/16 CGU（现配置）、fat LTO/1 CGU（main 配置）；另构建 thin/16 且仅导出 main 的诊断候选。分别保存独立二进制及原始证据，后者只用于解释导出表，不默认作为生产修复。
2. main 源码：fat/1（main 配置）及 thin/16（现配置），形成源码 × 配置 2×2 对照；区分“同源码改配置”与“同配置改源码”的效应。
3. 独立验证两轮数据，综合结论后进行对抗审查；若配置与源码存在交互效应，分别报告两个配置下的源码增量，不强行给唯一归因百分比。

## 每个候选的证据

- 源码 SHA、构建命令/环境覆盖、退出码和日志、锁文件与 Workflow JS hash。
- 二进制字节数/SHA-256、同样文件名打包的 tar.gz 字节数、可执行架构。
- `otool -l`/`llvm-size -m` 原始输出；__text、异常展开表、常量、__LINKEDIT 和 export trie 的字节数、导出项数。
- `--version`、`--help` 及离线 ACP initialize smoke；执行状态独立记录，不把启动成功当完整功能正确。

## 局限与安全

单平台对照不能推广到 Windows/Linux。main 与当前提交的依赖版本变动归入源码/依赖轴，不声称单独测出业务代码效应。候选 export 限制需额外评估动态查找/FFI；不得修改 panic 语义。实验不写真实用户配置/数据库，不发送模型请求，不 commit、不创建开发分支，不触碰原 worktree 的 WIP。
