#!/usr/bin/env python3
"""CI 测试清单的过滤器语义核对。

由 scripts/ci-test-inventory.sh 调用：读取 cargo metadata（--no-deps）与 ci.yml，
按 nextest 过滤器表达式的真实选择结果判定每个测试目标是否执行。

只支持 package/binary/kind 的 `=值` 精确匹配。出现其他谓词或匹配器直接报错，
避免清单静默忽略会收窄执行范围的语义。
"""

from __future__ import annotations

import json
import os
import re
import sys
from dataclasses import dataclass
from typing import Iterable, Sequence

TESTABLE_KINDS = {"lib", "bin", "test", "proc-macro"}
PREDICATE_FIELDS = {"package", "binary", "kind"}


class FilterSyntaxError(ValueError):
    """过滤器表达式使用了清单无法解释的语法。"""


@dataclass(frozen=True)
class Token:
    kind: str
    value: str
    pos: int


@dataclass(frozen=True)
class Target:
    package: str
    kind: str
    kinds: tuple[str, ...]
    name: str
    src_path: str

    @property
    def ident(self) -> str:
        return f"{self.package}:{self.kind}:{self.name}"


@dataclass
class InventoryResult:
    errors: list[str]
    executed: list[Target]
    build_only: list[Target]
    doctest_excluded: set[str]
    workspace_packages: set[str]


def tokenize(expression: str) -> list[Token]:
    tokens: list[Token] = []
    index = 0
    while index < len(expression):
        char = expression[index]
        if char.isspace():
            index += 1
            continue
        if char in "()&|=":
            tokens.append(Token(char, char, index))
            index += 1
            continue
        if char in "~#":
            tokens.append(Token(char, char, index))
            index += 1
            continue
        if char == "/":
            start = index
            index += 1
            value: list[str] = []
            while index < len(expression) and expression[index] != "/":
                if expression[index] == "\\" and index + 1 < len(expression):
                    value.append(expression[index:index + 2])
                    index += 2
                else:
                    value.append(expression[index])
                    index += 1
            if index == len(expression):
                raise FilterSyntaxError(f"未闭合的正则字面量 @{start}: {expression!r}")
            index += 1
            tokens.append(Token("regex", "".join(value), start))
            continue
        if char.isalnum() or char in "_-.":
            start = index
            while index < len(expression) and (
                expression[index].isalnum() or expression[index] in "_-."
            ):
                index += 1
            tokens.append(Token("word", expression[start:index], start))
            continue
        raise FilterSyntaxError(f"不支持的字符 {char!r} @{index}: {expression!r}")
    return tokens


class _Parser:
    def __init__(self, expression: str):
        self.expression = expression
        self.tokens = tokenize(expression)
        self.index = 0

    def _peek(self) -> Token | None:
        return self.tokens[self.index] if self.index < len(self.tokens) else None

    def _take(self) -> Token | None:
        token = self._peek()
        if token is not None:
            self.index += 1
        return token

    def _accept(self, kind: str, keyword: str) -> bool:
        token = self._peek()
        if token is None:
            return False
        if token.kind == kind or (token.kind == "word" and token.value == keyword):
            self.index += 1
            return True
        return False

    def parse(self):
        if not self.tokens:
            raise FilterSyntaxError("过滤器表达式为空")
        node = self._parse_or()
        token = self._peek()
        if token is not None:
            raise FilterSyntaxError(
                f"过滤器表达式有多余内容 {token.value!r} @{token.pos}: {self.expression!r}"
            )
        return node

    def _parse_or(self):
        node = self._parse_and()
        while self._accept("|", "or"):
            node = ("or", node, self._parse_and())
        return node

    def _parse_and(self):
        node = self._parse_not()
        while self._accept("&", "and"):
            node = ("and", node, self._parse_not())
        return node

    def _parse_not(self):
        token = self._peek()
        if token is not None and token.kind == "word" and token.value == "not":
            self.index += 1
            return ("not", self._parse_not())
        return self._parse_primary()

    def _parse_primary(self):
        token = self._take()
        if token is None:
            raise FilterSyntaxError(f"过滤器表达式缺少操作数: {self.expression!r}")
        if token.kind == "(":
            node = self._parse_or()
            closing = self._take()
            if closing is None or closing.kind != ")":
                raise FilterSyntaxError(f"括号未闭合 @{token.pos}: {self.expression!r}")
            return node
        if token.kind != "word":
            raise FilterSyntaxError(
                f"意外的记号 {token.value!r} @{token.pos}: {self.expression!r}"
            )
        predicate = token.value
        if predicate not in PREDICATE_FIELDS:
            raise FilterSyntaxError(
                f"不支持的过滤器谓词 {predicate}(...)：清单只理解 package/binary/kind 的精确匹配"
            )
        opening = self._take()
        if opening is None or opening.kind != "(":
            raise FilterSyntaxError(f"{predicate} 后缺少 '(' @{token.pos}: {self.expression!r}")
        matcher = self._take()
        if matcher is None:
            raise FilterSyntaxError(f"{predicate}(...) 未闭合 @{token.pos}: {self.expression!r}")
        if matcher.kind == ")":
            raise FilterSyntaxError(f"{predicate}(...) 缺少匹配器：只支持 {predicate}(=值)")
        if matcher.kind != "=":
            raise FilterSyntaxError(
                f"不支持的匹配器 {matcher.value!r} @{matcher.pos}：只支持 {predicate}(=值)"
            )
        value = self._take()
        if value is None or value.kind != "word":
            raise FilterSyntaxError(
                f"{predicate}(=...) 缺少名字 @{matcher.pos}: {self.expression!r}"
            )
        closing = self._take()
        if closing is None or closing.kind != ")":
            raise FilterSyntaxError(f"{predicate}(...) 未闭合 @{token.pos}: {self.expression!r}")
        return ("pred", predicate, value.value, token.pos)


def parse_expression(expression: str):
    """把 nextest 过滤器表达式解析为 AST；无法解释的语法抛 FilterSyntaxError。"""

    return _Parser(expression).parse()


def target_matches(node, target: Target) -> bool:
    operator = node[0]
    if operator == "pred":
        _, predicate, value, _ = node
        if predicate == "package":
            return target.package == value
        if predicate == "binary":
            return target.name == value
        return value in target.kinds
    if operator == "not":
        return not target_matches(node[1], target)
    if operator == "and":
        return target_matches(node[1], target) and target_matches(node[2], target)
    if operator == "or":
        return target_matches(node[1], target) or target_matches(node[2], target)
    raise AssertionError(f"未知 AST 节点: {node!r}")


def select_targets(node, targets: Iterable[Target]) -> frozenset[Target]:
    return frozenset(target for target in targets if target_matches(node, target))


def load_targets(metadata: dict) -> list[Target]:
    targets: list[Target] = []
    for package in metadata.get("packages", []):
        for target in package.get("targets", []):
            kinds = tuple(kind for kind in target.get("kind", []) if kind in TESTABLE_KINDS)
            if not kinds or not target.get("test", False):
                continue
            targets.append(
                Target(
                    package=package["name"],
                    kind=kinds[0],
                    kinds=kinds,
                    name=target["name"],
                    src_path=target.get("src_path", ""),
                )
            )
    return sorted(targets, key=lambda target: (target.package, target.kind, target.name))


def extract_expressions(workflow_text: str) -> list[str]:
    """提取 ci.yml 中的 `-E '...'`，并拒绝清单无法可靠读取的写法。"""

    expressions = re.findall(r"-E\s+'([^']*)'", workflow_text)
    if re.search(r"--filterset(?:\s|=)", workflow_text):
        raise FilterSyntaxError("清单只解析 `-E '...'`，请把 --filterset 改为 -E")
    flag_count = len(re.findall(r"(?:^|\s)(?:-E|--filterset)(?=\s)", workflow_text))
    if flag_count != len(expressions):
        raise FilterSyntaxError(
            "每次测试的过滤器必须写作 -E '...'（单引号）；"
            "清单无法读取 --filterset 或双引号形式"
        )
    return expressions


def _split_entry(entry: str) -> tuple[str, str, str] | None:
    parts = entry.split(":", 2)
    if len(parts) != 3 or not all(parts):
        return None
    return parts[0], parts[1], parts[2]


def matches_entry(target: Target, entry: tuple[str, str, str]) -> bool:
    package, kind, name = entry
    return (
        target.package == package
        and (kind == "*" or target.kind == kind)
        and (name == "*" or target.name == name)
    )


def _ident_list(targets: Iterable[Target]) -> str:
    return "[" + ", ".join(sorted(target.ident for target in targets)) + "]"


def evaluate(
    metadata: dict,
    workflow_text: str,
    workspace_excluded: set[str],
    include_entries: Sequence[str],
    doctest_excluded: set[str],
    required: Sequence[str],
    per_binary: Sequence[str],
) -> InventoryResult:
    errors: list[str] = []

    def fail(message: str) -> None:
        errors.append(message)

    targets = load_targets(metadata)
    all_targets = set(targets)
    workspace_packages = {target.package for target in targets}

    unknown = workspace_excluded - workspace_packages
    if unknown:
        fail(f"workspace_excluded 里有不是 workspace 包的名字: {sorted(unknown)}")

    includes: list[tuple[str, str, str]] = []
    for entry in include_entries:
        parsed_entry = _split_entry(entry)
        if parsed_entry is None:
            fail(f"step_includes 条目格式应为 package:kind:name: {entry}")
            continue
        if parsed_entry[0] not in workspace_packages:
            fail(f"step_includes 里有不是 workspace 包的名字: {entry}")
        includes.append(parsed_entry)

    include_packages = {entry[0] for entry in includes}
    if include_packages != workspace_excluded:
        fail(
            "step_includes 的包与 workspace_excluded 不一致: "
            f"缺 {sorted(workspace_excluded - include_packages)} "
            f"多 {sorted(include_packages - workspace_excluded)}"
        )

    # ---- 解析 ci.yml 的过滤器，按真实选择结果计算执行范围 ------------------

    executed: set[Target] = set()
    parsed_expressions: list[tuple[str, tuple]] = []
    try:
        expressions = extract_expressions(workflow_text)
    except FilterSyntaxError as exc:
        fail(f"{exc}")
        expressions = []

    if not expressions:
        fail("ci.yml 里找不到任何 nextest 过滤器（-E '...'）")

    parse_failed = False
    for expression in expressions:
        try:
            parsed = parse_expression(expression)
        except FilterSyntaxError as exc:
            fail(f"无法解析 ci.yml 过滤器 {expression!r}: {exc}")
            parse_failed = True
            continue
        parsed_expressions.append((expression, parsed))
        executed |= select_targets(parsed, all_targets)

    broad_steps = [(expr, ast) for expr, ast in parsed_expressions if ast[0] == "not"]
    if len(broad_steps) != 1:
        fail(f"应当恰好有一个 not (...) 排除步骤，实际 {len(broad_steps)} 个")

    expected_excluded_targets = {
        target for target in all_targets if target.package in workspace_excluded
    }
    expected_followup = {
        target for target in all_targets if any(matches_entry(target, entry) for entry in includes)
    }

    if not parse_failed and len(broad_steps) == 1:
        broad_ast = broad_steps[0][1]
        broad_selected = select_targets(broad_ast, all_targets)
        broad_excluded = all_targets - broad_selected
        if broad_excluded != expected_excluded_targets:
            fail(
                "第一个测试步骤的排除范围与 workspace_excluded 不一致（必须排除这些包的全部目标）: "
                f"缺少 {_ident_list(expected_excluded_targets - broad_excluded)} "
                f"多出 {_ident_list(broad_excluded - expected_excluded_targets)}"
            )

        followup_selections = [
            (expr, ast, select_targets(ast, all_targets))
            for expr, ast in parsed_expressions
            if ast is not broad_ast
        ]
        followup_actual: set[Target] = set()
        for _, _, selection in followup_selections:
            followup_actual |= selection

        if followup_actual != expected_followup:
            fail(
                "后续步骤的实际选择与 step_includes 不一致: "
                f"缺少 {_ident_list(expected_followup - followup_actual)} "
                f"多出 {_ident_list(followup_actual - expected_followup)}"
            )

        unexecuted = expected_excluded_targets - executed
        if unexecuted:
            fail(f"被第一个步骤排除但没有任何后续步骤执行的目标: {_ident_list(unexecuted)}")

        # ---- 逐 binary 非零门禁：每个目标必须有一条只选中它的表达式 ------
        for entry in per_binary:
            parsed_entry = _split_entry(entry)
            if parsed_entry is None:
                fail(f"per_binary_suites 条目格式应为 package:kind:name: {entry}")
                continue
            matches = [target for target in all_targets if matches_entry(target, parsed_entry)]
            if len(matches) != 1:
                fail(f"逐 binary 门禁里的目标不存在或不唯一: {entry}")
                continue
            target = matches[0]
            if not any(selection == {target} for _, _, selection in followup_selections):
                fail(
                    "缺少只选中该目标的独立过滤器（逐 binary 非零门禁）: "
                    f"{entry}；同一过滤器选中多个 binary 时 --no-tests=fail 只约束合并结果"
                )

    executed_targets = [target for target in targets if target in executed]
    build_only = [target for target in targets if target not in executed]

    # ---- 必跑套件 ---------------------------------------------------------

    for entry in required:
        parsed_entry = _split_entry(entry)
        if parsed_entry is None:
            fail(f"required_suites 条目格式应为 package:kind:name: {entry}")
            continue
        hit = any(
            target in executed and matches_entry(target, parsed_entry) for target in all_targets
        )
        if not hit:
            fail(f"必跑套件缺失或不在执行范围内: {entry}")

    # ---- doctest ----------------------------------------------------------

    doc_tail = workflow_text.split("test --doc", 1)
    if len(doc_tail) == 1:
        fail("ci.yml 里找不到 `cargo test --doc` 步骤")
    else:
        doc_block = doc_tail[1].split("- name:", 1)[0]
        ci_doctest_excluded = set(re.findall(r"--exclude\s+([A-Za-z0-9_.-]+)", doc_block))
        if ci_doctest_excluded != doctest_excluded:
            fail(
                "doctest 排除包与 doctest_excluded 不一致: "
                f"ci.yml={sorted(ci_doctest_excluded)} 清单={sorted(doctest_excluded)}"
            )

    return InventoryResult(
        errors=errors,
        executed=executed_targets,
        build_only=build_only,
        doctest_excluded=set(doctest_excluded),
        workspace_packages=workspace_packages,
    )


def _target_json(target: Target) -> dict:
    return {
        "package": target.package,
        "kind": target.kind,
        "name": target.name,
        "src_path": target.src_path,
    }


def _read_env_list(name: str) -> list[str]:
    return [line for line in os.environ.get(name, "").splitlines() if line]


def main(argv: Sequence[str] | None = None) -> int:
    args = list(sys.argv if argv is None else argv)
    if len(args) != 2:
        print(f"usage: {args[0]} <cargo-metadata.json>", file=sys.stderr)
        return 2

    with open(args[1], encoding="utf-8") as handle:
        metadata = json.load(handle)

    workflow = os.environ.get("WORKFLOW")
    if not workflow:
        print("WORKFLOW 环境变量未设置（应为 ci.yml 路径）", file=sys.stderr)
        return 2
    try:
        with open(workflow, encoding="utf-8") as handle:
            workflow_text = handle.read()
    except OSError as exc:
        result = InventoryResult(
            errors=[f"无法读取 {workflow}: {exc}"],
            executed=[],
            build_only=[],
            doctest_excluded=set(os.environ.get("DOCTEST_EXCLUDED", "").split()),
            workspace_packages=set(),
        )
    else:
        result = evaluate(
            metadata=metadata,
            workflow_text=workflow_text,
            workspace_excluded=set(os.environ.get("EXCLUDED_SET", "").split()),
            include_entries=_read_env_list("INCLUDES"),
            doctest_excluded=set(os.environ.get("DOCTEST_EXCLUDED", "").split()),
            required=_read_env_list("REQUIRED"),
            per_binary=_read_env_list("PER_BINARY"),
        )

    if os.environ.get("AS_JSON") == "true":
        json.dump(
            {
                "executed": [_target_json(target) for target in result.executed],
                "build_only": [_target_json(target) for target in result.build_only],
                "doctest_excluded": sorted(result.doctest_excluded),
                "errors": result.errors,
            },
            sys.stdout,
            ensure_ascii=False,
            indent=2,
        )
        sys.stdout.write("\n")
    else:
        print(f"executed   {len(result.executed)} test target(s)")
        for target in result.executed:
            print(f"  E {target.package:22} {target.kind:5} {target.name}")
        print(f"build-only {len(result.build_only)} test target(s)  (CI 只编译不执行)")
        for target in result.build_only:
            print(f"  B {target.package:22} {target.kind:5} {target.name}")
        doctest_packages = sorted(result.workspace_packages - result.doctest_excluded)
        print(
            f"doctests   {len(doctest_packages)} package(s)  "
            f"(不执行: {' '.join(sorted(result.doctest_excluded))})"
        )

        for message in result.errors:
            print(f"::error::{message}", file=sys.stderr)
        if not result.errors:
            print(
                f"OK: {len(_read_env_list('REQUIRED'))} 个必跑套件在执行范围内，"
                f"过滤器语义与 {workflow} 一致"
            )

    return 1 if result.errors else 0


if __name__ == "__main__":
    sys.exit(main())
