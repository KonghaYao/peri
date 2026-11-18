#!/usr/bin/env python3
"""Negative fixtures for feature/acceptance validation; no repository mutation."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("catalog", Path(__file__).with_name("check-feature-catalog.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.catalog = self.root / "catalog.md"
        self.acceptance = self.root / "acceptance.md"
        (self.root / "contract.md").write_text("# 契约\n# 契约\n<a id='exact'></a>\n")
        self.row = "| F-CORE-001 | L1 | 执行循环 | [契约](contract.md#契约) |\n"
        self.write()

    def write(self, row=None, evidence="缺口：尚未运行集成验证", map_id="F-CORE-001"):
        self.catalog.write_text("| ID | 等级 | 功能与边界 | 权威来源 |\n| --- | --- | --- | --- |\n" + (self.row if row is None else row))
        self.acceptance.write_text("| ID | 实现状态 | 契约 | 证据或缺口 | 验证状态 |\n| --- | --- | --- | --- | --- |\n" + f"| {map_id} | 已实现 | [契约](contract.md#契约) | {evidence} | 未运行 |\n")

    def errors(self):
        return guard.validate(self.root, self.catalog, self.acceptance)

    def test_valid_mapping_is_not_runtime_acceptance(self):
        self.assertEqual(self.errors(), [])

    def test_duplicate_id(self):
        self.write(self.row * 2)
        self.assertTrue(any("duplicate ID" in e for e in self.errors()))

    def test_malformed_id_cannot_hide_between_valid_rows(self):
        self.write(self.row + self.row.replace("F-CORE-001", "bad"))
        self.assertTrue(any("invalid feature ID" in e for e in self.errors()))

    def test_l6_rejected(self):
        self.write(self.row.replace("L1", "L6"))
        self.assertTrue(any("invalid level" in e for e in self.errors()))

    def test_visible_length_counts_link_labels_not_destinations(self):
        self.write(self.row.replace("执行循环", "字" * 101))
        self.assertTrue(any("100 characters" in e for e in self.errors()))
        self.assertEqual(guard.visible("| [短文](https://example.invalid/very-long-url) |"), "短文")

    def test_missing_source_and_anchor(self):
        for replacement in ("missing.md", "contract.md#missing"):
            with self.subTest(replacement=replacement):
                self.write(self.row.replace("contract.md#契约", replacement))
                self.assertTrue(any("missing" in e for e in self.errors()))

    def test_external_authority_not_accepted(self):
        self.write(self.row.replace("contract.md#契约", "https://example.invalid"))
        self.assertTrue(any("repository-local" in e for e in self.errors()))

    def test_critical_feature_needs_acceptance(self):
        self.acceptance.write_text("")
        self.assertTrue(any("missing acceptance" in e for e in self.errors()))

    def test_l5_does_not_require_e2e_mapping(self):
        self.write(self.row.replace("L1", "L5"))
        self.acceptance.write_text("")
        self.assertEqual(self.errors(), [])

    def test_empty_evidence_and_generic_claim_rejected(self):
        for text in ("", "通过", "缺口："):
            with self.subTest(text=text):
                self.write(evidence=text)
                self.assertTrue(any("explicit gap" in e for e in self.errors()))

    def test_unknown_mapping_rejected(self):
        self.write(map_id="F-CORE-999")
        self.assertTrue(any("unknown feature" in e for e in self.errors()))

    def test_code_fences_and_inline_code_are_not_links(self):
        self.assertEqual(guard.links("```md\n[x](missing)\n```\n`[x](missing)`\n[x](contract.md)"), ["contract.md"])

    def test_reference_links_and_encoded_anchor(self):
        self.assertEqual(guard.links("[see][ref]\n[ref]: contract.md#%E5%A5%91%E7%BA%A6"), ["contract.md#%E5%A5%91%E7%BA%A6"])
        self.assertIsNone(guard.destination_error(self.root, self.catalog, "contract.md#%E5%A5%91%E7%BA%A6"))

    def test_duplicate_heading_and_html_anchor(self):
        self.assertEqual(guard.anchors((self.root / "contract.md").read_text()), {"契约", "契约-1", "exact"})

    def test_snake_case_heading_and_code_link_label(self):
        self.assertEqual(guard.anchors("# run_react_loop\n# `foo_bar`\n"), {"run_react_loop", "foo_bar"})
        self.assertEqual(guard.visible("[正文 `foo_bar`](contract.md)"), "正文 foo_bar")

    def test_empty_boundary_rejected(self):
        self.write(self.row.replace("执行循环", ""))
        self.assertTrue(any("empty feature boundary" in e for e in self.errors()))

    def test_directory_and_self_authority_rejected(self):
        for destination in (".", "catalog.md", "acceptance.md"):
            self.write(self.row.replace("contract.md#契约", destination))
            self.assertTrue(any("Markdown contract" in e for e in self.errors()))

    def test_acceptance_contract_must_match_catalog(self):
        self.acceptance.write_text(self.acceptance.read_text().replace("[契约](contract.md#契约)", ""))
        self.assertTrue(any("differs from catalog" in e for e in self.errors()))

    def test_inventory_missing_duplicate_and_disposition(self):
        inventory = self.root / "inventory.md"
        inventory.write_text("| [contract](contract.md) | 保留 | 正文不变 |\n")
        self.assertEqual(guard.inventory_errors(self.root, inventory, ["contract.md"]), [])
        self.assertTrue(guard.inventory_errors(self.root, inventory, ["other.md"]))
        inventory.write_text(inventory.read_text() * 2)
        self.assertTrue(any("duplicate" in e for e in guard.inventory_errors(self.root, inventory, [])))
        inventory.write_text("| [contract](contract.md) | 保留 | |\n")
        self.assertTrue(any("disposition" in e for e in guard.inventory_errors(self.root, inventory, [])))

    def test_unrated_requires_explicit_rating_gap(self):
        self.write(self.row.replace("L1", "U"))
        self.assertTrue(any("rating gap" in e for e in self.errors()))
        self.write(self.row.replace("L1", "U"), evidence="缺口：评级裁决未找到")
        self.assertEqual(self.errors(), [])

    def test_status_columns_cannot_be_empty(self):
        for word in ("已实现", "未运行"):
            self.write()
            self.acceptance.write_text(self.acceptance.read_text().replace(word, ""))
            self.assertTrue(any("status must be nonempty" in e for e in self.errors()))

    def test_traversal_rejected(self):
        self.assertEqual(guard.destination_error(self.root, self.catalog, "../outside.md"), "destination escapes repository")


if __name__ == "__main__":
    unittest.main()
