pub(crate) const FORMAT_VERSION: i64 = 1;

pub(crate) const CREATE_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS session_work_head (
        session_id TEXT NOT NULL, lifecycle INTEGER NOT NULL CHECK(lifecycle>=0),
        format_version INTEGER NOT NULL CHECK(format_version=1),
        revision INTEGER NOT NULL CHECK(revision>=0),
        next_delivery_seq INTEGER NOT NULL CHECK(next_delivery_seq>=0),
        record_json TEXT NOT NULL, PRIMARY KEY(session_id,lifecycle))",
    "CREATE TABLE IF NOT EXISTS session_inputs (
        session_id TEXT NOT NULL, lifecycle INTEGER NOT NULL CHECK(lifecycle>=0),
        input_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>=0),
        fifo_seq INTEGER NOT NULL CHECK(fifo_seq>=0),
        generation INTEGER NOT NULL CHECK(generation>=0),
        status TEXT NOT NULL CHECK(status IN ('queued','published','withdrawn')),
        record_json TEXT NOT NULL, PRIMARY KEY(session_id,lifecycle,input_id),
        UNIQUE(session_id,lifecycle,fifo_seq))",
    "CREATE TABLE IF NOT EXISTS session_deliveries (
        delivery_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL,
        lifecycle INTEGER NOT NULL CHECK(lifecycle>=0),
        revision INTEGER NOT NULL CHECK(revision>=0),
        sequence INTEGER NOT NULL CHECK(sequence>=0),
        producer_namespace TEXT NOT NULL, event_id TEXT NOT NULL, purpose TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('pending','inProgress','satisfied','blocked','suppressed','abandoned')),
        processing_id TEXT, batch_ordinal INTEGER CHECK(batch_ordinal>=0),
        record_json TEXT NOT NULL, UNIQUE(session_id,lifecycle,sequence),
        UNIQUE(producer_namespace,event_id,session_id,lifecycle,purpose),
        UNIQUE(processing_id,batch_ordinal))",
    "CREATE TABLE IF NOT EXISTS session_processing (
        processing_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL,
        lifecycle INTEGER NOT NULL CHECK(lifecycle>=0),
        revision INTEGER NOT NULL CHECK(revision>=0),
        phase_sequence INTEGER NOT NULL CHECK(phase_sequence>=0),
        phase TEXT NOT NULL CHECK(phase IN ('reasonReady','reasonInFlight','actReady','blocked','settled','abandoned','legacyUnknown')),
        parent_session TEXT, delegation_id TEXT, record_json TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS session_effects (
        invocation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL,
        lifecycle INTEGER NOT NULL CHECK(lifecycle>=0),
        revision INTEGER NOT NULL CHECK(revision>=0), processing_id TEXT,
        phase_sequence INTEGER NOT NULL CHECK(phase_sequence>=0), tool_call_id TEXT,
        status TEXT NOT NULL CHECK(status IN ('prepared','dispatchAccepted','outcomeUnknown','settled')),
        owner_identity TEXT, owner_task_id TEXT, child_session TEXT,
        parent_session TEXT, delegation_id TEXT, record_json TEXT NOT NULL,
        UNIQUE(processing_id,phase_sequence,tool_call_id),
        UNIQUE(session_id,owner_identity,owner_task_id), UNIQUE(parent_session,delegation_id))",
    "CREATE TABLE IF NOT EXISTS session_payloads (
        storage_scope TEXT NOT NULL, payload_id TEXT NOT NULL,
        kind TEXT NOT NULL, codec TEXT NOT NULL, version INTEGER NOT NULL CHECK(version>0),
        byte_length INTEGER NOT NULL CHECK(byte_length>=0), sha256 TEXT NOT NULL,
        bytes BLOB NOT NULL, retention_class TEXT NOT NULL,
        PRIMARY KEY(storage_scope,payload_id),
        UNIQUE(storage_scope,kind,codec,version,sha256,byte_length),
        CHECK(length(bytes)=byte_length))",
    "CREATE TABLE IF NOT EXISTS session_work_events (
        event_key TEXT PRIMARY KEY NOT NULL, event_json TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS session_work_receipts (
        mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL,
        digest TEXT NOT NULL, resolution_json TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS session_work_commands (
        mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL,
        digest TEXT NOT NULL, command_json TEXT NOT NULL,
        command_scope TEXT, command_payload_id TEXT,
        kind TEXT NOT NULL DEFAULT 'mutation', subject_id TEXT, lifecycle INTEGER CHECK(lifecycle>=0),
        record_revision INTEGER CHECK(record_revision>=0), guard_token TEXT, record_json TEXT,
        reconciled INTEGER NOT NULL DEFAULT 0 CHECK(reconciled IN (0,1)))",
];

pub(crate) const CREATE_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_inputs_pending ON session_inputs(session_id,lifecycle,status,fifo_seq,input_id)",
    "CREATE INDEX IF NOT EXISTS idx_inputs_publication ON session_inputs(session_id,lifecycle,json_extract(record_json,'$.publicationId'))",
    "CREATE INDEX IF NOT EXISTS idx_deliveries_pending ON session_deliveries(session_id,lifecycle,status,sequence,delivery_id)",
    "CREATE INDEX IF NOT EXISTS idx_deliveries_processing ON session_deliveries(processing_id,batch_ordinal,delivery_id)",
    "CREATE INDEX IF NOT EXISTS idx_processing_phase ON session_processing(session_id,lifecycle,phase,processing_id)",
    "CREATE INDEX IF NOT EXISTS idx_processing_delegation ON session_processing(parent_session,delegation_id,processing_id)",
    "CREATE INDEX IF NOT EXISTS idx_effects_status ON session_effects(session_id,lifecycle,status,invocation_id)",
    "CREATE INDEX IF NOT EXISTS idx_effects_processing ON session_effects(processing_id,phase_sequence,invocation_id)",
    "CREATE INDEX IF NOT EXISTS idx_effects_child ON session_effects(child_session,delegation_id)",
    "CREATE INDEX IF NOT EXISTS idx_effects_task ON session_effects(session_id,owner_task_id,invocation_id)",
    "CREATE INDEX IF NOT EXISTS idx_work_commands_pending ON session_work_commands(session_id,mutation_id) WHERE reconciled=0",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_work_records ON session_work_commands(session_id,lifecycle,kind,subject_id) WHERE kind<>'mutation'",
];

pub(crate) fn initialization_sql() -> impl Iterator<Item = &'static str> {
    CREATE_TABLES.iter().chain(CREATE_INDEXES).copied()
}
