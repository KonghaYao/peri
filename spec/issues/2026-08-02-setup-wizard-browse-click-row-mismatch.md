# setup_wizard Browse 步骤鼠标点击行高与渲染不一致，后续项点击错位

**状态**：Open
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

setup wizard 的 Browse（provider 列表）步骤，`wizard_click` 按每个 provider 5 行（base_url 非空 6 行）反推行号，但 `render_browse` 实际渲染 6 行（base_url 非空 7 行）。多 provider 时后续项与提交行整体上移一行，鼠标点击命中的 provider 与用户看到的不一致。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `render_browse`（约 386-468 行）每个 provider：provider 行 1 + 可选 url 行 1 + 空行 1 + 3 行别名 + 空行 1 = **6 行**（base_url 非空 **7 行**）。
- `wizard_click` Browse 分支（约 833-839 行）：`item_h = if base_url.is_empty() { 5 } else { 6 }`。
- 第二个 provider 起点击命中行整体偏移 1 行；submit 行同样偏移（多个 provider 时偏移量累加）。

## 复现条件

- **复现频率**：必现（≥2 个 provider 或带 base_url 时）
- **触发步骤**：
  1. 进入 setup wizard Browse 步骤，添加 ≥2 个 provider
  2. 鼠标点击第二个 provider 或提交行
  3. 观察光标落在错误项上
- **环境**：任何含 base_url 或 ≥2 provider 的配置

## 期望改进方向

- `wizard_click` 的 item 高度改为 6/7，与 `render_browse` 对齐；补充两种 base_url 情况的命中测试。

## 涉及文件

- `peri-tui/src/kit/setup_wizard.rs` —— `render_browse`（约 378-505 行）与 `wizard_click`（约 784-856 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: wizard_click Browse 分支 item_h 由 5/6 改为 6/7 与 render_browse 对齐，并补两种 base_url 命中测试 |

## 修复记录

**改动摘要**（`peri-tui/src/kit/setup_wizard.rs`）：

1. `wizard_click` Browse 分支（约 839 行）item 高度由 `{ 5u16 } else { 6u16 }` 改为 `{ 6u16 } else { 7u16 }`，与 `render_browse` 实际行数对齐（provider 行 + 可选 url 行 + 空行 + 3 别名行 + 空行）；同步更新分支注释，注明与 render_browse 的行构成对应关系。
2. 新增 `#[cfg(test)] mod tests`（文件末尾），含两个命中测试：
   - `browse_click_without_base_url_hits_second_provider`：无 base_url 时第二个 provider 的 provider 行在 visual 7，点击命中后 `active_provider == 1` 且进入 Edit；
   - `browse_click_with_base_url_hits_second_provider`：带 base_url 时第二个 provider 的 provider 行在 visual 8，同理断言。

submit 行计算（`cur += 2` 错误提示、`visual == cur`）与 render 一致，未改动。

**验证**：`cargo check -p peri-tui --all-targets` 通过（无警告）；新增 2 测试通过（`cargo test -p peri-tui --lib -- setup_wizard` 9 passed）。
