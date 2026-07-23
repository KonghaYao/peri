# TypeScript/JavaScript 项目成熟度检查参考

---

## 代码统计

### 源文件与行数（排除 node_modules/dist/build）

```bash
# TS 文件
find . -name "*.ts" -not -path "./node_modules/*" -not -path "./dist/*" -not -path "./build/*" -not -path "./.next/*" -exec wc -l {} + 2>/dev/null | tail -1

# TSX 文件
find . -name "*.tsx" -not -path "./node_modules/*" -not -path "./dist/*" -not -path "./build/*" -not -path "./.next/*" -exec wc -l {} + 2>/dev/null | tail -1

# JS 文件
find . -name "*.js" -not -path "./node_modules/*" -not -path "./dist/*" -not -path "./build/*" -not -path "./.next/*" -exec wc -l {} + 2>/dev/null | tail -1

# JSX 文件
find . -name "*.jsx" -not -path "./node_modules/*" -not -path "./dist/*" -not -path "./build/*" -exec wc -l {} + 2>/dev/null | tail -1

# 总文件数
find . \( -name "*.ts" -o -name "*.tsx" -o -name "*.js" -o -name "*.jsx" \) -not -path "./node_modules/*" -not -path "./dist/*" -not -path "./build/*" | wc -l
```

排除目录黑名单：`node_modules/` `dist/` `build/` `.next/` `coverage/` `.turbo/` `storybook-static/`

### 包/工作区结构

```bash
# npm workspaces / pnpm / yarn
grep "workspaces" package.json 2>/dev/null

# monorepo 包列表
ls packages/ 2>/dev/null || ls apps/ 2>/dev/null

# 各包代码量（如果有 workspaces）
for pkg in packages/*/; do
  lines=$(find "$pkg" -name "*.ts" -o -name "*.tsx" -not -path "*/node_modules/*" -exec wc -l {} + 2>/dev/null | tail -1)
  echo "  $(basename $pkg): $lines"
done
```

### 多语言项目补充

如果有后端代码（Rust/Go/Python），也统计并在报告中标注。

---

## 测试

### 测试框架检测

```bash
# Jest
grep -E '"jest"|"vitest"' package.json 2>/dev/null

# Mocha
grep '"mocha"' package.json 2>/dev/null

# Playwright / Cypress (E2E)
grep -E '"@playwright|"cypress"' package.json 2>/dev/null

# 测试配置
ls jest.config.* vitest.config.* playwright.config.* 2>/dev/null
```

### 测试文件统计

```bash
# 测试文件数
find . -name "*.test.ts" -o -name "*.test.tsx" -o -name "*.spec.ts" -o -name "*.spec.tsx" -o -name "*.test.js" -o -name "*.spec.js" -not -path "./node_modules/*" | wc -l

# 测试目录
find . -type d -name "__tests__" -not -path "./node_modules/*" | wc -l
```

### 运行测试

```bash
# 根据包管理器选择
npm test 2>&1 | tail -30
# 或
pnpm test 2>&1 | tail -30
# 或
yarn test 2>&1 | tail -30
```

### 覆盖率

```bash
# 检查覆盖率配置
grep -E "coverage|istanbul|c8" package.json 2>/dev/null
grep -E "coverage" jest.config.* vitest.config.* 2>/dev/null

# 检查 CI 中的覆盖率上报
grep -r "codecov\|coveralls\|coverage" .github/ 2>/dev/null
```

### JS/TS 测试评分阈值

| 评分 | 测试/代码比 | 说明 |
|:---:|:----------:|------|
| ★★★★★ | > 50% | 优秀 |
| ★★★★☆ | 25-50% | 良好 |
| ★★★☆☆ | 10-25% | 基本合格 |
| ★★☆☆☆ | 3-10% | 不足 |
| ★☆☆☆☆ | < 3% | 严重不足 |

额外扣分条件：
- 核心模块零测试 → 最高 ★★☆☆☆
- 无类型覆盖的 TS 项目（大量 `any`）→ 扣半星
- CI 不跑测试 → 扣半星

---

## 代码质量

### 静态分析

```bash
# ESLint 配置存在
ls .eslintrc* eslint.config.* 2>/dev/null

# Prettier 配置
ls .prettierrc* prettier.config.* 2>/dev/null

# TypeScript 严格模式
grep "strict" tsconfig.json 2>/dev/null

# Biome 配置
ls biome.json 2>/dev/null
```

### 静态分析运行

```bash
# ESLint（如果配置了）
npx eslint . --ext .ts,.tsx 2>&1 | tail -5

# TypeScript 类型检查
npx tsc --noEmit 2>&1 | tail -5
```

### 代码模式

```bash
# any 类型（TS 项目）
grep -rn ": any" --include="*.ts" --include="*.tsx" --exclude-dir=node_modules | wc -l

# TODO/FIXME/HACK
grep -rn "TODO\|FIXME\|HACK\|XXX" --include="*.ts" --include="*.tsx" --exclude-dir=node_modules | wc -l

# console.log 残留
grep -rn "console\.\(log\|warn\|error\)" --include="*.ts" --include="*.tsx" --exclude-dir=node_modules | wc -l

# eval() 使用（危险）
grep -rn "eval(" --include="*.ts" --include="*.tsx" --include="*.js" --exclude-dir=node_modules | wc -l
```

### Pre-commit 检查

```bash
# Husky
ls .husky/ 2>/dev/null

# lint-staged
grep "lint-staged" package.json 2>/dev/null

# lefthook
cat lefthook.yml 2>/dev/null
```

---

## CI/CD

### CI 检测

```bash
# GitHub Actions
ls .github/workflows/ 2>/dev/null

# 其他
ls .gitlab-ci.yml Jenkinsfile 2>/dev/null
```

### CI 内容评估

读取 CI 文件，检查：
- 是否包含 `npm test` / `pnpm test`
- 是否包含 `tsc --noEmit`（类型检查）
- 是否包含 `eslint`
- 是否包含构建步骤（`npm run build`）
- 是否包含 `npm audit`
- 覆盖的操作系统数

### 部署

```bash
# Docker
ls Dockerfile docker-compose* 2>/dev/null

# Vercel/Netlify 配置
ls vercel.json netlify.toml 2>/dev/null

# 发布脚本
grep -E '"publish"|"release"|"deploy"' package.json 2>/dev/null
```

---

## 依赖审计

```bash
# npm audit
npm audit 2>&1 | tail -20

# 或 pnpm
pnpm audit 2>&1 | tail -20

# 依赖数量
grep -c "version" package-lock.json 2>/dev/null || grep -c "version" pnpm-lock.yaml 2>/dev/null
```

---

## 常见 JS/TS 特定问题

| 问题 | 检测方式 | 风险 |
|------|---------|:--:|
| 无 TypeScript | 无 tsconfig.json | 🟡 大型项目无类型安全 |
| strict: false | 检查 tsconfig strict | 🟡 类型检查不完整 |
| 无 ESLint | 无 eslint 配置 | 🟡 代码风格不统一 |
| `package-lock.json` 不一致 | 多个 lock 文件共存 | 🟡 包管理器混乱 |
| 过时的包管理器 | 检查 engines 字段 | 🟢 技术债 |
| 无 .nvmrc / .node-version | 无 Node 版本声明 | 🟢 CI 可能不兼容 |
