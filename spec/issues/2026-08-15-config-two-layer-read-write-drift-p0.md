# 双层配置读写路径漂移 P0：工作区生效却写回全局

**状态**：Fixed  
**优先级**：P0  
**创建日期**：2026-08-15

## 问题描述

当工作区 `.peri/settings.json` 与全局 `~/.peri/settings.json` 定义了不同的
provider/model 时，**生效的是工作区配置（load 时合并覆盖），但任何保存操作都
无条件写入全局文件**——合并后的完整快照（含工作区专属 providers / apiKey）
被 `persist_config` / TUI 面板保存点写回 `~/.peri/settings.json`，全局配置被
污染/覆盖。

根因链（证据完整，非推测）：

1. `store::load()` 合并全局 + 工作区（工作区非空字段覆盖全局）——读取正确。
2. 自由函数 `store::save()` / `effective_save_path()` / ACP `persist_config`
   内部各自独立决策目标路径，且无条件解析为全局路径——**写入与读取的解
   决策完全脱钩**，任何一方未来改动都可能再次漂移。
3. TUI 侧 `crate::config::save` = peri-acp 的 `store::save`，14 个保存点
   （config/login/model/theme 面板、setup wizard、每日色彩、`persist_config`）
   全部把内存中的**合并快照**写全局文件。

同类事故此前已发生过一次：项目级配置被写入全 false 的 `meta_harness` 后经
`load()` 合并透传写回全局配置导致功能全关（`config.rs` 历史 warn 注释），
当时仅告警未根治——本 bug 是同一类"读写作路径决策分离"问题的复发。

## 风险描述

- 工作区专属 provider / apiKey 泄漏进全局文件，全局配置被不可逆覆盖。
- 保存行为与用户直觉（工作区生效 → 工作区文件保存）相反。
- 多套"独立决策目标路径"的实现并存，任一新保存点都可能再次引入漂移。

## 影响范围

- 所有存在工作区 `.peri/settings.json` 的项目；TUI 面板任意保存、Setup
  向导、ACP `persist_config`、每日色彩持久化均受影响。

## 涉及文件

- `peri-acp/src/provider/store.rs` — `ConfigSource` 值对象重写（核心修复）。
- `peri-acp/src/provider/config.rs` — `extract_overrides` 分层提取 + 序列化
  "空 = 未填写 = 不落盘" + `PartialEq` 派生。
- `peri-acp/src/provider/config_test.rs` / `store_test.rs` — roundtrip 契约测试。
- `peri-acp/src/provider/mod.rs` — `ConfigSource` re-export。
- `peri-acp/src/host/mod.rs` / `assemble.rs` / `requests.rs` / `requests_test.rs`
  — `AcpServerConfig.config_path` → `config_source`，`persist_config` 唯一实现。
- `peri-acp-types/src/compact.rs` — `CompactConfig` `PartialEq` 派生。
- `peri-tui/src/config/mod.rs` — `save_effective` 统一保存入口。
- `peri-tui/src/kit/atoms.rs` / `kit/entry.rs` — `CONFIG_SOURCE_HANDLE`。
- `peri-tui/src/app/mod.rs` / `launch.rs` / `cli_print.rs` — 装配面接入。
- `peri-tui/src/kit/panels/{config,login,model,theme}.rs`、
  `app/setup_wizard/mod.rs`、`kit/entry.rs` — 14 个保存点迁移。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-15 | — | Open | user | 用户报告 P0：工作区生效但数据写入全局 |
| 2026-08-15 | Open | Fixed | agent | ConfigSource 值对象 + 分层写回重设计落地；peri-acp 413 / peri-tui 1246 测试 + clippy -D warnings 通过 |

## 修复记录

### 修复 #1（2026-08-15）—— Fixed

- **操作人**：agent
- **用户原意**：不希望两个配置文件的写入逻辑继续混乱；此类 P0 不应再出现。
  用户决策：保存写回**工作区文件**；回写语义为**覆盖提取（分层）**——
  工作区文件只含与全局不同的字段；providers 保持**整体替换**。
- **修复内容**：
  1. `ConfigSource` 不可变值对象：启动时**一次性探测**「全局 + 工作区」布局
     并缓存原始全局（分层基准）+ 合并配置；所有保存从该实例出发，读写作
     路径决策**单一来源**，删除自由函数 `save()` / `effective_save_path()`
     （编译期消灭第二套实现）。
  2. `save()` 分层写回：工作区存在 → `extract_overrides(merged, global)`
     只写差异字段到工作区文件（全局文件不动，全局 apiKey 不进入项目文件）；
     无工作区 → 写全局文件。`extract_overrides` 是 `merge_overrides` 的
     严格逆操作，roundtrip 契约测试覆盖。
  3. `active_alias` 分层豁免（恒收录）：serde 解析期缺省 "opus" 与显式
     "opus" 不可区分，剔除会导致工作区文件缺失该字段时解析出 "opus"
     经 merge 非空覆盖错误覆盖全局非 opus 值（roundtrip 破坏）。
  4. 序列化约定"空 = 未填写 = 不落盘"（`skip_serializing_if`）与 merge
     "空 = 未覆盖"对称。
  5. TUI 14 个保存点统一走 `save_effective`（读 `CONFIG_SOURCE_HANDLE`，
     与 ACP host 共享同一 `Arc<ConfigSource>`）；ACP `persist_config` 走
     `cfg.config_source.save`——TUI/ACP 两处写回不可能分叉。
  6. `cli_print` 装配点：`--settings` 用新增 `load_standalone`（单文件整体
     生效语义不变），默认路径 `load_lenient`（保持迁移前 `load().ok()`
     容错行为）。
- **涉及 commit**：`f3ce722a`（fix(config): 双层配置读写路径漂移 P0）
- **验证状态**：已验证

### 验证 #1（2026-08-15）

- 分层写回：工作区存在时工作区文件只含差异字段、全局文件不动（
  `test_config_source_save_routes_to_workspace_layered`）。
- roundtrip 契约：`merge_overrides(global, extract_overrides(merged, global))
  == merged`（providers / profiles 档位 / meta_harness 逐 key / extra /
  active_alias 豁免全覆盖）。
- 无工作区时保存写全局（`test_config_source_save_writes_global_when_no_workspace`）。
- `load_standalone` 不探测工作区、写回仍写指定文件。
- `cargo test -p peri-acp --lib`（413）/ `-p peri-tui --lib`（1246）全部通过；
  `cargo check --workspace`、`cargo clippy -p peri-acp -p peri-tui --all-targets
  -- -D warnings` 通过。

## 维护约定（防回归）

- **新增保存点必须经 `ConfigSource::save` / TUI `save_effective`**；禁止重新
  引入自由 `save()` 或任何独立的目标路径探测逻辑。
- **路径决策只在 `ConfigSource` 构造时发生一次**；测试用确定性构造
  `load_at(cwd, global_path)`，不依赖进程 cwd / 全局重定向。
- `merge_overrides` 与 `extract_overrides` 必须保持严格互逆——改 merge 语义
  必须同步改 extract 并跑 roundtrip 契约测试。
- 工作区文件始终是"项目覆盖"性质：只含与全局不同的字段，不拷贝全局凭据。
- 注意：`set_global_config_path`（`--config-file`）仅保留给 CLI 启动早期注入，
  属已知限制，不影响分层语义。
