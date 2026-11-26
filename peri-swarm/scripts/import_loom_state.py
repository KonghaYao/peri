#!/usr/bin/env python3
"""One-off offline import of a legacy Loom/libSQL Swarm state database into PostgreSQL.

The gateway no longer carries a Loom/Turso client, so the cutover is an offline,
operator-driven job: take a **read-only snapshot** of the old Swarm state
database, translate it here, import it into an **empty, dedicated** PostgreSQL
database, and reconcile the result before the new build starts.

This tool is Python-3 standard library only. It never runs the gateway and never
adds a runtime dependency to it.

## Supported snapshot

The snapshot must be an SQLite-format file whose schema is the one the Loom
build wrote (`swarm_meta` + the five `swarm_*` tables). Accepted ways to obtain
it, all read-only on the source:

1. the storage provider's physical backup/export of the logical database
   (a libSQL database file, optionally with its `-wal`/`-shm` siblings), or
2. `sqlite3 old.db '.dump' > dump.sql` against a restored copy, then
   `sqlite3 snapshot.db < dump.sql` to materialize a plain SQLite file.

A hot copy without its `-wal` file may miss the newest commits: stop writes
first (see README「从 Loom 割接」). Use a frozen copy and keep its WAL beside it.
Use `--immutable` only for a checkpointed snapshot with no non-empty WAL.
The manifest records the main file and non-empty WAL checksums separately.

## Cutover sequence

    1. stop the old gateway, keep the old Loom database read-only
    2. take the snapshot and checksum it (the manifest records sha256)
    3. create a dedicated empty PostgreSQL database (never the session database)
    4. python3 import_loom_state.py inspect --snapshot snapshot.db
    5. python3 import_loom_state.py export --snapshot snapshot.db --out-dir out/
    6. psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -f out/01-schema.sql
       psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -f out/02-data.sql
       psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -qAt -f out/03-verify.sql > out/observed.txt
    7. python3 import_loom_state.py check --expected out/expected.json --observed out/observed.txt
    8. start the new build; keep the old Loom database read-only for comparison

`01-schema.sql` re-runs the gateway's own DDL (mirrored from
`src/store/postgres/schema.rs`) and pins the `swarm_meta` identity row, so the
target needs no prior gateway start. A schema that already has any relation is
refused. With `--no-ddl`, start the new build once against the empty database,
stop it, then import only the rows.

The tool repairs three legacy crash artefacts (reported in the manifest and on
stdout): a stale `swarm_sessions.last_item_seq` (set to `MAX(seq)`), orphan
items/inputs/turns, and bindings that point at a session that no longer exists.
`--strict` turns any repair into an error instead.

Refused, because the new build cannot read them: unknown enum text, invalid
JSON in a JSON column, non-integer `seq`/turn timestamp, a `truncated` value
outside `0`/`1`, NUL bytes or invalid UTF-8 in TEXT, duplicate primary keys,
and a `swarm_meta.version` other than `1`.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import sqlite3
import sys
import urllib.parse
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable, Sequence

SCHEMA_VERSION = 1
ENGINE = "postgres"
CONTRACT = "peri.swarm.postgres/v1"

TEXT = "text"
INT = "int"
BOOL = "bool"

# ── legacy → target shape ───────────────────────────────────────────────────
#
# The target's column lists; the legacy schema used the same names (see
# `git show 9332fae19:peri-swarm/src/store/turso.rs`). Two additive legacy
# columns may be absent from older snapshots and are imported as NULL.

SESSIONS_COLUMNS: tuple[tuple[str, str], ...] = (
    ("logical_id", TEXT),
    ("root_session_id", TEXT),
    ("actor_key", TEXT),
    ("cwd", TEXT),
    ("state", TEXT),
    ("model", TEXT),
    ("instructions", TEXT),
    ("mcp_json", TEXT),
    ("environment_json", TEXT),
    ("metadata_json", TEXT),
    ("prompt_cache_options_json", TEXT),
    ("created_at", TEXT),
    ("last_activity_at", TEXT),
    ("last_item_seq", INT),
)
ITEMS_COLUMNS: tuple[tuple[str, str], ...] = (
    ("logical_id", TEXT),
    ("seq", INT),
    ("item_id", TEXT),
    ("kind", TEXT),
    ("source", TEXT),
    ("payload_json", TEXT),
    ("created_at", TEXT),
    ("truncated", BOOL),
)
INPUTS_COLUMNS: tuple[tuple[str, str], ...] = (
    ("logical_id", TEXT),
    ("input_id", TEXT),
    ("fingerprint", TEXT),
    ("state", TEXT),
    ("created_at", TEXT),
)
BINDINGS_COLUMNS: tuple[tuple[str, str], ...] = (
    ("logical_env_id", TEXT),
    ("logical_id", TEXT),
    ("sandbox_id", TEXT),
    ("workspace_path", TEXT),
    ("sandbox_root", TEXT),
    ("settings_profile", TEXT),
    ("token_file", TEXT),
    ("last_run_id", TEXT),
    ("last_used_at", TEXT),
)
TURNS_COLUMNS: tuple[tuple[str, str], ...] = (
    ("logical_id", TEXT),
    ("turn_id", TEXT),
    ("status", TEXT),
    ("created_at", INT),
    ("started_at", INT),
    ("completed_at", INT),
    ("error_json", TEXT),
    ("usage_json", TEXT),
    ("subagent_id", TEXT),
)

# Columns that older legacy builds added with `ALTER TABLE ... ADD COLUMN` and
# may legitimately be missing from a snapshot.
LEGACY_OPTIONAL_COLUMNS: dict[str, frozenset[str]] = {
    "swarm_sessions": frozenset({"metadata_json", "prompt_cache_options_json"}),
}

NULLABLE_COLUMNS: dict[str, frozenset[str]] = {
    "swarm_sessions": frozenset(
        {"model", "instructions", "mcp_json", "metadata_json", "prompt_cache_options_json"}
    ),
    "swarm_items": frozenset(),
    "swarm_inputs": frozenset(),
    "swarm_bindings": frozenset({"settings_profile", "token_file", "last_run_id"}),
    "swarm_turns": frozenset(
        {"started_at", "completed_at", "error_json", "usage_json", "subagent_id"}
    ),
}

# Insert order: parents before rows that reference them.
TABLES: tuple[tuple[str, tuple[tuple[str, str], ...], tuple[str, ...]], ...] = (
    ("swarm_sessions", SESSIONS_COLUMNS, ("logical_id",)),
    ("swarm_items", ITEMS_COLUMNS, ("logical_id", "seq")),
    ("swarm_inputs", INPUTS_COLUMNS, ("logical_id", "input_id")),
    ("swarm_bindings", BINDINGS_COLUMNS, ("logical_env_id",)),
    ("swarm_turns", TURNS_COLUMNS, ("logical_id", "turn_id")),
)

JSON_COLUMNS: dict[str, tuple[str, ...]] = {
    "swarm_sessions": (
        "mcp_json",
        "environment_json",
        "metadata_json",
        "prompt_cache_options_json",
    ),
    "swarm_items": ("source", "payload_json"),
    "swarm_turns": ("error_json", "usage_json"),
}

# `03-verify.sql` check name for each enum column (the tables share `state`).
ENUM_CHECK_NAMES: dict[str, str] = {
    "swarm_sessions": "session_state",
    "swarm_items": "item_kind",
    "swarm_inputs": "input_state",
    "swarm_turns": "turn_status",
}

ENUM_COLUMNS: dict[str, tuple[tuple[str, frozenset[str]], ...]] = {
    "swarm_sessions": (
        (
            "state",
            frozenset({"creating", "idle", "running", "recovering", "stopped", "failed"}),
        ),
    ),
    "swarm_items": (
        (
            "kind",
            frozenset(
                {
                    "message",
                    "thought",
                    "tool_call",
                    "tool_update",
                    "tool_result",
                    "receipt",
                    "approval",
                }
            ),
        ),
    ),
    "swarm_inputs": (("state", frozenset({"accepted", "handed_off", "failed"})),),
    "swarm_turns": (
        (
            "status",
            frozenset(
                {"queued", "in_progress", "waiting", "completed", "failed", "cancelled"}
            ),
        ),
    ),
}

# The gateway's own DDL, mirrored (whitespace-normalized) from
# `src/store/postgres/schema.rs`. `schema_test.rs` compares the two so this tool
# cannot drift from the build that will open the imported database.
CREATE_META_TABLE_SQL = """CREATE TABLE IF NOT EXISTS swarm_meta (
    singleton  SMALLINT PRIMARY KEY CHECK (singleton = 0),
    version    BIGINT NOT NULL,
    engine     TEXT   NOT NULL,
    contract   TEXT   NOT NULL,
    created_at TEXT   NOT NULL
)"""

CREATE_TABLE_SQL: tuple[str, ...] = (
    """CREATE TABLE IF NOT EXISTS swarm_sessions (
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
        last_item_seq    BIGINT NOT NULL DEFAULT 0
    )""",
    """CREATE TABLE IF NOT EXISTS swarm_items (
        logical_id   TEXT NOT NULL,
        seq          BIGINT NOT NULL,
        item_id      TEXT NOT NULL,
        kind         TEXT NOT NULL,
        source       TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        truncated    BOOLEAN NOT NULL DEFAULT FALSE,
        PRIMARY KEY (logical_id, seq)
    )""",
    """CREATE TABLE IF NOT EXISTS swarm_inputs (
        logical_id   TEXT NOT NULL,
        input_id     TEXT NOT NULL,
        fingerprint  TEXT NOT NULL,
        state        TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        PRIMARY KEY (logical_id, input_id)
    )""",
    """CREATE TABLE IF NOT EXISTS swarm_bindings (
        logical_env_id   TEXT PRIMARY KEY,
        logical_id       TEXT NOT NULL,
        sandbox_id       TEXT NOT NULL,
        workspace_path   TEXT NOT NULL,
        sandbox_root     TEXT NOT NULL,
        settings_profile TEXT,
        token_file       TEXT,
        last_run_id      TEXT,
        last_used_at     TEXT NOT NULL
    )""",
    """CREATE TABLE IF NOT EXISTS swarm_turns (
        logical_id   TEXT NOT NULL,
        turn_id      TEXT NOT NULL,
        status       TEXT NOT NULL,
        created_at   BIGINT NOT NULL,
        started_at   BIGINT,
        completed_at BIGINT,
        error_json   TEXT,
        usage_json   TEXT,
        subagent_id  TEXT,
        PRIMARY KEY (logical_id, turn_id)
    )""",
)

CREATE_INDEX_SQL: tuple[str, ...] = (
    "CREATE INDEX IF NOT EXISTS swarm_items_by_id ON swarm_items (logical_id, item_id)",
    "CREATE INDEX IF NOT EXISTS swarm_turns_by_created "
    "ON swarm_turns (logical_id, created_at, turn_id)",
)

# A relation-free schema only: the same rule the gateway applies before it pins
# its identity row.
EMPTY_SCHEMA_GUARD_SQL = """DO $$
DECLARE n integer;
BEGIN
  SELECT count(*) INTO n
    FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace ns ON ns.oid = c.relnamespace
   WHERE ns.nspname = current_schema()
     AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f');
  IF n <> 0 THEN
    RAISE EXCEPTION 'refusing to import: schema % already has % relation(s); import into a dedicated empty database', current_schema(), n;
  END IF;
END $$"""

EMPTY_TABLES_GUARD_SQL = f"""DO $$
DECLARE n bigint;
BEGIN
  IF (SELECT count(*) FROM swarm_meta) <> 1 THEN
    RAISE EXCEPTION 'swarm_meta must carry exactly one identity row';
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM swarm_meta
     WHERE version = {SCHEMA_VERSION} AND engine = '{ENGINE}' AND contract = '{CONTRACT}'
  ) THEN
    RAISE EXCEPTION 'swarm_meta is not the expected identity ({SCHEMA_VERSION}, {ENGINE}, {CONTRACT})';
  END IF;
  SELECT count(*) INTO n FROM swarm_sessions;
  IF n <> 0 THEN
    RAISE EXCEPTION 'swarm_sessions is not empty (% row(s)); import needs an empty state database', n;
  END IF;
  SELECT count(*) INTO n FROM swarm_items;
  IF n <> 0 THEN
    RAISE EXCEPTION 'swarm_items is not empty (% row(s)); import needs an empty state database', n;
  END IF;
  SELECT count(*) INTO n FROM swarm_inputs;
  IF n <> 0 THEN
    RAISE EXCEPTION 'swarm_inputs is not empty (% row(s)); import needs an empty state database', n;
  END IF;
  SELECT count(*) INTO n FROM swarm_bindings;
  IF n <> 0 THEN
    RAISE EXCEPTION 'swarm_bindings is not empty (% row(s)); import needs an empty state database', n;
  END IF;
  SELECT count(*) INTO n FROM swarm_turns;
  IF n <> 0 THEN
    RAISE EXCEPTION 'swarm_turns is not empty (% row(s)); import needs an empty state database', n;
  END IF;
END $$"""

CHUNK_ROWS = 200
SESSION_STATES = ENUM_COLUMNS["swarm_sessions"][0][1]
ITEM_KINDS = ENUM_COLUMNS["swarm_items"][0][1]
INPUT_STATES = ENUM_COLUMNS["swarm_inputs"][0][1]
TURN_STATUSES = ENUM_COLUMNS["swarm_turns"][0][1]


class ImportError(Exception):
    """The snapshot cannot be imported without losing or corrupting state."""


@dataclass
class Snapshot:
    path: Path
    sha256: str
    wal_sha256: str | None
    version: int
    rows: dict[str, list[dict[str, Any]]]
    present_columns: dict[str, list[str]]
    repairs: list[str] = field(default_factory=list)

    def source_description(self) -> str:
        wal = f", WAL sha256 {self.wal_sha256}" if self.wal_sha256 else ""
        return f"{self.path} (sha256 {self.sha256}{wal})"

    def table(self, name: str) -> list[dict[str, Any]]:
        return self.rows[name]


# ── snapshot access ─────────────────────────────────────────────────────────


def open_snapshot(path: Path, immutable: bool) -> sqlite3.Connection:
    if not path.is_file():
        raise ImportError(f"snapshot does not exist or is not a file: {path}")
    uri = "file:" + urllib.parse.quote(path.resolve().as_posix()) + "?mode=ro"
    if immutable:
        uri += "&immutable=1"
    try:
        connection = sqlite3.connect(uri, uri=True)
    except sqlite3.Error as error:
        raise ImportError(f"cannot open the snapshot read-only: {error}") from error
    connection.text_factory = bytes
    connection.row_factory = sqlite3.Row
    return connection


def file_stamp(path: Path) -> tuple[int, int, int, int, int] | None:
    """Treat a missing or empty WAL identically; SQLite can create an empty sidecar on open."""
    try:
        stat = path.stat()
    except FileNotFoundError:
        return None
    except OSError as error:
        raise ImportError(f"cannot inspect snapshot file {path}: {error}") from error
    if stat.st_size == 0:
        return None
    return stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def decode_name(raw: Any) -> str:
    if isinstance(raw, bytes):
        return raw.decode("utf-8", "strict")
    return str(raw)


def existing_tables(connection: sqlite3.Connection) -> set[str]:
    rows = connection.execute(
        "SELECT name FROM sqlite_master WHERE type = 'table'"
    ).fetchall()
    return {decode_name(row[0]) for row in rows}


def table_columns(connection: sqlite3.Connection, table: str) -> list[str]:
    rows = connection.execute(f"PRAGMA table_info({table})").fetchall()
    return [decode_name(row["name"]) for row in rows]


def read_meta_version(connection: sqlite3.Connection) -> int:
    rows = connection.execute("SELECT version FROM swarm_meta").fetchall()
    if len(rows) != 1:
        raise ImportError(
            f"swarm_meta must carry exactly one row, found {len(rows)}; "
            "this is not a complete legacy state database"
        )
    version = rows[0][0]
    if isinstance(version, bool) or not isinstance(version, int):
        raise ImportError(f"swarm_meta.version must be an integer, found {version!r}")
    if version != SCHEMA_VERSION:
        raise ImportError(
            f"swarm_meta.version is {version}; this tool only imports version "
            f"{SCHEMA_VERSION} snapshots"
        )
    return version


def decode_text(value: Any, table: str, column: str, where: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, bytes):
        raise ImportError(
            f"{table}.{column} at {where} is not TEXT "
            f"(sqlite type {type(value).__name__})"
        )
    try:
        text = value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ImportError(
            f"{table}.{column} at {where} is not valid UTF-8 ({error}); "
            "PostgreSQL TEXT cannot store these bytes"
        ) from error
    if "\x00" in text:
        raise ImportError(f"{table}.{column} at {where} contains a NUL byte")
    return text


def decode_int(value: Any, table: str, column: str, where: str) -> int | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise ImportError(
            f"{table}.{column} at {where} must be an integer "
            f"(sqlite type {type(value).__name__})"
        )
    return value


def decode_bool(value: Any, table: str, column: str, where: str) -> bool | None:
    number = decode_int(value, table, column, where)
    if number is None:
        return None
    if number not in (0, 1):
        raise ImportError(f"{table}.{column} at {where} must be 0/1, found {number}")
    return bool(number)


DECODERS: dict[str, Callable[[Any, str, str, str], Any]] = {
    TEXT: decode_text,
    INT: decode_int,
    BOOL: decode_bool,
}


def row_label(row: dict[str, Any], keys: Sequence[str]) -> str:
    return "(" + ", ".join(f"{key}={row.get(key)!r}" for key in keys) + ")"


def validate_json(table: str, column: str, value: str | None, where: str) -> None:
    if value is None:
        return
    try:
        json.loads(value)
    except json.JSONDecodeError as error:
        raise ImportError(
            f"{table}.{column} at {where} is not valid JSON ({error}); "
            "the gateway decodes this column with serde"
        ) from error


def load_table(
    connection: sqlite3.Connection,
    table: str,
    columns: Sequence[tuple[str, str]],
    keys: Sequence[str],
) -> list[dict[str, Any]]:
    present = table_columns(connection, table)
    expected = [name for name, _ in columns]
    optional = LEGACY_OPTIONAL_COLUMNS.get(table, frozenset())
    missing = [name for name in expected if name not in present and name not in optional]
    if missing:
        raise ImportError(
            f"{table} is missing required column(s) {missing}; "
            "this snapshot does not match the legacy Swarm schema"
        )
    extra = [name for name in present if name not in expected]
    if extra:
        raise ImportError(
            f"{table} has column(s) this tool does not know: {extra}; "
            "dropping them would lose state. Remove them from the snapshot copy, "
            "or extend the tool deliberately"
        )

    selected = ", ".join(name for name in expected if name in present)
    order = ", ".join(keys)
    raw_rows = connection.execute(
        f"SELECT {selected} FROM {table} ORDER BY {order}"
    ).fetchall()

    rows: list[dict[str, Any]] = []
    for index, raw_row in enumerate(raw_rows):
        row: dict[str, Any] = {}
        where = f"row {index} ({table} order {order})"
        for name, kind in columns:
            if name not in present:
                row[name] = None  # additive legacy column absent from this build
                continue
            row[name] = DECODERS[kind](raw_row[name], table, name, where)
            if (
                row[name] is None
                and name not in NULLABLE_COLUMNS.get(table, frozenset())
            ):
                raise ImportError(
                    f"{table}.{name} at {where} is NULL but the target column is NOT NULL"
                )
        label = row_label(row, keys)
        for name in JSON_COLUMNS.get(table, ()):
            validate_json(table, name, row[name], label)
        for name, allowed in ENUM_COLUMNS.get(table, ()):
            if row[name] not in allowed:
                raise ImportError(
                    f"{table}.{name} at {label} is {row[name]!r}; "
                    f"the gateway only reads {sorted(allowed)}"
                )
        if table == "swarm_items" and row["seq"] < 1:
            raise ImportError(f"{table}.seq at {label} must be >= 1")
        if table == "swarm_inputs":
            try:
                uuid.UUID(row["input_id"])
            except ValueError as error:
                raise ImportError(
                    f"{table}.input_id at {label} is not a UUID; the gateway only "
                    "accepts UUID idempotency keys, so this receipt could never be replayed"
                ) from error
        if table == "swarm_sessions" and row["last_item_seq"] < 0:
            raise ImportError(f"{table}.last_item_seq at {label} must be >= 0")
        rows.append(row)
    return rows


def check_duplicates(
    table: str, rows: Sequence[dict[str, Any]], keys: Sequence[str]
) -> None:
    seen: set[tuple[Any, ...]] = set()
    for row in rows:
        key = tuple(row[name] for name in keys)
        if key in seen:
            raise ImportError(
                f"{table} has a duplicate primary key {key}; refusing to import"
            )
        seen.add(key)


def repair_rows(
    rows: dict[str, list[dict[str, Any]]], strict: bool
) -> list[str]:
    """Fix crash artefacts the legacy non-transactional statements could leave."""
    repairs: list[str] = []
    sessions = rows["swarm_sessions"]
    session_ids = {row["logical_id"] for row in sessions}

    max_seq: dict[str, int] = {}
    for item in rows["swarm_items"]:
        max_seq[item["logical_id"]] = max(
            max_seq.get(item["logical_id"], 0), item["seq"]
        )

    for row in sessions:
        observed = max_seq.get(row["logical_id"], 0)
        if row["last_item_seq"] != observed:
            message = (
                f"swarm_sessions.last_item_seq for {row['logical_id']!r} is "
                f"{row['last_item_seq']}, MAX(seq) is {observed}"
            )
            if strict:
                raise ImportError(message + " (--strict)")
            repairs.append(message + " -> set to MAX(seq)")
            row["last_item_seq"] = observed

    for table in ("swarm_items", "swarm_inputs", "swarm_turns", "swarm_bindings"):
        kept = [row for row in rows[table] if row["logical_id"] in session_ids]
        dropped = len(rows[table]) - len(kept)
        if dropped:
            message = (
                f"{table}: {dropped} row(s) reference a logical_id without a session "
                "(a legacy non-transactional delete)"
            )
            if strict:
                raise ImportError(message + " (--strict)")
            repairs.append(message + " -> dropped")
            rows[table] = kept
    return repairs


def load_snapshot(path: Path, immutable: bool, strict: bool) -> Snapshot:
    main_path = path.resolve()
    wal_path = Path(str(main_path) + "-wal")
    initial = (file_stamp(main_path), file_stamp(wal_path))
    if immutable and initial[1] is not None:
        raise ImportError(
            "--immutable ignores a non-empty WAL; omit --immutable or use a checkpointed copy"
        )
    connection = open_snapshot(main_path, immutable)
    try:
        connection.execute("BEGIN")  # read every table from the same SQLite snapshot
        tables = existing_tables(connection)
        missing = [
            name
            for name in ("swarm_meta", *(table for table, _, _ in TABLES))
            if name not in tables
        ]
        if missing:
            raise ImportError(
                "snapshot is not a legacy Swarm state database: "
                f"found tables {sorted(tables)}, missing {missing}"
            )
        version = read_meta_version(connection)
        rows: dict[str, list[dict[str, Any]]] = {}
        present: dict[str, list[str]] = {}
        for table, columns, keys in TABLES:
            rows[table] = load_table(connection, table, columns, keys)
            check_duplicates(table, rows[table], keys)
            present[table] = table_columns(connection, table)
        main_sha256 = sha256_file(main_path)
        wal_sha256 = sha256_file(wal_path) if initial[1] is not None else None
        if initial != (file_stamp(main_path), file_stamp(wal_path)):
            raise ImportError("snapshot files changed during the read; export from a frozen copy")
    except OSError as error:
        raise ImportError(f"cannot read snapshot files from the frozen copy: {error}") from error
    finally:
        connection.close()

    repairs = repair_rows(rows, strict)
    return Snapshot(
        path=path,
        sha256=main_sha256,
        wal_sha256=wal_sha256,
        version=version,
        rows=rows,
        present_columns=present,
        repairs=repairs,
    )


# ── expected checks ────────────────────────────────────────────────────────


def expected_checks(snapshot: Snapshot) -> dict[str, str]:
    sessions = snapshot.table("swarm_sessions")
    items = snapshot.table("swarm_items")
    inputs = snapshot.table("swarm_inputs")
    bindings = snapshot.table("swarm_bindings")
    turns = snapshot.table("swarm_turns")
    session_ids = {row["logical_id"] for row in sessions}

    seqs: dict[str, list[int]] = {}
    for item in items:
        seqs.setdefault(item["logical_id"], []).append(item["seq"])

    def failures(predicate: Callable[[Any], bool], values: Iterable[Any]) -> int:
        return sum(1 for value in values if predicate(value))

    def duplicates(rows: Sequence[dict[str, Any]], keys: Sequence[str]) -> int:
        return len(rows) - len({tuple(row[key] for key in keys) for row in rows})

    input_counts: dict[str, int] = {}
    for row in inputs:
        input_counts[row["input_id"]] = input_counts.get(row["input_id"], 0) + 1
    shared_input_ids = sum(1 for count in input_counts.values() if count > 1)

    checks = {
        "meta_rows": "1",
        "meta_version": str(snapshot.version),
        "meta_engine": ENGINE,
        "meta_contract": CONTRACT,
        "sessions": str(len(sessions)),
        "items": str(len(items)),
        "inputs": str(len(inputs)),
        "bindings": str(len(bindings)),
        "turns": str(len(turns)),
        "items_payload_bytes": str(
            sum(len(row["payload_json"].encode("utf-8")) for row in items)
        ),
        "sessions_environment_bytes": str(
            sum(len(row["environment_json"].encode("utf-8")) for row in sessions)
        ),
        "last_item_seq_mismatch": str(
            failures(
                lambda row: row["last_item_seq"]
                != max(seqs.get(row["logical_id"], [0])),
                sessions,
            )
        ),
        "seq_starts_at_one": str(failures(lambda batch: min(batch) != 1, seqs.values())),
        "seq_gap_sessions": str(
            failures(lambda batch: len(batch) != max(batch) - min(batch) + 1, seqs.values())
        ),
        "duplicate_seq": str(duplicates(items, ("logical_id", "seq"))),
        "duplicate_inputs": str(duplicates(inputs, ("logical_id", "input_id"))),
        "duplicate_root_sessions": str(duplicates(sessions, ("root_session_id",))),
        "orphan_items": str(
            failures(lambda row: row["logical_id"] not in session_ids, items)
        ),
        "orphan_inputs": str(
            failures(lambda row: row["logical_id"] not in session_ids, inputs)
        ),
        "orphan_turns": str(
            failures(lambda row: row["logical_id"] not in session_ids, turns)
        ),
        "orphan_bindings": str(
            failures(lambda row: row["logical_id"] not in session_ids, bindings)
        ),
        "shared_input_ids": str(shared_input_ids),
    }
    # These must be 0: the loader already refused anything the gateway cannot
    # read. They stay real counts so the SQL side and this tool cannot silently
    # disagree about which shapes are accepted.
    tables: dict[str, list[dict[str, Any]]] = {
        "swarm_sessions": sessions,
        "swarm_items": items,
        "swarm_inputs": inputs,
        "swarm_turns": turns,
    }
    for table, columns in ENUM_COLUMNS.items():
        for column, allowed in columns:
            # Same names `03-verify.sql` prints; `state` alone would collide
            # between sessions and inputs.
            checks[f"invalid_{ENUM_CHECK_NAMES[table]}"] = str(
                failures(lambda row: row[column] not in allowed, tables[table])
            )
    for table, columns in JSON_COLUMNS.items():
        for column in columns:
            checks[f"invalid_{column}"] = str(
                failures(
                    lambda row: row[column] is not None and not is_json(row[column]),
                    tables[table],
                )
            )
    checks["null_environment_json"] = str(
        failures(lambda row: row["environment_json"] is None, sessions)
    )
    return checks


def is_json(text: str) -> bool:
    try:
        json.loads(text)
    except json.JSONDecodeError:
        return False
    return True


# ── SQL generation ─────────────────────────────────────────────────────────


def sql_literal(kind: str, value: Any) -> str:
    if value is None:
        return "NULL"
    if kind == BOOL:
        return "TRUE" if value else "FALSE"
    if kind == INT:
        return str(value)
    return "'" + str(value).replace("'", "''") + "'"


def insert_statements(
    table: str, columns: Sequence[tuple[str, str]], rows: Sequence[dict[str, Any]]
) -> list[str]:
    if not rows:
        return [f"-- {table}: no rows"]
    column_list = ", ".join(name for name, _ in columns)
    statements: list[str] = []
    for offset in range(0, len(rows), CHUNK_ROWS):
        chunk = rows[offset : offset + CHUNK_ROWS]
        values = ",\n    ".join(
            "("
            + ", ".join(sql_literal(kind, row[name]) for name, kind in columns)
            + ")"
            for row in chunk
        )
        statements.append(
            f"INSERT INTO {table} ({column_list}) VALUES\n    {values};"
        )
    return statements


def schema_sql(snapshot: Snapshot, created_at: str) -> str:
    parts = [
        "-- Generated by peri-swarm/scripts/import_loom_state.py",
        f"-- Source snapshot: {snapshot.source_description()}",
        "-- Target: PostgreSQL 16+, dedicated empty database, UTF8 encoding",
        "",
        "SET standard_conforming_strings = on;",
        "SET client_encoding = 'UTF8';",
        "",
        "BEGIN;",
        "",
        EMPTY_SCHEMA_GUARD_SQL + ";",
        "",
        CREATE_META_TABLE_SQL + ";",
        *[statement + ";" for statement in CREATE_TABLE_SQL],
        *[statement + ";" for statement in CREATE_INDEX_SQL],
        "",
        "INSERT INTO swarm_meta (singleton, version, engine, contract, created_at) VALUES",
        f"  (0, {SCHEMA_VERSION}, {sql_literal(TEXT, ENGINE)}, "
        f"{sql_literal(TEXT, CONTRACT)}, {sql_literal(TEXT, created_at)});",
        "",
        "COMMIT;",
    ]
    return "\n".join(parts) + "\n"


def data_sql(snapshot: Snapshot) -> str:
    parts = [
        "-- Generated by peri-swarm/scripts/import_loom_state.py",
        f"-- Source snapshot: {snapshot.source_description()}",
        "-- Rows are emitted in primary-key order; the guard refuses a non-empty target.",
        "",
        "SET standard_conforming_strings = on;",
        "SET client_encoding = 'UTF8';",
        "",
        "BEGIN;",
        "",
        EMPTY_TABLES_GUARD_SQL + ";",
        "",
    ]
    for table, columns, _ in TABLES:
        parts.append(f"-- {table}")
        parts.extend(insert_statements(table, columns, snapshot.table(table)))
        parts.append("")
    parts.append("COMMIT;")
    return "\n".join(parts) + "\n"


def verify_sql() -> str:
    """One `key=value` line per reconciliation check, in file order."""
    checks: list[tuple[str, str]] = [
        ("meta_rows", "SELECT count(*) FROM swarm_meta"),
        ("meta_version", "SELECT coalesce(max(version)::text, '') FROM swarm_meta"),
        ("meta_engine", "SELECT coalesce(max(engine), '') FROM swarm_meta"),
        ("meta_contract", "SELECT coalesce(max(contract), '') FROM swarm_meta"),
        ("sessions", "SELECT count(*) FROM swarm_sessions"),
        ("items", "SELECT count(*) FROM swarm_items"),
        ("inputs", "SELECT count(*) FROM swarm_inputs"),
        ("bindings", "SELECT count(*) FROM swarm_bindings"),
        ("turns", "SELECT count(*) FROM swarm_turns"),
        (
            "items_payload_bytes",
            "SELECT coalesce(sum(octet_length(payload_json)), 0) FROM swarm_items",
        ),
        (
            "sessions_environment_bytes",
            "SELECT coalesce(sum(octet_length(environment_json)), 0) FROM swarm_sessions",
        ),
        (
            "last_item_seq_mismatch",
            "SELECT count(*) FROM swarm_sessions s"
            " LEFT JOIN (SELECT logical_id, max(seq) AS max_seq FROM swarm_items GROUP BY 1) i"
            " USING (logical_id) WHERE s.last_item_seq <> coalesce(i.max_seq, 0)",
        ),
        (
            "seq_starts_at_one",
            "SELECT count(*) FROM (SELECT logical_id FROM swarm_items"
            " GROUP BY 1 HAVING min(seq) <> 1) bad",
        ),
        (
            "seq_gap_sessions",
            "SELECT count(*) FROM (SELECT logical_id FROM swarm_items"
            " GROUP BY 1 HAVING count(*) <> max(seq) - min(seq) + 1) bad",
        ),
        (
            "duplicate_seq",
            "SELECT count(*) - count(DISTINCT (logical_id, seq)) FROM swarm_items",
        ),
        (
            "duplicate_inputs",
            "SELECT count(*) - count(DISTINCT (logical_id, input_id)) FROM swarm_inputs",
        ),
        (
            "duplicate_root_sessions",
            "SELECT count(*) - count(DISTINCT root_session_id) FROM swarm_sessions",
        ),
        (
            "orphan_items",
            "SELECT count(*) FROM swarm_items i WHERE NOT EXISTS"
            " (SELECT 1 FROM swarm_sessions s WHERE s.logical_id = i.logical_id)",
        ),
        (
            "orphan_inputs",
            "SELECT count(*) FROM swarm_inputs i WHERE NOT EXISTS"
            " (SELECT 1 FROM swarm_sessions s WHERE s.logical_id = i.logical_id)",
        ),
        (
            "orphan_turns",
            "SELECT count(*) FROM swarm_turns t WHERE NOT EXISTS"
            " (SELECT 1 FROM swarm_sessions s WHERE s.logical_id = t.logical_id)",
        ),
        (
            "orphan_bindings",
            "SELECT count(*) FROM swarm_bindings b WHERE NOT EXISTS"
            " (SELECT 1 FROM swarm_sessions s WHERE s.logical_id = b.logical_id)",
        ),
        (
            "shared_input_ids",
            "SELECT count(*) FROM (SELECT input_id FROM swarm_inputs"
            " GROUP BY 1 HAVING count(*) > 1) bad",
        ),
        (
            "invalid_session_state",
            "SELECT count(*) FROM swarm_sessions WHERE state NOT IN ("
            + ",".join("'" + value + "'" for value in sorted(SESSION_STATES))
            + ")",
        ),
        (
            "invalid_item_kind",
            "SELECT count(*) FROM swarm_items WHERE kind NOT IN ("
            + ",".join("'" + value + "'" for value in sorted(ITEM_KINDS))
            + ")",
        ),
        (
            "invalid_input_state",
            "SELECT count(*) FROM swarm_inputs WHERE state NOT IN ("
            + ",".join("'" + value + "'" for value in sorted(INPUT_STATES))
            + ")",
        ),
        (
            "invalid_turn_status",
            "SELECT count(*) FROM swarm_turns WHERE status NOT IN ("
            + ",".join("'" + value + "'" for value in sorted(TURN_STATUSES))
            + ")",
        ),
    ]
    for table, columns in JSON_COLUMNS.items():
        for column in columns:
            checks.append(
                (
                    f"invalid_{column}",
                    f"SELECT count(*) FROM {table} WHERE {column} IS NOT NULL"
                    f" AND NOT ({column} IS JSON)",
                )
            )
    checks.append(
        (
            "null_environment_json",
            "SELECT count(*) FROM swarm_sessions WHERE environment_json IS NULL",
        )
    )

    parts = [
        "-- Generated by peri-swarm/scripts/import_loom_state.py",
        "-- Run with: psql -v ON_ERROR_STOP=1 -qAt -f 03-verify.sql",
        "-- Every result line is `check=value`; compare it with expected.json.",
        "",
    ]
    for name, query in checks:
        parts.append(f"SELECT '{name}=' || ({' '.join(query.split())})::text;")
    return "\n".join(parts) + "\n"


# ── commands ───────────────────────────────────────────────────────────────


def manifest(snapshot: Snapshot, created_at: str, include_ddl: bool) -> dict[str, Any]:
    return {
        "tool": "peri-swarm/scripts/import_loom_state.py",
        "created_at": created_at,
        "source_snapshot": str(snapshot.path),
        "source_sha256": snapshot.sha256,
        "source_wal_sha256": snapshot.wal_sha256,
        "legacy_meta_version": snapshot.version,
        "target_identity": {
            "version": SCHEMA_VERSION,
            "engine": ENGINE,
            "contract": CONTRACT,
        },
        "includes_ddl": include_ddl,
        "source_columns": snapshot.present_columns,
        "rows": {table: len(snapshot.table(table)) for table, _, _ in TABLES},
        "repairs": snapshot.repairs,
    }


def command_inspect(args: argparse.Namespace) -> int:
    snapshot = load_snapshot(Path(args.snapshot), args.immutable, strict=False)
    checks = expected_checks(snapshot)
    print(f"snapshot: {snapshot.path}")
    print(f"sha256:   {snapshot.sha256}")
    if snapshot.wal_sha256:
        print(f"wal sha256: {snapshot.wal_sha256}")
    print(f"legacy swarm_meta.version: {snapshot.version}")
    for table, _, _ in TABLES:
        print(f"  {table}: {len(snapshot.table(table))} row(s)")
    print("invariants:")
    for name in (
        "last_item_seq_mismatch",
        "seq_starts_at_one",
        "seq_gap_sessions",
        "duplicate_inputs",
        "duplicate_root_sessions",
        "shared_input_ids",
        "orphan_items",
        "orphan_inputs",
        "orphan_turns",
        "orphan_bindings",
    ):
        print(f"  {name}={checks[name]}")
    if snapshot.repairs:
        print("repairs `export` would apply (use --strict to fail instead):")
        for repair in snapshot.repairs:
            print(f"  - {repair}")
    print("status: importable")
    return 0


def command_export(args: argparse.Namespace) -> int:
    snapshot = load_snapshot(Path(args.snapshot), args.immutable, strict=args.strict)
    created_at = args.created_at or datetime.datetime.now(
        datetime.timezone.utc
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    written: list[Path] = []
    if not args.no_ddl:
        written.append(write(out_dir / "01-schema.sql", schema_sql(snapshot, created_at)))
    written.append(write(out_dir / "02-data.sql", data_sql(snapshot)))
    written.append(write(out_dir / "03-verify.sql", verify_sql()))
    written.append(
        write(
            out_dir / "expected.json",
            json.dumps(
                {
                    "checks": expected_checks(snapshot),
                    "manifest": manifest(snapshot, created_at, not args.no_ddl),
                },
                indent=2,
                ensure_ascii=False,
            )
            + "\n",
        )
    )

    print(f"snapshot: {snapshot.source_description()}")
    for table, _, _ in TABLES:
        print(f"  {table}: {len(snapshot.table(table))} row(s)")
    for repair in snapshot.repairs:
        print(f"repair: {repair}")
    print(f"identity created_at: {created_at}")
    print("written:")
    for path in written:
        print(f"  {path}")
    print("next:")
    if not args.no_ddl:
        print(
            '  psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 '
            f"-f {out_dir}/01-schema.sql"
        )
    print(
        '  psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 '
        f"-f {out_dir}/02-data.sql"
    )
    print(
        '  psql "$PERI_SWARM_STATE_STORE" -v ON_ERROR_STOP=1 -qAt '
        f"-f {out_dir}/03-verify.sql > {out_dir}/observed.txt"
    )
    print(
        "  python3 peri-swarm/scripts/import_loom_state.py check "
        f"--expected {out_dir}/expected.json --observed {out_dir}/observed.txt"
    )
    return 0


def write(path: Path, content: str) -> Path:
    path.write_text(content, encoding="utf-8")
    return path


def command_check(args: argparse.Namespace) -> int:
    expected = json.loads(Path(args.expected).read_text(encoding="utf-8"))["checks"]
    observed: dict[str, str] = {}
    for line in Path(args.observed).read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or "=" not in line:
            continue
        name, _, value = line.partition("=")
        observed[name.strip()] = value.strip()
    problems: list[str] = []
    for name, value in expected.items():
        if name not in observed:
            problems.append(f"missing check {name} (expected {value})")
        elif observed[name] != value:
            problems.append(f"{name}: expected {value}, observed {observed[name]}")
    for name in observed:
        if name not in expected:
            problems.append(f"unexpected check {name}={observed[name]}")
    if problems:
        print("reconciliation FAILED:")
        for problem in problems:
            print(f"  - {problem}")
        return 1
    print(f"reconciliation OK: {len(expected)} checks match")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Offline import of a legacy Loom/libSQL Swarm state snapshot into PostgreSQL."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    sub = parser.add_subparsers(dest="command", required=True)

    def common(name: str, help_text: str) -> argparse.ArgumentParser:
        subparser = sub.add_parser(name, help=help_text)
        subparser.add_argument(
            "--snapshot", required=True, help="read-only legacy SQLite snapshot"
        )
        subparser.add_argument(
            "--immutable",
            action="store_true",
            help="open a checkpointed snapshot with immutable=1; a non-empty -wal is refused",
        )
        return subparser

    common("inspect", "validate a snapshot and print its invariants").set_defaults(
        func=command_inspect
    )
    export = common("export", "write the import and reconciliation SQL")
    export.add_argument("--out-dir", required=True)
    export.add_argument(
        "--created-at",
        help="RFC3339 timestamp for swarm_meta.created_at (default: now, UTC seconds)",
    )
    export.add_argument(
        "--strict",
        action="store_true",
        help="fail instead of repairing legacy crash artefacts",
    )
    export.add_argument(
        "--no-ddl",
        action="store_true",
        help="skip 01-schema.sql (the target was already initialized by the new build)",
    )
    export.set_defaults(func=command_export)

    check = sub.add_parser("check", help="compare psql output with expected.json")
    check.add_argument("--expected", required=True)
    check.add_argument("--observed", required=True)
    check.set_defaults(func=command_check)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.func(args)
    except ImportError as error:
        print(f"refusing to import: {error}", file=sys.stderr)
        return 2
    except sqlite3.Error as error:
        print(
            f"refusing to import: cannot read the snapshot ({error}); "
            "make sure it is a plain SQLite/libSQL file and copy its -wal/-shm "
            "siblings when it was not checkpointed",
            file=sys.stderr,
        )
        return 2


if __name__ == "__main__":
    sys.exit(main())
