# P1-5：CI/lefthook 增加 doc test + 安全审计门禁

**状态**：Open
**优先级**：中
**类型**：工具链改进
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 工程规范与测试维度

## Problem Statement

当前 CI 和 lefthook pre-commit 存在两个盲区：

1. **Doc test 不被验证**：`cargo build`/`check`/`clippy` 不编译 doc comment 中的代码块，lefthook 也不跑 `cargo test --doc`。CLAUDE.md:417 已记录此问题但未修复。这意味着 doc comment 中的示例代码可能已经过时或编译失败而不被察觉。

2. **无依赖安全审计**：缺少 `cargo audit` 或 `cargo deny` 检查，无法发现已知漏洞的依赖版本。对生产级终端工具而言这是安全隐患。

## 建议方案

### lefthook pre-commit 增加
```yaml
- name: doc-tests
  run: cargo test --workspace --doc
- name: security-audit  
  run: cargo audit
```

### CI 同步增加
- `cargo test --workspace --doc` 步骤
- `cargo audit` 步骤（或使用 `cargo deny check advisories`）

## 涉及文件

- `lefthook.yml` — pre-commit 配置
- `.github/workflows/` — CI 配置（如有）

## 风险

- **低**：纯增量门禁。首次运行可能发现 doc test 编译失败，需一并修复
