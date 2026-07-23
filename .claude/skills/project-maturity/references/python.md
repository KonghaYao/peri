# Python 项目成熟度检查参考

---

## 代码统计

### 源文件与行数（排除虚拟环境）

```bash
# .py 文件总数
find . -name "*.py" -not -path "./.*" -not -path "./venv/*" -not -path "./.venv/*" -not -path "./__pycache__/*" -not -path "./.tox/*" | wc -l

# 总代码行数
find . -name "*.py" -not -path "./.*" -not -path "./venv/*" -not -path "./.venv/*" -not -path "./__pycache__/*" -not -path "./.tox/*" -exec wc -l {} + 2>/dev/null | tail -1

# 测试文件数
find . -name "test_*.py" -o -name "*_test.py" -not -path "./venv/*" -not -path "./.venv/*" | wc -l
```

排除目录黑名单：`venv/` `.venv/` `__pycache__/` `.tox/` `.mypy_cache/` `.pytest_cache/` `dist/` `build/` `*.egg-info/`

### 包结构

```bash
# 包管理工具
ls pyproject.toml setup.py setup.cfg requirements.txt Pipfile 2>/dev/null

# 包列表
find . -maxdepth 2 -name "__init__.py" -not -path "./venv/*" -not -path "./.venv/*" | sed 's|/__init__.py||' | sed 's|^\./||'
```

---

## 测试

### 测试框架检测

```bash
# pytest
grep -E "pytest" pyproject.toml setup.cfg requirements*.txt Pipfile 2>/dev/null

# unittest
grep -E "import unittest" --include="*.py" -r . 2>/dev/null | head -3

# tox / nox
ls tox.ini noxfile.py 2>/dev/null
```

### 运行测试

```bash
# pytest（如果可用）
python -m pytest --tb=short 2>&1 | tail -30

# 或通过 tox
tox 2>&1 | tail -30
```

### 覆盖率

```bash
# coverage.py 配置
grep -E "coverage|pytest-cov" pyproject.toml setup.cfg requirements*.txt 2>/dev/null

# CI 中的覆盖率上报
grep -r "codecov\|coveralls\|coverage" .github/ 2>/dev/null
```

---

## 代码质量

### 静态分析

```bash
# Linter 配置
ls .flake8 .pylintrc pyproject.toml 2>/dev/null
grep -E "ruff|flake8|pylint|black|isort|mypy" pyproject.toml 2>/dev/null

# Ruff 检查（如果配置了）
ruff check . 2>&1 | tail -5

# mypy 类型检查
mypy . 2>&1 | tail -10

# black 格式化检查
black --check . 2>&1 | tail -5
```

### 代码模式

```bash
# eval/exec（危险）
grep -rn "eval(\|exec(" --include="*.py" --exclude-dir=venv --exclude-dir=.venv | wc -l

# TODO/FIXME
grep -rn "TODO\|FIXME\|HACK\|XXX" --include="*.py" --exclude-dir=venv --exclude-dir=.venv | wc -l

# print 语句残留（应该用 logging）
grep -rn "print(" --include="*.py" --exclude-dir=venv --exclude-dir=.venv | wc -l
```

### Pre-commit

```bash
# pre-commit 配置
cat .pre-commit-config.yaml 2>/dev/null
```

---

## CI/CD

```bash
# 标准检查
ls .github/workflows/ .gitlab-ci.yml tox.ini 2>/dev/null
```

---

## 依赖审计

```bash
# pip-audit（如果可用）
pip-audit 2>&1 | tail -20

# safety check
safety check 2>&1 | tail -20

# 过期依赖
pip list --outdated 2>&1 | tail -20
```

---

## Python 特定评分阈值

| 维度 | ★★★★★ | ★★★★☆ | ★★★☆☆ |
|------|:---:|:---:|:---:|
| 测试/代码比 | > 60% | 30-60% | 10-30% |
| 类型注解覆盖率 | > 80% | 50-80% | 20-50% |
| lint 配置 | Ruff+MyPy+Black | Ruff 或 Flake8 | 无配置 |

---

## 常见 Python 问题

| 问题 | 检测方式 | 风险 |
|------|---------|:--:|
| 无类型注解 | 项目无 mypy 配置且无 type hints | 🟡 |
| `requirements.txt` 无版本锁定 | 检查是否有 `==` | 🟡 构建不可复现 |
| 无虚拟环境声明 | 无 .python-version 或 Pipfile | 🟢 |
| `setup.py` 而非 `pyproject.toml` | 检查构建系统 | 🟢 旧式配置 |
