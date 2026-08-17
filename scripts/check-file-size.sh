#!/usr/bin/env bash
# 大文件扫描门：按行数扫描源码，报告超阈值文件（拆分/重构参考，可接 CI 门）。
#
# 源码与测试分开设阈值——测试文件天然偏大，默认放宽（对齐
# check-layer-imports.sh 的测试豁免思路）；rg 尊重 .gitignore，
# target/node_modules 自动排除。
#
# 用法：bash scripts/check-file-size.sh [--min N] [--test-min N] [--no-tests] [--top N]
#   --min N       源码阈值，默认 1000；0 = 不检查源码
#   --test-min N  测试阈值，默认 4000；0 = 不检查测试
#   --no-tests    等价 --test-min 0
#   --top N       每类最多展示条数，默认 30
# 退出码：0 无超阈值；1 存在超阈值；2 参数错误

set -uo pipefail
cd "$(dirname "$0")/.."

command -v rg >/dev/null 2>&1 || { echo "❌ 需要 ripgrep (rg)"; exit 1; }

MIN=1000
TEST_MIN=4000
TOP=30

while [ $# -gt 0 ]; do
    case "$1" in
        --min) MIN="${2:?--min 缺少参数}"; shift 2 ;;
        --test-min) TEST_MIN="${2:?--test-min 缺少参数}"; shift 2 ;;
        --no-tests) TEST_MIN=0; shift ;;
        --top) TOP="${2:?--top 缺少参数}"; shift 2 ;;
        -h|--help)
            cat <<'EOF'
用法：bash scripts/check-file-size.sh [--min N] [--test-min N] [--no-tests] [--top N]
  --min N       源码阈值，默认 1000；0 = 不检查源码
  --test-min N  测试阈值，默认 4000；0 = 不检查测试
  --no-tests    等价 --test-min 0
  --top N       每类最多展示条数，默认 30
退出码：0 无超阈值；1 存在超阈值；2 参数错误
EOF
            exit 0 ;;
        *) echo "❌ 未知参数：$1（-h 查看用法）"; exit 2 ;;
    esac
done

files="$(rg --files \
    -g '*.rs' -g '*.ts' -g '*.tsx' -g '*.mjs' -g '*.js' \
    -g '!target' -g '!node_modules' -g '!dist' \
    2>/dev/null)" || true

if [ -z "$files" ]; then
    echo "❌ 未找到源码文件"
    exit 1
fi

total=0
viol_src=0
viol_test=0
report_src=""
report_test=""

while IFS= read -r f; do
    [ -z "$f" ] && continue
    total=$((total + 1))
    lines="$(wc -l < "$f" | tr -d '[:space:]')"
    case "$f" in
        *_test.rs|*_test.ts|*_test.tsx|*/_test/*|*/tests/*)
            if [ "$TEST_MIN" -gt 0 ] && [ "$lines" -gt "$TEST_MIN" ]; then
                viol_test=$((viol_test + 1))
                report_test+="$(printf '%8d  %s' "$lines" "$f")"$'\n'
            fi ;;
        *)
            if [ "$MIN" -gt 0 ] && [ "$lines" -gt "$MIN" ]; then
                viol_src=$((viol_src + 1))
                report_src+="$(printf '%8d  %s' "$lines" "$f")"$'\n'
            fi ;;
    esac
done <<< "$files"

echo "📊 大文件扫描：源码阈值 ${MIN} 行 / 测试阈值 ${TEST_MIN} 行"
echo ""

if [ -n "$report_src" ]; then
    echo "❌ 源码超阈值（Top ${TOP}）："
    printf '%s' "$report_src" | sort -rn | head -n "$TOP"
    echo ""
else
    echo "✅ 源码文件均未超阈值"
fi

if [ "$TEST_MIN" -gt 0 ]; then
    if [ -n "$report_test" ]; then
        echo "❌ 测试超阈值（Top ${TOP}）："
        printf '%s' "$report_test" | sort -rn | head -n "$TOP"
        echo ""
    else
        echo "✅ 测试文件均未超阈值"
    fi
fi

viol=$((viol_src + viol_test))
echo "共扫描 ${total} 个文件，超阈值 ${viol} 个（源码 ${viol_src} / 测试 ${viol_test}）"

if [ "$viol" -eq 0 ]; then
    echo "✅ 大文件扫描通过"
    exit 0
fi
exit 1
