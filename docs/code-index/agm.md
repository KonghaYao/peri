# agm 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：agm/src 源码、agm/README.md（pnpm 风格 agent 依赖管理器）

## 架构速览

- 数据流（install）：`agm.json（manifest）→ 解析依赖（git commit / registry semver）→ clone/下载到 ~/.agm/tmp → 原子 rename 入 ~/.agm/store（内容寻址）→ pick/omit 过滤 → 在 .claude/{skills,agents,mcp}/ 建 symlink → 更新 agm.json + agm.lock.json`
- 入口：`src/main.rs::main`（:64，clap 子命令分发）；核心编排在 `src/installer.rs::InstallContext`（:17）
- 稳定不变量：store 目录名 = 内容寻址（git：`git_{owner}_{repo}@{commit}`；registry：`{name}@{version}`，store.rs:16/:23）；lock 文件 `agm.lock.json` 记录 Resolution（Git{repo,commit} / Registry{integrity}）；symlink 幂等（adapter.rs:40-49 已指向同源直接返回）
- 现状：`update`/`gc`/`publish` 为 v1 占位（提示手动处理 / dry run），见 commands/update.rs、gc.rs、publish.rs

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 install 编排（git/registry） | `src/installer.rs` | `InstallContext::new`（:27）；`install_from_git`（:65）；`install_all`（:365） | git 模式：`resolve_head`（ls-remote）→ store 已有跳过 clone → 无则 `clone_head` → `install_to_store` → `filter_items` → 删 stale symlink 重建；registry 模式：`resolve_registry_version`（semver 范围）→ download_tarball + `extract_tarball`（:606）；lock 已有同 key 且 spec 为 Simple 时跳过（:399-405） |
| 改 symlink 安装/卸载 | `src/adapter.rs` | `ToolAdapter::install`（:35）；`uninstall`（:99）；`map_dir`（:20）；`try_symlink`（:110） | 目标已是指向同源的 symlink 则幂等返回；Windows 无权限（error 1314）回退 copy_dir_all（:58-84）；`ClaudeAdapter`（:127）是唯一内置 adapter，`get_adapter`（:135） |
| 改 pick/omit 过滤 | `src/filter.rs` | `filter_items`（:8）；`compile_patterns`（:38） | Simple spec 不过滤；Detailed：pick 先匹配（空 = 全过）再 omit 排除，同时匹配 name 与 glob 路径（README: pick 先、omit 后） |
| 改包内发现（无 agm.package.json 时） | `src/filter.rs` | `auto_detect_types`（:87）；`detect_package_items`（:143）；`find_skills_recursive`（:187）；`extract_skill_name`（:217） | 扫描 `.claude/skills` 与 `skills`（含 `base` glob 多根，`resolve_scan_roots` :61）下 `**/SKILL.md` 递归 + `agents/*.md`；有显式 `agm.package.json` 时以其导出为准（base 被忽略）；空结果回退包根 `.`（:176-178） |
| 改 manifest/lock 类型 | `src/types.rs` | `DependencySpec`（:10，Simple/Detailed{base,pick,omit}）；`ProjectManifest`（:44）；`LockFile`（:73）；`Resolution`（:111）；`PackageManifest`（:120） | agm.json 三张表 skills/agents/mcp 均为 `BTreeMap<String, DependencySpec>`；lock 按 importer(".") + packages 两级组织 |
| 改 git 操作 | `src/git.rs` | `clone_at_commit`（:6，--no-checkout + checkout）；`clone_head`（:57，--depth 1 + rev-parse）；`resolve_head`（:79，ls-remote --symref）；`parse_github_url`（:107） | 本地路径也支持（末两段当 owner/repo）；`is_valid_commit_hash`（:33）40 位 hex |
| 改 registry 交互 | `src/registry.rs` | `RegistryClient`（:22）；`get_package`（:42）；`get_version`（:59）；`download_tarball`（:76）；`publish`（:95） | Bearer token 可选；路径 `/packages/{name}`、`/packages/{name}/{version}`、`/packages/{name}/-/{tarball}` |
| 改 store 路径/清理 | `src/store.rs` + `src/config.rs` | `Store::git_package_path`（:16）；`registry_package_path`（:23）；`install_to_store`（:98，rename 原子入 store）；`AgmConfig`（config.rs:6） | `~/.agm/store`（config.rs:28）；`sanitize_repo_id`（store.rs:75，本地路径只取末两段防 MAX_PATH）；gc 命令仅列包数（commands/gc.rs） |
| 加 CLI 子命令 | `src/main.rs` | `Commands`（:16）；main 分发（:70-83） | 每个子命令对应 `commands/{name}.rs::execute`；全局 `-C/--dir` 指定项目目录 |
| 改 uninstall | `src/commands/uninstall.rs` | `execute`（:10） | 经 lock 定位 store 路径 → `detect_package_items` 重算实际 item symlink → 逐个删（含 legacy 包名 symlink）→ 更新 manifest 与 lock（:127-137） |

## 子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| CLI 入口/分发 | src/main.rs | `Cli`（:6）；`Commands`（:16）；`main`（:64） |
| 安装编排 | src/installer.rs | `InstallContext`（:17）；`install_from_git`（:65）；`install_all`（:365）；`remove_package_symlinks`（:322）；`update_lock`（:560）；`extract_tarball`（:606） |
| Store 管理 | src/store.rs | `Store`（:6）；`list_packages`（:35）；`read_package_manifest`（:50）；`remove`（:62）；`install_to_store`（:98） |
| 依赖解析 | src/resolver.rs | `is_git_dep`（:17，`@git/` 前缀）；`resolve_registry_version`（:30）；`collect_dependencies`（:60）；`PackageType`（:77）；`detect_conflicts`（:84，无消费点） |
| 发现与过滤 | src/filter.rs | `filter_items`（:8）；`auto_detect_types`（:87）；`detect_package_items`（:143）；`extract_skill_name`（:217） |
| 工具适配器 | src/adapter.rs | `ToolAdapter`（:15）；`ClaudeAdapter`（:127）；`get_adapter`（:135）；`symlink_name`（:148，冲突加 scope 前缀） |
| Git 子进程 | src/git.rs | `clone_at_commit`（:6）；`clone_head`（:57）；`resolve_head`（:79）；`parse_github_url`（:107） |
| Registry 客户端 | src/registry.rs | `PackageMetadata`（:8）；`VersionMetadata`（:14）；`RegistryClient`（:22） |
| 配置 | src/config.rs | `AgmConfig`（:6）；`agm_dir`（:44，`~/.agm`）；`config_path`（:50）；`load`（:55） |
| 错误 | src/error.rs | `AgmError`（ManifestNotFound/LockNotFound/PackageNotInManifest/InvalidCommitHash/InvalidGlobPattern 等） |
| 子命令 | src/commands/ | install（:7）/uninstall（:10）/list（:7）/init/gc/update/publish/self_update 各 `execute` |

## 跨模块契约

- 独立 CLI crate（workspace member，无 Rust 依赖方）；产物流向：`.claude/skills|agents|mcp/` 下 symlink 由工具方消费——`peri-agent/src/session/store.rs:61` 的 `FrozenContext.skill_summary` 注明「Skills 摘要（builtin + agm 加载的汇总）」；`peri-controller/src/controller.rs:46` 的 `AgentRef` 注释提及「agm 命名空间 / 内置定义名，解析归 Agent 层」
- 本仓库自身使用：根目录 `agm.json` / `agm.lock.json` 即本工具产物；`agm/install.sh` / `install.ps1` 是自安装脚本（`self_update` 经 curl|bash / irm|iex 拉取）
