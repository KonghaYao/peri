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
from validate_run import (
    cleanup_inputs,
    finding_contract_digest,
    finding_contract_snapshot,
    load_attested_historical_findings,
    open_directory_nofollow,
    open_regular_file_at,
    read_json_file_at,
    validate_decision_manifest,
    validate_run,
)


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
                    "id": "F-001", "classification": "execution_deviation",
                    "failure_pattern": "agent skips an existing instruction",
                    "root_cause": "the instruction was available but not followed",
                    "evidence": [f"{unit['inputs'][0]['path']} :: thread evidence"],
                    "counterevidence": [], "frequency": "1/1", "impact": "low", "confidence": "high",
                    "fact_source": "none", "target_surface": "none",
                    "why_this_surface": "an execution deviation does not establish a component gap",
                    "predicted_fixes": ["the same instruction is followed on a comparable task"],
                    "risk_regressions": ["none identified: no harness edit is proposed"],
                    "acceptance": {
                        "target": ["repeat without deviation"],
                        "preserved_success": ["preserve a comparable success"],
                    },
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


class SecureAttestationFileTest(unittest.TestCase):
    def test_json_content_and_digest_come_from_same_open_file(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            artifact_path = root / "artifact.json"
            retained_path = root / "artifact-retained.json"
            artifact_path.write_text('{"value":"trusted"}\n', encoding="utf-8")
            root_fd = open_directory_nofollow(root)
            real_open_regular_file_at = open_regular_file_at

            def swap_path_after_open(directory_fd, relative_path):
                file_fd = real_open_regular_file_at(directory_fd, relative_path)
                artifact_path.rename(retained_path)
                artifact_path.write_text('{"value":"replacement"}\n', encoding="utf-8")
                return file_fd

            try:
                with patch("validate_run.open_regular_file_at", side_effect=swap_path_after_open):
                    payload, digest = read_json_file_at(root_fd, "artifact.json")
            finally:
                os.close(root_fd)

            self.assertEqual(payload, {"value": "trusted"})
            self.assertEqual(digest, sha256_file(retained_path))


class RunManifestTest(ThreadDatabaseTestCase):
    def test_snapshot_run_is_stable_and_validator_rejects_missing_sidecars(self):
        self.add_thread("thread-1", "2026-08-24")
        run_dir, manifest = build_manifest(self.make_run_args())
        snapshot_digest = manifest["snapshot"]["sha256"]
        self.add_thread("thread-after-snapshot", "2026-08-24")

        validation = validate_run(run_dir)

        self.assertEqual(manifest["status"], "ready")
        self.assertEqual(manifest["repository_root"], str(Path("/repo").resolve()))
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
        self.manifest["repository_root"] = str(self.root)
        self.rewrite_manifest()
        self.write_valid_unit_outputs(self.run_dir, self.manifest)

    def rewrite_manifest(self):
        write_private_json(self.run_dir / "manifest.json", self.manifest)

    def source_sidecar_attestation(self):
        return [
            {
                "unit_id": unit["id"],
                "path": unit["sidecar_path"],
                "sha256": sha256_file(self.run_dir / unit["sidecar_path"]),
            }
            for unit in self.manifest["units"]
        ]

    def write_valid_decision_manifest(self):
        path = self.root / "spec" / "reviews" / "history-learn-2026-08-24.json"
        payload = {
            "version": 1,
            "run_id": self.manifest["run_id"],
            "source_run_dir": str(self.run_dir),
            "source_manifest_sha256": sha256_file(self.run_dir / "manifest.json"),
            "source_sidecars": self.source_sidecar_attestation(),
            "project_filter": self.manifest["project_filter"],
            "prior_attribution": [],
            "changes": [{
                "id": "CHG-001",
                "status": "proposed",
                "source_findings": [f"{self.manifest['units'][0]['id']}/F-001"],
                "classification": "execution_deviation",
                "failure_pattern": "agent skips an existing instruction",
                "root_cause": "the instruction was available but not followed",
                "baseline": "1/1 comparable thread showed the deviation in the current snapshot",
                "target_surface": "none",
                "files": [],
                "why_this_surface": "no component gap is established",
                "predicted_fixes": ["the instruction is followed on a comparable task"],
                "risk_regressions": ["none identified: no harness edit is proposed"],
                "acceptance": {
                    "target": ["repeat without deviation"],
                    "preserved_success": ["preserve a comparable success"],
                },
                "verification": [],
            }],
        }
        write_private_json(path, payload)
        return path, payload

    def write_prior_decision_manifest(self, day="2026-08-23", change_id="CHG-OLD"):
        path = self.root / "spec" / "reviews" / f"history-learn-{day}.json"
        payload = {
            "version": 1,
            "run_id": f"prior-{day}",
            "source_manifest_sha256": "a" * 64,
            "project_filter": self.manifest["project_filter"],
            "prior_attribution": [],
            "changes": [{
                "id": change_id,
                "status": "implemented",
                "classification": "execution_deviation",
                "target_surface": "none",
                "predicted_fixes": ["old predicted fix"],
                "risk_regressions": ["old regression risk"],
                "acceptance": {
                    "target": ["old target check"],
                    "preserved_success": ["old preserved-success check"],
                },
                "verification": [
                    {
                        "check": "old target check",
                        "command": "python3 -m unittest target",
                        "status": "passed",
                        "result": "target behavior passed",
                    },
                    {
                        "check": "old preserved-success check",
                        "command": "python3 -m unittest preserved",
                        "status": "passed",
                        "result": "preserved behavior passed",
                    },
                ],
            }],
        }
        write_private_json(path, payload)
        return path, payload

    def current_finding(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        return json.loads(sidecar_path.read_text(encoding="utf-8"))["findings"][0]

    def attribution_observation(self, prior_contract="old predicted fix", outcome="improved"):
        finding = self.current_finding()
        return {
            "source_finding": f"{self.manifest['units'][0]['id']}/F-001",
            "source_run_id": self.manifest["run_id"],
            "source_manifest_sha256": sha256_file(self.run_dir / "manifest.json"),
            "finding_contract": finding_contract_snapshot(finding),
            "finding_digest": finding_contract_digest(finding),
            "prior_contract": prior_contract,
            "outcome": outcome,
            "observed_delta": f"the target behavior was {outcome}",
        }

    def valid_prior_attribution(self, source="spec/reviews/history-learn-2026-08-23.json"):
        return {
            "source": source,
            "change_id": "CHG-OLD",
            "verdict": "keep",
            "rationale": "the comparable behavior improved without the predicted regression",
            "observed_fixes": [self.attribution_observation()],
            "observed_regressions": [],
        }

    def attest_historical_decision(self, path, payload):
        payload["run_id"] = self.manifest["run_id"]
        payload["source_run_dir"] = str(self.run_dir)
        payload["source_manifest_sha256"] = sha256_file(self.run_dir / "manifest.json")
        payload["source_sidecars"] = self.source_sidecar_attestation()
        for attribution in payload.get("prior_attribution", []):
            for field in ("observed_fixes", "observed_regressions"):
                for observation in attribution.get(field, []):
                    observation["source_run_id"] = payload["run_id"]
                    observation["source_manifest_sha256"] = payload["source_manifest_sha256"]
        write_private_json(path, payload)
        write_private_json(self.run_dir / "validation.json", {
            "status": "passed",
            "attestation": {
                "manifest_sha256": payload["source_manifest_sha256"],
                "sidecars": payload["source_sidecars"],
            },
            "decision_manifest": {
                "status": "passed",
                "path": str(path.resolve()),
                "sha256": sha256_file(path),
            },
        })

    def write_terminal_decision_manifest(self):
        prior_source, _ = self.write_prior_decision_manifest(day="2026-08-22")
        path = self.root / "spec" / "reviews" / "history-learn-2026-08-23.json"
        attribution = self.valid_prior_attribution(
            source="spec/reviews/history-learn-2026-08-22.json"
        )
        attribution["observed_fixes"][0]["source_run_id"] = "prior-2026-08-23"
        attribution["observed_fixes"][0]["source_manifest_sha256"] = "b" * 64
        payload = {
            "version": 1,
            "run_id": "prior-2026-08-23",
            "source_manifest_sha256": "b" * 64,
            "project_filter": self.manifest["project_filter"],
            "prior_attribution": [attribution],
            "changes": [],
        }
        self.attest_historical_decision(path, payload)
        return prior_source, path, payload

    def test_decision_manifest_rejects_nonexact_sidecar_attestation(self):
        cases = {
            "missing": None,
            "incomplete": [],
            "wrong_digest": [{
                **self.source_sidecar_attestation()[0],
                "sha256": "0" * 64,
            }],
        }
        for case, source_sidecars in cases.items():
            with self.subTest(case=case):
                path, payload = self.write_valid_decision_manifest()
                if source_sidecars is None:
                    del payload["source_sidecars"]
                else:
                    payload["source_sidecars"] = source_sidecars
                write_private_json(path, payload)

                validation = validate_decision_manifest(self.run_dir, path)

                self.assertEqual(validation["status"], "failed")
                self.assertIn("decision source sidecar attestation mismatch", validation["errors"])

    def test_validation_attestation_binds_manifest_and_sidecars(self):
        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "passed")
        self.assertEqual(validation["attestation"], {
            "manifest_sha256": sha256_file(self.run_dir / "manifest.json"),
            "sidecars": self.source_sidecar_attestation(),
        })

    def test_validator_accepts_exact_digest_complete_sidecar(self):
        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "passed")

    def test_validator_rejects_unlocatable_finding_evidence(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["findings"][0]["evidence"] = ["thread evidence without input path"]
        write_private_json(sidecar_path, sidecar)

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertIn(
            "finding 0 evidence needs an input path and locator",
            validation["units"][0]["errors"],
        )

    def test_validator_rejects_incomplete_change_contract(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        del sidecar["findings"][0]["risk_regressions"]
        write_private_json(sidecar_path, sidecar)

        validation = validate_run(self.run_dir)

        self.assertEqual(validation["status"], "failed")
        self.assertIn(
            "finding 0 missing: risk_regressions",
            validation["units"][0]["errors"],
        )
        self.assertIn(
            "finding 0 needs non-empty risk_regressions",
            validation["units"][0]["errors"],
        )

    def test_decision_manifest_accepts_traceable_falsifiable_change(self):
        path, _ = self.write_valid_decision_manifest()

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "passed")

    def test_decision_manifest_rejects_other_repository_reviews_directory(self):
        other_path = self.root / "other" / "spec" / "reviews" / "history-learn-2026-08-24.json"
        _, payload = self.write_valid_decision_manifest()
        write_private_json(other_path, payload)

        validation = validate_decision_manifest(self.run_dir, other_path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("decision manifest is outside the run repository", validation["errors"])

    def test_decision_manifest_rejects_unknown_finding_and_missing_regression_probe(self):
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["source_findings"] = ["unit-999/F-999"]
        payload["changes"][0]["acceptance"] = {
            "target": ["only target check"],
            "preserved_success": [],
        }
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("change 0 references unknown finding", validation["errors"])
        self.assertIn(
            "change 0 acceptance needs non-empty preserved_success checks",
            validation["errors"],
        )

    def test_decision_manifest_rejects_acceptance_that_omits_finding_checks(self):
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["acceptance"] = {
            "target": ["different target check"],
            "preserved_success": ["different preserved check"],
        }
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("change 0 acceptance omits source finding target checks", validation["errors"])

    def test_decision_manifest_rejects_duplicate_acceptance_checks(self):
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["acceptance"] = {
            "target": ["same check"],
            "preserved_success": ["same check"],
        }
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("change 0 acceptance checks must be unique", validation["errors"])

    def test_acceptance_normalizes_whitespace_for_uniqueness(self):
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["acceptance"] = {
            "target": ["same check"],
            "preserved_success": ["  same   check\n"],
        }
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertIn("change 0 acceptance checks must be unique", validation["errors"])

    def test_decision_manifest_requires_verification_for_implemented_change(self):
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["status"] = "implemented"
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("change 0 implemented without verification", validation["errors"])

    def test_decision_manifest_accepts_bound_prior_attribution(self):
        self.write_prior_decision_manifest()
        path, payload = self.write_valid_decision_manifest()
        payload["prior_attribution"] = [self.valid_prior_attribution()]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "passed")

    def test_decision_manifest_rejects_unbound_prior_sources_and_change_ids(self):
        self.write_prior_decision_manifest()
        path, payload = self.write_valid_decision_manifest()
        payload["prior_attribution"] = [
            self.valid_prior_attribution("../history-learn-2026-08-23.json"),
            {**self.valid_prior_attribution(), "change_id": "CHG-MISSING"},
            self.valid_prior_attribution("spec/reviews/history-learn-2026-08-22.json"),
        ]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("prior attribution 0 has noncanonical source", validation["errors"])
        self.assertIn("prior attribution 1 references unknown change", validation["errors"])
        self.assertIn("prior attribution 2 references unknown change", validation["errors"])

    def test_decision_manifest_rejects_strong_verdict_without_observation(self):
        self.write_prior_decision_manifest()
        path, payload = self.write_valid_decision_manifest()
        attribution = self.valid_prior_attribution()
        attribution["observed_fixes"] = []
        payload["prior_attribution"] = [attribution]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("prior attribution 0 keep needs fixed or improved outcome", validation["errors"])

    def test_decision_manifest_rejects_change_contract_that_differs_from_finding(self):
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["classification"] = "skill_gap"
        payload["changes"][0]["target_surface"] = "skill"
        payload["changes"][0]["files"] = [".claude/skills/learn-from-history/SKILL.md"]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("change 0 classification differs from source finding", validation["errors"])
        self.assertIn("change 0 target_surface differs from source finding", validation["errors"])

    def test_decision_manifest_rejects_unbound_attribution_observation(self):
        self.write_prior_decision_manifest()
        path, payload = self.write_valid_decision_manifest()
        attribution = self.valid_prior_attribution()
        attribution["observed_fixes"][0]["prior_contract"] = "invented prediction"
        payload["prior_attribution"] = [attribution]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn(
            "prior attribution 0 observed_fixes 0 does not match prior predicted_fixes",
            validation["errors"],
        )

    def test_decision_manifest_rejects_verdict_outcome_conflicts(self):
        self.write_prior_decision_manifest()
        path, payload = self.write_valid_decision_manifest()
        keep = self.valid_prior_attribution()
        keep["observed_fixes"] = [self.attribution_observation(outcome="regressed")]
        payload["prior_attribution"] = [keep]
        write_private_json(path, payload)

        keep_validation = validate_decision_manifest(self.run_dir, path)

        self.assertIn("prior attribution 0 keep conflicts with regressed outcome", keep_validation["errors"])

        revert = self.valid_prior_attribution()
        revert["verdict"] = "revert"
        revert["observed_regressions"] = [self.attribution_observation("old regression risk", "improved")]
        payload["prior_attribution"] = [revert]
        write_private_json(path, payload)

        revert_validation = validate_decision_manifest(self.run_dir, path)

        self.assertIn("prior attribution 0 revert needs regressed outcome", revert_validation["errors"])
        self.assertIn("prior attribution 0 revert conflicts with fixed or improved outcome", revert_validation["errors"])

    def test_decision_manifest_rejects_tampered_finding_contract_snapshot(self):
        self.write_prior_decision_manifest()
        path, payload = self.write_valid_decision_manifest()
        attribution = self.valid_prior_attribution()
        attribution["observed_fixes"][0]["finding_contract"]["failure_pattern"] = "invented"
        payload["prior_attribution"] = [attribution]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertIn("prior attribution 0 observed_fixes 0 finding_contract mismatch", validation["errors"])

    def test_historical_self_and_forward_attributions_do_not_close_changes(self):
        _, self_payload = self.write_prior_decision_manifest(day="2026-08-22")
        self_payload["prior_attribution"] = [
            self.valid_prior_attribution("spec/reviews/history-learn-2026-08-22.json")
        ]
        write_private_json(
            self.root / "spec" / "reviews" / "history-learn-2026-08-22.json",
            self_payload,
        )
        self.write_prior_decision_manifest(day="2026-08-23", change_id="CHG-LATER")
        self_payload["prior_attribution"].append({
            **self.valid_prior_attribution("spec/reviews/history-learn-2026-08-23.json"),
            "change_id": "CHG-LATER",
        })
        write_private_json(
            self.root / "spec" / "reviews" / "history-learn-2026-08-22.json",
            self_payload,
        )
        current_path, _ = self.write_valid_decision_manifest()

        validation = validate_decision_manifest(self.run_dir, current_path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn(
            "eligible prior change not attributed: spec/reviews/history-learn-2026-08-22.json#CHG-OLD",
            validation["errors"],
        )
        self.assertIn(
            "eligible prior change not attributed: spec/reviews/history-learn-2026-08-23.json#CHG-LATER",
            validation["errors"],
        )

    def test_decision_manifest_requires_valid_run(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["status"] = "pending"
        write_private_json(sidecar_path, sidecar)
        path, _ = self.write_valid_decision_manifest()

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("run must pass validation before decision validation", validation["errors"])

    def test_validator_rejects_non_object_manifests_without_crashing(self):
        manifest_path = self.run_dir / "manifest.json"
        write_private_json(manifest_path, [])

        run_validation = validate_run(self.run_dir)
        decision_path, _ = self.write_valid_decision_manifest()
        decision_validation = validate_decision_manifest(self.run_dir, decision_path)

        self.assertEqual(run_validation["status"], "failed")
        self.assertIn("manifest must be an object", run_validation["errors"])
        self.assertEqual(decision_validation["status"], "failed")
        self.assertIn("run manifest must be an object", decision_validation["errors"])

    def test_decision_manifest_does_not_require_already_terminal_change(self):
        self.write_terminal_decision_manifest()
        path, _ = self.write_valid_decision_manifest()

        with patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)):
            validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "passed")

    def test_decision_manifest_rejects_repeated_terminal_attribution(self):
        self.write_terminal_decision_manifest()
        path, payload = self.write_valid_decision_manifest()
        payload["prior_attribution"] = [
            self.valid_prior_attribution("spec/reviews/history-learn-2026-08-22.json")
        ]
        write_private_json(path, payload)

        with patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)):
            validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("prior attribution 0 repeats terminal attribution", validation["errors"])

    def test_corrupt_historical_decisions_fail_closed(self):
        prior_path = self.root / "spec" / "reviews" / "history-learn-2026-08-23.json"
        write_private_json(prior_path, [])
        current_path, _ = self.write_valid_decision_manifest()

        non_object = validate_decision_manifest(self.run_dir, current_path)

        self.assertIn("prior decision must be an object: history-learn-2026-08-23.json", non_object["errors"])

        write_private_json(prior_path, {
            "version": 1,
            "run_id": "prior",
            "source_manifest_sha256": "a" * 64,
            "prior_attribution": [],
            "changes": [],
        })

        missing_project = validate_decision_manifest(self.run_dir, current_path)

        self.assertIn("prior decision missing project_filter: history-learn-2026-08-23.json", missing_project["errors"])

    def test_unattested_historical_terminal_does_not_close_change(self):
        self.write_prior_decision_manifest(day="2026-08-22")
        terminal_path = self.root / "spec" / "reviews" / "history-learn-2026-08-23.json"
        attribution = self.valid_prior_attribution("spec/reviews/history-learn-2026-08-22.json")
        write_private_json(terminal_path, {
            "version": 1,
            "run_id": self.manifest["run_id"],
            "source_run_dir": str(self.run_dir),
            "source_manifest_sha256": sha256_file(self.run_dir / "manifest.json"),
            "project_filter": self.manifest["project_filter"],
            "prior_attribution": [attribution],
            "changes": [],
        })
        current_path, _ = self.write_valid_decision_manifest()

        validation = validate_decision_manifest(self.run_dir, current_path)

        self.assertIn(
            "eligible prior change not attributed: spec/reviews/history-learn-2026-08-22.json#CHG-OLD",
            validation["errors"],
        )

    def test_attested_historical_loader_rejects_invalid_path_without_crashing(self):
        source_path, payload = self.write_prior_decision_manifest(day="2026-08-23")
        payload["source_run_dir"] = "\0"
        write_private_json(source_path, payload)

        with patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)):
            findings = load_attested_historical_findings(
                self.root.resolve(),
                "spec/reviews/history-learn-2026-08-23.json",
                payload,
            )

        self.assertIsNone(findings)

    def test_attested_historical_loader_rejects_symlink_loop_without_crashing(self):
        with tempfile.TemporaryDirectory() as run_root:
            root = Path(run_root)
            (root / "loop-a").symlink_to("loop-b")
            (root / "loop-b").symlink_to("loop-a")
            payload = {
                "run_id": "run",
                "source_run_dir": str(root / "loop-a" / "run"),
                "source_manifest_sha256": "a" * 64,
            }

            with patch("validate_run.DEFAULT_RUN_ROOT", str(root / "loop-a")):
                findings = load_attested_historical_findings(
                    self.root.resolve(),
                    "spec/reviews/history-learn-2026-08-23.json",
                    payload,
                )

        self.assertIsNone(findings)

    def test_attested_historical_loader_rejects_core_artifact_symlinks(self):
        for artifact_name in ("manifest.json", "validation.json"):
            with self.subTest(artifact_name=artifact_name):
                source_path, payload = self.write_prior_decision_manifest(day="2026-08-23")
                payload["prior_attribution"] = []
                self.attest_historical_decision(source_path, payload)
                artifact_path = self.run_dir / artifact_name
                original = artifact_path.read_bytes()
                with tempfile.TemporaryDirectory() as outside_dir:
                    outside_artifact = Path(outside_dir) / artifact_name
                    outside_artifact.write_bytes(original)
                    artifact_path.unlink()
                    artifact_path.symlink_to(outside_artifact)

                    with patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)):
                        findings = load_attested_historical_findings(
                            self.root.resolve(),
                            "spec/reviews/history-learn-2026-08-23.json",
                            payload,
                        )

                self.assertIsNone(findings)
                artifact_path.unlink()
                artifact_path.write_bytes(original)
                os.chmod(artifact_path, 0o600)

    def test_attested_manifest_content_and_digest_use_same_open_file(self):
        source_path, payload = self.write_prior_decision_manifest(day="2026-08-23")
        payload["prior_attribution"] = []
        self.attest_historical_decision(source_path, payload)
        manifest_path = self.run_dir / "manifest.json"
        retained_manifest = self.run_dir / "manifest-retained.json"
        real_open_regular_file_at = open_regular_file_at
        swapped = False

        def swap_manifest_after_open(root_fd, relative_path):
            nonlocal swapped
            file_fd = real_open_regular_file_at(root_fd, relative_path)
            if relative_path == "manifest.json" and not swapped:
                manifest_path.rename(retained_manifest)
                manifest_path.write_text("{}\n", encoding="utf-8")
                swapped = True
            return file_fd

        with (
            patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)),
            patch("validate_run.open_regular_file_at", side_effect=swap_manifest_after_open),
        ):
            findings = load_attested_historical_findings(
                self.root.resolve(),
                "spec/reviews/history-learn-2026-08-23.json",
                payload,
            )

        self.assertIn(f"{self.manifest['units'][0]['id']}/F-001", findings)
        self.assertEqual(payload["source_manifest_sha256"], sha256_file(retained_manifest))

    def test_attested_sidecar_read_stays_on_open_file_after_path_replacement(self):
        source_path, payload = self.write_prior_decision_manifest(day="2026-08-23")
        payload["prior_attribution"] = []
        self.attest_historical_decision(source_path, payload)
        relative_sidecar = self.manifest["units"][0]["sidecar_path"]
        sidecar_path = self.run_dir / relative_sidecar
        retained_sidecar = self.run_dir / "summaries" / "unit-retained.json"
        real_open_regular_file_at = open_regular_file_at
        swapped = False

        def swap_sidecar_after_open(root_fd, relative_path):
            nonlocal swapped
            file_fd = real_open_regular_file_at(root_fd, relative_path)
            if relative_path == relative_sidecar and not swapped:
                sidecar_path.rename(retained_sidecar)
                sidecar_path.write_text(
                    json.dumps({"findings": [{"id": "F-EVIL"}]}),
                    encoding="utf-8",
                )
                swapped = True
            return file_fd

        with (
            patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)),
            patch("validate_run.open_regular_file_at", side_effect=swap_sidecar_after_open),
        ):
            findings = load_attested_historical_findings(
                self.root.resolve(),
                "spec/reviews/history-learn-2026-08-23.json",
                payload,
            )

        self.assertIn(f"{self.manifest['units'][0]['id']}/F-001", findings)
        self.assertNotIn(f"{self.manifest['units'][0]['id']}/F-EVIL", findings)

    def test_attested_loader_requires_exact_validation_attestation(self):
        cases = {
            "missing": None,
            "incomplete": {"manifest_sha256": None, "sidecars": []},
            "wrong_sidecar_digest": {
                "manifest_sha256": sha256_file(self.run_dir / "manifest.json"),
                "sidecars": [{
                    **self.source_sidecar_attestation()[0],
                    "sha256": "0" * 64,
                }],
            },
        }
        for case, attestation in cases.items():
            with self.subTest(case=case):
                source_path, payload = self.write_valid_decision_manifest()
                self.attest_historical_decision(source_path, payload)
                validation_path = self.run_dir / "validation.json"
                validation = json.loads(validation_path.read_text(encoding="utf-8"))
                if attestation is None:
                    del validation["attestation"]
                else:
                    validation["attestation"] = attestation
                write_private_json(validation_path, validation)

                with patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)):
                    findings = load_attested_historical_findings(
                        self.root.resolve(),
                        "spec/reviews/history-learn-2026-08-24.json",
                        payload,
                    )

                self.assertIsNone(findings)

    def test_attested_loader_rejects_sidecar_replaced_after_validation(self):
        source_path, payload = self.write_valid_decision_manifest()
        run_report = validate_run(self.run_dir)
        decision_report = validate_decision_manifest(self.run_dir, source_path)
        self.assertEqual(decision_report["status"], "passed")
        run_report["decision_manifest"] = decision_report
        write_private_json(self.run_dir / "validation.json", run_report)
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        replacement = json.loads(sidecar_path.read_text(encoding="utf-8"))
        replacement["post_validation_change"] = True
        write_private_json(sidecar_path, replacement)

        with patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)):
            findings = load_attested_historical_findings(
                self.root.resolve(),
                "spec/reviews/history-learn-2026-08-24.json",
                payload,
            )

        self.assertIsNone(findings)

    def test_attested_historical_loader_rejects_sidecar_symlink(self):
        source_path, payload = self.write_prior_decision_manifest(day="2026-08-23")
        payload["prior_attribution"] = []
        self.attest_historical_decision(source_path, payload)
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        with tempfile.TemporaryDirectory() as outside_dir:
            outside_sidecar = Path(outside_dir) / "unit.json"
            outside_sidecar.write_text(sidecar_path.read_text(encoding="utf-8"), encoding="utf-8")
            sidecar_path.unlink()
            sidecar_path.symlink_to(outside_sidecar)

            with patch("validate_run.DEFAULT_RUN_ROOT", str(self.run_dir.parent)):
                findings = load_attested_historical_findings(
                    self.root.resolve(),
                    "spec/reviews/history-learn-2026-08-23.json",
                    payload,
                )

        self.assertIsNone(findings)

    def test_historical_source_run_dir_escape_and_invalid_path_fail_closed(self):
        self.write_prior_decision_manifest(day="2026-08-22")
        terminal_path = self.root / "spec" / "reviews" / "history-learn-2026-08-23.json"
        attribution = self.valid_prior_attribution("spec/reviews/history-learn-2026-08-22.json")
        payload = {
            "version": 1,
            "run_id": self.manifest["run_id"],
            "source_run_dir": "/etc",
            "source_manifest_sha256": sha256_file(self.run_dir / "manifest.json"),
            "project_filter": self.manifest["project_filter"],
            "prior_attribution": [attribution],
            "changes": [],
        }
        write_private_json(terminal_path, payload)
        current_path, _ = self.write_valid_decision_manifest()

        escaped = validate_decision_manifest(self.run_dir, current_path)

        self.assertIn(
            "eligible prior change not attributed: spec/reviews/history-learn-2026-08-22.json#CHG-OLD",
            escaped["errors"],
        )

        payload["source_run_dir"] = "\0"
        write_private_json(terminal_path, payload)

        invalid = validate_decision_manifest(self.run_dir, current_path)

        self.assertIn(
            "eligible prior change not attributed: spec/reviews/history-learn-2026-08-22.json#CHG-OLD",
            invalid["errors"],
        )

    def test_historical_terminal_needs_valid_finding_snapshot(self):
        self.write_prior_decision_manifest(day="2026-08-22")
        terminal_path = self.root / "spec" / "reviews" / "history-learn-2026-08-23.json"
        attribution = self.valid_prior_attribution("spec/reviews/history-learn-2026-08-22.json")
        attribution["observed_fixes"][0]["finding_digest"] = "0" * 64
        write_private_json(terminal_path, {
            "version": 1,
            "run_id": "prior-2026-08-23",
            "source_manifest_sha256": "b" * 64,
            "project_filter": self.manifest["project_filter"],
            "prior_attribution": [attribution],
            "changes": [],
        })
        current_path, _ = self.write_valid_decision_manifest()

        validation = validate_decision_manifest(self.run_dir, current_path)

        self.assertIn(
            "eligible prior change not attributed: spec/reviews/history-learn-2026-08-22.json#CHG-OLD",
            validation["errors"],
        )

    def test_malformed_historical_terminal_verdict_does_not_hide_eligible_change(self):
        self.write_prior_decision_manifest(day="2026-08-22")
        path = self.root / "spec" / "reviews" / "history-learn-2026-08-23.json"
        malformed = self.valid_prior_attribution("spec/reviews/history-learn-2026-08-22.json")
        malformed["observed_fixes"] = []
        write_private_json(path, {
            "version": 1,
            "run_id": "prior-2026-08-23",
            "source_manifest_sha256": "c" * 64,
            "project_filter": self.manifest["project_filter"],
            "prior_attribution": [malformed],
            "changes": [],
        })
        current_path, _ = self.write_valid_decision_manifest()

        validation = validate_decision_manifest(self.run_dir, current_path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn(
            "eligible prior change not attributed: spec/reviews/history-learn-2026-08-22.json#CHG-OLD",
            validation["errors"],
        )

    def test_decision_manifest_rejects_unstable_finding_and_change_ids(self):
        sidecar_path = self.run_dir / self.manifest["units"][0]["sidecar_path"]
        sidecar = json.loads(sidecar_path.read_text(encoding="utf-8"))
        sidecar["findings"][0]["id"] = "finding one"
        write_private_json(sidecar_path, sidecar)

        run_validation = validate_run(self.run_dir)

        self.assertEqual(run_validation["status"], "failed")
        self.assertIn("finding 0 needs stable F-* id", run_validation["units"][0]["errors"])

        self.write_valid_unit_outputs(self.run_dir, self.manifest)
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["id"] = "change one"
        write_private_json(path, payload)

        decision_validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(decision_validation["status"], "failed")
        self.assertIn("change 0 needs stable CHG-* id", decision_validation["errors"])

    def test_decision_manifest_accepts_structured_implemented_verification(self):
        path, payload = self.write_valid_decision_manifest()
        change = payload["changes"][0]
        change["status"] = "implemented"
        change["verification"] = [
            {
                "check": check,
                "command": f"python3 -m unittest check-{index}",
                "status": "passed",
                "result": "check passed",
            }
            for index, check in enumerate(
                change["acceptance"]["target"] + change["acceptance"]["preserved_success"]
            )
        ]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "passed")

    def test_decision_manifest_rejects_free_text_or_incomplete_verification(self):
        path, payload = self.write_valid_decision_manifest()
        change = payload["changes"][0]
        change["status"] = "implemented"
        change["verification"] = ["tests passed"]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertEqual(validation["status"], "failed")
        self.assertIn("change 0 verification 0 must be an object", validation["errors"])
        self.assertIn("change 0 implemented without passed acceptance checks", validation["errors"])

    def test_implemented_verification_rejects_conflicts_duplicates_and_unknown_checks(self):
        path, payload = self.write_valid_decision_manifest()
        change = payload["changes"][0]
        change["status"] = "implemented"
        target = change["acceptance"]["target"][0]
        preserved = change["acceptance"]["preserved_success"][0]
        change["verification"] = [
            {"check": target, "command": "test target", "status": "passed", "result": "passed"},
            {"check": target, "command": "test target again", "status": "failed", "result": "failed"},
            {"check": preserved, "command": "test preserved", "status": "blocked", "result": "blocked"},
            {"check": "unknown", "command": "test unknown", "status": "passed", "result": "passed"},
        ]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertIn(f"change 0 verification has duplicate check: {target}", validation["errors"])
        self.assertIn("change 0 verification references unknown acceptance check", validation["errors"])
        self.assertIn("change 0 implemented has non-passed verification", validation["errors"])

    def test_proposed_change_rejects_verification_records(self):
        path, payload = self.write_valid_decision_manifest()
        payload["changes"][0]["verification"] = [{
            "check": payload["changes"][0]["acceptance"]["target"][0],
            "command": "test target",
            "status": "passed",
            "result": "passed",
        }]
        write_private_json(path, payload)

        validation = validate_decision_manifest(self.run_dir, path)

        self.assertIn("change 0 proposed change must not have verification", validation["errors"])

    def test_decision_manifest_rejects_symlink_path_escape(self):
        with tempfile.TemporaryDirectory() as outside_dir:
            link = self.root / "linked"
            link.symlink_to(outside_dir, target_is_directory=True)
            path, payload = self.write_valid_decision_manifest()
            payload["changes"][0]["files"] = ["linked/config.json"]
            write_private_json(path, payload)

            validation = validate_decision_manifest(self.run_dir, path)

        self.assertIn("change 0 file is not repository-relative: linked/config.json", validation["errors"])

    def test_old_all_run_accepts_explicit_repository_root(self):
        del self.manifest["repository_root"]
        self.manifest["project_filter"] = None
        self.rewrite_manifest()
        path, payload = self.write_valid_decision_manifest()
        payload["source_manifest_sha256"] = sha256_file(self.run_dir / "manifest.json")
        payload["project_filter"] = None
        write_private_json(path, payload)

        without_override = validate_decision_manifest(self.run_dir, path)
        with_override = validate_decision_manifest(
            self.run_dir,
            path,
            repository_root_override=self.root,
        )

        self.assertIn("run manifest repository root is unavailable", without_override["errors"])
        self.assertEqual(with_override["status"], "passed")

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
