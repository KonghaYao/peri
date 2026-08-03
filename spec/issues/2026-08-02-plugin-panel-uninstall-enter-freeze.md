# Plugin 面板 Enter 卸载卡死——同线程 RwLock read→write 重入死锁

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-02
**标签**：plugin, TUI, 死锁, RwLock, 临时生命周期

## 问题描述

Plugin 面板中，Installed 详情页选中 Uninstall 后按 Enter 进入确认模式，**UI 事件线程永久冻结**（页面卡死，所有按键/鼠标无响应，只能强杀进程）。排查发现这不是卸载功能缺失——卸载入口与 ACP 后端链路（`plugin/uninstall` → `uninstall_plugin()` → snapshot 推送）均已实现，卡死在进确认模式的瞬间。

## 根因

Rust 临时生命周期陷阱 + `parking_lot::RwLock` 不可重入：

```rust
// peri-tui/src/kit/panels/plugin.rs（修复前）
match (in_detail, confirm_action.read().is_some(), key.code) {
    ...
    (true, false, KeyCode::Enter) => {
        ...
        "uninstall" => {
            *confirm_action.write() = Some("uninstall".into());  // ← 死锁
        }
```

**match scrutinee 中的临时 `RwLockReadGuard` 存活到整个 match 表达式结束**（Rust temporary scope 规则），分支体内对同一 atom（`confirm_action`）执行 `write()` 即同线程 read→write 重入。parking_lot 写锁等待所有读锁释放（包括自己线程持有的那把）→ 事件线程挂起 → UI 冻结。

关键规则差异（实验验证，`/tmp` 最小复现）：

| scrutinee 形式 | 临时 guard 存活范围 | 块内同 atom write |
|----------------|--------------------|-------------------|
| `if cond` | 进入块前已 drop | 安全 |
| `match scrutinee` | **整个 match 表达式**（含所有分支体） | **死锁** |
| `if let scrutinee` | **整个 if-let 块** | **死锁** |
| match `guard arm`（`_ if ...`） | **整个 arm 体** | **死锁** |
| `let x = ...read()...;` | 语句结束即 drop | 安全 |

注意 `lock.read().is_some()` 这种**方法调用 receiver** 的临时值在调用结束即 drop（不构成死锁）；但 match scrutinee / if-let scrutinee / guard arm 中的临时值会延长存活。首次排查时误判 receiver 规则，靠 e2e 复现（`UI_FROZEN: true`）才坐实。

## 受影响路径（同类死锁共 5 处，均已修复）

| # | 位置（修复前） | 触发方式 | 状态 |
|---|---------------|---------|------|
| 1 | `match (in_detail, confirm_action.read().is_some(), ...)` + 分支内 `confirm_action.write()` | **详情页 Enter 选 Uninstall（用户报告）** | ✅ 已修复 |
| 2 | `if discover_detail_idx.read().is_some() && let ...` + 块内 `discover_detail_idx.write()` | Discover 详情鼠标点击 Install/Back | ✅ 已修复 |
| 3 | `if marketplace_detail.read().is_some() && let ...` + 块内 `marketplace_detail.write()` | Marketplace 详情鼠标点击 Refresh/Delete | ✅ 已修复 |
| 4 | `if let Some(detail) = *detail_plugin_idx.read() && ...` + 块内 `detail_plugin_idx.write()`（back/enable/disable/update 共 3 处） | Installed 详情鼠标点击 | ✅ 已修复 |
| 5 | `_ if *search_focus.read() => match ...` + arm 内 `search_focus.write()` | Discover 搜索框聚焦按 Esc | ✅ 已修复 |

## 修复方式

所有修复均为同一模式：**把 scrutinee 中的 `.read()` 提取为局部变量（bool / Option 值），让 guard 在语句结束即 drop**，再进入 match/if-let：

```rust
let confirm_active = confirm_action.read().is_some();
match (in_detail, confirm_active, key.code) { ... }
```

详见 `peri-tui/src/kit/panels/plugin.rs` 修复后的 5 处注释（`[bug] 卸载卡死` 标记）。

## 验证

- **e2e 实证复现**：修复前按键序列（/plugin → Enter 详情 → down → Enter）后 `UI_FROZEN: true`、确认提示不出现；修复后 `UI_FROZEN: false`、确认模式正常显示。
- **完整卸载流程**：构造测试插件（`origin` 必须为合法枚举值 `PeriInstalled`，manifest 的 `author` 必须是对象 `{"name": ...}` 而非字符串，否则 `load_installed_plugins`/`load_manifest` 静默失败跳过插件）跑通 详情→Uninstall→确认→执行，UI 全程响应，`installed_plugins.json` 记录真实移除。测试插件与临时文件已清理。
- 回归：`tests/panels/plugin.test.ts` 通过；`cargo clippy -p peri-tui --all-targets -- -D warnings` 通过；`cargo test -p peri-tui --lib` 717 通过。

## 排查经验（防复发）

1. **`if` 条件安全 ≠ `match`/`if let` 安全**——临时值存活规则按表达式类型区分，改动 handler 时先想清楚 scrutinee 里有没有 `.read()`。
2. 事件 handler 中对 state 的写法准则：**先 `let` 提取值，再 match/if**；块内需要 write 的 atom 绝不允许其 guard 出现在同一表达式的 scrutinee/guard arm 中。
3. 同文件 `operation_loading` 处已有此模式的既有注释（L962-963），本次是系统性扫全。
4. 其他面板（login.rs/theme.rs/mcp.rs/cron.rs/tasks.rs）扫描后无同类模式；`theme.rs` 的 `match *tab.read()` 为渲染只读路径，安全。
5. 后续新增 handler 建议纳入 code review checklist：`match`/`if let`/`guard arm` scrutinee 含 `.read()` 时检查分支体内是否对同一 atom `.write()`。
