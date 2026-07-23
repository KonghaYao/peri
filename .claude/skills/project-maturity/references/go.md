# Go 项目成熟度检查参考

---

## 代码统计

### 源文件与行数

```bash
# .go 文件总数（排除 vendor）
find . -name "*.go" -not -path "./vendor/*" | wc -l

# 总代码行数
find . -name "*.go" -not -path "./vendor/*" -exec wc -l {} + 2>/dev/null | tail -1

# 测试文件数
find . -name "*_test.go" -not -path "./vendor/*" | wc -l
```

排除目录黑名单：`vendor/` `node_modules/` `dist/`

### 模块结构

```bash
# Go workspace / module
cat go.mod 2>/dev/null | head -5

# 包列表
find . -maxdepth 3 -name "*.go" -not -path "./vendor/*" -exec dirname {} \; | sort -u | head -20
```

---

## 测试

### 运行测试

```bash
# 完整测试
go test ./... 2>&1 | tail -30

# 竞态检测
go test -race ./... 2>&1 | tail -10

# 覆盖率
go test -coverprofile=coverage.out ./... 2>&1 | tail -5
go tool cover -func=coverage.out 2>/dev/null | tail -5
```

### 基准测试

```bash
# 检查是否有 benchmark
grep -r "func Benchmark" --include="*_test.go" . | wc -l
```

---

## 代码质量

### 静态分析

```bash
# go vet
go vet ./... 2>&1 | tail -10

# golangci-lint（如果配置了）
golangci-lint run 2>&1 | tail -10

# 格式化检查
gofmt -l . 2>&1 | wc -l

# 检查 golangci-lint 配置
ls .golangci.yml .golangci.yaml 2>/dev/null
```

### 代码模式

```bash
# TODO/FIXME
grep -rn "TODO\|FIXME\|HACK\|XXX" --include="*.go" --exclude-dir=vendor | wc -l

# panic 调用（生产代码中）
grep -rn "panic(" --include="*.go" --exclude="*_test.go" --exclude-dir=vendor | wc -l
```

---

## CI/CD

```bash
ls .github/workflows/ .gitlab-ci.yml 2>/dev/null
```

---

## 依赖审计

```bash
# govulncheck（如果可用）
govulncheck ./... 2>&1 | tail -20

# 过期依赖
go list -u -m all 2>&1 | grep "\[" | head -20

# 依赖数量
grep -c "require" go.mod 2>/dev/null
```

---

## Go 特定评分阈值

| 维度 | ★★★★★ | ★★★★☆ | ★★★☆☆ |
|------|:---:|:---:|:---:|
| 测试覆盖率 | > 70% | 40-70% | 15-40% |
| go vet 结果 | 0 issues | < 5 | < 20 |
| golangci-lint | 已配置+0 issue | 已配置 | 未配置 |

---

## 常见 Go 问题

| 问题 | 检测方式 | 风险 |
|------|---------|:--:|
| 无 `go.sum` | 文件缺失 | 🔴 依赖不可验证 |
| vendor 目录过大 | 检查 vendor/ 大小 | 🟢 可选 |
| 无 golangci-lint 配置 | 无 .golangci.yml | 🟡 代码风格不统一 |
| `interface{}` 遗留 | grep 检查（Go 1.18+ 应用 `any`） | 🟢 现代性 |
