# Rust 项目成熟度检查参考

---

## 代码统计

### 源文件与行数（排除 target/worktrees）

```bash
# 总 Rust 文件数
find . -name "*.rs" -not -path "./target/*" -not -path "./worktrees/*" | wc -l

# 总 Rust 代码行数
find . -name "*.rs" -not -path "./target/*" -not -path "./worktrees/*" -exec wc -l {} + 2>/dev/null | tail -1
```

排除目录黑名单：`target/` `worktrees/` `side-projects/` `node_modules/` `dist/`

### Crate 结构

```bash
# 获取 workspace 成员列表
cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
for p in d['packages']:
    print(f'{p[\"name\"]}: {p.get(\"description\",\"\")}')
"

# 各 crate 代码量统计
for dir in <crate1> <crate2> ...; do
  lines=$(find "$dir/src" -name "*.rs" -exec cat {} + 2>/dev/null | wc -l)
  test_lines=$(find "$dir" -name "*.rs" -path "*/tests/*" -exec cat {} + 2>/dev/null | wc -l)
  echo "  $dir: src=$lines, tests=$test_lines"
done
```

### 多语言项目补充

如果项目包含 JS/TS/Python 等，也统计对应代码量并在报告中标注。

---

## 测试

### 单元测试统计

```bash
# 含 #[test] 的文件数
find . -name "*.rs" -not -path "./target/*" -not -path "./worktrees/*" | xargs grep -l "#\[test\]" 2>/dev/null | wc -l

# #[test] 函数总数
find . -name "*.rs" -not -path "./target/*" -not -path "./worktrees/*" | xargs grep -c "#\[test\]" 2>/dev/null | awk -F: '{s+=$2} END {print s}'

# #[cfg(test)] 模块数
find . -name "*.rs" -not -path "./target/*" -not -path "./worktrees/*" | xargs grep -l "#\[cfg(test)\]" 2>/dev/null | wc -l

# 测试目录下的测试文件数
find . -path "*/tests/*" -name "*.rs" -not -path "./target/*" -not -path "./worktrees/*" | wc -l
```

### 运行测试

```bash
# 完整测试（注意超时设置）
cargo test --workspace 2>&1 | tail -30

# 仅统计测试结果摘要
cargo test --workspace 2>&1 | grep -E "test result:|running|FAILED"
```

### 覆盖率工具检测

```bash
# 检查 tarpaulin 配置
grep -r "tarpaulin" Cargo.toml .github/ 2>/dev/null

# 检查 CI 中是否有覆盖率步骤
grep -r "coverage\|tarpaulin\|codecov\|coveralls" .github/ 2>/dev/null
```

### Rust 测试评分阈值

| 评分 | 测试/代码比 | 说明 |
|:---:|:----------:|------|
| ★★★★★ | > 15% | Rust 类型系统已经消除大量错误，15% 即可视为优秀 |
| ★★★★☆ | 8-15% | 良好 |
| ★★★☆☆ | 3-8% | 基本合格 |
| ★★☆☆☆ | 1-3% | 不足 |
| ★☆☆☆☆ | < 1% | 严重不足 |

额外扣分条件：
- 核心 crate（> 10,000 行）零测试 → 最高 ★★☆☆☆
- CI 不跑测试 → 扣半星

---

## 代码质量

### 静态分析

```bash
# Clippy（全部 warning）
cargo clippy --workspace 2>&1 | grep "warning:" | wc -l

# 检查是否能通过 -D warnings
cargo clippy --workspace -- -D warnings 2>&1 | tail -5

# 构建检查
cargo build --workspace 2>&1 | tail -5

# 格式化检查
cargo fmt --check 2>&1 | wc -l
```

### 代码模式扫描

```bash
# unsafe 块数量
grep -r "unsafe" --include="*.rs" --exclude-dir=target --exclude-dir=worktrees | wc -l

# .unwrap() 调用数（潜在的 panic 点）
grep -rn "\.unwrap()" --include="*.rs" --exclude-dir=target --exclude-dir=worktrees | wc -l

# .expect() 调用数（带错误信息的 unwrap）
grep -rn "\.expect(" --include="*.rs" --exclude-dir=target --exclude-dir=worktrees | wc -l

# TODO/FIXME/HACK/XXX
grep -rn "TODO\|FIXME\|HACK\|XXX" --include="*.rs" --exclude-dir=target --exclude-dir=worktrees | wc -l
```

### Pre-commit 检查

```bash
# lefthook
cat lefthook.yml 2>/dev/null

# 或检查其他 pre-commit 方案
ls .pre-commit-config.yaml .husky/ 2>/dev/null
```

### Rust unwrap 密度阈值

| 评分 | unwrap/千行 | 说明 |
|:---:|:----------:|------|
| ★★★★★ | < 5 | 极少 panic 点 |
| ★★★★☆ | 5-15 | 可接受 |
| ★★★☆☆ | 15-30 | 偏高，应审查 |
| ★★☆☆☆ | 30-50 | 较多风险点 |
| ★☆☆☆☆ | > 50 | 大量潜在 panic |

---

## CI/CD

### CI Pipeline 检测位置

```bash
# GitHub Actions
ls .github/workflows/ 2>/dev/null

# GitLab CI
ls .gitlab-ci.yml 2>/dev/null

# 其他
ls Jenkinsfile azure-pipelines.yml circleci/ 2>/dev/null
```

### CI 内容评估

读取 `.github/workflows/ci.yml`（或等效文件），检查：
- 是否包含 `cargo build`
- 是否包含 `cargo test`
- 是否包含 `cargo clippy`
- 是否包含 `cargo fmt --check`
- 是否包含 `cargo audit`
- 覆盖的操作系统数
- 是否触发于 PR

### Release 流程

```bash
# 检查 release workflow
ls .github/workflows/release* 2>/dev/null

# 检查 Cross 编译配置
cat Cross.toml 2>/dev/null

# 检查安装脚本
ls scripts/install* 2>/dev/null
```

### 版本标签

```bash
# 标签总数
git tag -l | wc -l

# 版本标签命名规范检查（semver）
git tag -l | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+' | sort -V | tail -5

# 非标准标签
git tag -l | grep -v -E '^v[0-9]+'
```

---

## 依赖审计

```bash
# 安全审计（如果已安装 cargo-audit）
cargo audit 2>&1 | tail -20

# 检查 CI 中是否有 audit 步骤
grep -r "cargo audit\|cargo-deny" .github/ lefthook.yml 2>/dev/null

# 依赖数量
grep -c "version" Cargo.lock 2>/dev/null

# 过期依赖（如果已安装 cargo-outdated）
cargo outdated 2>&1 | head -20
```

---

## 常见 Rust 特定问题标记

| 问题 | 检测方式 | 风险 |
|------|---------|:--:|
| `println!`/`eprintln!` 残留 | grep 检查 | 🟡 应使用 tracing |
| 缺少 `#![deny(unsafe_code)]` | 检查 lib.rs | 🟡 安全态度 |
| `cargo doc` 是否能通过 | `cargo doc --no-deps 2>&1` | 🟢 文档生成 |
| edition 不一致 | 检查各 crate Cargo.toml 的 edition 字段 | 🟢 技术债 |
| 未使用的依赖 | `cargo udeps`（如果可用） | 🟢 膨胀 |
