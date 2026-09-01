import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from datetime import date
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

SKILL_DIR = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = SKILL_DIR / "scripts"
sys.path.insert(0, str(SCRIPTS_DIR))

import extract_range
from extract_daily import (
    MAX_PLAIN_TEXT_CHARS,
    PARSE_FAILURE_MARKER,
    TRUNCATION_MARKER,
    extract_date_by_thread,
    parse_message,
    query_active_days,
    redact_sensitive,
    thread_file_stem,
)
from run_history import build_manifest, parse_args, plan_units, sha256_file, snapshot_database, write_private_json, write_private_text
from validate_run import cleanup_inputs, validate_run


class ThreadDatabaseTestCase(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.db_path = self.root / "threads.db"
        with sqlite3.connect(self.db_path) as conn:
            conn.executescript("""
                CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    cwd TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    message_count INTEGER NOT NULL,
                    hidden INTEGER NOT NULL
                );
                CREATE TABLE messages (
                    message_id TEXT PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL
                );
            """)

    def tearDown(self):
        self.temp_dir.cleanup()

    def add_thread(self, thread_id, day, cwd="/repo", messages=None, hidden=0):
        messages = messages or [("user", "question"), ("assistant", "answer"), ("user", "thanks")]
        with sqlite3.connect(self.db_path) as conn:
            conn.execute(
                "INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?)",
                (thread_id, thread_id, cwd, f"{day}T08:00:00", f"{day}T12:00:00", len(messages), hidden),
            )
            for index, (role, content) in enumerate(messages):
                conn.execute(
                    "INSERT INTO messages VALUES (?, ?, ?, ?)",
                    (f"{thread_id}-message-{index:03d}", thread_id, role, json.dumps(content)),
                )

    def make_run_args(self, **overrides):
        values = {
            "db": str(self.db_path), "days": 1, "cwd": "/repo", "all": False,
            "run_root": str(self.root / "runs"), "max_units": 7,
            "target_kb": 250, "max_threads": 15, "today": "2026-08-24",
        }
        values.update(overrides)
        return SimpleNamespace(**values)

    def write_valid_unit_outputs(self, run_dir, manifest):
        for unit in manifest["units"]:
            write_private_text(run_dir / unit["summary_path"], "# summary\n")
            sidecar = {
                "unit_id": unit["id"],
                "status": "analyzed",
                "input_files": [
                    {"path": item["path"], "sha256": item["sha256"], "status": "analyzed", "notes": ""}
                    for item in unit["inputs"]
                ],
                "thread_count": unit["expected_thread_count"],
                "message_count": unit["expected_message_count"],
                "findings": [{
                    "id": "F-001", "classification": "execution_deviation", "evidence": ["thread evidence"],
                    "counterevidence": [], "frequency": "1/1", "impact": "low", "confidence": "high",
                    "fact_source": "none", "acceptance": "repeat without deviation",
                }],
                "blocked": [],
                "degraded_inputs_reviewed": [
                    item["path"] for item in unit["inputs"]
                    if item["truncations"] or item["parse_failures"]
                ],
            }
            write_private_json(run_dir / unit["sidecar_path"], sidecar)


class SnapshotDatabaseTest(unittest.TestCase):
    def test_wal_source_backup_supports_readonly_query_without_mutating_source(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source_path = root / "source.db"
            snapshot_dir = root / "snapshot"
            snapshot_dir.mkdir(mode=0o700)
            snapshot_path = snapshot_dir / "threads.db"

            source = sqlite3.connect(source_path)
            try:
                self.assertEqual(source.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal")
                source.execute("PRAGMA wal_autocheckpoint=0")
                source.execute("CREATE TABLE records (id INTEGER PRIMARY KEY, value TEXT NOT NULL)")
                source.execute("INSERT INTO records (value) VALUES ('before-backup')")
                source.commit()
                wal_path = Path(f"{source_path}-wal")
                shm_path = Path(f"{source_path}-shm")
                self.assertTrue(wal_path.is_file())
                wal_before = (wal_path.read_bytes(), wal_path.stat().st_mtime_ns, wal_path.stat().st_ino)
                self.assertTrue(shm_path.is_file())
                source_bytes = source_path.read_bytes()

                snapshot_database(source_path, snapshot_path)

                with sqlite3.connect(f"file:{snapshot_path}?mode=ro", uri=True) as snapshot:
                    self.assertEqual(snapshot.execute("SELECT value FROM records").fetchall(), [("before-backup",)])
                    self.assertEqual(snapshot.execute("PRAGMA journal_mode").fetchone()[0], "delete")

                self.assertEqual(source_path.read_bytes(), source_bytes)
                self.assertEqual(
                    (wal_path.read_bytes(), wal_path.stat().st_mtime_ns, wal_path.stat().st_ino),
                    wal_before,
                )
                self.assertTrue(shm_path.is_file())
            finally:
                source.close()

            self.assertEqual(os.stat(snapshot_dir).st_mode & 0o777, 0o700)
            self.assertEqual(os.stat(snapshot_path).st_mode & 0o777, 0o600)
            with sqlite3.connect(f"file:{source_path}?mode=ro", uri=True) as source:
                self.assertEqual(source.execute("PRAGMA journal_mode").fetchone()[0], "wal")
                self.assertEqual(source.execute("SELECT value FROM records").fetchall(), [("before-backup",)])

    def test_target_connect_failure_closes_readonly_source(self):
        source = MagicMock()
        with patch(
            "run_history.sqlite3.connect",
            side_effect=[source, sqlite3.OperationalError("target unavailable")],
        ):
            with self.assertRaisesRegex(sqlite3.OperationalError, "target unavailable"):
                snapshot_database("source.db", "snapshot.db")

        source.close.assert_called_once_with()


class QueryActiveDaysTest(ThreadDatabaseTestCase):
    def test_days_seven_returns_exactly_seven_natural_dates_including_today(self):
        for day in range(17, 26):
            self.add_thread(f"thread-{day}", f"2026-08-{day:02d}")

        rows = query_active_days(str(self.db_path), days=7, cwd="/repo", today=date(2026, 8, 24))

        self.assertEqual([row["day"] for row in rows], [f"2026-08-{day:02d}" for day in range(24, 17, -1)])
        self.assertEqual(sum(row["thread_count"] for row in rows), 7)

    def test_project_filter_respects_path_boundary_and_like_literals(self):
        self.add_thread("exact", "2026-08-24", cwd="/repo/a_b%")
        self.add_thread("child", "2026-08-24", cwd="/repo/a_b%/child")
        self.add_thread("sibling", "2026-08-24", cwd="/repo/a_b%other")
        self.add_thread("wildcard-like", "2026-08-24", cwd="/repo/axbZZ")

        rows = query_active_days(str(self.db_path), days=1, cwd="/repo/a_b%", today=date(2026, 8, 24))

        self.assertEqual(rows[0]["thread_count"], 2)

    def test_windows_project_filter_is_host_independent_and_boundary_safe(self):
        self.add_thread("exact", "2026-08-24", cwd=r"C:\Work\Repo")
        self.add_thread("child", "2026-08-24", cwd=r"c:\work\repo\child")
        self.add_thread("sibling", "2026-08-24", cwd=r"C:\Work\Repository")
        self.add_thread("other-drive", "2026-08-24", cwd=r"D:\Work\Repo")

        rows = query_active_days(str(self.db_path), days=1, cwd=r"C:\WORK\REPO", today=date(2026, 8, 24))

        self.assertEqual(rows[0]["thread_count"], 2)

    def test_days_must_be_positive(self):
        with self.assertRaisesRegex(ValueError, "days 必须大于 0"):
            query_active_days(str(self.db_path), days=0, today=date(2026, 8, 24))


class ExtractionIntegrityTest(ThreadDatabaseTestCase):
    def test_truncation_and_parse_failure_are_explicit_and_secret_is_redacted(self):
        long_secret = "api_key=redaction-sentinel-value " + "x" * MAX_PLAIN_TEXT_CHARS
        _, _, text, _, _, text_stats = parse_message(("m1", "user", json.dumps(long_secret)))
        _, _, malformed, _, _, malformed_stats = parse_message(("m2", "user", "not-json"))

        self.assertIn(TRUNCATION_MARKER, text)
        self.assertIn("[REDACTED]", text)
        self.assertNotIn("redaction-sentinel-value", text)
        self.assertEqual(text_stats["truncations"], 1)
        self.assertEqual(malformed, PARSE_FAILURE_MARKER)
        self.assertEqual(malformed_stats["parse_failures"], 1)

        _, _, unsupported, _, _, unsupported_stats = parse_message((
            "m3", "user", json.dumps({"content": [{"type": "unexpected"}]})
        ))
        self.assertIn(PARSE_FAILURE_MARKER, unsupported)
        self.assertEqual(unsupported_stats["parse_failures"], 1)

    def test_redaction_covers_auth_headers_and_connection_userinfo(self):
        text = "Authorization: Basic redaction-sentinel database_url=postgres://user:redaction-sentinel@db/repo"

        redacted = redact_sensitive(text)

        self.assertNotIn("redaction-sentinel", redacted)
        self.assertGreaterEqual(redacted.count("[REDACTED]"), 2)

    def test_short_prefix_collision_produces_distinct_private_files_and_stats(self):
        prefix = "same-prefix-"
        self.add_thread(prefix + "one", "2026-08-24", messages=[("user", "x" * (MAX_PLAIN_TEXT_CHARS + 1)), ("assistant", "ok"), ("user", "done")])
        self.add_thread(prefix + "two", "2026-08-24")
        out_dir = self.root / "out"

        count, results = extract_date_by_thread("2026-08-24", str(self.db_path), str(out_dir), cwd="/repo")

        self.assertEqual(count, 2)
        self.assertEqual(len(results), 2)
        self.assertNotEqual(thread_file_stem(prefix + "one"), thread_file_stem(prefix + "two"))
        for result in results.values():
            self.assertEqual(os.stat(result["path"]).st_mode & 0o777, 0o600)
        self.assertEqual(os.stat(out_dir).st_mode & 0o777, 0o700)
        self.assertEqual(sum(result["truncations"] for result in results.values()), 1)


class WorkloadPlanningTest(unittest.TestCase):
    def test_plan_units_balances_by_size_and_preserves_each_input_once(self):
        items = [
            {"day": "2026-08-24", "path": f"thread-{index}.txt", "size_kb": size}
            for index, size in enumerate((120, 110, 10, 10))
        ]

        units = plan_units(items, target_kb=130, max_threads=2, max_units=2)

        paths = [item["path"] for unit in units for item in unit]
        self.assertEqual(len(units), 2)
        self.assertCountEqual(paths, [item["path"] for item in items])
        self.assertEqual(len(paths), len(set(paths)))


class RunManifestTest(ThreadDatabaseTestCase):
    def test_snapshot_run_is_stable_and_validator_rejects_missing_sidecars(self):
        self.add_thread("thread-1", "2026-08-24")
        run_dir, manifest = build_manifest(self.make_run_args())
        snapshot_digest = manifest["snapshot"]["sha256"]
        self.add_thread("thread-after-snapshot", "2026-08-24")

        validation = validate_run(run_dir)

        self.assertEqual(manifest["status"], "ready")
        self.assertEqual(manifest["totals"]["thread_count"], 1)
        self.assertEqual(snapshot_digest, sha256_file(run_dir / manifest["snapshot"]["path"]))
        self.assertEqual(validation["status"], "failed")
        self.assertIn("one or more units failed validation", validation["errors"])
        self.assertEqual(os.stat(run_dir).st_mode & 0o777, 0o700)

    def test_runs_are_unique_and_generated_inputs_are_private(self):
        self.add_thread("thread-1", "2026-08-24")

        first_run, first_manifest = build_manifest(self.make_run_args())
        second_run, _ = build_manifest(self.make_run_args())

        self.assertNotEqual(first_run, second_run)
        self.assertEqual(os.stat(first_run).st_mode & 0o777, 0o700)
        private_files = [first_run / "manifest.json", first_run / first_manifest["snapshot"]["path"]]
        private_files.extend(first_run / item["path"] for item in first_manifest["units"][0]["inputs"])
        private_files.append(first_run / first_manifest["units"][0]["prompt_path"])
        for path in private_files:
            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600, str(path))
        self.assertEqual(os.stat(first_run / "summaries").st_mode & 0o777, 0o700)

    def test_failed_initialization_removes_partial_run(self):
        with patch("run_history.snapshot_database", side_effect=sqlite3.OperationalError("expected failure")):
            with self.assertRaises(sqlite3.OperationalError):
                build_manifest(self.make_run_args())

        run_root = self.root / "runs"
        self.assertEqual(list(run_root.iterdir()), [])

    def test_empty_snapshot_validates_without_analysis_units(self):
        run_dir, manifest = build_manifest(self.make_run_args())

        validation = validate_run(run_dir)

        self.assertEqual(manifest["status"], "empty")
        self.assertEqual(manifest["units"], [])
        self.assertEqual(validation["status"], "passed")

    def test_cli_rejects_cwd_and_all_together(self):
        with self.assertRaises(SystemExit) as error:
            parse_args(["--cwd", "/repo", "--all"])

        self.assertEqual(error.exception.code, 2)


class RunValidationTest(ThreadDatabaseTestCase):
    def setUp(self):
        super().setUp()
        self.add_thread("thread-1", "2026-08-24")
        self.run_dir, self.manifest = build_manifest(self.make_run_args())
        self.write_valid_unit_outputs(self.run_dir, self.manifest)

    def rewrite_manifest(self):
        write_private_json(self.run_dir / "manifest.json", self.manifest)

    def test_validator_accepts_exact_digest_complete_sidecar(self):
        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "passed")

    def test_validator_rejects_tampered_snapshot_and_input(self):
        snapshot_path = self.run_dir / self.manifest["snapshot"]["path"]
        input_path = self.run_dir / self.manifest["units"][0]["inputs"][0]["path"]
        with open(snapshot_path, "ab") as handle:
            handle.write(b"tamper")
        with open(input_path, "a", encoding="utf-8") as handle:
            handle.write("tamper")

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("snapshot digest changed", validation["errors"])
        self.assertTrue(any("input digest changed" in error for error in validation["units"][0]["errors"]))

    def test_validator_rejects_manifest_path_escape(self):
        self.manifest["units"][0]["summary_path"] = "../outside.md"
        self.rewrite_manifest()

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertTrue(any("invalid summary_path" in error for error in validation["units"][0]["errors"]))

    def test_validator_rejects_malformed_sidecar(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        write_private_text(sidecar_path, "{")

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertTrue(any("sidecar unreadable" in error for error in validation["units"][0]["errors"]))

    def test_validator_rejects_nonterminal_unit_status(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["status"] = "pending"
        write_private_json(sidecar_path, sidecar)

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("unit status is not analyzed", validation["units"][0]["errors"])

    def test_validator_rejects_sensitive_sidecar_content(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["input_files"][0]["notes"] = "api_key=credential-placeholder-value"
        write_private_json(sidecar_path, sidecar)

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("sidecar contains sensitive credential pattern", validation["units"][0]["errors"])

    def test_validator_rejects_duplicate_thread_identity(self):
        duplicate = dict(self.manifest["days"][0]["files"][0])
        duplicate["path"] = "extracted/2026-08-24/duplicate.txt"
        source = self.run_dir / self.manifest["days"][0]["files"][0]["path"]
        target = self.run_dir / duplicate["path"]
        write_private_text(target, source.read_text(encoding="utf-8"))
        duplicate["sha256"] = sha256_file(target)
        self.manifest["days"][0]["files"].append(duplicate)
        self.manifest["days"][0]["thread_count"] += 1
        self.manifest["totals"]["thread_count"] += 1
        self.manifest["totals"]["message_count"] += duplicate["message_count"]
        self.rewrite_manifest()

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertTrue(any("duplicate thread_id" in error for error in validation["errors"]))

    def test_cleanup_rejects_noncanonical_run_root(self):
        marker = self.run_dir / "snapshot" / "threads.db"

        with self.assertRaisesRegex(ValueError, "cleanup is limited"):
            cleanup_inputs(self.run_dir, {"status": "passed"})

        self.assertTrue(marker.exists())

    def test_cleanup_removes_only_sensitive_inputs_from_canonical_run(self):
        canonical_root = self.root / "canonical-runs"
        with patch("validate_run.DEFAULT_RUN_ROOT", str(canonical_root)):
            canonical_run, canonical_manifest = build_manifest(self.make_run_args(run_root=str(canonical_root)))
            self.write_valid_unit_outputs(canonical_run, canonical_manifest)
            report = validate_run(canonical_run)

            cleanup_inputs(canonical_run, report)

        self.assertFalse((canonical_run / "snapshot").exists())
        self.assertFalse((canonical_run / "extracted").exists())
        self.assertFalse((canonical_run / "prompts").exists())
        self.assertTrue((canonical_run / "manifest.json").exists())
        self.assertTrue((canonical_run / canonical_manifest["units"][0]["summary_path"]).exists())
        self.assertTrue((canonical_run / canonical_manifest["units"][0]["sidecar_path"]).exists())


class ExtractRangeCompatibilityTest(ThreadDatabaseTestCase):
    def make_extract_args(self, merge=False):
        return SimpleNamespace(
            db=str(self.db_path), query_active_days=False, days=7, cwd="/repo", all=False,
            merge=merge, out=None, out_root=str(self.root / "exports"),
        )

    def test_split_export_returns_nonzero_after_partial_failure(self):
        args = self.make_extract_args()
        results = {"thread": {"size_kb": 1, "errors": 0, "cwd": "/repo"}}
        with patch.object(extract_range, "parse_date_range", return_value=(args, "2026-08-23", "2026-08-24")), patch.object(
            extract_range, "extract_date_by_thread", side_effect=[(1, results), RuntimeError("expected failure")]
        ):
            status = extract_range.main()

        self.assertEqual(status, 1)

    def test_merge_export_returns_nonzero_after_partial_failure(self):
        args = self.make_extract_args(merge=True)
        first_output = Path(args.out_root) / "learn-day-2026-08-23.txt"

        def extract_side_effect(day, _db, output_path, cwd=None):
            if day == "2026-08-24":
                raise RuntimeError("expected failure")
            write_private_text(output_path, "# extracted\n")
            self.assertEqual(cwd, "/repo")
            return 1, {}

        with patch.object(extract_range, "parse_date_range", return_value=(args, "2026-08-23", "2026-08-24")), patch.object(
            extract_range, "extract_date", side_effect=extract_side_effect
        ):
            status = extract_range.main()

        self.assertEqual(status, 1)
        self.assertTrue(first_output.exists())

    def test_cli_entry_propagates_partial_failure_exit_status(self):
        self.add_thread("good", "2026-08-23")
        self.add_thread("bad-output", "2026-08-24")
        out_root = self.root / "cli-exports"
        out_root.mkdir()
        (out_root / "learn-2026-08-24").write_text("blocks directory creation", encoding="utf-8")

        completed = subprocess.run([
            sys.executable, str(SCRIPTS_DIR / "extract_range.py"), "2026-08-23", "2026-08-24",
            "--db", str(self.db_path), "--cwd", "/repo", "--out-root", str(out_root),
        ], text=True, capture_output=True)

        self.assertEqual(completed.returncode, 1)
        self.assertIn("提取失败日期: 2026-08-24", completed.stderr)


if __name__ == "__main__":
    unittest.main()
