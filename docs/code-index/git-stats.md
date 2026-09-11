# git-stats 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-10
> 依据：`side-projects/git-stats/Cargo.toml`、源码与 testing standard（无项目级 CLAUDE.md）

## 架构速览

- 独立 CLI crate：`side-projects/git-stats/Cargo.toml` 自带 `[workspace]`，根 workspace 验证不覆盖。
- 数据流：`Cli/resolve_dates → git::fetch_commits → git::log::parse_log_output → analysis::aggregate → render::render_table`。
- Git 进程边界、字节协议解析、commit 分类与 co-author 提取、归属聚合、终端展示各有单一模块。
- 统计按 email 合并；输入沿用 git log 的 newest-first 顺序，展示名取该 email 首次出现的名字；每次归属仍累计完整行数、文件数与类型。
- AI co-author 归属规则维持当前实验行为：若 co-author email 匹配 `claude-code` 或 `@anthropic.com`，仅 AI 获得统计，否则作者与所有 co-author 各获完整统计。

## 速查表

| 我想做什么 | 主文件（项目目录内） | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 CLI 或日期窗口 | `src/main.rs` | `Cli`；`resolve_dates`；`main` | 保留 `--days` / `--since` / `--until` / `--repo` 参数及日期冲突规则；Git 失败打印错误并 exit 1 |
| 改 Git 调用 | `src/git.rs` | `build_git_command`（:10）；`fetch_commits`（:30） | 运行 no-merges/no-renames 的 numstat log；stdout 保持 bytes 交给 parser，非成功状态保留 stderr 错误 |
| 改记录 framing / numstat | `src/git/log.rs` | `parse_log_output`（:5）；`take_field`（:35）；`parse_numstat`（:49） | 每 commit 以 NUL 开始，hash/name/email/subject/body 五个 NUL 字段；`-z` 令整条 numstat 路径 NUL 结尾，路径 tab/newline 和消息 marker 不作为记录边界；二进制 `-` 计 0，截断或非法计数返回错误 |
| 改 commit 分类或 co-author 识别 | `src/commit.rs` | `CommitType::from_subject`（:27）；`parse_co_authors`（:89）；`ParsedCommit`（:75） | subject 识别 conventional commit 类型；body 按行识别大小写不敏感的 `Co-Authored-By`；不截掉含 marker 的正文 |
| 改作者名称与归属聚合 | `src/analysis.rs` | `aggregate`（:74）；`PersonStats::add_commit`（:44）；`is_ai_email`（:63） | email 首次入表时确定名称，较旧 commit 不覆盖；同提交 email 去重；统计按提交数降序/email升序稳定排序 |
| 改终端表格 | `src/render.rs` | `render_table`（:7） | 消费 PersonStats，保留现有列、颜色与空窗口提示；不执行 Git 或聚合 |
| 验证真实 Git 边界与名称顺序 | `src/git_test.rs` | `git::tests` | 独占临时目录 + 隔离 Git 配置 + 固定日期；分别验证 marker/特殊文件名/空提交/二进制/co-author 全链，以及同 email 最新作者和 co-author 名称 |
| 验证坏 framing 的错误结果 | `src/git/log_test.rs` | `git::log::tests` | 截断 body、非法 numstat 不得被跳过或记为 0；空 log 合法；marker 与字段/记录边界分离 |

## 验证入口

从根目录运行 `cargo test --manifest-path side-projects/git-stats/Cargo.toml`；
真实 fixture 测试要求本机有 Git，不访问网络或用户仓库，不依赖实际日期。
CLI 示例：`cargo run --manifest-path side-projects/git-stats/Cargo.toml -- --repo <path> --since 2020-01-01 --until 2020-02-01`。

## 跨模块契约

- 本工具不跨生产 crate 传递事件、取消或配置，不另建生产契约。
- TEST-HERMETIC-001 / TEST-EVIDENCE-001：临时仓库、固定日期与实际退出状态是验证事实源；见 `docs/standards/testing.md`。
- 测试规范 §8.3：独立项目必须按自身 manifest 验证，根 workspace 通过不能代替。
