#!/usr/bin/env bash
# CI 测试清单：枚举 workspace 的测试目标，按 CI 实际执行范围分类，并核对清单
# 与 .github/workflows/ci.yml 的过滤器一致。
#
# 目的有三个：
#   1. 给出「实际执行的目标」与「只构建未执行的目标」的可核对清单（迁移前后比较
#      测试身份用，不是只看总数）；
#   2. `--check` 守住必跑套件：CI 依赖的测试二进制一旦被改名/删除，这里先失败，
#      而不是等某个步骤静默命中零例；
#   3. `--check` 守住清单与 ci.yml 的一致性：一个包若从第一个测试步骤里排除、却
#      没有任何后续步骤接手，就会静默失去覆盖；这里直接判失败。
#
# 用 `cargo metadata --no-deps`：只读 manifest，不解析依赖，因此不需要 patch 配置、
# 不触发网络、也不编译。
#
# 「某步骤过滤器命中零例」不在这里兜底：ci.yml 的每个 nextest 步骤都带
# `--no-tests=fail`（nextest 0.9.85 起为默认值），命中零例时以退出码 4 失败，
# 比本脚本更贴近真实执行。
#
# 用法: ci-test-inventory.sh [--check] [--json]
#   --check  必跑套件缺失、或清单与 ci.yml 脱节时 exit 1
#   --json   输出机器可读清单（配合 --check 时结论仍然只走 stderr）

set -euo pipefail

check=false
as_json=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --check) check=true; shift ;;
        --json) as_json=true; shift ;;
        -h|--help)
            sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "usage: $0 [--check] [--json]" >&2
            exit 2
            ;;
    esac
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

workflow=".github/workflows/ci.yml"

# 执行范围的事实源：与 ci.yml 各测试步骤的 `-E` 一一对应。改任一侧都会让
# `--check` 失败，因此两边必须同时更新。
#
# 第一个步骤是「workspace 减去下面 9 个包」，其余步骤只挑具名目标；
# 两者合起来构成全部执行范围。
workspace_excluded=(
    peri-acp
    peri-agent
    peri-tui
    peri-middlewares
    peri-mcp-common
    peri-mcp-web
    peri-mcp-artifact
    peri-mcp-cron
    peri-mcp-workspace
)

# 其余步骤显式执行的目标，形如 `<package>:<target-kind>:<target-name>`；
# kind 或 name 为 `*` 表示该 package 的全部测试目标。
step_includes=(
    'peri-acp:lib:*'
    'peri-agent:lib:*'
    'peri-agent:test:middleware_tests'
    'peri-agent:test:compact_followup_behavior_test'
    'peri-agent:test:compact_failure_adversarial_test'
    'peri-agent:test:compact_pressure_adversarial_test'
    'peri-agent:test:compact_session_adversarial_test'
    'peri-agent:test:compact_authority_projection_test'
    'peri-tui:lib:*'
    'peri-tui:bin:*'
    'peri-tui:test:meta_session_cli'
    'peri-middlewares:lib:*'
    'peri-mcp-common:*:*'
    'peri-mcp-web:*:*'
    'peri-mcp-artifact:*:*'
    'peri-mcp-cron:*:*'
    'peri-mcp-workspace:*:*'
)

# doctest 步骤的范围：`cargo test --doc --workspace --exclude ...`。
# nextest 不支持 doctest，因此单独一个入口，与原 `cargo test` 的范围保持一致。
doctest_excluded=(
    peri-acp
    peri-agent
    peri-tui
    peri-middlewares
)

# 必跑套件（--check 的判据）。ARC-COMPACT-001 要求 Compact 三套对抗测试真实执行
# 且各自命中非零用例，因此它们单列而不是依赖通配。
required_suites=(
    'peri-agent:test:compact_pressure_adversarial_test'
    'peri-agent:test:compact_session_adversarial_test'
    'peri-agent:test:compact_authority_projection_test'
    'peri-agent:test:middleware_tests'
    'peri-agent:test:compact_followup_behavior_test'
    'peri-agent:test:compact_failure_adversarial_test'
    'peri-agent:lib:*'
    'peri-acp:lib:*'
    'peri-tui:test:meta_session_cli'
    'peri-middlewares:lib:*'
)

metadata="$(cargo metadata --no-deps --format-version 1)"

WORKFLOW="$workflow" \
EXCLUDED_SET="$(printf '%s\n' "${workspace_excluded[@]}")" \
INCLUDES="$(printf '%s\n' "${step_includes[@]}")" \
DOCTEST_EXCLUDED="$(printf '%s\n' "${doctest_excluded[@]}")" \
REQUIRED="$(printf '%s\n' "${required_suites[@]}")" \
AS_JSON="$as_json" \
python3 - "$metadata" <<'PY'
import json
import os
import re
import sys

meta = json.loads(sys.argv[1])
workflow_path = os.environ["WORKFLOW"]
workspace_excluded = set(os.environ["EXCLUDED_SET"].split())
include_entries = [e for e in os.environ["INCLUDES"].split()]
doctest_excluded = set(os.environ["DOCTEST_EXCLUDED"].split())
required = [line for line in os.environ["REQUIRED"].splitlines() if line]
as_json = os.environ["AS_JSON"] == "true"

errors = []


def fail(message):
    errors.append(message)


# ---- workspace 实际存在的测试目标 -------------------------------------------

testable_kinds = {"lib", "bin", "test", "proc-macro"}

targets = []
for pkg in sorted(meta["packages"], key=lambda p: p["name"]):
    for target in pkg["targets"]:
        kinds = [k for k in target["kind"] if k in testable_kinds]
        if not kinds or not target.get("test", False):
            continue
        targets.append({
            "package": pkg["name"],
            "kind": kinds[0],
            "name": target["name"],
            "src_path": target["src_path"],
        })

workspace_packages = {t["package"] for t in targets}

# ---- 清单声明的执行范围 -----------------------------------------------------

unknown = workspace_excluded - workspace_packages
if unknown:
    fail(f"workspace_excluded 里有不是 workspace 包的名字: {sorted(unknown)}")

include_all_packages = set()
include_kinds = set()
include_named = set()
for entry in include_entries:
    pkg, kind, name = entry.split(":", 2)
    if pkg not in workspace_packages:
        fail(f"step_includes 里有不是 workspace 包的名字: {entry}")
    if kind == "*":
        include_all_packages.add(pkg)
    elif name == "*":
        include_kinds.add((pkg, kind))
    else:
        include_named.add((pkg, kind, name))

excluded_without_owner = workspace_excluded - {e.split(":", 1)[0] for e in include_entries}
if excluded_without_owner:
    fail(f"被第一个测试步骤排除、也没有后续步骤接手: {sorted(excluded_without_owner)}")

for target in targets:
    pkg, kind, name = target["package"], target["kind"], target["name"]
    covered = (
        pkg in include_all_packages
        or (pkg, kind) in include_kinds
        or (pkg, kind, name) in include_named
    )
    target["state"] = "executed" if covered or pkg not in workspace_excluded else "build-only"

executed = [t for t in targets if t["state"] == "executed"]
build_only = [t for t in targets if t["state"] == "build-only"]

# ---- 与 ci.yml 的一致性 -----------------------------------------------------

try:
    workflow_text = open(workflow_path, encoding="utf-8").read()
except OSError as exc:
    fail(f"无法读取 {workflow_path}: {exc}")
    workflow_text = ""

if workflow_text:
    expressions = re.findall(r"-E\s+'([^']*)'", workflow_text)
    if not expressions:
        fail(f"{workflow_path} 里找不到任何 nextest 过滤器（-E '...'）")

    bare = re.findall(r"package\(\s*(?![=~#/])", workflow_text) + re.findall(
        r"binary\(\s*(?![=~#/])", workflow_text
    )
    if bare:
        fail("ci.yml 的过滤器只支持 package(=名字) / binary(=名字)，否则清单无法核对")

    ci_excluded = set()
    ci_excluded_exprs = 0
    ci_included = set()
    for expr in expressions:
        packages = set(re.findall(r"package\(=([A-Za-z0-9_.-]+)\)", expr))
        if expr.strip().startswith("not ("):
            ci_excluded_exprs += 1
            ci_excluded |= packages
        else:
            ci_included |= packages

    if ci_excluded_exprs != 1:
        fail(f"应当恰好有一个 `not (...)` 排除步骤，实际 {ci_excluded_exprs} 个")
    if ci_excluded != workspace_excluded:
        fail(
            "ci.yml 的排除包与 workspace_excluded 不一致: "
            f"ci.yml={sorted(ci_excluded)} 清单={sorted(workspace_excluded)}"
        )
    if ci_excluded != ci_included:
        fail(
            "从第一个测试步骤排除的包没有全部被后续步骤接手: "
            f"缺 {sorted(ci_excluded - ci_included)}"
        )
    listed_packages = {entry.split(":", 1)[0] for entry in include_entries}
    if ci_included != listed_packages:
        fail(
            "ci.yml 的包含包与 step_includes 不一致: "
            f"ci.yml={sorted(ci_included)} 清单={sorted(listed_packages)}"
        )

    ci_binaries = set(re.findall(r"binary\(=([A-Za-z0-9_.-]+)\)", workflow_text))
    listed_binaries = {name for _, _, name in include_named}
    if ci_binaries != listed_binaries:
        fail(
            "ci.yml 的 binary(=名字) 与 step_includes 不一致: "
            f"ci.yml={sorted(ci_binaries)} 清单={sorted(listed_binaries)}"
        )

    doc_tail = workflow_text.split("test --doc", 1)
    if len(doc_tail) == 1:
        fail(f"{workflow_path} 里找不到 `cargo test --doc` 步骤")
    else:
        doc_block = doc_tail[1].split("- name:", 1)[0]
        ci_doctest_excluded = set(re.findall(r"--exclude\s+([A-Za-z0-9_.-]+)", doc_block))
        if ci_doctest_excluded != doctest_excluded:
            fail(
                "doctest 排除包与 doctest_excluded 不一致: "
                f"ci.yml={sorted(ci_doctest_excluded)} 清单={sorted(doctest_excluded)}"
            )

# ---- 必跑套件 ---------------------------------------------------------------

missing = []
for entry in required:
    pkg, kind, name = entry.split(":", 2)
    hit = any(
        t["package"] == pkg
        and (kind == "*" or t["kind"] == kind)
        and (name == "*" or t["name"] == name)
        and t["state"] == "executed"
        for t in targets
    )
    if not hit:
        missing.append(entry)
for entry in missing:
    fail(f"必跑套件缺失或不在执行范围内: {entry}")

# ---- 输出 -------------------------------------------------------------------

if as_json:
    json.dump(
        {
            "executed": executed,
            "build_only": build_only,
            "doctest_excluded": sorted(doctest_excluded),
            "errors": errors,
        },
        sys.stdout,
        ensure_ascii=False,
        indent=2,
    )
    sys.stdout.write("\n")
else:
    print(f"executed   {len(executed)} test target(s)")
    for t in executed:
        print(f"  E {t['package']:22} {t['kind']:5} {t['name']}")
    print(f"build-only {len(build_only)} test target(s)  (CI 只编译不执行)")
    for t in build_only:
        print(f"  B {t['package']:22} {t['kind']:5} {t['name']}")
    doctest_pkgs = sorted(workspace_packages - doctest_excluded)
    print(f"doctests   {len(doctest_pkgs)} package(s)  (不执行: {' '.join(sorted(doctest_excluded))})")

for message in errors:
    print(f"::error::{message}", file=sys.stderr)
if errors:
    sys.exit(1)

if not as_json:
    print(f"OK: {len(required)} 个必跑套件在执行范围内，清单与 {workflow_path} 一致")
PY
