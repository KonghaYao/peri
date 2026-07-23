# 通用项目成熟度检查参考（兜底）

当项目语言不在已知 reference 列表中（Rust/TS/Python/Go）时，使用此通用策略。

---

## 代码统计（通用）

```bash
# 按后缀统计所有源代码
echo "=== 代码文件分布 ==="
find . -type f \( \
  -name "*.rs" -o -name "*.ts" -o -name "*.tsx" -o -name "*.js" -o -name "*.jsx" \
  -o -name "*.py" -o -name "*.go" -o -name "*.java" -o -name "*.kt" \
  -o -name "*.rb" -o -name "*.php" -o -name "*.c" -o -name "*.cpp" -o -name "*.h" \
  -o -name "*.swift" -o -name "*.scala" -o -name "*.cs" -o -name "*.fs" \
  -o -name "*.vue" -o -name "*.svelte" \
  \) \
  -not -path "*/node_modules/*" -not -path "*/target/*" -not -path "*/.git/*" \
  -not -path "*/dist/*" -not -path "*/build/*" -not -path "*/vendor/*" \
  | sed 's/.*\.//' | sort | uniq -c | sort -rn

echo "=== 总代码行数 ==="
find . -type f \( ...同上后缀... \) -not -path "*/node_modules/*" ... -exec wc -l {} + 2>/dev/null | tail -1
```

---

## Git 活跃度（通用，适用于所有语言）

```bash
# 总提交数
git log --oneline --all | wc -l

# 首次提交
git log --format="%ai" --reverse | head -1

# 最后提交
git log --format="%ai" | head -1

# 近 30 天提交趋势
git log --format="%ad" --date=short --since="30 days ago" | sort | uniq -c | tail -30

# 贡献者分布
git shortlog -sn --all | head -10

# 版本标签
git tag -l | wc -l
git tag -l | sort -V | tail -10

# 合并提交数
git log --oneline --all --merges | wc -l

# 活跃天数
git log --format="%ad" --date=short --all | sort -u | wc -l
```

---

## 文档（通用）

```bash
# README 质量
wc -l README.md README.rst README 2>/dev/null

# CHANGELOG
wc -l CHANGELOG.md CHANGES.md HISTORY.md 2>/dev/null

# 其他文档
find docs/ -name "*.md" -o -name "*.rst" 2>/dev/null | wc -l
ls CONTRIBUTING.md CODE_OF_CONDUCT.md SECURITY.md LICENSE 2>/dev/null
```

---

## CI/CD（通用）

```bash
# 各种 CI 系统
ls .github/workflows/ .gitlab-ci.yml Jenkinsfile azure-pipelines.yml \
   .circleci/ .travis.yml .drone.yml 2>/dev/null

# Docker
ls Dockerfile docker-compose* .dockerignore 2>/dev/null

# Makefile / Task runner
ls Makefile Taskfile.yml justfile 2>/dev/null
```

---

## 安全（通用）

```bash
# .gitignore 检查
grep "\.env" .gitignore 2>/dev/null
grep "credentials\|secret\|token\|key" .gitignore 2>/dev/null

# 密钥硬编码检查（快速扫描，不精确）
grep -rn "API_KEY\|SECRET\|PASSWORD\|TOKEN" --include="*.env" . 2>/dev/null | head -5

# SECURITY.md
wc -l SECURITY.md 2>/dev/null
```

---

## 通用评分调整

当语言特定的检查命令不可用时，使用以下替代评估：

| 维度 | 替代评估方式 |
|------|------------|
| 测试 | 检查是否有测试目录/文件，README 中是否提到测试命令 |
| 代码质量 | 检查是否有 linter/formatter 配置 |
| 依赖审计 | 检查 CI 中是否有 audit 步骤 |
| 构建 | 检查是否有构建脚本/Makefile/package.json scripts |

**关键原则**：对于无法精确检测的维度，标注"未获取"而非猜测评分。通用的核心维度（Git 活跃度、文档、CI/CD）仍然可以精确评估。
