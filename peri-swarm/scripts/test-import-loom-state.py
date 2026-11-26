#!/usr/bin/env python3
"""Fixture-driven tests for `scripts/import_loom_state.py` (no PostgreSQL required).

Run: `python3 peri-swarm/scripts/test-import-loom-state.py`

The same module can emit the legacy SQLite snapshot used by the acceptance
drill (see docs/verification/peri-swarm-postgres-state-2026-11-24.md):

    python3 peri-swarm/scripts/test-import-loom-state.py --write-fixture /tmp/old.db

The snapshot is **test data**: it is written in the legacy Loom schema
(`swarm_meta(version)` + `INTEGER` flags) so the importer's column mapping and
the gateway's on-disk contract are exercised end to end.
"""

from __future__ import annotations

import codecs
import hashlib
import json
import re
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
PERI_SWARM = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))
import import_loom_state as tool  # noqa: E402

FIXTURE_CREATED_AT = "2026-11-01T00:00:00Z"

LEGACY_DDL = """
CREATE TABLE swarm_meta (version INTEGER NOT NULL);
CREATE TABLE swarm_sessions (
    logical_id       TEXT PRIMARY KEY,
    root_session_id  TEXT NOT NULL UNIQUE,
    actor_key        TEXT NOT NULL,
    cwd              TEXT NOT NULL,
    state            TEXT NOT NULL,
    model            TEXT,
    instructions     TEXT,
    mcp_json         TEXT,
    environment_json TEXT NOT NULL,
    metadata_json    TEXT,
    prompt_cache_options_json TEXT,
    created_at       TEXT NOT NULL,
    last_activity_at TEXT NOT NULL,
    last_item_seq    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE swarm_items (
    logical_id   TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    item_id      TEXT NOT NULL,
    kind         TEXT NOT NULL,
    source       TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    truncated    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (logical_id, seq)
);
CREATE TABLE swarm_inputs (
    logical_id   TEXT NOT NULL,
    input_id     TEXT NOT NULL,
    fingerprint  TEXT NOT NULL,
    state        TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (logical_id, input_id)
);
CREATE TABLE swarm_bindings (
    logical_env_id   TEXT PRIMARY KEY,
    logical_id       TEXT NOT NULL,
    sandbox_id       TEXT NOT NULL,
    workspace_path   TEXT NOT NULL,
    sandbox_root     TEXT NOT NULL,
    settings_profile TEXT,
    token_file       TEXT,
    last_run_id      TEXT,
    last_used_at     TEXT NOT NULL
);
CREATE TABLE swarm_turns (
    logical_id   TEXT NOT NULL,
    turn_id      TEXT NOT NULL,
    status       TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    started_at   INTEGER,
    completed_at INTEGER,
    error_json   TEXT,
    usage_json   TEXT,
    subagent_id  TEXT,
    PRIMARY KEY (logical_id, turn_id)
);
"""

ROOT_SOURCE = json.dumps({"agentId": "root", "taskId": None}, separators=(",", ":"))


def fingerprint(text: str) -> str:
    """Mirror `InputSubmission::fingerprint` (serde_json canonical object).

    The workspace enables serde_json's `preserve_order`, so the map keeps the
    `json!` insertion order (`kind`, `mode`, `text`, `attachments`,
    `pluginSelections`, `originalDraft`); the digest is SHA-256 over the UTF-8
    bytes, lowercase hex. Verified against a live gateway row (see the test).
    """
    canonical = (
        '{"kind":"user","mode":"turn",'
        f'"text":{json.dumps(text, ensure_ascii=False)},'
        '"attachments":[],"pluginSelections":[],"originalDraft":null}'
    )
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


OLD_INPUT_ID = "8d6e2c43-3f7a-4c9e-9a15-1f0a2b3c4d5e"
OLD_INPUT_TEXT = "hello loom"


def write_legacy_fixture(path: Path) -> Path:
    """Two legacy sessions: one plain, one hosted with a binding and a turn."""
    if path.exists():
        path.unlink()
    connection = sqlite3.connect(path)
    try:
        connection.executescript(LEGACY_DDL)
        connection.execute("INSERT INTO swarm_meta (version) VALUES (1)")
        connection.executemany(
            "INSERT INTO swarm_sessions (logical_id, root_session_id, actor_key, cwd, state,"
            " model, instructions, mcp_json, environment_json, metadata_json,"
            " prompt_cache_options_json, created_at, last_activity_at, last_item_seq)"
            " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    "agent-old-1",
                    "root-old-1",
                    "actor-old-1",
                    "/workspace/agent-old-1",
                    "idle",
                    "sonnet",
                    None,
                    None,
                    json.dumps({"type": "none"}, separators=(",", ":")),
                    json.dumps({"tenant": "alpha"}, separators=(",", ":")),
                    None,
                    FIXTURE_CREATED_AT,
                    "2026-11-01T00:03:00Z",
                    6,
                ),
                (
                    "agent-old-2",
                    "root-old-2",
                    "actor-old-2",
                    "/workspace/agent-old-2",
                    "stopped",
                    None,
                    "be brief",
                    None,
                    json.dumps(
                        {"type": "openai_hosted", "id": "env-old-2"},
                        separators=(",", ":"),
                    ),
                    None,
                    None,
                    FIXTURE_CREATED_AT,
                    "2026-11-01T00:04:00Z",
                    2,
                ),
            ],
        )
        items = [
            ("agent-old-1", 1, "itim-old-1", "message", {"messageId": "msg-old-1", "text": "hello loom"}),
            ("agent-old-1", 2, "itim-old-1", "message", {"messageId": "msg-old-1", "text": "hel"}),
            ("agent-old-1", 3, "itim-old-1", "message", {"messageId": "msg-old-1", "text": "lo "}),
            ("agent-old-1", 4, "itim-old-1", "message", {"messageId": "msg-old-1", "text": "loom"}),
            (
                "agent-old-1",
                5,
                "itim-old-1",
                "message",
                {"messageId": "msg-old-1", "text": "hello loom", "complete": True},
            ),
            ("agent-old-1", 6, "itim-old-2", "receipt", {"event": "turn.completed"}),
            ("agent-old-2", 1, "itim-old-3", "message", {"messageId": "msg-old-2", "text": "hosted"}),
            ("agent-old-2", 2, "itim-old-3", "message", {"messageId": "msg-old-2", "text": "hosted"}),
        ]
        connection.executemany(
            "INSERT INTO swarm_items (logical_id, seq, item_id, kind, source, payload_json,"
            " created_at, truncated) VALUES (?,?,?,?,?,?,?,?)",
            [
                (
                    logical_id,
                    seq,
                    item_id,
                    kind,
                    ROOT_SOURCE,
                    json.dumps(payload, separators=(",", ":")),
                    FIXTURE_CREATED_AT,
                    0,
                )
                for logical_id, seq, item_id, kind, payload in items
            ],
        )
        connection.execute(
            "INSERT INTO swarm_inputs (logical_id, input_id, fingerprint, state, created_at)"
            " VALUES (?,?,?,?,?)",
            ("agent-old-1", OLD_INPUT_ID, fingerprint(OLD_INPUT_TEXT), "accepted", FIXTURE_CREATED_AT),
        )
        connection.execute(
            "INSERT INTO swarm_bindings (logical_env_id, logical_id, sandbox_id, workspace_path,"
            " sandbox_root, settings_profile, token_file, last_run_id, last_used_at)"
            " VALUES (?,?,?,?,?,?,?,?,?)",
            (
                "env-old-2",
                "agent-old-2",
                "sandbox-old-2",
                "/workspace/agent-old-2",
                "/workspace",
                None,
                None,
                "run-old-1",
                FIXTURE_CREATED_AT,
            ),
        )
        connection.execute(
            "INSERT INTO swarm_turns (logical_id, turn_id, status, created_at, started_at,"
            " completed_at, error_json, usage_json, subagent_id) VALUES (?,?,?,?,?,?,?,?,?)",
            (
                "agent-old-1",
                "turn-old-1",
                "completed",
                1_761_955_200,
                1_761_955_201,
                1_761_955_205,
                None,
                json.dumps({"inputTokens": 12, "outputTokens": 7}, separators=(",", ":")),
                None,
            ),
        )
        connection.commit()
    finally:
        connection.close()
    return path


class FixtureCase(unittest.TestCase):
    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.addCleanup(self._directory.cleanup)
        self.directory = Path(self._directory.name)
        self.snapshot = write_legacy_fixture(self.directory / "old-loom.db")

    def copy_snapshot(self, name: str) -> Path:
        target = self.directory / name
        target.write_bytes(self.snapshot.read_bytes())
        return target

    def execute(self, path: Path, sql: str) -> None:
        connection = sqlite3.connect(path)
        try:
            connection.executescript(sql)
            connection.commit()
        finally:
            connection.close()


class FixtureTests(FixtureCase):
    def test_fixture_is_importable_unchanged(self) -> None:
        snapshot = tool.load_snapshot(self.snapshot, immutable=False, strict=True)
        checks = tool.expected_checks(snapshot)
        self.assertEqual(checks["sessions"], "2")
        self.assertEqual(checks["items"], "8")
        self.assertEqual(checks["inputs"], "1")
        self.assertEqual(checks["bindings"], "1")
        self.assertEqual(checks["turns"], "1")
        self.assertEqual(checks["last_item_seq_mismatch"], "0")
        self.assertEqual(checks["seq_starts_at_one"], "0")
        self.assertEqual(checks["seq_gap_sessions"], "0")
        self.assertEqual(checks["duplicate_inputs"], "0")
        self.assertEqual(snapshot.repairs, [])
        self.assertEqual(checks["meta_engine"], "postgres")
        self.assertEqual(checks["meta_contract"], "peri.swarm.postgres/v1")

    def test_fingerprint_mirrors_the_serde_canonical_object(self) -> None:
        # `42b956...` was read back from a live gateway (`swarm_inputs.fingerprint`
        # after POST /events {"text":"hello loom"}). If the field order or the
        # null/empty rendering drifts, the gateway answers 409 instead of 202 on
        # a replayed legacy input.
        self.assertEqual(
            fingerprint("hello loom"),
            "42b9566591c24388f93a43988e2bad6b5877b625f1d432d7f92694dc63e7611f",
        )

    def test_export_writes_schema_data_verify_and_expected(self) -> None:
        out = self.directory / "out"
        self.assertEqual(
            tool.main(
                [
                    "export",
                    "--snapshot",
                    str(self.snapshot),
                    "--out-dir",
                    str(out),
                    "--created-at",
                    FIXTURE_CREATED_AT,
                ]
            ),
            0,
        )
        schema = (out / "01-schema.sql").read_text(encoding="utf-8")
        data = (out / "02-data.sql").read_text(encoding="utf-8")
        verify = (out / "03-verify.sql").read_text(encoding="utf-8")
        expected = json.loads((out / "expected.json").read_text(encoding="utf-8"))

        self.assertIn("CREATE TABLE IF NOT EXISTS swarm_sessions", schema)
        self.assertIn("last_item_seq    BIGINT NOT NULL DEFAULT 0", schema)
        self.assertIn("truncated    BOOLEAN NOT NULL DEFAULT FALSE", schema)
        self.assertIn(
            "(0, 1, 'postgres', 'peri.swarm.postgres/v1', '2026-11-01T00:00:00Z')",
            schema,
        )
        self.assertIn("SET standard_conforming_strings = on;", schema)
        self.assertIn("BEGIN;", schema)
        self.assertIn("COMMIT;", schema)

        self.assertIn("INSERT INTO swarm_sessions", data)
        self.assertIn("'agent-old-1', 'root-old-1'", data)
        self.assertIn("FALSE", data)  # legacy truncated=0 becomes BOOLEAN FALSE
        self.assertIn("INSERT INTO swarm_items", data)
        self.assertIn("invalid_payload_json", verify)
        self.assertIn("seq_gap_sessions", verify)
        self.assertIn("duplicate_inputs", verify)
        self.assertEqual(expected["manifest"]["source_sha256"], hashlib.sha256(
            self.snapshot.read_bytes()
        ).hexdigest())
        self.assertEqual(expected["checks"]["items"], "8")
        self.assertTrue(expected["manifest"]["includes_ddl"])

    def test_no_ddl_skips_the_schema_file(self) -> None:
        out = self.directory / "out"
        tool.main(
            [
                "export",
                "--snapshot",
                str(self.snapshot),
                "--out-dir",
                str(out),
                "--no-ddl",
            ]
        )
        self.assertFalse((out / "01-schema.sql").exists())
        manifest = json.loads((out / "expected.json").read_text(encoding="utf-8"))["manifest"]
        self.assertFalse(manifest["includes_ddl"])

    def test_check_accepts_its_own_expected_output(self) -> None:
        out = self.directory / "out"
        tool.main(["export", "--snapshot", str(self.snapshot), "--out-dir", str(out)])
        expected = json.loads((out / "expected.json").read_text(encoding="utf-8"))["checks"]
        observed = out / "observed.txt"
        observed.write_text(
            "".join(f"{name}={value}\n" for name, value in expected.items()),
            encoding="utf-8",
        )
        self.assertEqual(
            tool.main(
                ["check", "--expected", str(out / "expected.json"), "--observed", str(observed)]
            ),
            0,
        )

    def test_check_reports_a_mismatch(self) -> None:
        out = self.directory / "out"
        tool.main(["export", "--snapshot", str(self.snapshot), "--out-dir", str(out)])
        expected = json.loads((out / "expected.json").read_text(encoding="utf-8"))["checks"]
        expected["items"] = "7"
        observed = out / "observed.txt"
        observed.write_text(
            "".join(f"{name}={value}\n" for name, value in expected.items()),
            encoding="utf-8",
        )
        self.assertEqual(
            tool.main(
                ["check", "--expected", str(out / "expected.json"), "--observed", str(observed)]
            ),
            1,
        )

    def test_stale_last_item_seq_is_repaired_or_refused(self) -> None:
        snapshot = self.copy_snapshot("stale.db")
        self.execute(snapshot, "UPDATE swarm_sessions SET last_item_seq = 99 WHERE logical_id = 'agent-old-1';")
        repaired = tool.load_snapshot(snapshot, immutable=False, strict=False)
        self.assertEqual(
            repaired.table("swarm_sessions")[0]["last_item_seq"], 6
        )
        self.assertTrue(repaired.repairs)

    def test_strict_refuses_a_stale_last_item_seq(self) -> None:
        snapshot = self.copy_snapshot("strict.db")
        self.execute(snapshot, "UPDATE swarm_sessions SET last_item_seq = 99 WHERE logical_id = 'agent-old-1';")
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=True)

    def test_orphan_rows_are_dropped_or_refused(self) -> None:
        snapshot = self.copy_snapshot("orphan.db")
        self.execute(
            snapshot,
            "INSERT INTO swarm_items (logical_id, seq, item_id, kind, source, payload_json,"
            " created_at, truncated) VALUES"
            " ('ghost', 1, 'itim-ghost', 'message', '{}', '{}', '2026-11-01T00:00:00Z', 0);",
        )
        merged = tool.load_snapshot(snapshot, immutable=False, strict=False)
        self.assertEqual(len(merged.table("swarm_items")), 8)
        self.assertTrue(any("swarm_items" in repair for repair in merged.repairs))
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=True)


class RefusalTests(FixtureCase):
    def test_unknown_enum_text_is_refused(self) -> None:
        snapshot = self.copy_snapshot("enum.db")
        self.execute(snapshot, "UPDATE swarm_sessions SET state = 'zombie' WHERE logical_id = 'agent-old-1';")
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_invalid_json_is_refused(self) -> None:
        snapshot = self.copy_snapshot("json.db")
        self.execute(snapshot, "UPDATE swarm_items SET payload_json = 'not json' WHERE seq = 1;")
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_truncated_outside_zero_one_is_refused(self) -> None:
        snapshot = self.copy_snapshot("bool.db")
        self.execute(snapshot, "UPDATE swarm_items SET truncated = 2 WHERE seq = 1;")
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_non_uuid_input_id_is_refused(self) -> None:
        snapshot = self.copy_snapshot("input-id.db")
        self.execute(
            snapshot,
            "UPDATE swarm_inputs SET input_id = 'old-input-1';",
        )
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_invalid_utf8_is_refused(self) -> None:
        snapshot = self.copy_snapshot("utf8.db")
        self.execute(snapshot, "UPDATE swarm_items SET payload_json = CAST(x'ff' AS TEXT) WHERE seq = 1;")
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_duplicate_input_receipt_is_refused(self) -> None:
        snapshot = self.copy_snapshot("dup.db")
        self.execute(
            snapshot,
            "CREATE TABLE inputs_no_pk (logical_id TEXT, input_id TEXT, fingerprint TEXT,"
            " state TEXT, created_at TEXT);"
            " INSERT INTO inputs_no_pk SELECT * FROM swarm_inputs;"
            " INSERT INTO inputs_no_pk SELECT * FROM swarm_inputs;"
            " DROP TABLE swarm_inputs;"
            " ALTER TABLE inputs_no_pk RENAME TO swarm_inputs;",
        )
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_unknown_extra_column_is_refused(self) -> None:
        snapshot = self.copy_snapshot("extra.db")
        self.execute(snapshot, "ALTER TABLE swarm_sessions ADD COLUMN future_flag TEXT;")
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_missing_required_column_is_refused(self) -> None:
        snapshot = self.copy_snapshot("missing.db")
        self.execute(
            snapshot,
            "ALTER TABLE swarm_sessions DROP COLUMN last_activity_at;",
        )
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_absent_legacy_optional_columns_become_null(self) -> None:
        snapshot = self.copy_snapshot("optional.db")
        self.execute(
            snapshot,
            "CREATE TABLE sessions_old (logical_id TEXT PRIMARY KEY, root_session_id TEXT NOT NULL UNIQUE,"
            " actor_key TEXT NOT NULL, cwd TEXT NOT NULL, state TEXT NOT NULL, model TEXT,"
            " instructions TEXT, mcp_json TEXT, environment_json TEXT NOT NULL,"
            " created_at TEXT NOT NULL, last_activity_at TEXT NOT NULL,"
            " last_item_seq INTEGER NOT NULL DEFAULT 0);"
            " INSERT INTO sessions_old (logical_id, root_session_id, actor_key, cwd, state, model,"
            " instructions, mcp_json, environment_json, created_at, last_activity_at, last_item_seq)"
            " SELECT logical_id, root_session_id, actor_key, cwd, state, model, instructions,"
            " mcp_json, environment_json, created_at, last_activity_at, last_item_seq FROM swarm_sessions;"
            " DROP TABLE swarm_sessions;"
            " ALTER TABLE sessions_old RENAME TO swarm_sessions;",
        )
        loaded = tool.load_snapshot(snapshot, immutable=False, strict=False)
        self.assertIsNone(loaded.table("swarm_sessions")[0]["metadata_json"])
        self.assertIsNone(loaded.table("swarm_sessions")[0]["prompt_cache_options_json"])

    def test_schema_version_other_than_one_is_refused(self) -> None:
        snapshot = self.copy_snapshot("version.db")
        self.execute(snapshot, "UPDATE swarm_meta SET version = 2;")
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)

    def test_a_foreign_database_is_refused(self) -> None:
        snapshot = self.directory / "foreign.db"
        connection = sqlite3.connect(snapshot)
        try:
            connection.execute("CREATE TABLE foreign_table (id INTEGER)")
            connection.commit()
        finally:
            connection.close()
        with self.assertRaises(tool.ImportError):
            tool.load_snapshot(snapshot, immutable=False, strict=False)


class DriftTests(unittest.TestCase):
    """The bundled DDL must stay the build's DDL (`schema.rs`)."""

    def rust_literals(self, assigment: str) -> list[str]:
        source = (PERI_SWARM / "src/store/postgres/schema.rs").read_text(encoding="utf-8")
        start = source.index(assigment)
        bracket = source.index("= [", start) + 2
        end = source.index("];", bracket)
        block = source[bracket + 1 : end]
        literals = re.findall(r'"((?:[^"\\]|\\.)*)"', block, re.DOTALL)
        self.assertTrue(literals, f"no literals found after {assigment}")
        return [codecs.decode(literal, "unicode_escape") for literal in literals]

    def test_create_table_statements_match_schema_rs(self) -> None:
        rust = self.rust_literals("const CREATE_TABLE_SQL: [&str; 5]")
        self.assertEqual(len(rust), 5)
        self.assertEqual(len(tool.CREATE_TABLE_SQL), 5)
        for expected, actual in zip(rust, tool.CREATE_TABLE_SQL):
            self.assertEqual(
                " ".join(expected.split()), " ".join(actual.split())
            )

    def test_index_statements_match_schema_rs(self) -> None:
        rust = self.rust_literals("const CREATE_INDEX_SQL: [&str; 2]")
        self.assertEqual(
            [" ".join(statement.split()) for statement in rust],
            [" ".join(statement.split()) for statement in tool.CREATE_INDEX_SQL],
        )

    def test_meta_table_matches_schema_rs(self) -> None:
        source = (PERI_SWARM / "src/store/postgres/schema.rs").read_text(encoding="utf-8")
        start = source.index("const CREATE_META_TABLE_SQL: &str =")
        block = source[start : source.index(";", source.index('"', start) + 1)]
        literal = re.search(r'"((?:[^"\\]|\\.)*)"', block, re.DOTALL)
        self.assertIsNotNone(literal)
        expected = codecs.decode(literal.group(1), "unicode_escape")
        self.assertEqual(" ".join(expected.split()), " ".join(tool.CREATE_META_TABLE_SQL.split()))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--write-fixture":
        target = Path(sys.argv[2])
        write_legacy_fixture(target)
        print(f"wrote legacy fixture: {target}")
        sys.exit(0)
    unittest.main()
