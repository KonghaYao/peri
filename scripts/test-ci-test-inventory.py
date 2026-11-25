#!/usr/bin/env python3
"""Fixture-driven tests for scripts/ci_test_inventory.py (no cargo required)."""

from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parent
FIXTURES = SCRIPTS / "fixtures" / "ci-test-inventory"
MODULE = SCRIPTS / "ci_test_inventory.py"

sys.path.insert(0, str(SCRIPTS))
import ci_test_inventory as ci  # noqa: E402


COMPACTS = [
    "compact_pressure_adversarial_test",
    "compact_session_adversarial_test",
    "compact_session_manual_adversarial_test",
    "compact_authority_projection_test",
]
# 主调用排除的 target 与后续逐 binary 门禁的期望覆盖都只涉及这四套对抗。
EXCLUDED = [f"peri-agent:test:{compact}" for compact in COMPACTS]
INCLUDES = list(EXCLUDED)
REQUIRED = [
    *EXCLUDED,
    "peri-agent:test:middleware_tests",
    "peri-agent:lib:*",
    "peri-acp:lib:*",
    "peri-acp:test:integration_test",
    "peri-tui:test:meta_session_cli",
    "peri-middlewares:test:mcp_isolation_contract",
    "peri-model:test:responses_wire_test",
]
PER_BINARY = list(EXCLUDED)
DOCTEST_EXCLUDED = {"peri-acp", "peri-tui"}


def render_workflow(expressions: list[str], doctest: bool = True) -> str:
    lines = ["jobs:", "  build:", "    steps:"]
    for index, expression in enumerate(expressions):
        lines += [
            f"      - name: step-{index}",
            "        shell: bash",
            "        run: >-",
            "            cargo nextest run --locked --workspace --no-tests=fail",
            f"            -E '{expression}'",
        ]
    if doctest:
        lines += [
            "      - name: doctests",
            "        shell: bash",
            "        run: >-",
            "            cargo test --doc --workspace "
            "--exclude peri-acp --exclude peri-tui",
        ]
    return "\n".join(lines) + "\n"


class InventoryEvaluationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.metadata = json.loads((FIXTURES / "metadata.json").read_text(encoding="utf-8"))
        cls.normal_text = (FIXTURES / "workflow-normal.yml").read_text(encoding="utf-8")
        cls.normal_expressions = ci.extract_expressions(cls.normal_text)

    def evaluate(self, workflow_text=None, metadata=None):
        return ci.evaluate(
            metadata=metadata or self.metadata,
            workflow_text=workflow_text or self.normal_text,
            excluded_targets=EXCLUDED,
            include_entries=INCLUDES,
            doctest_excluded=DOCTEST_EXCLUDED,
            required=REQUIRED,
            per_binary=PER_BINARY,
        )

    def selected(self, expression: str) -> set[str]:
        targets = ci.load_targets(self.metadata)
        return {target.ident for target in ci.select_targets(ci.parse_expression(expression), targets)}

    def test_normal_workflow_executes_every_target(self):
        result = self.evaluate()
        self.assertEqual(result.errors, [])
        executed = {target.ident for target in result.executed}
        self.assertEqual(result.build_only, [])
        self.assertIn("peri-acp:test:integration_test", executed)
        self.assertIn("peri-tui:bin:peri", executed)
        self.assertIn("peri-model:test:responses_wire_test", executed)
        self.assertIn("peri-agent:test:compact_authority_projection_test", executed)

    def test_kind_predicate_selects_only_matching_targets(self):
        self.assertEqual(self.selected("kind(=bin)"), {"peri-tui:bin:peri"})
        self.assertEqual(self.selected("kind(bin)"), {"peri-tui:bin:peri"})
        self.assertEqual(
            self.selected("package(=peri-acp) & kind(=lib)"),
            {"peri-acp:lib:peri_acp"},
        )
        test_targets = {
            "peri-acp:test:integration_test",
            "peri-acp:test:prompt_cache_boundary",
            "peri-agent:test:compact_pressure_adversarial_test",
            "peri-agent:test:compact_session_adversarial_test",
            "peri-agent:test:compact_session_manual_adversarial_test",
            "peri-agent:test:compact_authority_projection_test",
            "peri-agent:test:middleware_tests",
            "peri-tui:test:meta_session_cli",
            "peri-middlewares:test:mcp_isolation_contract",
            "peri-model:test:responses_wire_test",
        }
        self.assertEqual(self.selected("kind(=test)"), test_targets)

    def test_negation_and_boolean_precedence(self):
        libs_without_acp = {
            "peri-agent:lib:peri_agent",
            "peri-tui:lib:peri_tui",
            "peri-middlewares:lib:peri_middlewares",
            "peri-mcp-common:lib:peri_mcp_common",
            "peri-model:lib:peri_model",
        }
        self.assertEqual(
            self.selected("not package(=peri-acp) & kind(=lib)"),
            libs_without_acp,
        )
        self.assertEqual(
            self.selected("!package(=peri-acp) & kind(=lib)"),
            libs_without_acp,
        )
        self.assertEqual(
            self.selected("package(=peri-acp) | package(=peri-agent) & kind(=lib)"),
            {
                "peri-acp:lib:peri_acp",
                "peri-acp:test:integration_test",
                "peri-acp:test:prompt_cache_boundary",
                "peri-agent:lib:peri_agent",
            },
        )
        self.assertEqual(
            self.selected("package(=peri-acp) + kind(=bin)"),
            {
                "peri-acp:lib:peri_acp",
                "peri-acp:test:integration_test",
                "peri-acp:test:prompt_cache_boundary",
                "peri-tui:bin:peri",
            },
        )
        selected = self.selected(
            "package(=peri-agent) and (kind(=test) or kind(=bin))"
        )
        self.assertEqual(
            selected,
            {f"peri-agent:test:{compact}" for compact in COMPACTS}
            | {"peri-agent:test:middleware_tests"},
        )
        self.assertEqual(
            self.selected(
                "package(=peri-agent) - binary(=compact_pressure_adversarial_test)"
            ),
            {
                "peri-agent:lib:peri_agent",
                "peri-agent:test:compact_session_adversarial_test",
                "peri-agent:test:compact_session_manual_adversarial_test",
                "peri-agent:test:compact_authority_projection_test",
                "peri-agent:test:middleware_tests",
            },
        )

    def test_narrowed_gate_filter_is_rejected(self):
        # 门禁收窄成同时要求 kind(=lib) 后命中零例；主调用又把它排除在外，
        # 于是该 target 既不执行、也不满足逐 binary 门禁与必跑套件。
        gate = "package(=peri-agent) & binary(=compact_authority_projection_test)"
        expressions = [
            expression + " & kind(=lib)" if expression == gate else expression
            for expression in self.normal_expressions
        ]
        result = self.evaluate(render_workflow(expressions))
        self.assertTrue(
            any(
                "后续步骤的实际选择与 step_includes 不一致" in error
                and "peri-agent:test:compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )
        self.assertTrue(
            any(
                "没有任何测试步骤执行的目标" in error
                and "peri-agent:test:compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )
        self.assertTrue(
            any(
                "逐 binary 非零门禁" in error
                and "peri-agent:test:compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )
        self.assertTrue(
            any(
                "必跑套件缺失" in error
                and "peri-agent:test:compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )

    def test_main_negation_must_exclude_every_declared_target(self):
        expressions = [
            "not (package(=peri-agent) & binary(=compact_pressure_adversarial_test))",
            *self.normal_expressions[1:],
        ]
        result = self.evaluate(render_workflow(expressions))
        self.assertTrue(
            any(
                "排除范围与 main_excluded 不一致" in error
                and "compact_session_adversarial_test" in error
                for error in result.errors
            ),
            result.errors,
        )

    def test_main_negation_must_not_skip_ungated_targets(self):
        main = self.normal_expressions[0][:-1] + " | package(=peri-model))"
        result = self.evaluate(render_workflow([main, *self.normal_expressions[1:]]))
        self.assertTrue(
            any(
                "排除范围与 main_excluded 不一致" in error and "多出" in error
                for error in result.errors
            ),
            result.errors,
        )
        self.assertTrue(
            any(
                "没有任何测试步骤执行的目标" in error
                and "peri-model:lib:peri_model" in error
                for error in result.errors
            ),
            result.errors,
        )

    def test_union_of_compact_suites_fails_per_binary_gate(self):
        union = "package(=peri-agent) & (" + " | ".join(
            f"binary(={compact})" for compact in COMPACTS
        ) + ")"
        result = self.evaluate(render_workflow([self.normal_expressions[0], union]))
        per_binary_errors = [
            error for error in result.errors if "逐 binary 非零门禁" in error
        ]
        self.assertEqual(len(per_binary_errors), len(COMPACTS), result.errors)
        self.assertFalse(
            any("后续步骤的实际选择与 step_includes 不一致" in error for error in result.errors),
            result.errors,
        )

    def test_removed_target_is_reported_as_missing(self):
        metadata = copy.deepcopy(self.metadata)
        for package in metadata["packages"]:
            if package["name"] == "peri-agent":
                package["targets"] = [
                    target
                    for target in package["targets"]
                    if target["name"] != "compact_authority_projection_test"
                ]
        result = self.evaluate(metadata=metadata)
        self.assertTrue(
            any(
                "逐 binary 门禁里的目标不存在" in error
                and "compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )
        self.assertTrue(
            any(
                "必跑套件缺失" in error
                and "compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )

    def test_step_removed_from_workflow_is_reported_as_unexecuted(self):
        expressions = [
            expression
            for expression in self.normal_expressions
            if expression != "package(=peri-agent) & binary(=compact_authority_projection_test)"
        ]
        result = self.evaluate(render_workflow(expressions))
        self.assertTrue(
            any(
                "没有任何测试步骤执行的目标" in error
                and "compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )
        self.assertTrue(
            any(
                "必跑套件缺失" in error
                and "compact_authority_projection_test" in error
                for error in result.errors
            ),
            result.errors,
        )

    def test_doctest_exclusions_must_match_manifest(self):
        result = self.evaluate(self.normal_text.replace("--exclude peri-tui", ""))
        self.assertTrue(
            any("doctest 排除包与 doctest_excluded 不一致" in error for error in result.errors),
            result.errors,
        )


class FilterExpressionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.metadata = json.loads((FIXTURES / "metadata.json").read_text(encoding="utf-8"))

    def assert_syntax_error(self, expression: str):
        with self.assertRaises(ci.FilterSyntaxError, msg=expression):
            ci.parse_expression(expression)

    def test_unsupported_predicates_and_matchers_are_rejected(self):
        for expression in (
            "package(peri-acp)",
            "package(~peri-.*)",
            "package(/peri-.*/)",
            "package(#default)",
            "test(=integration_test)",
            "deps(peri-acp)",
            "rdeps(peri-acp)",
        ):
            with self.subTest(expression=expression):
                self.assert_syntax_error(expression)

    def test_malformed_expressions_are_rejected(self):
        for expression in (
            "",
            "package(=peri-acp) &",
            "package(=peri-acp",
            "package(=peri-acp) && kind(=lib)",
            "package(=peri-acp) or",
            "and package(=peri-acp)",
        ):
            with self.subTest(expression=expression):
                self.assert_syntax_error(expression)

    def test_filter_flag_must_be_single_quoted(self):
        with self.assertRaises(ci.FilterSyntaxError):
            ci.extract_expressions('run: nextest run -E "package(=peri-acp)"')
        with self.assertRaises(ci.FilterSyntaxError):
            ci.extract_expressions("run: nextest run --filterset 'package(=peri-acp)'")
        self.assertEqual(
            ci.extract_expressions("run: nextest run -E 'package(=peri-acp)'"),
            ["package(=peri-acp)"],
        )
        self.assertEqual(
            ci.extract_expressions(
                "# -E 'package(=commented)'\n"
                "run: nextest run -E 'package(=peri-acp)'"
            ),
            ["package(=peri-acp)"],
        )
        self.assertEqual(
            ci.extract_expressions(
                "run: nextest run -E 'package(=peri-agent) & binary(=one)' "
                "-E 'package(=peri-agent) & binary(=two)'"
            ),
            [
                "package(=peri-agent) & binary(=one)",
                "package(=peri-agent) & binary(=two)",
            ],
        )

    def test_attached_or_concatenated_filter_arguments_are_rejected(self):
        # nextest 接受 -E'...' 与 shell 相邻引号拼接；清单只按「-E 空白 '...'」读取，
        # 必须显式失败，否则附加/拼接的过滤器会被静默忽略或截断。
        for workflow in (
            "run: nextest run -E'package(=peri-acp)'",
            "run: nextest run -E 'package(=peri-acp)' -E'package(=peri-agent)'",
            "run: nextest run -E=package(=peri-acp)",
            "run: nextest run -E 'package(=peri-acp)'' | package(=peri-agent)'",
            "run: nextest run -E 'package(=peri-acp)'\" | package(=peri-agent)\"",
        ):
            with self.subTest(workflow=workflow):
                with self.assertRaises(ci.FilterSyntaxError):
                    ci.extract_expressions(workflow)

    def test_metadata_loader_skips_non_test_targets(self):
        idents = {target.ident for target in ci.load_targets(self.metadata)}
        self.assertNotIn("peri-ignored:lib:peri_ignored", idents)
        self.assertIn("peri-agent:lib:peri_agent", idents)


class CommandLineTests(unittest.TestCase):
    def run_cli(self, extra_env=None):
        env = os.environ.copy()
        env.update(
            {
                "WORKFLOW": str(FIXTURES / "workflow-normal.yml"),
                "EXCLUDED_TARGETS": "\n".join(EXCLUDED),
                "INCLUDES": "\n".join(INCLUDES),
                "DOCTEST_EXCLUDED": "\n".join(sorted(DOCTEST_EXCLUDED)),
                "REQUIRED": "\n".join(REQUIRED),
                "PER_BINARY": "\n".join(PER_BINARY),
                "AS_JSON": "false",
            }
        )
        env.update(extra_env or {})
        return subprocess.run(
            [sys.executable, str(MODULE), str(FIXTURES / "metadata.json")],
            capture_output=True,
            text=True,
            env=env,
            timeout=30,
        )

    def test_cli_passes_without_cargo(self):
        result = self.run_cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("OK:", result.stdout)
        self.assertNotIn("::error::", result.stderr)

    def test_json_output_is_machine_readable(self):
        result = self.run_cli({"AS_JSON": "true"})
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["errors"], [])
        self.assertTrue(payload["executed"])
        self.assertIn("build_only", payload)


if __name__ == "__main__":
    unittest.main()
