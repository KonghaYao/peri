import { Database } from "bun:sqlite";
import { ExecutionAdmissionCore, type ExecutionAdmissionCoreOptions,
    type AdmissionLedger, type AdmissionRequestRecord, type AdmissionStepClaim } from "./admission-core";
import { SqliteExecutionRegistry } from "./sqlite-registry";
import type { ExecutionMutation } from "./types";

export type { AdmissionSnapshot, AdmissionRequest, AdmissionOutcome, SettlementRequest, SettlementOutcome, EntryRequest, EntryOutcome } from "./admission-core";
type ExecutionAdmissionServiceOptions = Omit<ExecutionAdmissionCoreOptions, "registry" | "ledger" | "allowBestEffort"> & { database: string };

class SqliteAdmissionLedger implements AdmissionLedger {
    private readonly database: Database;
    constructor(path: string) {
        this.database = new Database(path);
        try {
            this.database.exec("PRAGMA busy_timeout=5000; PRAGMA synchronous=FULL");
            this.database.exec("CREATE TABLE IF NOT EXISTS sdk_admission_requests (request_id TEXT PRIMARY KEY, digest TEXT NOT NULL, ticket TEXT NOT NULL)");
            this.database.exec("CREATE TABLE IF NOT EXISTS sdk_admission_steps (step_id TEXT PRIMARY KEY, command TEXT NOT NULL)");
        } catch (error) { this.database.close(); throw error; }
    }
    async readRequest(requestKey: string): Promise<AdmissionRequestRecord | null> {
        const row = this.database.query<{ digest: string; ticket: string }, [string]>("SELECT digest,ticket FROM sdk_admission_requests WHERE request_id=?").get(requestKey);
        return row ? { digest: row.digest, ticket: JSON.parse(row.ticket) } : null;
    }
    async claimRequest(requestKey: string, proposed: AdmissionRequestRecord): Promise<AdmissionRequestRecord> {
        this.database.query("INSERT OR IGNORE INTO sdk_admission_requests VALUES (?,?,?)").run(requestKey, proposed.digest, JSON.stringify(proposed.ticket));
        return (await this.readRequest(requestKey))!;
    }
    async claimStep(stepId: string, proposed: ExecutionMutation): Promise<AdmissionStepClaim> {
        const inserted = this.database.query("INSERT OR IGNORE INTO sdk_admission_steps VALUES (?,?)").run(stepId, JSON.stringify(proposed));
        const row = this.database.query<{ command: string }, [string]>("SELECT command FROM sdk_admission_steps WHERE step_id=?").get(stepId)!;
        return { inserted: inserted.changes === 1, command: JSON.parse(row.command) };
    }
    close(): void { this.database.close(); }
}

export class ExecutionAdmissionService extends ExecutionAdmissionCore {
    private readonly sqliteRegistry: SqliteExecutionRegistry;
    private readonly sqliteLedger: SqliteAdmissionLedger;
    constructor(private readonly serviceOptions: ExecutionAdmissionServiceOptions) {
        const registry = new SqliteExecutionRegistry({ path: serviceOptions.database });
        let ledger: SqliteAdmissionLedger;
        try { ledger = new SqliteAdmissionLedger(serviceOptions.database); }
        catch (error) { registry.close(); throw error; }
        super({ ...serviceOptions, registry, ledger });
        this.sqliteRegistry = registry;
        this.sqliteLedger = ledger;
    }
    close(): void { this.sqliteLedger.close(); this.sqliteRegistry.close(); }
    instanceSessions(): Promise<string[]> { return this.sqliteRegistry.instanceSessions(this.serviceOptions.instance); }
}
