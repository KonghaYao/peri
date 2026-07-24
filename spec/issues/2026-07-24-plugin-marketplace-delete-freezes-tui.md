> **状态**：Closed
> **创建时间**：2026-07-24
> **关闭时间**：2026-07-24
> **类型**：Bug
> **优先级**：紧急
> **标签**：plugin, marketplace, TUI, deadlock
> **修复 commit**：`0fd2d187`

# Marketplace 删除操作导致 TUI 完全卡死

## 问题描述

在 Plugin 面板的 Marketplaces 标签页中，进入 marketplace 详情、选择 Delete 操作、按 Enter 确认后，TUI 彻底卡死，无法响应任何键盘事件。

## 根因分析

**直接原因**：`*confirm_action.write() = None;`（`plugin.rs:626`）在事件处理器线程内执行时，与 generational-box 的 `SyncStorage` 中 `parking_lot::RwLock` 形成**不可重入读-写死锁**。

### 死锁链路

```
1. Render 阶段：render_marketplaces() → confirm_action.read()
   → GenerationalBox::try_read() → SyncStorage::get_split_ref()
   → parking_lot::RwLock::read()  ← 获取读锁

2. Event 处理：用户按 Enter 确认删除
   → confirm_action.write() = None
   → GenerationalBox::try_write() → SyncStorage::get_split_mut()
   → parking_lot::RwLock::write()  ← 同一线程尝试获取写锁
   → 死锁！parking_lot 不允许同一线程 read→write 重入
```

### 关键发现

- `generational-box` 的 `SyncStorage` 为每个 `State` 独立分配 `parking_lot::RwLock`
- `parking_lot::RwLock` 不支持同一线程的重入操作
- ratatui-kit 的 render 和 event handler 在同一 tokio worker 线程上执行
- render 释放读锁的时间点与 event handler 的执行存在竞态

## 修复方案

### 核心策略：将 State 写操作移到独立 OS 线程

所有 `confirm_action` 和 `operation_loading` 的 `write()` 操作从事件处理器线程移到 `std::thread::spawn` 独立线程，完全避开同一线程内的 `parking_lot::RwLock` 重入。

```rust
// 修复前（死锁）
*confirm_action.write() = None;  // 事件处理器线程 → 死锁

// 修复后
std::thread::spawn(move || {
    // I/O 操作
    // ...
    *confirm_action.write() = None;  // 独立线程 → 正常
    *operation_loading.write() = None;
});
```

### 辅助修复

1. **缓存锁优化**：`get/refresh_marketplace_cache()` 和 `get/refresh_discover_cache()` 改为锁外完成 I/O 再拿锁替换数据
2. **缓存类型**：`Vec<T>` → `Option<Vec<T>>`，区分"未初始化"与"空数据集"，防止删除最后一个 marketplace 后每帧触发 I/O
3. **operation_loading 清除路径**：提前提取读值到局部变量，防止 read→write 重入
4. **同步 I/O 后移**：Add marketplace、Refresh、enable/disable、导航等多处同步 I/O 移入 `spawn_blocking`

## 影响范围

- `peri-tui/src/kit/panels/plugin.rs`：confirm handler、缓存函数、多处事件处理器
- `peri-tui/src/kit/panels/theme.rs`：`persist_theme`、`toggle_daily_color`

## 验证方式

1. 打开 Plugin 面板 → Marketplaces 标签
2. 选择一个 marketplace → 选择 Delete → 确认
3. TUI 应保持响应，删除完成后确认弹窗自动关闭并刷新列表
