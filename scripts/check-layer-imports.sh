#!/usr/bin/env bash
# §0 全边依赖门（Seam 3 完整版）
#
# 校验 workspace 各层 crate 不越层 import（docs/top-level.md §0「未声明边一律
# 禁止」；ARC-BOUNDARY-001；伞形 PRD 决策 2「依赖方向 CI 门」）。
#
# 取代 scripts/check-resources-imports.sh / check-acp-imports.sh /
# check-tui-imports.sh，统一入口：
# - 八条边（TUI / ACP 业务面 / ACP→model / Controller / Runtime / Resources /
#   Middlewares / Agent）×（use 导入 / 全路径引用）双模式——全路径模式防
#   「use 规避」复发（批 2 REJECT 教训：118 处 peri_agent:: 全路径引用）
# - 豁免清单唯一事实源 = scripts/.t.conf（每条豁免标注归属
#   收紧任务：L2/L4/L5/M-TUI/M-res/v1 退役）
# - 测试文件（*_test.rs / *_test/ / tests/）全局豁免：测试经引用业务 crate
#   仅验证行为，不构成协议/业务面依赖
#
# 用法：bash scripts/check-layer-imports.sh
# 退出码：0 全部通过；1 存在越层 import

set -uo pipefail
cd "$(dirname "$0")/.."

CONF="scripts/import-exemptions.conf"
if [ ! -f "$CONF" ]; then
    echo "❌ 缺少豁免清单 $CONF"
    exit 1
fi

# 全局测试豁免
TEST_EXEMPTS="_test.rs _test/ tests/"

fail=0
rules=0

while IFS='@' read -r src pattern exempts label; do
    # conf 行字段形如 `x @ y`，@ 两侧空格需 trim
    src="${src#"${src%%[![:space:]]*}"}"; src="${src%"${src##*[![:space:]]}"}"
    pattern="${pattern#"${pattern%%[![:space:]]*}"}"; pattern="${pattern%"${pattern##*[![:space:]]}"}"
    exempts="${exempts#"${exempts%%[![:space:]]*}"}"; exempts="${exempts%"${exempts##*[![:space:]]}"}"
    label="${label#"${label%%[![:space:]]*}"}"; label="${label%"${label##*[![:space:]]}"}"
    # 跳过注释与空行
    [ -z "$src" ] && continue
    case "$src" in \#*) continue ;; esac
    rules=$((rules + 1))

    # use 语句由 use 行（pattern 以 "use " 开头）单独校验；全路径行排除 use
    # 语句行避免双报
    hits="$(grep -rnE "$pattern" "$src" --include="*.rs" 2>/dev/null || true)"
    case "$pattern" in
        use\ *) ;;                                    # use 行：保留 use 语句
        *) hits="$(printf '%s\n' "$hits" | grep -vE ":[0-9]+:[[:space:]]*(pub[[:space:]]+)?use[[:space:]]+" || true)" ;;
    esac

    remaining=""
    if [ -n "$hits" ]; then
        while IFS= read -r line; do
            [ -z "$line" ] && continue
            file="${line%%:*}"
            skip=0
            for e in $exempts; do
                [ -z "$e" ] && continue
                case "$file" in *"$e"*) skip=1; break ;; esac
            done
            if [ "$skip" -eq 0 ]; then
                for t in $TEST_EXEMPTS; do
                    case "$file" in *"$t") skip=1; break ;; esac
                done
            fi
            if [ "$skip" -eq 0 ]; then
                remaining="${remaining}${line}"$'\n'
            fi
        done <<< "$hits"
    fi

    if [ -n "$remaining" ]; then
        printf '❌ [%s] 越层 import（模式：%s）：\n' "$label" "$pattern"
        printf '%s' "$remaining"
        fail=$((fail + 1))
    else
        printf '✅ [%s]\n' "$label"
    fi
done < "$CONF"

echo ""
echo "§0 依赖门：共 $rules 条规则，违规边 $fail"
if [ "$fail" -eq 0 ]; then
    echo "✅ 全边依赖门通过（Seam 3 完整版）"
    exit 0
fi
exit 1
