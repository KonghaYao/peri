> 归档于 2026-07-18，原路径 spec/issues/2026-07-18-compact-directory-removal.md
# compact 目录物理删除——config.rs 上移到 compact_v2 并消除空壳

**状态**：Done
**优先级**：中
**创建日期**：2026-07-18
**父 issue**：`spec/issues/residual-code-scan-20260718.md` (P1-3)

## 背景

`peri-agent/src/agent/compact/mod.rs` 是一个目录外壳——v1 compact 主体（full / micro / re_inject / invariant）已于 2026Q1 物理删除，当前仅 re-export：

```rust
pub use config::CompactConfig;
pub const CONTINUATION_HINT: &str = ...;
```

- `CompactConfig` 实际定义在 `compact/config.rs`
- `CONTINUATION_HINT` 是字符串常量
- 目录名 `compact` 容易让人误以为 v1 代码还活着

## 目标

将 `compact/config.rs` 上移到 `compact_v2/` 下，删除 `compact/` 目录，消除空壳。

## 当前阻断

`CompactConfig` 和 `CONTINUATION_HINT` 被 **14+ 处**引用，跨 3 个 crate：

| 引用路径示例 | Crate |
|-------------|-------|
| `use crate::agent::compact::CompactConfig` | peri-agent (stages/compact.rs 等) |
| `use peri_agent::agent::compact::CompactConfig` | peri-acp |
| `use peri_agent::agent::compact::CONTINUATION_HINT` | peri-tui |

## 迁移步骤

1. 将 `compact/config.rs` 移动到 `compact_v2/config.rs`
2. 将 `CONTINUATION_HINT` 常量移入 `compact_v2/mod.rs`（或保留在 config.rs）
3. 在 `compact_v2/mod.rs` 添加 re-export 保持兼容：
   ```rust
   pub use config::CompactConfig;
   pub const CONTINUATION_HINT: &str = config::CONTINUATION_HINT;
   ```
4. 删除 `compact/` 目录（含 mod.rs）
5. 全局替换引用路径：
   - `agent::compact::CompactConfig` → `agent::compact_v2::CompactConfig`
   - `agent::compact::CONTINUATION_HINT` → `agent::compact_v2::CONTINUATION_HINT`
6. 同步更新 `lib.rs` prelude 相关导出

## 影响文件（估）

| Crate | 文件数 | 操作 |
|-------|:-----:|------|
| peri-agent | ~8 | 移动 + 内部引用更新 |
| peri-acp | ~3 | 引用路径更新 |
| peri-tui | ~2 | 引用路径更新 |

## 验证标准

- [ ] `cargo build --workspace` 编译通过
- [ ] `cargo test -p peri-agent --lib` 全过
- [ ] `peri-agent/src/agent/compact/` 目录不再存在
- [ ] `use peri_agent::agent::compact::` 全局搜索返回零匹配
